//! `export wav|mp3|parts`: rendered takes (render-if-needed: cached lines cost 0 TTS, `out/mix.wav` is refreshed) plus the
//! spoken AI disclosure -> one file mastered to a loudness preset (two-pass gain + limiter, measured afterwards with ebur128).
//! `parts`: `parts/NN-<who>.wav` (trimmed clips at their mix level, no limiter) + `manifest.json` (schema stimmwerk/1,
//! omnivoice-compatible `paragraphs`) + the mastered `<name>.wav`.
//!
//! KI-Kennzeichnung: by default the first speaker (cast order) reads `settings.disclosure_text` (default per `lang`) as the
//! closing line. `--no-disclosure` drops only the spoken line: the MP3 still gets ID3 COMM (the text) and TXXX
//! `AI_GENERATED=true`, and the manifest `ai` block says `disclosed: false` – the metadata stays honest.
//!
//! Music: `[[cues]]` (`crate::cues`) are mixed on top of the speech – gen cues are rendered first (acestep, cached in
//! `takes/music/`, 0 calls when cached), then one peak divisor for speech + cues, then the master chain. A bed (loop to
//! the end) keeps playing under the spoken disclosure and fades out over the last `cues::BED_FADE` s of the mix.
use crate::render::{self, Event, Prog, Quality, Say};
use crate::{Project, cues, music, speakers};
use anyhow::{Context, Result, bail};
use funkhaus_audio::chapters;
pub use funkhaus_audio::mix::Loudness;
use funkhaus_audio::mix::{self, Fmt, Measure, Tags};
use funkhaus_audio::script::{Line, plain};
use funkhaus_voice::tts;
use serde::Serialize;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

pub const SCHEMA: &str = "stimmwerk/1";
pub const ARTIST: &str = "Stimmwerk";
/// Pause before the spoken disclosure (s).
pub const DISCLOSURE_GAP: f32 = 1.0;
/// Accepted deviation of the measured integrated loudness from the preset (LU).
pub const TOL_LU: f64 = 0.5;
/// ffmpeg binary override; default `ffmpeg` on PATH.
pub const FFMPEG_ENV: &str = "STIMMWERK_FFMPEG";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Wav,
    Mp3,
    Parts,
}

#[derive(Clone, Debug)]
pub struct Opts {
    pub kind: Kind,
    /// None: `settings.loudness`
    pub loudness: Option<Loudness>,
    /// None: `settings.disclosure`
    pub disclosure: Option<bool>,
    /// None: `<proj>/out`
    pub out: Option<PathBuf>,
    pub quality: Quality,
    /// sync project: export even with too_long/overlap cues
    pub allow_too_long: bool,
}

#[derive(Serialize, Debug)]
pub struct Target {
    pub lufs: f32,
    pub tp: f32,
}

#[derive(Serialize, Debug)]
pub struct Ai {
    /// spoken closing line present
    pub disclosed: bool,
    pub text: String,
    /// MP3: ID3 TXXX AI_GENERATED=true + COMM = text (always). WAV/parts: false – RIFF only keeps the comment (INFO ICMT),
    /// which then ends in `[AI_GENERATED=true]`.
    pub id3: bool,
}

#[derive(Serialize, Debug)]
pub struct Report {
    pub kind: Kind,
    pub preset: Loudness,
    pub file: PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manifest: Option<PathBuf>,
    pub parts: usize,
    pub duration: f64,
    pub target: Target,
    pub measured: Measure,
    /// |LUFS - target| ≤ 0.5 and TP ≤ target TP
    pub in_spec: bool,
    pub gain_db: f64,
    /// how far the mix's sample peak would exceed the limiter ceiling after `gain_db` (≈ max gain reduction, dB)
    pub limiter_gr_db: f64,
    pub ai: Ai,
    pub chapters: usize,
    pub tts_calls: usize,
    pub rendered: Vec<usize>,
    pub cached: usize,
    pub elapsed: f64,
    /// sync project: fit summary of the cues
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sync: Option<crate::sync::Summary>,
    /// music/ambience cues as placed in the mix
    pub cues: Vec<cues::Placed>,
    /// acestep renders of gen cues (0 = all cached)
    pub acestep_calls: usize,
    /// cue warnings (cut at the mix end, starts after it): also in manifest.json `warnings`
    pub warnings: Vec<String>,
}

pub fn ffmpeg() -> String {
    std::env::var(FFMPEG_ENV).ok().filter(|c| !c.is_empty()).unwrap_or_else(|| "ffmpeg".into())
}

pub fn export(p: &Project, dirs: &[PathBuf], o: &Opts, cancel: &AtomicBool, prog: &Prog) -> Result<Report> {
    render::with_real_say(p, o.quality, cancel, prog, |say| music::with_real_run(cancel, prog, |run| export_run(p, dirs, o, cancel, prog, say, run)))
}

/// `export` with an injectable `say(wav, text, voice, seed)`; gen cues must be cached (no acestep runner).
pub fn export_with(p: &Project, dirs: &[PathBuf], o: &Opts, cancel: &AtomicBool, prog: &Prog, say: &mut Say) -> Result<Report> {
    export_run(p, dirs, o, cancel, prog, say, &mut |_, _| bail!("Musik nicht im Cache und kein acestep (Test)"))
}

/// Paragraph of the manifest; times in seconds of the mix (sample-exact: `start = timeline start / sr`).
#[derive(Serialize, Debug, Clone, PartialEq)]
pub struct Para {
    pub i: usize,
    pub who: String,
    pub text: String,
    pub file: String,
    pub start: f64,
    pub end: f64,
    pub duration: f64,
    /// speaker's cast index (1-based); overlays (`over`) on their own track `speakers + cast index`, so an overlay never
    /// shares a track with the line it overlaps (HyperFrames: no duplicate audio track)
    pub track: usize,
    pub over: bool,
    /// sample peak of the part file after the master chain (dBFS)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub peak_dbfs: Option<f64>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub disclosure: bool,
    /// sync project: 1-based cue, its slot (s), fit factor and status
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cue: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub slot: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub factor: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<crate::sync::Kind>,
}

/// Manifest paragraphs from the export timeline (`lines`, trimmed clip lengths). `disc`: last line is the disclosure.
pub fn paragraphs(p: &Project, lines: &[Line], lens: &[usize], sr: u32, disc: bool) -> Vec<Para> {
    paragraphs_at(p, lines, lens, &mix::timeline(lines, lens, sr).0, sr, disc)
}

/// `paragraphs` at given start samples (sync: cue starts).
pub fn paragraphs_at(p: &Project, lines: &[Line], lens: &[usize], starts: &[usize], sr: u32, disc: bool) -> Vec<Para> {
    let w = if lines.len() > 99 { 3 } else { 2 };
    lines
        .iter()
        .zip(lens)
        .zip(starts)
        .enumerate()
        .map(|(k, ((l, &n), &s))| Para {
            i: k + 1,
            who: l.who.clone(),
            text: plain(&l.text).split_whitespace().collect::<Vec<_>>().join(" "),
            file: format!("parts/{:0w$}-{}.wav", k + 1, l.who),
            start: s as f64 / sr as f64,
            end: (s + n) as f64 / sr as f64,
            duration: n as f64 / sr as f64,
            track: p.meta.speakers.get_index_of(&l.who).map_or(0, |t| t + 1 + if l.over { p.meta.speakers.len() } else { 0 }),
            over: l.over,
            peak_dbfs: None,
            disclosure: disc && k + 1 == lines.len(),
            cue: None,
            slot: None,
            factor: None,
            status: None,
        })
        .collect()
}

/// Removes the temp file on drop.
/// Temp file removed on drop (also when an error unwinds past it).
pub(crate) struct Tmp(pub(crate) PathBuf);
impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn db(x: f32) -> f64 {
    render::r2(20.0 * (x.max(1e-9) as f64).log10())
}

fn peak(x: &[f32]) -> f32 {
    x.iter().fold(0f32, |m, v| m.max(v.abs()))
}

/// `parts/NN-<who>.wav` files listed in the previous `manifest.json` of `out` (only those are ours to delete).
fn old_parts(out: &Path) -> Vec<PathBuf> {
    let Ok(s) = std::fs::read_to_string(out.join("manifest.json")) else { return vec![] };
    let Ok(v) = serde_json::from_str::<Value>(&s) else { return vec![] };
    if v["schema"] != SCHEMA {
        return vec![];
    }
    v["paragraphs"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|p| p["file"].as_str())
        .filter_map(|f| f.strip_prefix("parts/"))
        .filter(|n| !n.contains('/') && !n.starts_with('.') && n.ends_with(".wav"))
        .map(|n| out.join("parts").join(n))
        .collect()
}

/// `export` with injectable TTS (`say`) and acestep (`run`, see `music::Run`).
pub fn export_run(p: &Project, dirs: &[PathBuf], o: &Opts, cancel: &AtomicBool, prog: &Prog, say: &mut Say, run: &mut music::Run) -> Result<Report> {
    let t0 = std::time::Instant::now();
    let ld = match o.loudness {
        Some(l) => l,
        None => p.meta.settings.loudness.parse().map_err(|e: String| anyhow::anyhow!("settings.loudness: {e}"))?,
    };
    let spoken = o.disclosure.unwrap_or(p.meta.settings.disclosure);
    let text = match p.meta.disclosure_text() {
        Some(t) => t,
        None if spoken => bail!("kein KI-Hinweistext für lang {:?}: settings.disclosure_text setzen oder --no-disclosure", p.meta.lang),
        None => crate::project::default_disclosure("en").unwrap_or_default().into(),
    };
    let sync = crate::sync::load(p)?;
    let c = match &sync {
        Some(s) => crate::sync::ctx(p, s, dirs, o.quality)?,
        None => render::ctx(p, dirs, o.quality)?,
    };
    let ff = ffmpeg();
    funkhaus_sys::which(&ff).context("ffmpeg nicht gefunden")?;
    let bad: Vec<String> = cues::lint(p).into_iter().filter(|i| i.level == "error").map(|i| i.msg).collect();
    if !bad.is_empty() {
        bail!("{} – `stimmwerk cue ls`", bad.join("; "));
    }

    // 1. render-if-needed (cache) under the project lock
    let lk = render::lock(p)?;
    let out = o.out.clone().unwrap_or_else(|| p.dir.join("out"));
    let pd = out.join("parts");
    let stem = p.dir.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "export".into());
    std::fs::create_dir_all(&out)?;
    if o.out.is_none() {
        render::sweep(&out);
        render::sweep(&pd);
    } else {
        // a folder shared with others: only temps of our own outputs
        render::sweep_in(&out, Some(&[stem.clone(), format!("{stem}-raw"), "manifest".into()]));
        let own: Vec<String> = old_parts(&out).iter().filter_map(|f| f.file_stem()).flat_map(|s| [s.to_string_lossy().into_owned(), format!("{}-raw", s.to_string_lossy())]).collect();
        render::sweep_in(&pd, Some(&own));
    }
    let mut calls = 0;
    let mut counted = |w: &Path, x: &str, v: &funkhaus_voice::persona::VoiceSpec, s: u64| -> Result<()> {
        calls += 1;
        say(w, x, v, s)
    };
    // sync project: same path as `sync render` (atempo, cue starts), gated on the fit
    let (rendered, cached, mut clips, sr, srep) = match &sync {
        Some(s) => {
            let (r, clips, sr) = crate::sync::run(p, &c, s, o.quality, cancel, prog, &mut counted)?;
            if !r.summary.ok && !o.allow_too_long {
                let bad: Vec<String> = r.cues.iter().filter(|c| matches!(c.status, crate::sync::Kind::TooLong | crate::sync::Kind::Overlap)).map(|c| c.index.to_string()).collect();
                bail!("Sync: Cue {} zu lang oder überlappend – Text kürzen (`stimmwerk sync render` zeigt den Bericht) oder --allow-too-long", bad.join(", "));
            }
            let ri = r.render.clone().context("Sync-Render lieferte keinen Render-Bericht")?;
            (ri.rendered, ri.cached, clips, sr, Some(r))
        }
        None => {
            let (rep, clips, sr) = render::run(p, &c, &render::Opts { quality: o.quality, ..Default::default() }, cancel, prog, &mut counted)?;
            (rep.rendered, rep.cached, clips, sr, None)
        }
    };
    let mut lines = c.lines.clone();
    let (mut disc_voices, mut disc_clips) = (tts::Voices::new(), vec![]);

    // 1b. music of gen cues (acestep, cached under its key: 0 calls when every file is there)
    let mut acalls = 0;
    let gens: Vec<&cues::Gen> = p.meta.cues.iter().filter_map(|c| c.gen_.as_ref()).collect();
    if gens.iter().any(|g| music::lookup(p, g).is_none()) {
        prog(Event::Stage { name: "music".into() });
    }
    for g in gens {
        music::generate_with(p, g, false, &mut |a, out| {
            acalls += 1;
            run(a, out)
        })
        .map_err(|e| render::cancelled(e, cancel))?;
    }

    // 2. disclosure line by the first cast speaker; own cache dir so normal renders never prune it
    if spoken {
        prog(Event::Stage { name: "disclosure".into() });
        let (who, s) = p.meta.speakers.first().context("keine Sprecher in project.toml")?;
        let Some(n) = &s.persona else { bail!("KI-Hinweis: Sprecher {who} hat keine Persona") };
        let voices = &mut disc_voices;
        voices.insert(who.clone(), speakers::spec(dirs, n)?);
        let l = Line::new(who.clone(), DISCLOSURE_GAP, text.clone());
        let dir = c.dir.join("ai");
        std::fs::create_dir_all(&dir)?;
        let txt = |m: &str| prog(Event::Msg { text: m.into() });
        let q = tts::RenderReq { t: &c.t, lines: std::slice::from_ref(&l), voices, lex: &c.lex, lang: &c.t.lang, dir: &dir, fresh: false, rr: &Default::default(), prog: &txt, cancel: Some(cancel), on_line: None };
        let parts = tts::render_with(&q, &mut counted).map_err(|e| render::cancelled(e, cancel))?;
        disc_clips = parts.iter().flatten().filter_map(|p| if let tts::Part::Wav(w) = p { Some(w.clone()) } else { None }).collect();
        let (dc, dsr) = tts::clips(&parts)?;
        if dsr != sr {
            bail!("KI-Hinweis: Abtastrate {dsr} ≠ {sr}");
        }
        clips.extend(dc);
        lines.push(l);
    }

    // 3. mix (float, no clipping before the gain) + master into a hidden temp, renamed when complete
    prog(Event::Stage { name: "export".into() });
    prog(Event::Master);
    let cl: Vec<&[f32]> = clips.iter().map(Vec::as_slice).collect();
    // sync: absolute cue starts; disclosure DISCLOSURE_GAP after the last cue end (or overhanging clip), then 0.5 s tail
    let sync_starts = sync.as_ref().map(|s| {
        let mut st = crate::sync::starts(s, sr);
        // end of the last placed clip (cue clips, then the disclosure once pushed)
        let last = |st: &[usize]| st.iter().zip(&cl).map(|(a, c)| a + c.len()).fold(crate::sync::end(s, sr), usize::max);
        if spoken {
            st.push(last(&st) + (DISCLOSURE_GAP * sr as f32).round() as usize);
        }
        let total = last(&st) + if spoken { sr as usize / 2 } else { 0 };
        (st, total)
    });
    let lens: Vec<usize> = clips.iter().map(Vec::len).collect();
    let (starts, total) = match &sync_starts {
        Some((st, total)) => (st.clone(), *total),
        None => mix::timeline(&lines, &lens, sr),
    };
    // cues anchor on script lines only (not the disclosure); a bed runs to the mix end, which then is BED_FADE after the last clip
    let ns = c.lines.len();
    let end = starts.iter().zip(&lens).map(|(s, n)| s + n).max().unwrap_or(0);
    let total = cues::mix_len(&p.meta.cues, ns, end, total, sr);
    let mut m = mix::place_raw(&starts, &cl, &lines.iter().map(mix::line_gain).collect::<Vec<_>>(), total);
    let placed = cues::mix_into(p, &mut m, &starts[..ns], sr, |cu| cues::source(p, cu, &ff, sr))?;
    let warnings = cues::warnings(&placed);
    for w in &warnings {
        prog(Event::Msg { text: format!("Warnung: {w}") });
    }
    let k = mix::norm(&mut m);
    let raw = Tmp(render::part_path(&out.join(format!("{stem}-raw.wav"))));
    mix::write_f32(&raw.0, &m, sr)?;
    let (fmt, ext) = if o.kind == Kind::Mp3 { (Fmt::Mp3, "mp3") } else { (Fmt::Wav, "wav") };
    let file = out.join(format!("{stem}.{ext}"));
    let tmpf = Tmp(render::part_path(&file));
    let tags = Tags { title: &p.meta.title, artist: ARTIST, comment: Some(&text) };
    let gain = mix::master(&ff, &raw.0, &tmpf.0, ld, fmt, tags, true)?;
    // limiter work on the mix: sample peak after the gain above the ceiling
    let limiter_gr_db = render::r2((db(peak(&m)) + gain as f64 - ld.limit_db_for(fmt) as f64).max(0.0));
    drop(raw);

    // 4. chapters (`# SEG`): ID3 CHAP in the MP3, list in the manifest
    let marks = chapters::marks(&p.script.format());
    let chs = if marks.is_empty() || sync.is_some() { vec![] } else { chapters::spans(&marks, &lines, &lens, sr, 0) };
    if fmt == Fmt::Mp3 && !chs.is_empty() {
        let meta = Tmp(render::part_path(&out.join(format!("{stem}.ffmeta"))));
        std::fs::write(&meta.0, chapters::ffmeta(&p.meta.title, ARTIST, &chs))?;
        chapters::embed(&ff, &tmpf.0, &meta.0, &chs)?;
    }

    let measured = mix::measure_file(&ff, &tmpf.0)?;
    let (ti, ttp, _) = ld.target();
    let in_spec = (measured.lufs - ti as f64).abs() <= TOL_LU && measured.tp <= ttp as f64;
    if !in_spec {
        prog(Event::Msg { text: format!("Warnung: gemessen {:.1} LUFS / {:.1} dBTP, Ziel {ti} LUFS / ≤ {ttp} dBTP", measured.lufs, measured.tp) });
    }
    let duration = m.len() as f64 / sr as f64;
    let id3 = fmt == Fmt::Mp3;
    let ai = Ai { disclosed: spoken, text: text.clone(), id3 };

    // 5. parts: each clip at mix level through the master's chain (same gain + limiter) -> matches the master where
    //    the line plays alone, peak ≤ ceiling; + manifest
    let (mut manifest, mut nparts) = (None, 0);
    if o.kind == Kind::Parts {
        prog(Event::Stage { name: "parts".into() });
        std::fs::create_dir_all(&pd)?;
        for f in old_parts(&out).iter().filter(|f| f.is_file()) {
            std::fs::remove_file(f).with_context(|| f.display().to_string())?;
        }
        let mut paras = match &sync_starts {
            Some((st, _)) => paragraphs_at(p, &lines, &lens, st, sr, spoken),
            None => paragraphs(p, &lines, &lens, sr, spoken),
        };
        if let Some(r) = &srep {
            for (pa, c) in paras.iter_mut().zip(&r.cues) {
                (pa.cue, pa.slot, pa.factor, pa.status) = (Some(c.index), Some(c.slot), c.factor, Some(c.status));
            }
        }
        for ((pa, l), c) in paras.iter_mut().zip(&lines).zip(&clips) {
            let f = out.join(&pa.file);
            let rawp = Tmp(render::part_path(&f.with_file_name(format!("{}-raw.wav", f.file_stem().unwrap_or_default().to_string_lossy()))));
            let x: Vec<f32> = c.iter().map(|v| v * mix::line_gain(l) / k).collect();
            mix::write_f32(&rawp.0, &x, sr)?;
            render::atomic(&f, |t| mix::chain_file(&ff, &rawp.0, t, ld, fmt, gain, true))?;
            pa.peak_dbfs = Some(db(peak(&mix::read(&f)?.0)));
        }
        nparts = paras.len();
        let parts_peak = paras.iter().filter_map(|p| p.peak_dbfs).fold(f64::NEG_INFINITY, f64::max);
        let v = json!({
            "schema": SCHEMA, "title": p.meta.title, "lang": p.meta.lang, "preset": ld, "sr": sr,
            "lufs": measured.lufs, "tp": measured.tp, "target": {"lufs": ti, "tp": ttp},
            "gap": p.meta.settings.gap, "duration": duration, "mix": file.file_name().map(|f| f.to_string_lossy()),
            "parts_gain_db": render::r2(gain as f64 - 20.0 * (k as f64).log10()),
            "limit_dbfs": render::r2(ld.limit_db_for(fmt) as f64),
            "limiter_gr_db": limiter_gr_db,
            "parts_peak_dbfs": if parts_peak.is_finite() { json!(parts_peak) } else { Value::Null },
            "ai": {"disclosed": spoken, "text": text, "id3": id3, "riff_comment": format!("{text} [{}]", mix::WAV_AI_FLAG)},
            "paragraphs": paras,
            "cues": placed,
            "warnings": warnings,
            "chapters": chs.iter().map(|c| json!({"n": c.n, "title": c.title, "start": c.start_ms as f64 / 1000.0, "end": c.end_ms as f64 / 1000.0})).collect::<Vec<Value>>(),
        });
        let mut v = v;
        if let (Some(s), Some(r)) = (&sync, &srep) {
            v["sync"] = json!({"schema": crate::sync::SCHEMA, "limit": s.limit, "ok": r.summary.ok, "summary": r.summary});
        }
        let f = out.join("manifest.json");
        crate::project::write(&f, &serde_json::to_string_pretty(&v)?)?;
        manifest = Some(f);
    }
    std::fs::rename(&tmpf.0, &file).with_context(|| file.display().to_string())?;
    // exported audio of ref voices (clones/anchored designs) is deleted with the voice on revoke
    let outs: Vec<PathBuf> = std::iter::once(file.clone()).chain(if o.kind == Kind::Parts { old_parts(&out) } else { vec![] }).collect();
    let disc = disc_voices.keys().next().cloned().unwrap_or_default();
    let used = c.voices.keys().map(|k| (k.as_str(), vec![])).chain(std::iter::once((disc.as_str(), disc_clips)));
    let mut all = c.voices.clone();
    all.extend(disc_voices.clone());
    // the export is written already: a failing index must not turn it into an error, but must not be silent either
    if let Err(e) = render::record_ref_audio(p, &all, used, &outs) {
        prog(Event::Msg { text: format!("Warnung: Export nicht im Stimmen-Index vermerkt ({e:#}) – ein späterer Widerruf findet diese Datei nicht automatisch") });
    }
    drop(lk);
    prog(Event::Done);

    Ok(Report {
        kind: o.kind,
        preset: ld,
        file,
        manifest,
        parts: nparts,
        duration: render::r2(duration),
        target: Target { lufs: ti, tp: ttp },
        measured,
        in_spec,
        gain_db: render::r2(gain as f64),
        limiter_gr_db,
        ai,
        chapters: chs.len(),
        tts_calls: calls,
        rendered,
        cached,
        elapsed: render::r2(t0.elapsed().as_secs_f64()),
        sync: srep.map(|r| r.summary),
        cues: placed,
        acestep_calls: acalls,
        warnings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    const FIX: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/stimmwerk/project/leuchtturm.studio");

    fn fixture() -> (tempfile::TempDir, Project) {
        let t = tempfile::tempdir().unwrap();
        let d = t.path().join("l.studio");
        std::fs::create_dir_all(&d).unwrap();
        for f in ["project.toml", "script.txt", "lexicon.tsv"] {
            std::fs::copy(Path::new(FIX).join(f), d.join(f)).unwrap();
        }
        (t, Project::load(&d).unwrap())
    }

    /// Fake TTS: 0.25 s noise-ish tone, level by seed.
    fn fake(n: &Cell<usize>) -> impl FnMut(&Path, &str, &funkhaus_voice::persona::VoiceSpec, u64) -> Result<()> + '_ {
        move |w, _, _, s| {
            n.set(n.get() + 1);
            let a = 0.2 + (s % 100) as f32 * 0.002;
            mix::write(w, &(0..6000).map(|i| a * (i as f32 * 0.07).sin()).collect::<Vec<_>>(), 24_000)
        }
    }

    #[test]
    fn manifest_times_are_the_timeline() {
        let (_t, p) = fixture();
        let lines: Vec<Line> = p.script.lines().cloned().collect();
        let lens = [24_000, 12_000, 4_800, 30_000, 20_000, 18_000, 22_000];
        let ps = paragraphs(&p, &lines, &lens, 24_000, false);
        let (starts, _) = mix::timeline(&lines, &lens, 24_000);
        assert_eq!(ps.len(), 7);
        for ((pa, s), n) in ps.iter().zip(&starts).zip(lens) {
            assert_eq!((pa.start, pa.duration, pa.end), (*s as f64 / 24e3, n as f64 / 24e3, (s + n) as f64 / 24e3));
        }
        assert_eq!((ps[2].file.as_str(), ps[2].who.as_str(), ps[2].over, ps[2].track), ("parts/03-Jonas.wav", "Jonas", true, p.meta.speakers.len() + 3));
        assert!(ps[2].start < ps[1].end, "overlay overlaps");
        assert_eq!(ps[1].text, "Da ist er, der Leuchtturm!");
        assert_eq!(ps[3].text, "Er ist kleiner, als ich dachte. Aber schön.");
    }

    /// Fake TTS that drives the limiter: quiet speech-band tone with loud, smoothly enveloped bursts (high crest factor).
    fn hot(w: &Path, _: &str, _: &funkhaus_voice::persona::VoiceSpec, s: u64) -> Result<()> {
        let env = |i: usize| {
            let k = (i % 6000) as f32;
            if k < 1200.0 { (std::f32::consts::PI * k / 1200.0).sin().powi(2) } else { 0.0 }
        };
        let x: Vec<f32> = (0..12_000).map(|i| (0.02 + 0.95 * env(i) + (s % 7) as f32 * 0.001) * (i as f32 * 0.13).sin()).collect();
        mix::write(w, &x, 24_000)
    }

    #[test]
    fn parts_limited_like_master_and_wav_comment() {
        let (_t, p) = fixture();
        let pd = p.dir.join("out/parts");
        std::fs::create_dir_all(&pd).unwrap();
        std::fs::write(pd.join("notes.wav"), b"mine").unwrap();
        std::fs::write(pd.join("10-Intro.wav"), b"mine too").unwrap();
        std::fs::write(pd.join("99-old.wav"), b"x").unwrap();
        // only files the previous manifest lists are ours to replace
        let old = json!({"schema": SCHEMA, "paragraphs": [{"file": "parts/99-old.wav"}, {"file": "parts/../escape.wav"}]});
        std::fs::write(p.dir.join("out/manifest.json"), old.to_string()).unwrap();
        std::fs::write(p.dir.join("out/escape.wav"), b"x").unwrap();
        let o = Opts { kind: Kind::Parts, loudness: Some(Loudness::Podcast), disclosure: None, out: None, quality: Quality::Final, allow_too_long: false };
        let r = export_with(&p, &[], &o, &AtomicBool::new(false), &|_| {}, &mut hot).unwrap();
        assert!(r.limiter_gr_db > 3.0, "limiter must work: {}", r.limiter_gr_db);
        assert!(r.in_spec, "{:?}", r.measured);
        let ceil = Loudness::Podcast.limit_lin(Fmt::Wav);
        let v: Value = serde_json::from_str(&std::fs::read_to_string(r.manifest.unwrap()).unwrap()).unwrap();
        for pa in v["paragraphs"].as_array().unwrap() {
            let (x, _) = mix::read(&p.dir.join("out").join(pa["file"].as_str().unwrap())).unwrap();
            let pk = peak(&x);
            assert!(pk <= ceil + 1e-3, "{} peak {pk} > {ceil}", pa["file"]);
            assert!((pa["peak_dbfs"].as_f64().unwrap() - db(pk)).abs() < 0.01);
        }
        assert!(v["parts_peak_dbfs"].as_f64().unwrap() <= Loudness::Podcast.limit_db_for(Fmt::Wav) as f64 + 0.01);
        assert_eq!(v["limiter_gr_db"].as_f64(), Some(r.limiter_gr_db));
        assert!(pd.join("notes.wav").exists() && pd.join("10-Intro.wav").exists() && !pd.join("99-old.wav").exists());
        assert!(p.dir.join("out/escape.wav").exists());
        // WAV: no ID3; the AI flag lives in the RIFF comment
        assert!(!r.ai.id3 && v["ai"]["id3"] == false);
        let probe = funkhaus_sys::which(&ffmpeg()).and_then(|b| b.parent().map(|d| d.join("ffprobe"))).filter(|p| p.exists()).or_else(|| funkhaus_sys::which("ffprobe")).expect("ffprobe");
        let o = std::process::Command::new(probe).args(["-v", "error", "-show_entries", "format_tags=comment", "-of", "default=nw=1:nk=1"]).arg(&r.file).output().unwrap();
        let c = String::from_utf8_lossy(&o.stdout);
        assert!(c.contains("[AI_GENERATED=true]") && c.contains(&r.ai.text), "{c}");
        assert!(!std::fs::read_dir(p.dir.join("out")).unwrap().chain(std::fs::read_dir(&pd).unwrap()).flatten().any(|e| e.file_name().to_string_lossy().contains(".part.")));
    }

    #[test]
    fn export_parts_and_mp3() {
        // no skip: without ffmpeg this test fails (export is untested otherwise)
        let (_t, p) = fixture();
        let n = Cell::new(0);
        let st = AtomicBool::new(false);
        let o = Opts { kind: Kind::Parts, loudness: None, disclosure: None, out: None, quality: Quality::Final, allow_too_long: false };
        let r = export_with(&p, &[], &o, &st, &|_| {}, &mut fake(&n)).unwrap();
        assert_eq!((r.tts_calls, r.parts, r.chapters, r.preset), (9, 8, 2, Loudness::Hoerbuch)); // 8 clips + disclosure
        assert!(r.ai.disclosed && p.dir.join("out/mix.wav").is_file());
        let v: Value = serde_json::from_str(&std::fs::read_to_string(r.manifest.as_ref().unwrap()).unwrap()).unwrap();
        assert_eq!((v["schema"].as_str(), v["preset"].as_str(), v["sr"].as_u64(), v["mix"].as_str()), (Some(SCHEMA), Some("hoerbuch"), Some(24_000), Some("l.wav")));
        let ps = v["paragraphs"].as_array().unwrap();
        assert_eq!(ps.len(), 8);
        assert_eq!((ps[7]["who"].as_str(), ps[7]["disclosure"].as_bool(), ps[7]["text"].as_str()), (Some("ERZ"), Some(true), p.meta.disclosure_text().as_deref()));
        for pa in ps {
            let (x, sr) = mix::read(&p.dir.join("out").join(pa["file"].as_str().unwrap())).unwrap();
            assert_eq!(x.len() as f64 / sr as f64, pa["duration"].as_f64().unwrap());
        }
        // parts at mix level: summing them reproduces the mastered mix (before the limiter) – check one clean sample
        let (mixw, _) = mix::read(&p.dir.join("out/l.wav")).unwrap();
        let (p0, _) = mix::read(&p.dir.join("out/parts/01-ERZ.wav")).unwrap();
        let i = p0.iter().enumerate().max_by(|a, b| a.1.abs().total_cmp(&b.1.abs())).unwrap().0;
        assert!((mixw[i] - p0[i]).abs() < 0.01, "mix {} part {}", mixw[i], p0[i]);
        assert_eq!(v["chapters"].as_array().unwrap().len(), 2);
        assert!(v["duration"].as_f64().unwrap() > ps[7]["end"].as_f64().unwrap());
        // second export: everything cached; mp3 without spoken disclosure still flags AI
        n.set(0);
        let o = Opts { kind: Kind::Mp3, loudness: Some(Loudness::Podcast), disclosure: Some(false), out: None, quality: Quality::Final, allow_too_long: false };
        let r = export_with(&p, &[], &o, &st, &|_| {}, &mut fake(&n)).unwrap();
        assert_eq!((n.get(), r.cached, r.ai.disclosed, r.ai.id3), (0, 7, false, true));
        assert!(r.file.ends_with("out/l.mp3") && (r.measured.lufs + 16.0).abs() <= TOL_LU, "{:?}", r.measured);
        assert!(!std::fs::read_dir(p.dir.join("out")).unwrap().flatten().any(|e| e.file_name().to_string_lossy().starts_with('.')), "temp files left");
        // disclosure clip survives a normal render (own cache dir)
        render::render_with(&p, &[], &render::Opts::default(), &st, &|_| {}, &mut fake(&n)).unwrap();
        n.set(0);
        let o = Opts { kind: Kind::Wav, loudness: Some(Loudness::Video), disclosure: None, out: Some(p.dir.join("x")), quality: Quality::Final, allow_too_long: false };
        let r = export_with(&p, &[], &o, &st, &|_| {}, &mut fake(&n)).unwrap();
        assert_eq!((n.get(), r.parts, r.manifest.is_none()), (0, 0, true));
        assert!(r.file.ends_with("x/l.wav"));
    }

    /// Sync project: cue starts, atempo, gate on too_long, disclosure after the last cue, manifest fields.
    #[test]
    fn export_sync_gate_disclosure_manifest() {
        let t = tempfile::tempdir().unwrap();
        let mut p = Project::create(&t.path().join("s.studio"), "S", "de").unwrap();
        let fit3 = include_str!("../../../fixtures/stimmwerk/sync/fit3.srt");
        crate::sync::import(&mut p, fit3, "fit3.srt", None, false).unwrap();
        p.meta.speakers["ERZ"].persona = Some("gt".into());
        p.save_meta().unwrap();
        // tone of chars/13 s, like the sync tests
        let n = Cell::new(0);
        let mut say = |w: &Path, x: &str, _: &funkhaus_voice::persona::VoiceSpec, _: u64| -> Result<()> {
            n.set(n.get() + 1);
            let len = (x.chars().count() as f64 / 13.0 * 24_000.0).round() as usize;
            mix::write(w, &(0..len).map(|i| 0.3 * (i as f32 * 0.05).sin()).collect::<Vec<_>>(), 24_000)
        };
        let st = AtomicBool::new(false);
        let mut o = Opts { kind: Kind::Parts, loudness: Some(Loudness::Podcast), disclosure: None, out: None, quality: Quality::Final, allow_too_long: false };
        let e = export_with(&p, &[], &o, &st, &|_| {}, &mut say).unwrap_err();
        assert!(e.to_string().contains("Cue 3 zu lang") && e.to_string().contains("--allow-too-long"), "{e}");
        assert!(p.dir.join("out/sync.wav").is_file() && !p.dir.join("out/s.wav").exists());
        o.allow_too_long = true;
        let r = export_with(&p, &[], &o, &st, &|_| {}, &mut say).unwrap();
        assert_eq!((r.chapters, r.sync.as_ref().map(|s| (s.ok, s.atempo, s.too_long))), (0, Some((false, 1, 1))));
        assert_eq!(r.tts_calls, 1, "only the disclosure is new");
        let v: Value = serde_json::from_str(&std::fs::read_to_string(r.manifest.unwrap()).unwrap()).unwrap();
        assert_eq!((v["sync"]["schema"].as_str(), v["sync"]["limit"].as_f64()), (Some(crate::sync::SCHEMA), Some(1.12)));
        let ps = v["paragraphs"].as_array().unwrap();
        assert_eq!(ps.len(), 4);
        assert_eq!((ps[0]["start"].as_f64(), ps[1]["start"].as_f64(), ps[2]["start"].as_f64()), (Some(1.0), Some(4.0), Some(8.0)));
        assert_eq!((ps[1]["cue"].as_u64(), ps[1]["slot"].as_f64(), ps[1]["status"].as_str()), (Some(2), Some(3.28), Some("atempo")));
        assert!((ps[1]["duration"].as_f64().unwrap() - 3.28).abs() <= crate::sync::TOL && ps[1]["factor"].as_f64().unwrap() > 1.0);
        assert_eq!(ps[2]["status"].as_str(), Some("too_long"));
        // disclosure: 1 s after the end of the overhanging last clip (past the cue end at 10 s)
        let last = ps[2]["end"].as_f64().unwrap();
        assert!(last > 10.0 && ps[3]["disclosure"] == true && ps[3]["cue"].is_null());
        assert!((ps[3]["start"].as_f64().unwrap() - (last + DISCLOSURE_GAP as f64)).abs() < 1e-3, "{} vs {last}", ps[3]["start"]);
    }

    /// Hörspiel fixture with both cues (gen intro ducked at line 1, imported ambience loop from line 18 to the end):
    /// gen cue rendered once via the fake acestep and cached afterwards (0 calls), cue starts = line starts of the
    /// manifest paragraphs, the bed ends with the mix (BED_FADE tail, also under the disclosure), lint gate.
    #[test]
    fn export_mixes_cues_and_caches_music() {
        let t = tempfile::tempdir().unwrap();
        let fix = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/stimmwerk/hoerspiel"));
        let d = t.path().join("h.studio");
        std::fs::create_dir_all(d.join("assets/ambience")).unwrap();
        for f in ["project.toml", "script.txt"] {
            std::fs::copy(fix.join(f), d.join(f)).unwrap();
        }
        mix::write(&d.join("assets/ambience/meer_cc0.wav"), &vec![0.2f32; 24_000 * 8], 24_000).unwrap();
        let mut p = Project::load(&d).unwrap();
        // cue 1 as generation instead of the placeholder file
        p.meta.cues[0].file = None;
        p.meta.cues[0].gen_ = Some(crate::cues::Gen { prompt: "calm maritime intro, acoustic guitar".into(), dur: 12, seed: 1, bpm: 70, key: Some("A minor".into()), model: None });
        p.save_meta().unwrap();
        let (n, g) = (Cell::new(0), Cell::new(0));
        let mut run = |a: &[String], o: &Path| {
            g.set(g.get() + 1);
            assert!(a.contains(&"-i".to_string()) && a.contains(&"12".to_string()) && a.contains(&"wav24".to_string()), "{a:?}");
            mix::write(o, &vec![0.4f32; 48_000 * 12], 48_000) // acestep rate ≠ mix rate: resampled by ffmpeg
        };
        let st = AtomicBool::new(false);
        let o = Opts { kind: Kind::Parts, loudness: None, disclosure: Some(false), out: None, quality: Quality::Final, allow_too_long: false };
        let r = export_run(&p, &[], &o, &st, &|_| {}, &mut fake(&n), &mut run).unwrap();
        assert_eq!((r.acestep_calls, r.cues.len(), r.tts_calls), (1, 2, 25), "{:?}", r.cues);
        assert!(p.dir.join("takes/music").join(format!("{}.wav", crate::music::key(p.meta.cues[0].gen_.as_ref().unwrap()))).is_file());
        let v: Value = serde_json::from_str(&std::fs::read_to_string(r.manifest.as_ref().unwrap()).unwrap()).unwrap();
        let ps = v["paragraphs"].as_array().unwrap();
        let (c1, c2) = (&r.cues[0], &r.cues[1]);
        assert_eq!((c1.start, c1.duck, c1.source.starts_with("gen:")), (ps[0]["start"].as_f64().unwrap(), true, true));
        assert_eq!((c2.start, c2.duck, c2.clamped), (ps[17]["start"].as_f64().unwrap(), false, false));
        let speech_end = ps.iter().map(|x| x["end"].as_f64().unwrap()).fold(0.0, f64::max);
        assert!((c2.end - r.duration).abs() < 1e-9, "bed ends with the mix: {} vs {}", c2.end, r.duration);
        assert!((c1.end - c1.start - 12.0).abs() < 0.01 && !c1.clamped);
        assert_eq!(v["cues"].as_array().unwrap().len(), 2);
        assert!(r.warnings.is_empty() && v["warnings"] == json!([]), "{:?}", r.warnings);
        assert!((r.duration - (speech_end + cues::BED_FADE)).abs() < 0.001, "a bed ends BED_FADE after the last line: {}", r.duration);
        // with the spoken disclosure the bed runs under it and ends BED_FADE after it
        let od = Opts { disclosure: Some(true), ..o.clone() };
        let r = export_run(&p, &[], &od, &st, &|_| {}, &mut fake(&n), &mut run).unwrap();
        let v: Value = serde_json::from_str(&std::fs::read_to_string(r.manifest.as_ref().unwrap()).unwrap()).unwrap();
        let ps = v["paragraphs"].as_array().unwrap();
        let disc_end = ps.last().unwrap()["end"].as_f64().unwrap();
        assert!(ps.last().unwrap()["disclosure"] == true && disc_end > speech_end);
        assert!((r.duration - (disc_end + cues::BED_FADE)).abs() < 0.001 && (r.cues[1].end - r.duration).abs() < 1e-9, "{} {} {:?}", r.duration, disc_end, r.cues);
        // without a bed the tail stays 0.5 s
        let mut q = p.clone();
        q.meta.cues[1].loop_until_line = Some(20);
        let r = export_run(&q, &[], &o, &st, &|_| {}, &mut fake(&n), &mut run).unwrap();
        assert!((r.duration - (speech_end + 0.5)).abs() < 0.001 && (r.cues[1].end - ps[19]["start"].as_f64().unwrap()).abs() < 1e-9, "{:?}", r.cues);
        // second export: everything cached (0 TTS, 0 acestep)
        n.set(0);
        let r = export_run(&p, &[], &o, &st, &|_| {}, &mut fake(&n), &mut run).unwrap();
        assert_eq!((r.acestep_calls, g.get(), n.get(), r.cues.len()), (0, 1, 0, 2));
        // `export_with` (no runner) works on the cached music too; a lint error refuses before any TTS
        assert_eq!(export_with(&p, &[], &o, &st, &|_| {}, &mut fake(&n)).unwrap().acestep_calls, 0);
        // an 8 s cue on the last (0.25 s) line is cut at the mix end: warning in report + manifest
        let mut late = crate::cues::Cue::new(crate::cues::Kind::Ambience);
        (late.at_line, late.file) = (Some(25), Some("assets/ambience/meer_cc0.wav".into()));
        p.meta.cues.push(late);
        let r = export_with(&p, &[], &o, &st, &|_| {}, &mut fake(&n)).unwrap();
        assert!(r.cues[2].clamped && r.warnings.len() == 1 && r.warnings[0].starts_with("Cue 3:") && r.warnings[0].contains("abgeschnitten"), "{:?}", r.warnings);
        let v: Value = serde_json::from_str(&std::fs::read_to_string(r.manifest.as_ref().unwrap()).unwrap()).unwrap();
        assert_eq!(v["warnings"].as_array().unwrap().len(), 1);
        p.meta.cues.pop();
        p.meta.cues[1].at_line = Some(99);
        let e = export_with(&p, &[], &o, &st, &|_| {}, &mut fake(&n)).unwrap_err();
        assert!(e.to_string().contains("Cue 2") && e.to_string().contains("at_line 99"), "{e}");
    }

    /// Paths with spaces and umlauts (project and custom --out); a custom --out only loses our own dead temps.
    #[test]
    fn export_spaces_umlauts_custom_out() {
        let t = tempfile::tempdir().unwrap();
        let d = t.path().join("Mein Ordner/Hörspiel Ä.studio");
        std::fs::create_dir_all(&d).unwrap();
        for f in ["project.toml", "script.txt", "lexicon.tsv"] {
            std::fs::copy(Path::new(FIX).join(f), d.join(f)).unwrap();
        }
        let p = Project::load(&d).unwrap();
        let out = t.path().join("Ausgabe für alle");
        std::fs::create_dir_all(&out).unwrap();
        let foreign = [".anderes.part.999999-1.wav", ".Hörspiel Ä-alt.part.999999-1.wav", "x.part.wav"];
        for f in foreign {
            std::fs::write(out.join(f), b"x").unwrap();
        }
        std::fs::write(out.join(".Hörspiel Ä.part.999999-3.wav"), b"x").unwrap(); // ours, dead process
        let n = Cell::new(0);
        let o = Opts { kind: Kind::Mp3, loudness: Some(Loudness::Podcast), disclosure: None, out: Some(out.clone()), quality: Quality::Final, allow_too_long: false };
        let r = export_with(&p, &[], &o, &AtomicBool::new(false), &|_| {}, &mut fake(&n)).unwrap();
        assert!(r.file.ends_with("Ausgabe für alle/Hörspiel Ä.mp3") && r.file.is_file());
        assert!(r.in_spec, "{:?}", r.measured);
        assert!(!out.join(".Hörspiel Ä.part.999999-3.wav").exists());
        for f in foreign {
            assert!(out.join(f).exists(), "{f} is not ours");
        }
    }
}
