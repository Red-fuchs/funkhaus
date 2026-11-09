//! Sync (Etappe 6): SRT/VTT cues -> one script line per cue + `sync.json` (times), TTS per cue, fit into the cue slot.
//!
//! Fit ladder per cue (`fit`): fits (≤ slot) | atempo (≤ `limit` × slot, ffmpeg `atempo` to exactly the slot; `limit` from
//! sync.json, default `LIMIT`) | too_long (placed in full, overhang reported, never cut) | overlap (cue windows collide, or
//! the clip reaches the next cue). Cues are kept in file order; neighbours/room are taken by start time.
//! `check` estimates (chars / the persona's speaking rate measured from cached takes, else 13 per s; no TTS); `render`
//! measures the rendered, trimmed clips.
//!
//! Files: `<proj>/sync.json` (`stimmwerk.sync/1`, index-aligned with script.txt), clips in `takes/<q>/sync/` (atempo
//! versions `<hash>-<factor>.wav` next to them), own takes in `takes/sync/rerolls.json` (a script render never prunes
//! them), mix `out/sync.wav` (cue starts are absolute).
use crate::project::{self, est_secs, est_secs_at};
use crate::render::{self, Ctx, Event, Prog, Quality, Say, State};
use crate::script::{Item, Script, id_ok};
use crate::{Project, import::id_of};
use anyhow::{Context, Result, bail};
use funkhaus_audio::{mix, script::Line};
use funkhaus_voice::tts;
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

pub const LIMIT: f64 = 1.12;
/// Accepted `limit` in sync.json (atempo factor range).
pub const LIMIT_RANGE: std::ops::RangeInclusive<f64> = 1.0..=2.0;
pub const SCHEMA: &str = "stimmwerk.sync/1";
pub const REPORT_SCHEMA: &str = "stimmwerk.sync.report/1";
/// Script text of an empty cue: silence in its slot, no TTS (`ID|0|` would not parse).
pub const SILENT: &str = "[pause 0s]";
/// Accepted deviation of an atempo clip from its slot (s).
pub const TOL: f64 = 0.005;

// ---------- parsing ----------

#[derive(Clone, Debug, PartialEq)]
pub struct Cue {
    /// 1-based order in the file
    pub idx: usize,
    /// ms
    pub start: u32,
    pub end: u32,
    /// tags stripped, lines joined with " "
    pub text: String,
    /// VTT `<v Name>`
    pub voice: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Parsed {
    pub cues: Vec<Cue>,
    /// skipped blocks
    pub warnings: Vec<String>,
}

/// `.vtt`/`.srt` by extension, else `WEBVTT` sniff.
pub fn is_vtt(src: &str, name: &str) -> bool {
    match Path::new(name).extension().map(|e| e.to_string_lossy().to_lowercase()).as_deref() {
        Some("vtt") => true,
        Some("srt") => false,
        _ => src.trim_start_matches('\u{feff}').starts_with("WEBVTT"),
    }
}

pub fn parse(src: &str, name: &str) -> Result<Parsed> {
    if is_vtt(src, name) { parse_vtt(src) } else { parse_srt(src) }
}

pub fn parse_srt(src: &str) -> Result<Parsed> {
    cues(&blocks(src), false)
}

pub fn parse_vtt(src: &str) -> Result<Parsed> {
    let mut b = blocks(src);
    let head = b.first().map(|(_, l)| l[0]).unwrap_or("");
    if !(head == "WEBVTT" || head.starts_with("WEBVTT ") || head.starts_with("WEBVTT\t")) {
        bail!("keine WebVTT-Datei: erste Zeile muss WEBVTT sein");
    }
    // no blank line after the header: the first cue shares its block -> split at the timing line
    let mut warnings = vec![];
    if let Some(ai) = b[0].1.iter().position(|l| l.contains("-->")) {
        let (at, lines) = b.remove(0);
        warnings.push(format!("Zeile {}: Cue klebt am WEBVTT-Kopf (keine Leerzeile davor)", at + ai));
        b.insert(0, (at + ai, lines[ai..].to_vec()));
    } else {
        b.remove(0);
    }
    let mut r = cues(&b, true)?;
    warnings.append(&mut r.warnings);
    r.warnings = warnings;
    Ok(r)
}

/// Blocks separated by blank lines, with their 1-based first line number (BOM stripped).
fn blocks(src: &str) -> Vec<(usize, Vec<&str>)> {
    let src = src.strip_prefix('\u{feff}').unwrap_or(src);
    let (mut out, mut cur, mut at) = (vec![], vec![], 0);
    for (i, l) in src.lines().enumerate() {
        if l.trim().is_empty() {
            if !cur.is_empty() {
                out.push((at, std::mem::take(&mut cur)));
            }
        } else {
            if cur.is_empty() {
                at = i + 1;
            }
            cur.push(l);
        }
    }
    if !cur.is_empty() {
        out.push((at, cur));
    }
    out
}

fn cues(blocks: &[(usize, Vec<&str>)], vtt: bool) -> Result<Parsed> {
    let mut r = Parsed::default();
    for (at, b) in blocks {
        let first = b[0].trim();
        let word = first.split_whitespace().next().unwrap_or("");
        let arrow = b.iter().position(|l| l.contains("-->"));
        if vtt && matches!(word, "NOTE" | "STYLE" | "REGION") {
            if word == "NOTE" && arrow.is_some() {
                r.warnings.push(format!("Zeile {at}: NOTE-Block mit „-->“ übersprungen"));
            }
            continue;
        }
        let Some(ai) = arrow else {
            r.warnings.push(format!("Zeile {at}: Block ohne Zeitangabe übersprungen"));
            continue;
        };
        let ln = at + ai;
        let (a, rest) = b[ai].split_once("-->").expect("contains -->");
        let bad = |t: &str| {
            let want = if vtt { "[HH:]MM:SS.mmm" } else { "HH:MM:SS,mmm" };
            anyhow::anyhow!("Zeile {ln}: Zeitstempel {t:?} ungültig (erwartet {want})")
        };
        let (a, z) = (a.trim(), rest.split_whitespace().next().unwrap_or(""));
        let start = ts(a, vtt).ok_or_else(|| bad(a))?;
        let end = ts(z, vtt).ok_or_else(|| bad(z))?;
        if end < start {
            bail!("Zeile {ln}: Ende {z} liegt vor dem Anfang {a}");
        }
        let raw = b[ai + 1..].join(" ");
        r.cues.push(Cue { idx: r.cues.len() + 1, start, end, text: clean(&raw), voice: voice(&raw) });
    }
    if let Some(i) = r.cues.windows(2).position(|w| w[1].start < w[0].start) {
        r.warnings.push(format!("Cue {} beginnt vor Cue {}: Datei nicht nach Zeit sortiert (Reihenfolge bleibt, Nachbarn nach Zeit)", i + 2, i + 1));
    }
    Ok(r)
}

/// ms of `HH:MM:SS,mmm` (SRT, `.` tolerated) or `[HH:][MM:]SS.mmm` (VTT, dot only). Hours may exceed 99; a field longer
/// than 9 digits (or a result past `u32`, ~1193 h) is invalid.
fn ts(s: &str, vtt: bool) -> Option<u32> {
    let (main, frac) = s.rsplit_once(['.', ','])?;
    if (vtt && s.contains(',')) || frac.is_empty() || frac.len() > 3 || !frac.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let ms: u64 = format!("{frac:0<3}").parse().ok()?;
    let digits = |x: &str| (1..=9).contains(&x.len()) && x.bytes().all(|b| b.is_ascii_digit());
    let p: Vec<u64> = main.split(':').map(|x| if digits(x) { x.parse().ok() } else { None }).collect::<Option<_>>()?;
    let (h, m, sec) = match p[..] {
        [h, m, s] => (h, m, s),
        [m, s] if vtt => (0, m, s),
        [s] if vtt => (0, 0, s),
        _ => return None,
    };
    if (p.len() > 1 && sec >= 60) || (p.len() > 2 && m >= 60) {
        return None;
    }
    u32::try_from(((h * 60 + m) * 60 + sec) * 1000 + ms).ok()
}

/// Name of the first `<v Name>` / `<v.class Name>` tag.
fn voice(s: &str) -> Option<String> {
    let mut rest = s;
    while let Some(a) = rest.find("<v") {
        let t = &rest[a + 2..];
        let end = t.find('>')?;
        if t.starts_with([' ', '\t', '.']) {
            let name = t[..end].split_once(char::is_whitespace).map(|(_, n)| n.split_whitespace().collect::<Vec<_>>().join(" "));
            if let Some(n) = name.filter(|n| !n.is_empty()) {
                return Some(n);
            }
        }
        rest = &t[end..];
    }
    None
}

/// Strips `<tags>` (`<` followed by a letter, `/` or a `<hh:mm:ss.mmm>` timestamp; a raw `<` as in "3 < 5" stays) and the
/// common entities, collapses whitespace.
fn clean(s: &str) -> String {
    let is_tag = |rest: &str| {
        let body = rest.split('>').next().unwrap_or("");
        rest.starts_with(|c: char| c.is_ascii_alphabetic() || c == '/') || (body.contains(':') && body.chars().all(|c| c.is_ascii_digit() || c == ':' || c == '.'))
    };
    let (mut out, mut tag) = (String::new(), false);
    for (i, c) in s.char_indices() {
        match c {
            '<' if !tag && is_tag(&s[i + 1..]) => tag = true,
            '>' if tag => tag = false,
            _ if !tag => out.push(c),
            _ => {}
        }
    }
    let out = [("&lt;", "<"), ("&gt;", ">"), ("&nbsp;", " "), ("&quot;", "\""), ("&#39;", "'"), ("&apos;", "'"), ("&amp;", "&")].iter().fold(out, |o, (e, r)| o.replace(e, r));
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Indices of `cues` sorted by start (stable: file order for equal starts).
fn order(cues: &[Cue]) -> Vec<usize> {
    let mut o: Vec<usize> = (0..cues.len()).collect();
    o.sort_by_key(|&i| cues[i].start);
    o
}

/// All pairs (file indices, `a < b`) whose windows collide (`start < end` of the other, strict: touching is fine), in any
/// file order.
pub fn overlaps(cues: &[Cue]) -> Vec<(usize, usize)> {
    let o = order(cues);
    let mut out = vec![];
    for (k, &i) in o.iter().enumerate() {
        // sorted by start: once a later cue starts at/after my end, no further one can collide; the second condition
        // keeps a zero-length cue at my start out
        out.extend(o[k + 1..].iter().take_while(|&&j| cues[j].start < cues[i].end).filter(|&&j| cues[i].start < cues[j].end).map(|&j| (i.min(j), i.max(j))));
    }
    out.sort_unstable();
    out
}

// ---------- fit ----------

#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Fits,
    Atempo,
    TooLong,
    Overlap,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Fit {
    pub kind: Kind,
    /// measured / slot (1.0 for fits; +inf for a zero slot)
    pub factor: f64,
    /// seconds past the cue end (too_long: clipped at the next cue; overlap: up to the next cue)
    pub overhang: f64,
}

/// `slot` = cue length, `room` = next.start - cur.start (None = last cue), `measured` = speech length (all s), `limit` =
/// maximum atempo factor (sync.json `limit`, default `LIMIT`).
pub fn fit(slot: f64, room: Option<f64>, measured: f64, limit: f64) -> Fit {
    let room = room.unwrap_or(f64::INFINITY);
    let f = measured / slot;
    let at = |kind, factor, overhang| Fit { kind, factor, overhang };
    if slot > room {
        at(Kind::Overlap, f, 0.0)
    } else if measured <= slot {
        at(Kind::Fits, 1.0, 0.0)
    } else if f <= limit {
        at(Kind::Atempo, f, 0.0)
    } else if measured > room {
        at(Kind::Overlap, f, room - slot)
    } else {
        at(Kind::TooLong, f, measured.min(room) - slot)
    }
}

fn secs(ms: u32) -> f64 {
    ms as f64 / 1000.0
}

fn slot_of(c: &Cue) -> f64 {
    secs(c.end.saturating_sub(c.start))
}

/// Start (ms) of the cue following each cue by time (None: last), indexed in file order.
fn next_starts(cues: &[Cue]) -> Vec<Option<u32>> {
    let mut nxt = vec![None; cues.len()];
    for w in order(cues).windows(2) {
        nxt[w[0]] = Some(cues[w[1]].start);
    }
    nxt
}

/// Fit per cue with `measured`; a pair from `overlaps` is overlap on both sides (factor/overhang kept for information).
fn fits(cues: &[Cue], measured: &[f64], limit: f64) -> Vec<Fit> {
    debug_assert_eq!(cues.len(), measured.len());
    let mut flag = vec![false; cues.len()];
    for (a, b) in overlaps(cues) {
        (flag[a], flag[b]) = (true, true);
    }
    cues.iter()
        .zip(next_starts(cues))
        .enumerate()
        .map(|(i, (c, nx))| {
            let f = fit(slot_of(c), nx.map(|n| secs(n.saturating_sub(c.start))), measured[i], limit);
            if flag[i] { Fit { kind: Kind::Overlap, ..f } } else { f }
        })
        .collect()
}

/// Estimate: measured = spoken chars / 13 (+ `[pause]`s), limit `LIMIT`.
pub fn dry_run(cues: &[Cue]) -> Vec<Fit> {
    fits(cues, &cues.iter().map(|c| est_secs(&c.text)).collect::<Vec<_>>(), LIMIT)
}

// ---------- project file ----------

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SyncCue {
    pub start_ms: u32,
    pub end_ms: u32,
    /// speaker ID (= script line)
    pub who: String,
    /// text as in script.txt (empty cue: `SILENT`)
    pub text: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Sync {
    pub schema: String,
    pub limit: f64,
    /// imported file name (no path)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    pub cues: Vec<SyncCue>,
    /// differences to script.txt taken over by `load` (script text/speaker wins)
    #[serde(skip)]
    pub notes: Vec<String>,
}

impl Sync {
    pub fn save(&self, dir: &Path) -> Result<()> {
        project::write(&dir.join(project::SYNC), &(serde_json::to_string_pretty(self)? + "\n"))
    }

    pub fn lines(&self) -> Vec<Line> {
        self.cues.iter().map(|c| Line::new(c.who.clone(), 0.0, c.text.clone())).collect()
    }

    fn as_cues(&self) -> Vec<Cue> {
        self.cues.iter().enumerate().map(|(i, c)| Cue { idx: i + 1, start: c.start_ms, end: c.end_ms, text: c.text.clone(), voice: None }).collect()
    }
}

/// `sync.json` of a project, aligned with its script (None: no sync project). Line count must match; a changed text or
/// speaker in script.txt wins (noted); both changed in one line = lines moved -> error.
pub fn load(p: &Project) -> Result<Option<Sync>> {
    let f = p.dir.join(project::SYNC);
    let s = match std::fs::read_to_string(&f) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| f.display().to_string()),
    };
    let mut y: Sync = serde_json::from_str(&s).with_context(|| f.display().to_string())?;
    if y.schema != SCHEMA {
        bail!("{}: Schema {:?} nicht unterstützt (erwartet {SCHEMA})", f.display(), y.schema);
    }
    if !(y.limit.is_finite() && LIMIT_RANGE.contains(&y.limit)) {
        bail!("{}: limit {} außerhalb {}..{} (Standard {LIMIT})", f.display(), y.limit, LIMIT_RANGE.start(), LIMIT_RANGE.end());
    }
    if let Some((i, c)) = y.cues.iter().enumerate().find(|(_, c)| c.end_ms < c.start_ms) {
        bail!("{}: Cue {} endet ({} ms) vor seinem Anfang ({} ms)", f.display(), i + 1, c.end_ms, c.start_ms);
    }
    if let Some(n) = p.script.bad().first() {
        bail!("script.txt Zeile {n} fehlerhaft – `stimmwerk lint`");
    }
    let ls: Vec<&Line> = p.script.lines().collect();
    let fix = "Zeiten stehen in sync.json: Zeilen nur ändern, nicht einfügen/löschen/verschieben; sonst neu importieren mit `stimmwerk sync import --replace`";
    if ls.len() != y.cues.len() {
        bail!("sync.json hat {} Cues, script.txt {} Zeilen – {fix}", y.cues.len(), ls.len());
    }
    for (i, (c, l)) in y.cues.iter_mut().zip(ls).enumerate() {
        if l.over {
            bail!("Cue {}: Einwürfe (+ID) gibt es im Sync-Projekt nicht – {fix}", i + 1);
        }
        if l.gap != 0.0 {
            y.notes.push(format!("Cue {}: GAP {} in script.txt wird ignoriert (Zeiten stehen in sync.json)", i + 1, l.gap));
        }
        let (dw, dt) = (c.who != l.who, c.text != l.text);
        if dw && dt {
            bail!("Cue {}: Sprecher und Text weichen von script.txt ab (Zeilen verschoben?) – {fix}", i + 1);
        }
        if dw || dt {
            y.notes.push(format!("Cue {}: {} in script.txt geändert", i + 1, if dw { "Sprecher" } else { "Text" }));
            (c.who, c.text) = (l.who.clone(), l.text.clone());
        }
    }
    Ok(Some(y))
}

#[derive(Serialize, Debug, Clone, PartialEq)]
pub struct ImportReport {
    /// srt | vtt
    pub format: &'static str,
    pub cues: usize,
    /// cues without text (silence in their slot)
    pub empty: usize,
    pub speakers_added: Vec<String>,
    pub warnings: Vec<String>,
}

/// SRT/VTT -> script.txt (one line per cue, gap 0) + sync.json. A non-empty script is only replaced with `replace`.
/// Speaker: VTT `<v Name>` -> `id_of(Name)` (label = Name), else `speaker`, the first speaker, or `ERZ`.
/// Writes sync.json first, then project.toml + script.txt (`load` catches a half-done import by the line count).
pub fn import(p: &mut Project, src: &str, name: &str, speaker: Option<&str>, replace: bool) -> Result<ImportReport> {
    if !p.script.is_empty() && !replace {
        bail!("script.txt ist nicht leer – mit --replace ersetzen (Skript und Zeiten werden überschrieben, Besetzung bleibt)");
    }
    let ps = parse(src, name)?;
    if ps.cues.is_empty() {
        bail!("keine Cues in {name}");
    }
    let fallback = speaker.map(String::from).or_else(|| p.meta.speakers.keys().next().cloned()).unwrap_or_else(|| crate::import::DEFAULT_SPEAKER.into());
    if !id_ok(&fallback) {
        bail!("Sprecher-ID {fallback:?} ungültig (Buchstaben, Ziffern, _ -; nicht B – reserviert für Einwürfe)");
    }
    let mut added = vec![];
    let mut cues = vec![];
    for c in &ps.cues {
        let (id, label) = match &c.voice {
            Some(v) => (id_of(v), Some(v.as_str())),
            None => (fallback.clone(), None),
        };
        if p.add_speaker(&id, label.filter(|l| *l != id)) {
            added.push(id.clone());
        }
        let text = if c.text.is_empty() { SILENT.to_string() } else { c.text.clone() };
        cues.push(SyncCue { start_ms: c.start, end_ms: c.end, who: id, text });
    }
    let fmt = if is_vtt(src, name) { "vtt" } else { "srt" };
    let s = Sync { schema: SCHEMA.into(), limit: LIMIT, source: Path::new(name).file_name().map(|f| f.to_string_lossy().into_owned()), cues, notes: vec![] };
    let script = Script { items: s.lines().into_iter().map(Item::Line).collect() };
    // every cue must read back as the same line (no text that script.txt would mangle)
    let back = Script::parse(&script.format())?;
    if back != script {
        bail!("Cue-Text lässt sich nicht verlustfrei in script.txt schreiben");
    }
    s.save(&p.dir)?;
    p.script = script;
    p.save()?;
    Ok(ImportReport { format: fmt, cues: s.cues.len(), empty: ps.cues.iter().filter(|c| c.text.is_empty()).count(), speakers_added: added, warnings: ps.warnings })
}

// ---------- report ----------

#[derive(Serialize, Debug, Clone)]
pub struct CueRep {
    /// 1-based
    pub index: usize,
    pub start: f64,
    pub end: f64,
    /// end - start
    pub slot: f64,
    /// = slot (target length)
    pub target: f64,
    /// start of the next cue by time (absolute, s)
    pub next_start: Option<f64>,
    pub text: String,
    pub who: String,
    /// spoken characters
    pub chars: usize,
    /// estimate chars / `rate` (+ pauses)
    pub needed: f64,
    /// speaking rate behind `needed` (chars/s): measured from this persona's cached takes, else 13
    pub rate: f64,
    /// estimate (`check`) or trimmed clip length (`render`)
    pub measured: f64,
    /// = measured (speech duration)
    pub duration: f64,
    /// measured / slot; 1.0 for fits; null for a zero slot
    pub factor: Option<f64>,
    pub overhang: f64,
    /// seconds the speech would have to lose to fit at the maximum atempo factor (too_long / overlap; else 0)
    pub cut_s: f64,
    pub status: Kind,
    pub warning: Option<String>,
    /// LLM shortening (Etappe 7), null for now
    pub suggestion: Option<()>,
}

#[derive(Serialize, Debug, Clone, PartialEq)]
pub struct Summary {
    pub total: usize,
    pub fits: usize,
    pub atempo: usize,
    pub too_long: usize,
    pub overlap: usize,
    pub max_factor: Option<f64>,
    /// sum of `cut_s`
    pub cut_s: f64,
    /// no too_long / overlap
    pub ok: bool,
}

/// What `render` did.
#[derive(Serialize, Debug, Clone)]
pub struct RenderInfo {
    pub quality: Quality,
    pub tts_calls: usize,
    /// 1-based cues that needed TTS
    pub rendered: Vec<usize>,
    pub cached: usize,
    /// ffmpeg atempo runs / atempo clips reused
    pub atempo_runs: usize,
    pub atempo_cached: usize,
    /// out/sync.wav
    pub wav: PathBuf,
    pub secs: f64,
    pub elapsed: f64,
}

#[derive(Serialize, Debug, Clone)]
pub struct Report {
    pub schema: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input: Option<String>,
    pub limit: f64,
    /// estimate | rendered
    pub measure: &'static str,
    pub cues: Vec<CueRep>,
    pub warnings: Vec<String>,
    pub summary: Summary,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub render: Option<RenderInfo>,
}

fn r3(x: f64) -> f64 {
    (x * 1000.0).round() / 1000.0
}

/// Speaking rate per speaker ID (`rates`); missing speakers use `project::CHARS_PER_S`.
pub type Rates = std::collections::BTreeMap<String, f64>;

/// Minimum measured speech per persona before its rate replaces the default.
const MIN_RATE_SECS: f64 = 2.0;

fn rate_of(rates: &Rates, who: &str) -> f64 {
    rates.get(who).copied().unwrap_or(project::CHARS_PER_S)
}

/// Speaking rate (spoken chars per second) per speaker ID, from the cached takes of this project's lines spoken by the
/// same persona (script and sync takes, final before draft, silence-trimmed like `render`; a line counts only with all
/// its clips). Personas with less
/// than `MIN_RATE_SECS` of measured speech are left out (default 13 per s).
pub fn rates(p: &Project, dirs: &[PathBuf]) -> Rates {
    let mut voices = tts::Voices::new();
    for (id, sp) in &p.meta.speakers {
        if let Some(v) = sp.persona.as_deref().and_then(|n| crate::speakers::spec(dirs, n).ok()) {
            voices.insert(id.clone(), v);
        }
    }
    let lines: Vec<Line> = p.script.lines().cloned().collect();
    let (t, lex) = (render::key_cfg(p), funkhaus_voice::lexicon::Lexicon::load(&p.lexicon_path()));
    let folders: Vec<PathBuf> = [Quality::Final, Quality::Draft].into_iter().flat_map(|q| [render::takes_dir(p, q), render::sync_takes_dir(p, q)]).collect();
    let mut by: std::collections::BTreeMap<&str, (usize, f64)> = Default::default();
    for (i, l) in lines.iter().enumerate() {
        let Some(name) = p.meta.speakers.get(&l.who).and_then(|s| s.persona.as_deref()).filter(|_| voices.contains_key(&l.who)) else { continue };
        let (Ok(keys), chars) = (tts::line_keys(&t, &lines, &voices, &lex, &t.lang, i), project::spoken_chars(&l.text)) else { continue };
        if keys.is_empty() || chars == 0 {
            continue;
        }
        // trimmed as `render` measures a clip (leading/trailing silence does not count)
        let len = |f: &Path| mix::read(f).ok().map(|(x, sr)| mix::trim(&x, sr, 0.01, 0.03).len() as f64 / sr as f64);
        let got = folders.iter().find_map(|d| keys.iter().map(|k| len(&d.join(format!("{k:016x}.wav")))).sum::<Option<f64>>());
        if let Some(secs) = got.filter(|s| *s > 0.0) {
            let e = by.entry(name).or_default();
            (e.0, e.1) = (e.0 + chars, e.1 + secs);
        }
    }
    let per: std::collections::BTreeMap<&str, f64> = by.into_iter().filter(|(_, (_, s))| *s >= MIN_RATE_SECS).map(|(n, (c, s))| (n, c as f64 / s)).collect();
    p.meta.speakers.iter().filter_map(|(id, s)| per.get(s.persona.as_deref()?).map(|r| (id.clone(), (r * 100.0).round() / 100.0))).collect()
}

fn report(s: &Sync, measured: &[f64], measure: &'static str, rates: &Rates, mut warnings: Vec<String>) -> Report {
    let cues = s.as_cues();
    let pairs = overlaps(&cues);
    let fs = fits(&cues, measured, s.limit);
    let nxt = next_starts(&cues);
    let reps: Vec<CueRep> = cues
        .iter()
        .zip(&s.cues)
        .zip(fs)
        .enumerate()
        .map(|(i, ((c, sc), f))| {
            let slot = slot_of(c);
            let chars = project::spoken_chars(&c.text);
            let factor = f.factor.is_finite().then(|| (f.factor * 10_000.0).round() / 10_000.0);
            let partner = pairs.iter().find(|(a, b)| *a == i || *b == i).map(|(a, b)| if *a == i { b + 1 } else { a + 1 });
            let cut = if matches!(f.kind, Kind::TooLong | Kind::Overlap) { r3((measured[i] - slot * s.limit).max(0.0)) } else { 0.0 };
            let warning = match (f.kind, partner) {
                _ if slot == 0.0 && measured[i] > 0.0 => Some(format!("Cue ohne Dauer (Anfang = Ende): {:.2} s Sprache passt in kein Zeitfenster", measured[i])),
                (Kind::Overlap, Some(j)) => Some(format!("überlappt mit Cue {j}")),
                (Kind::Overlap, None) => Some(format!("Sprache ({:.2} s) reicht in den nächsten Cue – Text um ca. {cut:.2} s kürzen", measured[i])),
                (Kind::TooLong, _) => Some(format!("zu lang: {:.2} s für {slot:.2} s (Faktor {:.2} > {}) – Text um ca. {cut:.2} s kürzen; ragt {:.2} s über das Cue-Ende", measured[i], f.factor, s.limit, f.overhang)),
                _ if chars == 0 => Some("leerer Cue: Stille".to_string()),
                _ => None,
            };
            let rate = rate_of(rates, &sc.who);
            CueRep {
                index: i + 1,
                start: secs(c.start),
                end: secs(c.end),
                slot,
                target: slot,
                next_start: nxt[i].map(secs),
                text: c.text.clone(),
                who: sc.who.clone(),
                chars,
                needed: r3(est_secs_at(&c.text, rate)),
                rate,
                measured: r3(measured[i]),
                duration: r3(measured[i]),
                factor,
                overhang: r3(f.overhang),
                cut_s: cut,
                status: f.kind,
                warning,
                suggestion: None,
            }
        })
        .collect();
    let n = |k| reps.iter().filter(|r| r.status == k).count();
    let summary = Summary {
        total: reps.len(),
        fits: n(Kind::Fits),
        atempo: n(Kind::Atempo),
        too_long: n(Kind::TooLong),
        overlap: n(Kind::Overlap),
        max_factor: reps.iter().filter_map(|r| r.factor).reduce(f64::max),
        cut_s: r3(reps.iter().map(|r| r.cut_s).sum()),
        ok: n(Kind::TooLong) + n(Kind::Overlap) == 0,
    };
    warnings.extend(s.notes.iter().cloned());
    Report { schema: REPORT_SCHEMA, input: s.source.clone(), limit: s.limit, measure, cues: reps, warnings, summary, render: None }
}

/// Dry run of a sync project at the default rate: estimate, no TTS, no ffmpeg.
pub fn check(s: &Sync) -> Report {
    check_with(s, &Rates::new())
}

/// Dry run with speaking rates per speaker (`rates`; e.g. measured takes of a project).
pub fn check_with(s: &Sync, rates: &Rates) -> Report {
    let est: Vec<f64> = s.cues.iter().map(|c| est_secs_at(&c.text, rate_of(rates, &c.who))).collect();
    report(s, &est, "estimate", rates, vec![])
}

/// Dry run of a sync project with each persona's measured speaking rate (`rates`).
pub fn check_project(p: &Project, s: &Sync, dirs: &[PathBuf]) -> Report {
    check_with(s, &rates(p, dirs))
}

/// Dry run of a subtitle file without a project (`stimmwerk sync in.srt`): speaker = `<v>` slug or `DEFAULT_SPEAKER`.
pub fn check_file(src: &str, name: &str) -> Result<Report> {
    let ps = parse(src, name)?;
    if ps.cues.is_empty() {
        bail!("keine Cues in {name}");
    }
    let who = |c: &Cue| c.voice.as_deref().map(id_of).unwrap_or_else(|| crate::import::DEFAULT_SPEAKER.into());
    let cues = ps.cues.iter().map(|c| SyncCue { start_ms: c.start, end_ms: c.end, who: who(c), text: if c.text.is_empty() { SILENT.into() } else { c.text.clone() } }).collect();
    let s = Sync { schema: SCHEMA.into(), limit: LIMIT, source: Some(name.into()), cues, notes: vec![] };
    let est: Vec<f64> = s.cues.iter().map(|c| est_secs(&c.text)).collect();
    Ok(report(&s, &est, "estimate", &Rates::new(), ps.warnings))
}

// ---------- render ----------

/// `atempo` chain for `factor`: one filter inside 0.5–2 (every ffmpeg), halves/doubles chained outside.
fn atempo_filter(mut f: f64) -> String {
    let mut v = vec![];
    while f > 2.0 {
        v.push("atempo=2".to_string());
        f /= 2.0;
    }
    while f < 0.5 {
        v.push("atempo=0.5".to_string());
        f *= 2.0;
    }
    v.push(format!("atempo={f:.6}"));
    v.join(",")
}

/// ffmpeg: `inp` sped up by `factor` and padded/cut to exactly `slot` s (mono, `sr`, 16 bit).
pub fn atempo(ff: &str, inp: &Path, out: &Path, factor: f64, slot: f64, sr: u32) -> Result<()> {
    if !(0.1..=100.0).contains(&factor) || slot.is_nan() || slot <= 0.0 {
        bail!("atempo: Faktor {factor} / Ziel {slot} s ungültig (erlaubt 0,1 bis 100)");
    }
    let o = std::process::Command::new(ff)
        .args(["-hide_banner", "-loglevel", "error", "-nostdin", "-y", "-i"])
        .arg(inp)
        .args(["-af", &format!("{},apad", atempo_filter(factor)), "-t", &format!("{slot:.6}"), "-ar", &sr.to_string(), "-ac", "1", "-c:a", "pcm_s16le"])
        .arg(out)
        .output()
        .with_context(|| format!("{ff} nicht startbar"))?;
    if !o.status.success() {
        bail!("ffmpeg atempo: {}", String::from_utf8_lossy(&o.stderr).trim());
    }
    Ok(())
}

fn samples(ms: u32, sr: u32) -> usize {
    (ms as f64 * sr as f64 / 1000.0).round() as usize
}

/// Cue starts in samples (mix timeline of a sync project).
pub fn starts(s: &Sync, sr: u32) -> Vec<usize> {
    s.cues.iter().map(|c| samples(c.start_ms, sr)).collect()
}

/// End of the last cue in samples.
pub fn end(s: &Sync, sr: u32) -> usize {
    s.cues.iter().map(|c| samples(c.end_ms, sr)).max().unwrap_or(0)
}

fn atempo_name(n: &str) -> bool {
    n.strip_suffix(".wav").and_then(|n| n.split_once('-')).is_some_and(|(h, f)| h.len() == 16 && h.bytes().all(|b| b.is_ascii_hexdigit()) && f.parse::<f64>().is_ok())
}

pub fn render(p: &Project, s: &Sync, dirs: &[PathBuf], q: Quality, cancel: &AtomicBool, prog: &Prog) -> Result<Report> {
    render::with_real_say(p, q, cancel, prog, |say| render_with(p, s, dirs, q, cancel, prog, say))
}

pub(crate) fn ctx(p: &Project, s: &Sync, dirs: &[PathBuf], q: Quality) -> Result<Ctx> {
    render::ctx_lines(p, s.lines(), render::sync_takes_dir(p, q), dirs)
}

/// `render` with an injectable `say(wav, text, voice, seed)`.
pub fn render_with(p: &Project, s: &Sync, dirs: &[PathBuf], q: Quality, cancel: &AtomicBool, prog: &Prog, say: &mut Say) -> Result<Report> {
    let c = ctx(p, s, dirs, q)?;
    let l = render::lock(p)?;
    let r = run(p, &c, s, q, cancel, prog, say)?.0;
    drop(l);
    prog(Event::Done);
    Ok(r)
}

/// Assumes the project lock is held. TTS per cue (cache), fit, atempo, `out/sync.wav`. Returns the report and the placed
/// clips (atempo applied, too_long in full) with their rate.
pub(crate) fn run(p: &Project, c: &Ctx, s: &Sync, q: Quality, cancel: &AtomicBool, prog: &Prog, say: &mut Say) -> Result<(Report, Vec<Vec<f32>>, u32)> {
    let t0 = std::time::Instant::now();
    let n = c.lines.len();
    let out_dir = p.dir.join("out");
    for d in [&c.dir, &out_dir] {
        std::fs::create_dir_all(d)?;
        render::sweep(d);
    }
    let root = render::sync_rerolls_dir(p);
    std::fs::create_dir_all(&root)?;
    let mut rf = render::load_rerolls(&root, prog)?;
    let base: Vec<Vec<u64>> = (0..n).map(|i| c.keys(i)).collect::<Result<_>>()?;
    if rf.prune(&base.iter().flatten().copied().collect()) {
        rf.save(&root)?;
    }
    let rr = rf.rerolls();
    prog(Event::Stage { name: "sync".into() });
    for i in 0..n {
        prog(Event::Line { i: i + 1, n, state: State::Queued, tts_secs: None });
    }
    let spoke = RefCell::new(vec![false; n]);
    let started = RefCell::new(std::time::Instant::now());
    let on_line = |k: usize, st: tts::LineState| {
        let (state, secs) = match st {
            tts::LineState::Rendering => {
                *started.borrow_mut() = std::time::Instant::now();
                (State::Rendering, None)
            }
            tts::LineState::Cached => (State::Cached, None),
            tts::LineState::Done => {
                spoke.borrow_mut()[k] = true;
                (State::Done, Some(render::r2(started.borrow().elapsed().as_secs_f64())))
            }
            tts::LineState::Failed => (State::Failed, None),
        };
        prog(Event::Line { i: k + 1, n, state, tts_secs: secs });
    };
    let mut calls = 0;
    let mut counted = |w: &Path, x: &str, v: &funkhaus_voice::persona::VoiceSpec, sd: u64| -> Result<()> {
        calls += 1;
        say(w, x, v, sd)
    };
    let txt = |m: &str| prog(Event::Msg { text: m.into() });
    let req = tts::RenderReq { t: &c.t, lines: &c.lines, voices: &c.voices, lex: &c.lex, lang: &c.t.lang, dir: &c.dir, fresh: false, rr: &rr, prog: &txt, cancel: Some(cancel), on_line: Some(&on_line) };
    let parts = tts::render_with(&req, &mut counted).map_err(|e| render::cancelled(e, cancel))?;
    let (mut clips, sr) = tts::clips(&parts)?;
    let measured: Vec<f64> = clips.iter().map(|x| x.len() as f64 / sr as f64).collect();
    let mut rep = report(s, &measured, "rendered", &Rates::new(), vec![]);

    // atempo: exactly the slot, cached by clip + slot + factor
    let (mut runs, mut hits) = (0, 0);
    let mut used = std::collections::HashSet::new();
    let mut files: Vec<Vec<PathBuf>> = parts.iter().map(|ps| ps.iter().filter_map(|p| if let tts::Part::Wav(w) = p { Some(w.clone()) } else { None }).collect()).collect();
    let mut ff = None;
    for (i, r) in rep.cues.iter().enumerate() {
        if r.status != Kind::Atempo {
            continue;
        }
        let cue = &s.cues[i];
        let ms = cue.end_ms.saturating_sub(cue.start_ms);
        let slot = secs(ms);
        let f = measured[i] / slot;
        let stems: Vec<String> = files[i].iter().filter_map(|w| w.file_stem().map(|x| x.to_string_lossy().into_owned())).collect();
        let mut key: Vec<&str> = stems.iter().map(String::as_str).collect();
        let ms = ms.to_string();
        key.extend(["atempo", &ms]);
        let name = format!("{:016x}-{f:.4}.wav", tts::fnv(&key));
        let out = c.dir.join(&name);
        if mix::read(&out).is_ok_and(|(x, xsr)| xsr == sr && (x.len() as f64 / sr as f64 - slot).abs() <= TOL) {
            hits += 1;
        } else {
            let ff = ff.get_or_insert_with(crate::export::ffmpeg);
            funkhaus_sys::which(ff).context("ffmpeg nicht gefunden (für atempo)")?;
            // removed on every exit path (Drop), including `?`
            let tmp = crate::export::Tmp(render::part_path(&c.dir.join(format!("{}-in.wav", &name[..16]))));
            mix::write_f32(&tmp.0, &clips[i], sr)?;
            render::atomic(&out, |t| atempo(ff, &tmp.0, t, f, slot, sr)).with_context(|| format!("Cue {}", i + 1))?;
            runs += 1;
        }
        let (x, xsr) = mix::read(&out)?;
        let got = x.len() as f64 / xsr as f64;
        if xsr != sr || (got - slot).abs() > TOL {
            bail!("Cue {}: atempo ergab {got:.3} s statt {slot:.3} s", i + 1);
        }
        clips[i] = x;
        files[i].push(out);
        used.insert(name);
    }
    // atempo versions of old takes/texts
    for e in std::fs::read_dir(&c.dir).into_iter().flatten().flatten() {
        let nm = e.file_name().to_string_lossy().into_owned();
        if atempo_name(&nm) && !used.contains(&nm) {
            let _ = std::fs::remove_file(e.path());
        }
    }

    prog(Event::Mix);
    let cl: Vec<&[f32]> = clips.iter().map(Vec::as_slice).collect();
    let (m, _) = mix::place(&starts(s, sr), &cl, &vec![1.0; n], end(s, sr));
    let wav = out_dir.join("sync.wav");
    render::atomic(&wav, |t| mix::write(t, &m, sr))?;
    render::record_ref_audio(p, &c.voices, c.lines.iter().zip(&files).map(|(l, f)| (l.who.as_str(), f.clone())), std::slice::from_ref(&wav))?;
    let spoke = spoke.into_inner();
    let rendered: Vec<usize> = (0..n).filter(|&i| spoke[i]).map(|i| i + 1).collect();
    rep.render = Some(RenderInfo {
        quality: q,
        tts_calls: calls,
        cached: n - rendered.len(),
        rendered,
        atempo_runs: runs,
        atempo_cached: hits,
        wav,
        secs: render::r2(m.len() as f64 / sr as f64),
        elapsed: render::r2(t0.elapsed().as_secs_f64()),
    });
    Ok((rep, clips, sr))
}

#[derive(Serialize, Debug)]
pub struct Reroll {
    pub cue: usize,
    pub take: usize,
    pub salt: Option<u64>,
    pub undo: bool,
    pub keys: Vec<String>,
    pub report: Report,
}

#[allow(clippy::too_many_arguments)]
pub fn reroll(p: &Project, s: &Sync, dirs: &[PathBuf], cue: usize, undo: bool, q: Quality, cancel: &AtomicBool, prog: &Prog) -> Result<Reroll> {
    render::with_real_say(p, q, cancel, prog, |say| reroll_with(p, s, dirs, cue, undo, q, cancel, prog, say))
}

/// New take (or `undo`: previous) of 1-based `cue` in the sync namespace, then full sync render.
#[allow(clippy::too_many_arguments)]
pub fn reroll_with(p: &Project, s: &Sync, dirs: &[PathBuf], cue: usize, undo: bool, q: Quality, cancel: &AtomicBool, prog: &Prog, say: &mut Say) -> Result<Reroll> {
    let c = ctx(p, s, dirs, q)?;
    if cue < 1 || cue > c.lines.len() {
        bail!("Cue {cue} außerhalb 1..={}", c.lines.len());
    }
    let l = render::lock(p)?;
    let keys = c.keys(cue - 1)?;
    if keys.is_empty() {
        bail!("Cue {cue} hat keinen gesprochenen Text");
    }
    let root = render::sync_rerolls_dir(p);
    std::fs::create_dir_all(&root)?;
    let mut f = render::load_rerolls(&root, prog)?;
    let (take, salt) = f.bump(cue, &c.lines[cue - 1].text, &keys, undo)?;
    f.save(&root)?;
    let report = run(p, &c, s, q, cancel, prog, say)?.0;
    drop(l);
    prog(Event::Done);
    Ok(Reroll { cue, take, salt, undo, keys: keys.iter().map(|k| format!("{k:016x}")).collect(), report })
}

#[cfg(test)]
mod tests {
    use super::Kind::*;
    use super::*;
    use std::cell::Cell;

    const EDGE: &str = include_str!("../../../fixtures/stimmwerk/sync/edge.srt");
    const FIT3: &str = include_str!("../../../fixtures/stimmwerk/sync/fit3.srt");
    const FIT3_VTT: &str = include_str!("../../../fixtures/stimmwerk/sync/fit3.vtt");
    const OK2: &str = include_str!("../../../fixtures/stimmwerk/sync/ok2.srt");
    const VOICE: &str = include_str!("../../../fixtures/stimmwerk/sync/voice.vtt");

    fn srt(s: &str) -> Vec<Cue> {
        parse_srt(s).unwrap().cues
    }
    fn vtt(s: &str) -> Vec<Cue> {
        parse_vtt(s).unwrap().cues
    }
    fn kinds(c: &[Cue]) -> Vec<Kind> {
        dry_run(c).iter().map(|f| f.kind).collect()
    }
    fn near(a: f64, b: f64) -> bool {
        (a - b).abs() <= 1e-3
    }
    fn span(c: &[Cue]) -> Vec<(u32, u32)> {
        c.iter().map(|c| (c.start, c.end)).collect()
    }

    #[test]
    fn srt_rules() {
        let c = srt("\u{feff}1\n00:00:01,000 --> 00:00:03,000\nHi\n");
        assert_eq!((c[0].start, c[0].end, c[0].text.as_str()), (1000, 3000, "Hi"));
        assert_eq!(span(&srt("00:00:01,500 --> 00:00:02,250")), [(1500, 2250)]);
        assert_eq!(span(&srt("00:00:01.500 --> 00:00:02.250")), [(1500, 2250)]);
        assert!(parse_srt("00:01,000 --> 00:03,000\nx").unwrap_err().to_string().contains("ungültig"));
        assert!(parse_srt("00:00:03,000 --> 00:00:01,000\nx").unwrap_err().to_string().contains("vor dem Anfang"));
        assert_eq!(srt("00:00:01,000 --> 00:00:02,000\nohne Index")[0].text, "ohne Index");
        // non-sequential indices, unsorted times: file order kept (idx = file position), noted once
        let p = parse_srt("5\n00:00:05,000 --> 00:00:06,000\nA\n\n2\n00:00:01,000 --> 00:00:02,000\nB\n\n9\n00:00:07,000 --> 00:00:08,000\nC").unwrap();
        let c = &p.cues;
        assert_eq!(c.iter().map(|c| (c.idx, c.start, c.end, c.text.as_str())).collect::<Vec<_>>(), [(1, 5000, 6000, "A"), (2, 1000, 2000, "B"), (3, 7000, 8000, "C")]);
        assert_eq!(p.warnings.len(), 1, "{:?}", p.warnings);
        assert!(p.warnings[0].contains("Cue 2 beginnt vor Cue 1") && p.warnings[0].contains("sortiert"));
        assert!(parse_srt(FIT3).unwrap().warnings.is_empty());
        let lf = srt("1\n00:00:01,000 --> 00:00:02,000\nA\nB\n");
        assert_eq!(srt("1\r\n00:00:01,000 --> 00:00:02,000\r\nA\r\nB\r\n"), lf);
        assert_eq!(lf[0].text, "A B");
        assert_eq!(srt("1\n00:00:01,000 --> 00:00:02,000")[0].text, "");
        let p = parse_srt("1\nHallo\n").unwrap();
        assert_eq!((p.cues.len(), p.warnings.len()), (0, 1));
        let err = |s: &str| parse_srt(s).unwrap_err().to_string();
        assert!(err("00:00:01,000 --> 00:00:02,0000\nx").contains("\"00:00:02,0000\" ungültig"));
        assert!(err("00:00:61,000 --> 00:00:62,000\nx").contains("Zeile 1: Zeitstempel \"00:00:61,000\""));
        assert!(err("00:00:01,000 --> 00:00:02,000\nx\n\n00:60:01,000 --> 00:60:02,000\ny").starts_with("Zeile 4:"));
    }

    #[test]
    fn ts_hours_and_digit_runs() {
        // hours ≥ 100 are fine, the u32 ceiling (~1193 h) and over-long digit runs are not (no overflow panic)
        assert_eq!(span(&srt("100:00:00,000 --> 100:00:01,500\nx")), [(360_000_000, 360_001_500)]);
        assert_eq!(span(&srt("999:59:59,999 --> 1000:00:00,000\nx")), [(3_599_999_999, 3_600_000_000)]);
        assert!(parse_srt("1193:00:00,000 --> 1194:00:00,000\nx").unwrap_err().to_string().contains("ungültig"));
        assert!(parse_srt("18446744073709551615:00:00,000 --> 18446744073709551615:00:01,000\nx").is_err());
        assert!(parse_srt("99999999999999999999999:00:00,000 --> 00:00:01,000\nx").is_err());
        assert!(parse_srt("0000000001:00:00,000 --> 00:00:01,000\nx").is_err(), "10 digits");
        assert_eq!(span(&srt("000000001:00:00,000 --> 000000001:00:01,000\nx")), [(3_600_000, 3_601_000)]);
        assert_eq!(span(&vtt("WEBVTT\n\n100:00:00.000 --> 100:00:01.000\nx")), [(360_000_000, 360_001_000)]);
    }

    #[test]
    fn clean_raw_lt_and_entities() {
        let t = |s: &str| vtt(&format!("WEBVTT\n\n00:00:01.000 --> 00:00:02.000\n{s}"))[0].text.clone();
        assert_eq!(t("3 < 5 Grad"), "3 < 5 Grad");
        assert_eq!(t("3 <5 Grad"), "3 <5 Grad");
        assert_eq!(t("a < b > c"), "a < b > c");
        assert_eq!(t("<i>3 < 5</i> Grad"), "3 < 5 Grad");
        assert_eq!(t("<00:00:01.500>a<00:01.500> b <1.500>c"), "a b <1.500>c");
        assert_eq!(t("<1> x"), "<1> x");
        assert_eq!(t("&quot;Ja&quot; &#39;nein&#39; &apos;ok&apos; &amp;lt; &nbsp;"), "\"Ja\" 'nein' 'ok' &lt;");
        assert_eq!(t("&amp;quot;"), "&quot;");
        assert_eq!(srt("00:00:01,000 --> 00:00:02,000\n2 < 3 und <b>fett</b>")[0].text, "2 < 3 und fett");
    }

    #[test]
    fn atempo_filter_chains_outside_ffmpeg_range() {
        assert_eq!(atempo_filter(1.5), "atempo=1.500000");
        assert_eq!(atempo_filter(2.0), "atempo=2.000000");
        assert_eq!(atempo_filter(5.0), "atempo=2,atempo=2,atempo=1.250000");
        assert_eq!(atempo_filter(0.2), "atempo=0.5,atempo=0.5,atempo=0.800000");
    }

    #[test]
    fn vtt_without_blank_line_after_header() {
        let p = parse_vtt("WEBVTT\n00:00:01.000 --> 00:00:03.000\nErste\n\n00:00:04.000 --> 00:00:05.000\nZweite").unwrap();
        assert_eq!(p.cues.iter().map(|c| c.text.as_str()).collect::<Vec<_>>(), ["Erste", "Zweite"]);
        assert_eq!(span(&p.cues), [(1000, 3000), (4000, 5000)]);
        assert_eq!(p.warnings.len(), 1);
        assert!(p.warnings[0].starts_with("Zeile 2:") && p.warnings[0].contains("Leerzeile"), "{:?}", p.warnings);
        // header lines + cue id glued to the first cue
        let p = parse_vtt("WEBVTT - Titel\nKind: captions\nintro\n00:00:01.000 --> 00:00:03.000\nHi").unwrap();
        assert_eq!((p.cues.len(), p.cues[0].text.as_str(), p.warnings.len()), (1, "Hi", 1));
        assert!(p.warnings[0].starts_with("Zeile 4:"));
        // header only: no cue, no warning about the header; a proper header stays warning-free
        assert!(parse_vtt("WEBVTT\nKind: captions\n").unwrap().warnings.is_empty());
        assert!(parse_vtt(FIT3_VTT).unwrap().warnings.is_empty());
        // bad timestamp in the glued cue reports its real line
        assert!(parse_vtt("WEBVTT\n00:00:01,000 --> 00:00:03,000\nHi").unwrap_err().to_string().starts_with("Zeile 2:"));
    }

    #[test]
    fn srt_edge_fixture() {
        let c = srt(EDGE);
        assert_eq!(span(&c), [(0, 3500), (3000, 6000), (6000, 9000), (9200, 10000)]);
        assert_eq!((c[1].text.as_str(), c[2].text.as_str(), c[2].text.chars().count()), ("Das ist ein Test mit zwei Zeilen.", "Das ist kursiv.", 15));
        assert_eq!(overlaps(&c), [(0, 1)]);
        assert_eq!(kinds(&c), [Overlap, Overlap, Fits, Fits]);
    }

    #[test]
    fn vtt_rules() {
        assert!(parse_vtt("00:00:01.000 --> 00:00:03.000\nHi").unwrap_err().to_string().contains("WEBVTT"));
        assert_eq!(vtt("\u{feff}WEBVTT\n\n00:00:01.000 --> 00:00:03.000\nHi").len(), 1);
        assert_eq!(vtt("WEBVTT - Titel\nKind: captions\n\n00:00:01.000 --> 00:00:03.000\nHi").len(), 1);
        assert!(parse_vtt("WEBVTTX\n\n00:00:01.000 --> 00:00:03.000\nHi").is_err());
        assert_eq!(span(&vtt("WEBVTT\n\n01.000 --> 03.000\nx\n\n00:04.000 --> 00:05.500\ny")), [(1000, 3000), (4000, 5500)]);
        assert!(parse_vtt("WEBVTT\n\n00:00:01,000 --> 00:00:03,000\nHi").is_err());
        let c = vtt("WEBVTT\n\nintro-1\n00:00:01.000 --> 00:00:03.000 align:start position:10% line:0 size:80% foo:bar\nHi");
        assert_eq!(span(&c), [(1000, 3000)]);
        let c = vtt("WEBVTT\n\nNOTE kommentar\nzeile 2\n\nNOTE\nzeile\n\nSTYLE\n::cue { color: yellow }\n\nREGION\nid:r1\nwidth:40%\n\n00:00:01.000 --> 00:00:02.000\nNur ich");
        assert_eq!((c.len(), c[0].text.as_str()), (1, "Nur ich"));
        let p = parse_vtt("WEBVTT\n\nNOTE 00:00:01.000 --> 00:00:02.000\n\n00:00:03.000 --> 00:00:04.000\nx").unwrap();
        assert_eq!((p.cues.len(), p.warnings.len()), (1, 1));
        let t = |s: &str| vtt(&format!("WEBVTT\n\n00:00:01.000 --> 00:00:02.000\n{s}"))[0].text.clone();
        assert_eq!(t("<b>x</b> <i>y</i> <u>z</u>"), "x y z");
        assert_eq!(t("<c.yellow>x</c>"), "x");
        assert_eq!(t("<i><b>x</b></i>"), "x");
        assert_eq!(t("a <00:00:01.500> b"), "a b");
        assert_eq!(t("Zeile eins\nZeile zwei"), "Zeile eins Zeile zwei");
        assert_eq!(t("Tom &amp; Jerry &lt;3"), "Tom & Jerry <3");
        let c = vtt("WEBVTT\n\n00:00:01.000 --> 00:00:02.000\n<v.loud Bernd Graf><i>Hallo</i></v>");
        assert_eq!((c[0].voice.as_deref(), c[0].text.as_str()), (Some("Bernd Graf"), "Hallo"));
        // edge.srt as VTT: same cues and overlap flags
        let e = format!("WEBVTT\n\n{}", EDGE.replace(',', "."));
        assert_eq!(span(&vtt(&e)), span(&srt(EDGE)));
        assert_eq!(kinds(&vtt(&e)), [Overlap, Overlap, Fits, Fits]);
    }

    #[test]
    fn fixtures_fit3_ok2_voice() {
        assert_eq!(srt(FIT3), vtt(FIT3_VTT));
        assert_eq!(kinds(&srt(FIT3)), [Fits, Atempo, TooLong]);
        assert_eq!(kinds(&srt(OK2)), [Fits, Atempo]);
        let v = vtt(VOICE);
        assert_eq!((v[0].voice.as_deref(), v[0].text.as_str()), (Some("Anna Müller"), "Hallo"));
        assert!(parse(VOICE, "x.txt").is_ok() && parse(FIT3, "fit3").is_ok());
    }

    #[test]
    fn overlap_rules() {
        let c = |a, b| Cue { idx: 0, start: a, end: b, text: "x".into(), voice: None };
        assert_eq!(overlaps(&[c(0, 3500), c(3000, 6000)]), [(0, 1)]);
        assert!(overlaps(&[c(0, 6000), c(6000, 7000)]).is_empty());
        assert!(overlaps(&[c(0, 9000), c(9200, 9300)]).is_empty());
        // all pairs, not only neighbours: a long cue covering two later ones
        assert_eq!(overlaps(&[c(0, 10_000), c(1000, 2000), c(5000, 6000), c(10_000, 11_000)]), [(0, 1), (0, 2)]);
        // unsorted file order: pairs by time, reported as file indices
        assert_eq!(overlaps(&[c(5000, 7000), c(0, 1000), c(6000, 8000)]), [(0, 2)]);
        assert_eq!(overlaps(&[c(6000, 8000), c(5000, 7000)]), [(0, 1)]);
        // zero-length cue at another cue's start is not an overlap
        assert!(overlaps(&[c(1000, 1000), c(1000, 2000)]).is_empty());
        assert!(overlaps(&[c(1000, 2000), c(1000, 1000)]).is_empty());
    }

    #[test]
    fn unsorted_cues_room_by_time() {
        // file order: A 5-6 s, B 1-2 s, C 7-8 s. Room of A is C (2 s), B's room is A (4 s); C is last.
        let c = srt("00:00:05,000 --> 00:00:06,000\nA\n\n00:00:01,000 --> 00:00:02,000\nB\n\n00:00:07,000 --> 00:00:08,000\nC");
        assert_eq!(next_starts(&c), [Some(7000), Some(5000), None]);
        // A: 2.5 s of speech > room 2 s -> overlap (would be too_long if room came from file order = B at 1 s ... = underflow 0)
        let f = fits(&c, &[2.5, 0.5, 9.0], LIMIT);
        assert_eq!(f.iter().map(|f| f.kind).collect::<Vec<_>>(), [Overlap, Fits, TooLong]);
        assert!(near(f[0].overhang, 1.0) && near(f[2].overhang, 8.0));
        let r = check_file("00:00:05,000 --> 00:00:06,000\nA\n\n00:00:01,000 --> 00:00:02,000\nB", "u.srt").unwrap();
        assert_eq!((r.cues[0].next_start, r.cues[1].next_start), (None, Some(5.0)));
        assert!(r.warnings.iter().any(|w| w.contains("sortiert")), "{:?}", r.warnings);
    }

    #[test]
    fn fit_limit_passthrough() {
        // same ratio 1.13: too_long at 1.12, atempo at 1.2; limit 1.0 = no atempo at all
        assert_eq!(fit(1.0, None, 1.13, LIMIT).kind, TooLong);
        assert_eq!(fit(1.0, None, 1.13, 1.2).kind, Atempo);
        assert_eq!(fit(1.0, None, 1.001, 1.0).kind, TooLong);
        assert_eq!(fit(1.0, None, 1.0, 1.0).kind, Fits);
        let mut s = sync_of(FIT3, "fit3.srt");
        assert_eq!(check(&s).cues.iter().map(|c| c.status).collect::<Vec<_>>(), [Fits, Atempo, TooLong]);
        s.limit = 1.05;
        let r = check(&s);
        assert_eq!(r.cues.iter().map(|c| c.status).collect::<Vec<_>>(), [Fits, TooLong, TooLong]);
        assert!(r.cues[1].warning.as_deref().unwrap().contains("> 1.05") && near(r.cues[1].cut_s, r.cues[1].measured - 3.28 * 1.05), "{:?}", r.cues[1].warning);
        assert_eq!(r.limit, 1.05);
        s.limit = 2.0;
        assert_eq!(check(&s).cues.iter().map(|c| c.status).collect::<Vec<_>>(), [Fits, Atempo, TooLong]);
        assert!(near(check(&s).cues[2].cut_s, check(&s).cues[2].measured - 2.0 * 2.0));
    }

    #[test]
    fn fit_ladder() {
        let k = |s, r, m| fit(s, r, m, LIMIT);
        assert_eq!(k(2.0, Some(3.0), 0.846), Fit { kind: Fits, factor: 1.0, overhang: 0.0 });
        assert_eq!(k(2.0, None, 2.0).kind, Fits);
        assert_eq!(k(2.0, None, 0.0).kind, Fits);
        let x = k(3.28, Some(4.0), 3.46);
        assert!(x.kind == Atempo && near(x.factor, 1.0549));
        assert!(k(2.0, None, 2.24).kind == Atempo && near(k(2.0, None, 2.24).factor, 1.12));
        let x = k(2.0, None, 2.2401);
        assert!(x.kind == TooLong && near(x.factor, 1.12005) && near(x.overhang, 0.2401));
        let x = k(2.0, None, 5.692);
        assert!(x.kind == TooLong && near(x.factor, 2.846) && near(x.overhang, 3.692));
        let x = k(2.0, Some(3.0), 2.8);
        assert!(x.kind == TooLong && near(x.factor, 1.4) && near(x.overhang, 0.8));
        let x = k(2.0, Some(2.5), 4.0);
        assert!(x.kind == Overlap && near(x.factor, 2.0) && near(x.overhang, 0.5));
        let x = k(2.0, Some(2.5), 2.5);
        assert!(x.kind == TooLong && near(x.factor, 1.25) && near(x.overhang, 0.5));
        let x = k(3.5, Some(3.0), 1.385);
        assert!(x.kind == Overlap && near(x.factor, 0.396) && x.overhang == 0.0);
        let x = k(0.0, None, 0.3);
        assert!(x.kind == TooLong && x.factor.is_infinite() && near(x.overhang, 0.3));
        assert_eq!(k(3.0, Some(3.0), 2.54).kind, Fits);
        // limit 1.12, not 1.15
        assert_eq!(k(1.0, None, 1.13).kind, TooLong);
    }

    fn sync_of(src: &str, name: &str) -> Sync {
        let cues = parse(src, name).unwrap().cues.iter().map(|c| SyncCue { start_ms: c.start, end_ms: c.end, who: "ERZ".into(), text: c.text.clone() }).collect();
        Sync { schema: SCHEMA.into(), limit: LIMIT, source: None, cues, notes: vec![] }
    }

    #[test]
    fn check_report_json() {
        let r = check(&sync_of(FIT3, "fit3.srt"));
        let v = serde_json::to_value(&r).unwrap();
        let st: Vec<&str> = v["cues"].as_array().unwrap().iter().map(|c| c["status"].as_str().unwrap()).collect();
        assert_eq!(st, ["fits", "atempo", "too_long"]);
        assert_eq!((v["measure"].as_str(), v["summary"]["ok"].as_bool(), v["summary"]["too_long"].as_u64()), (Some("estimate"), Some(false), Some(1)));
        let c1 = &v["cues"][1];
        assert_eq!((c1["chars"].as_u64(), c1["target"].as_f64(), c1["slot"].as_f64()), (Some(45), Some(3.28), Some(3.28)));
        assert!(near(c1["factor"].as_f64().unwrap(), 1.0554) && c1["duration"] == c1["measured"] && c1["suggestion"].is_null());
        assert!(v["cues"][2]["warning"].as_str().unwrap().contains("zu lang"));
        let vt = serde_json::to_value(check(&sync_of(FIT3_VTT, "fit3.vtt"))).unwrap();
        assert_eq!(vt, v);
        assert!(check(&sync_of(OK2, "ok2.srt")).summary.ok);
        let s = check_file(EDGE, "edge.srt").unwrap();
        assert_eq!(s.cues.iter().map(|c| c.status).collect::<Vec<_>>(), [Overlap, Overlap, Fits, Fits]);
        assert_eq!((s.input.as_deref(), s.cues[0].warning.as_deref()), (Some("edge.srt"), Some("überlappt mit Cue 2")));
        // zero slot: factor null, no NaN in JSON, own message (not "zu lang ... Faktor inf"), speaker = ERZ
        let z = check_file("00:00:01,000 --> 00:00:01,000\nHallo Welt.", "z.srt").unwrap();
        assert_eq!((z.cues[0].factor, z.cues[0].status, z.cues[0].who.as_str()), (None, TooLong, crate::import::DEFAULT_SPEAKER));
        let w = z.cues[0].warning.as_deref().unwrap();
        assert!(w.starts_with("Cue ohne Dauer") && !w.contains("inf") && w.contains("0.85 s"), "{w}");
        assert!(serde_json::to_string(&z).unwrap().contains("\"factor\":null"));
        assert_eq!(z.summary.max_factor, None);
        // zero slot without text: silence, fits
        let z = check_file("00:00:01,000 --> 00:00:01,000\n", "z.srt").unwrap();
        assert_eq!((z.cues[0].status, z.cues[0].warning.as_deref()), (Fits, Some("leerer Cue: Stille")));
    }

    fn project() -> (tempfile::TempDir, Project) {
        let t = tempfile::tempdir().unwrap();
        let p = Project::create(&t.path().join("s.studio"), "S", "de").unwrap();
        (t, p)
    }

    #[test]
    fn import_load_rules() {
        let (_t, mut p) = project();
        let r = import(&mut p, VOICE, "voice.vtt", None, false).unwrap();
        assert_eq!((r.format, r.cues, r.empty), ("vtt", 4, 1));
        assert_eq!(r.speakers_added, ["Anna_Müller", "Bernd", "ERZ"]);
        assert_eq!(p.meta.speakers["Anna_Müller"].label.as_deref(), Some("Anna Müller"));
        assert_eq!(p.meta.speakers["Bernd"].label, None);
        let q = Project::load(&p.dir).unwrap();
        let ls: Vec<_> = q.script.lines().map(|l| (l.who.as_str(), l.text.as_str())).collect();
        // `#` at the text start stays text (script.txt has ID|GAP| before it), empty cue = silence
        assert_eq!(ls, [("Anna_Müller", "Hallo"), ("Bernd", "Guten Tag, Anna."), ("ERZ", "#1 ist die Antwort."), ("ERZ", SILENT)]);
        let s = load(&q).unwrap().unwrap();
        assert_eq!((s.cues.len(), s.cues[2].start_ms, s.source.as_deref()), (4, 5500, Some("voice.vtt")));
        assert_eq!(check(&s).cues[3].warning.as_deref(), Some("leerer Cue: Stille"));
        // roundtrip bit-identical
        let raw = std::fs::read(p.dir.join(project::SYNC)).unwrap();
        s.save(&p.dir).unwrap();
        assert_eq!(std::fs::read(p.dir.join(project::SYNC)).unwrap(), raw);
        assert!(p.summary()["sync"].as_bool().unwrap());
        // non-empty script needs --replace; casting kept
        p.meta.speakers["ERZ"].persona = Some("gt".into());
        assert!(import(&mut p, FIT3, "fit3.srt", None, false).unwrap_err().to_string().contains("--replace"));
        let r = import(&mut p, FIT3, "fit3.srt", Some("ERZ"), true).unwrap();
        assert_eq!((r.cues, r.speakers_added.len()), (3, 0));
        assert_eq!(Project::load(&p.dir).unwrap().meta.speakers["ERZ"].persona.as_deref(), Some("gt"));
        assert!(import(&mut p, FIT3, "fit3.srt", Some("B"), true).unwrap_err().to_string().contains("nicht B – reserviert"));
        assert_eq!(id_of("O"), "O_");
        // hand-edited sync.json: limit outside the range, end before start -> clear errors; limit 1.5 is passed through
        let sf = p.dir.join(project::SYNC);
        let raw = std::fs::read_to_string(&sf).unwrap();
        let edit = |from: &str, to: &str| std::fs::write(&sf, raw.replacen(from, to, 1)).unwrap();
        edit("\"limit\": 1.12", "\"limit\": 2.5");
        assert!(load(&Project::load(&p.dir).unwrap()).unwrap_err().to_string().contains("limit 2.5 außerhalb 1..2"));
        edit("\"limit\": 1.12", "\"limit\": 0.9");
        assert!(load(&Project::load(&p.dir).unwrap()).is_err());
        edit("\"limit\": 1.12", "\"limit\": 1.5");
        let s = load(&Project::load(&p.dir).unwrap()).unwrap().unwrap();
        assert_eq!((s.limit, check(&s).limit, check(&s).cues[1].status), (1.5, 1.5, Atempo));
        edit("\"end_ms\": 7280", "\"end_ms\": 3999");
        let e = load(&Project::load(&p.dir).unwrap()).unwrap_err().to_string();
        assert!(e.contains("Cue 2 endet (3999 ms) vor seinem Anfang (4000 ms)"), "{e}");
        std::fs::write(&sf, &raw).unwrap();
        // GAP ≠ 0 in script.txt is ignored with a note
        std::fs::write(p.dir.join("script.txt"), "ERZ|0|Alles klar.\nERZ|0.5|Die Lieferung kommt morgen früh am Vormittag.\nERZ|0|Bitte denken Sie daran, die Unterlagen am Montag vollständig mitzubringen.\n").unwrap();
        let s = load(&Project::load(&p.dir).unwrap()).unwrap().unwrap();
        assert_eq!(s.notes, ["Cue 2: GAP 0.5 in script.txt wird ignoriert (Zeiten stehen in sync.json)"]);
        assert!(check(&s).warnings.iter().any(|w| w.contains("GAP 0.5")));
        // load: text edit in script wins (noted); count mismatch / moved lines are errors
        std::fs::write(p.dir.join("script.txt"), "ERZ|0|Alles klar.\nERZ|0|Die Lieferung kommt morgen.\nERZ|0|Bitte denken Sie daran, die Unterlagen am Montag vollständig mitzubringen.\n").unwrap();
        let s = load(&Project::load(&p.dir).unwrap()).unwrap().unwrap();
        assert_eq!((s.cues[1].text.as_str(), s.notes.len()), ("Die Lieferung kommt morgen.", 1));
        assert_eq!(check(&s).cues[1].status, Fits);
        std::fs::write(p.dir.join("script.txt"), "ERZ|0|Alles klar.\nERZ|0|Zwei.\n").unwrap();
        assert!(load(&Project::load(&p.dir).unwrap()).unwrap_err().to_string().contains("3 Cues"));
        std::fs::write(p.dir.join("script.txt"), "ERZ|0|Alles klar.\nMara|0|Anders.\nERZ|0|Drei.\n").unwrap();
        assert!(load(&Project::load(&p.dir).unwrap()).unwrap_err().to_string().contains("verschoben"));
        let (_t2, q) = project();
        assert!(load(&q).unwrap().is_none());
    }

    /// Fake TTS: a tone of chars/13 s (empty text never reaches TTS).
    fn fake(n: &Cell<usize>) -> impl FnMut(&Path, &str, &funkhaus_voice::persona::VoiceSpec, u64) -> Result<()> + '_ {
        move |w, x, _, s| {
            n.set(n.get() + 1);
            let len = (x.chars().count() as f64 / 13.0 * 24_000.0).round() as usize;
            mix::write(w, &(0..len).map(|i| (0.3 + (s % 10) as f32 * 0.01) * (i as f32 * 0.05).sin()).collect::<Vec<_>>(), 24_000)
        }
    }

    fn synced(src: &str, name: &str) -> (tempfile::TempDir, Project, Sync) {
        let (t, mut p) = project();
        import(&mut p, src, name, None, false).unwrap();
        p.meta.speakers["ERZ"].persona = Some("gt".into());
        p.save().unwrap();
        let s = load(&p).unwrap().unwrap();
        (t, p, s)
    }

    #[test]
    fn render_fit3_statuses_cache_and_reroll_namespace() {
        let (_t, p, s) = synced(FIT3, "fit3.srt");
        let n = Cell::new(0);
        let st = AtomicBool::new(false);
        let r = render_with(&p, &s, &[], Quality::Final, &st, &|_| {}, &mut fake(&n)).unwrap();
        assert_eq!(r.cues.iter().map(|c| c.status).collect::<Vec<_>>(), [Fits, Atempo, TooLong]);
        assert_eq!((r.measure, r.summary.ok, n.get()), ("rendered", false, 3));
        let ri = r.render.as_ref().unwrap();
        assert_eq!((ri.tts_calls, ri.rendered.clone(), ri.atempo_runs), (3, vec![1, 2, 3], 1));
        // too_long placed in full: sync.wav reaches past the last cue end (10 s) by the overhang
        let (m, sr) = mix::read(&ri.wav).unwrap();
        assert!(m.len() as f64 / sr as f64 > 10.0 + r.cues[2].overhang - 0.1, "{}", m.len());
        assert!(m[samples(4000, sr) + 10].abs() > 0.0 && m[samples(3900, sr)] == 0.0);
        // second run: no TTS, no ffmpeg
        n.set(0);
        let r2 = render_with(&p, &s, &[], Quality::Final, &st, &|_| {}, &mut fake(&n)).unwrap();
        let ri = r2.render.unwrap();
        assert_eq!((n.get(), ri.cached, ri.atempo_runs, ri.atempo_cached), (0, 3, 0, 1));
        // reroll in the sync namespace survives a script render
        let rr = reroll_with(&p, &s, &[], 1, false, Quality::Final, &st, &|_| {}, &mut fake(&n)).unwrap();
        assert_eq!((rr.take, n.get()), (1, 1));
        render::render_with(&p, &[], &render::Opts::default(), &st, &|_| {}, &mut fake(&n)).unwrap();
        assert!(render::sync_rerolls_dir(&p).join("rerolls.json").is_file());
        n.set(0);
        let rr = reroll_with(&p, &s, &[], 1, true, Quality::Final, &st, &|_| {}, &mut fake(&n)).unwrap();
        assert_eq!((rr.take, n.get()), (0, 0));
        n.set(0);
        render_with(&p, &s, &[], Quality::Final, &st, &|_| {}, &mut fake(&n)).unwrap();
        assert_eq!(n.get(), 0, "script render must not prune sync clips");
    }

    #[test]
    fn render_atempo_exact_slot_and_silent_cue() {
        let (_t, p, s) = synced(&format!("{OK2}\n3\n00:00:08,000 --> 00:00:09,000\n\n"), "ok2.srt");
        let n = Cell::new(0);
        let (r, clips, sr) = {
            let c = ctx(&p, &s, &[], Quality::Draft).unwrap();
            run(&p, &c, &s, Quality::Draft, &AtomicBool::new(false), &|_| {}, &mut fake(&n)).unwrap()
        };
        assert_eq!(r.cues.iter().map(|c| c.status).collect::<Vec<_>>(), [Fits, Atempo, Fits]);
        assert!(r.summary.ok && n.get() == 2);
        assert!((clips[1].len() as f64 / sr as f64 - 3.28).abs() <= TOL, "{}", clips[1].len());
        assert!(clips[2].is_empty());
        let names: Vec<String> = std::fs::read_dir(render::sync_takes_dir(&p, Quality::Draft)).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        assert_eq!(names.iter().filter(|n| atempo_name(n)).count(), 1, "{names:?}");
        assert!(!names.iter().any(|n| n.contains(".part.")), "{names:?}");
    }

    /// check estimates with the persona's speaking rate measured from cached takes (fallback 13 per s) and reports how
    /// much a too long cue would have to lose (`cut_s`).
    #[test]
    fn check_uses_measured_rate_and_reports_cut() {
        let (_t, p, s) = synced(FIT3, "fit3.srt");
        assert!(rates(&p, &[]).is_empty(), "nothing rendered yet");
        let d = check_project(&p, &s, &[]);
        assert_eq!(d.cues.iter().map(|c| (c.status, c.rate)).collect::<Vec<_>>(), [(Fits, 13.0), (Atempo, 13.0), (TooLong, 13.0)]);
        let c3 = &d.cues[2];
        assert!(near(c3.cut_s, c3.measured - c3.slot * LIMIT) && c3.cut_s > 0.0 && d.cues[1].cut_s == 0.0);
        assert!(near(d.summary.cut_s, c3.cut_s) && c3.warning.as_deref().unwrap().contains("kürzen"));
        // a fast voice: 26 chars/s rendered -> the estimate follows it
        let fast = |w: &Path, x: &str, _: &funkhaus_voice::persona::VoiceSpec, _: u64| {
            let len = (x.chars().count() as f64 / 26.0 * 24_000.0).round() as usize;
            mix::write(w, &vec![0.2; len], 24_000)
        };
        render_with(&p, &s, &[], Quality::Draft, &AtomicBool::new(false), &|_| {}, &mut { fast }).unwrap();
        let r = rates(&p, &[]);
        assert!((r["ERZ"] - 26.0).abs() < 0.5, "{r:?}");
        let d = check_project(&p, &s, &[]);
        assert!(d.cues.iter().all(|c| (c.rate - r["ERZ"]).abs() < 1e-9));
        assert!(d.cues[2].needed < c3.needed / 1.9 && d.summary.cut_s < c3.cut_s, "{:?}", d.summary);
        // other speakers / unknown persona keep the default
        assert_eq!(check_with(&s, &Rates::from([("X".to_string(), 30.0)])).cues[0].rate, 13.0);
    }
}
