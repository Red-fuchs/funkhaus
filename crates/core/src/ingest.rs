use crate::{Config, Prog};
use serde::{Deserialize, Serialize};
use anyhow::{Context, Result, bail};
use std::io::Read;
use std::path::{Path, PathBuf};

pub const UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.0 Safari/605.1.15";
const TEXT: &[&str] = &["md", "markdown", "txt", "text", "pdf", "srt", "vtt", "html", "htm", "docx"];
const MEDIA: &[&str] = &["mp3", "wav", "m4a", "aac", "flac", "ogg", "opus", "mp4", "mov", "mkv", "webm"];

fn ext(p: &Path) -> String {
    p.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default()
}

pub fn supported(p: &Path) -> bool {
    let e = ext(p);
    TEXT.contains(&e.as_str()) || MEDIA.contains(&e.as_str())
}

/// Supported files below `dir`, sorted, hidden entries skipped.
pub fn walk(dir: &Path) -> Vec<PathBuf> {
    walk_report(dir).0
}

/// Like `walk`, plus the number of iCloud placeholders (`.name.ext.icloud`, not downloaded).
pub fn walk_report(dir: &Path) -> (Vec<PathBuf>, usize) {
    let (mut v, mut icloud) = (vec![], 0);
    let Ok(rd) = std::fs::read_dir(dir) else { return (v, 0) };
    for e in rd.flatten() {
        let p = e.path();
        let name = e.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            if name.ends_with(".icloud") && supported(Path::new(name.trim_end_matches(".icloud"))) {
                icloud += 1;
            }
            continue;
        }
        if p.is_dir() {
            let (f, i) = walk_report(&p);
            v.extend(f);
            icloud += i;
        } else if supported(&p) {
            v.push(p);
        }
    }
    v.sort();
    (v, icloud)
}

pub fn extract(p: &Path, cfg: &Config) -> Result<Fetched> {
    let e = ext(p);
    let stem = p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let read = || std::fs::read_to_string(p).map(|s| s.replace("\r\n", "\n"));
    let mut cs = None;
    let mut oc = vec![];
    let (title, text) = match e.as_str() {
        "md" | "markdown" | "txt" | "text" => {
            let t = read()?;
            (md_title(&t).unwrap_or(stem), t)
        }
        "pdf" => (stem, pdf(&std::fs::read(p)?)?),
        "srt" | "vtt" => {
            let raw = read()?;
            let c = cues(&raw);
            let t = cue_text(&c);
            cs = Some(c);
            oc = outcut_vtt(&raw);
            (stem, t)
        }
        "html" | "htm" => {
            let t = read()?;
            (html_title(&t).unwrap_or(stem), html(&t)?)
        }
        "docx" => (stem, docx(p)?),
        e if MEDIA.contains(&e) => (stem, asr(p, cfg)?),
        _ => bail!("Dateityp nicht unterstützt: .{e}"),
    };
    let kind = if MEDIA.contains(&e.as_str()) { "audio".into() } else { e };
    // co-cut sidecar `<stem>.outcut.json` next to the file
    let side = p.with_file_name(format!("{}.outcut.json", p.file_stem().unwrap_or_default().to_string_lossy()));
    if side.exists() {
        oc.extend(outcut_json(&side)?);
    }
    merge(&mut oc);
    Ok(Fetched { title, kind, text: tidy(&text), cues: cs, outcut: oc, ..Default::default() })
}

/// Result of importing one URL.
#[derive(Debug, Default, Clone)]
pub struct Fetched {
    pub title: String,
    pub kind: String,
    pub text: String,
    pub extractor: Option<String>,
    pub has_transcript: Option<bool>,
    pub warnings: Vec<String>,
    /// timed caption lines (SRT/VTT files, video captions)
    pub cues: Option<Vec<Cue>>,
    /// excluded time ranges (private parts), never chunked
    pub outcut: Vec<Outcut>,
}

/// Excluded time range of a timed source. `src`: vtt | json | cli.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
pub struct Outcut {
    pub t0: f64,
    pub t1: f64,
    #[serde(default)]
    pub reason: String,
    #[serde(default)]
    pub src: String,
}

impl Outcut {
    pub fn hits(&self, t0: f64, t1: f64) -> bool {
        t0 < self.t1 && t1 > self.t0
    }
}

/// `NOTE outcut:start [reason]` … `NOTE outcut:end` → range from the first to the last cue in between.
pub fn outcut_vtt(s: &str) -> Vec<Outcut> {
    let (mut v, mut open): (Vec<Outcut>, Option<Outcut>) = (vec![], None);
    for l in s.lines().map(str::trim) {
        if let Some(r) = l.strip_prefix("NOTE outcut:start") {
            open = Some(Outcut { t0: f64::NAN, t1: f64::NAN, reason: r.trim().trim_matches(['(', ')']).to_string(), src: "vtt".into() });
        } else if l.starts_with("NOTE outcut:end") {
            v.extend(open.take().filter(|o| o.t0.is_finite()));
        } else if let (Some(o), Some((a, b))) = (open.as_mut(), l.split_once("-->")) {
            let (a, b) = (ts(a), ts(b.split_whitespace().next().unwrap_or("")));
            if let (Some(a), Some(b)) = (a, b) {
                if o.t0.is_nan() {
                    o.t0 = a;
                }
                o.t1 = b;
            }
        }
    }
    v
}

/// `<stem>.outcut.json`: `[{t0,t1,reason}]`, `{ranges:[…]}` or co-cut `{markers:[{start,end,kind,action,label}]}`
/// (only `action == "outcut"`).
pub fn outcut_json(p: &Path) -> Result<Vec<Outcut>> {
    let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(p)?).with_context(|| p.display().to_string())?;
    let items = v.as_array().or_else(|| v["markers"].as_array()).or_else(|| v["ranges"].as_array()).cloned().unwrap_or_default();
    let mut out = vec![];
    for m in items {
        if m.get("action").is_some_and(|a| a != "outcut") {
            continue;
        }
        let t = |a: &str, b: &str| m[a].as_f64().or_else(|| m[b].as_f64()).or_else(|| m[a].as_str().or(m[b].as_str()).and_then(ts));
        let (Some(t0), Some(t1)) = (t("t0", "start"), t("t1", "end")) else { bail!("{}: t0/t1 fehlt", p.display()) };
        if t1 <= t0 {
            bail!("{}: Ende {t1} nicht nach Anfang {t0}", p.display());
        }
        let reason = ["reason", "label", "kind"].iter().find_map(|k| m[*k].as_str()).unwrap_or("").to_string();
        out.push(Outcut { t0, t1, reason, src: "json".into() });
    }
    Ok(out)
}

/// Sorts and joins overlapping/touching ranges.
pub fn merge(v: &mut Vec<Outcut>) {
    v.sort_by(|a, b| a.t0.total_cmp(&b.t0));
    let mut out: Vec<Outcut> = vec![];
    for o in v.drain(..) {
        match out.last_mut() {
            Some(l) if o.t0 <= l.t1 => {
                l.t1 = l.t1.max(o.t1);
                if !o.reason.is_empty() && !l.reason.contains(&o.reason) {
                    l.reason = if l.reason.is_empty() { o.reason } else { format!("{}; {}", l.reason, o.reason) };
                }
            }
            _ => out.push(o),
        }
    }
    *v = out;
}

/// Stored text minus the cues flagged in `drop` (located in order like `chunk_cued`); the rest stays byte-identical.
pub fn cut(text: &str, cues: &[Cue], drop: &[bool]) -> String {
    let mut cur = text.find("## Transkript").unwrap_or(0);
    let (mut out, mut last) = (String::new(), 0);
    for (c, &d) in cues.iter().zip(drop) {
        if let Some(p) = text[cur..].find(&c.text) {
            let (a, b) = (cur + p, cur + p + c.text.len());
            if d && a >= last {
                out.push_str(&text[last..a]);
                last = if text[b..].starts_with('\n') { b + 1 } else { b };
            }
            cur = b;
        }
    }
    out.push_str(&text[last..]);
    out
}

/// Video/media pages go through yt-dlp (captions + description); everything else is fetched as HTML/PDF/text.
pub fn fetch_url(url: &str, cfg: &Config, prog: Prog) -> Result<Fetched> {
    if let Some(bin) = crate::util::which(&cfg.ytdlp) {
        prog("Prüfe Medienseite (yt-dlp)");
        match ytdlp_info(&bin, url) {
            Ok(info) if info["extractor"].as_str().is_some_and(|e| !e.starts_with("generic")) => return video(&bin, url, &info, prog),
            Ok(_) => {}
            Err(e) => prog(&format!("yt-dlp: {e:#} – lade als Webseite")),
        }
    }
    web(url)
}

fn web(url: &str) -> Result<Fetched> {
    let r = ureq::AgentBuilder::new()
        .user_agent(UA)
        .timeout(std::time::Duration::from_secs(60))
        .build()
        .get(url)
        .set("Accept-Language", "de,en;q=0.8")
        .call()
        .with_context(|| format!("Abruf fehlgeschlagen: {url}"))?;
    let ct = r.content_type().to_lowercase();
    let mut buf = vec![];
    r.into_reader().take(50 << 20).read_to_end(&mut buf)?;
    let name = url.trim_end_matches('/').rsplit('/').next().unwrap_or(url).to_string();
    let (title, kind, text) = if ct.contains("pdf") || url.to_lowercase().ends_with(".pdf") {
        (name, "pdf", pdf(&buf)?)
    } else if ct.contains("html") {
        let s = String::from_utf8_lossy(&buf);
        (html_title(&s).unwrap_or(name), "url", html(&s)?)
    } else {
        (name, "url", String::from_utf8_lossy(&buf).into_owned())
    };
    let text = tidy(&text);
    let warnings = if kind == "url" { consent_wall(&text).into_iter().collect() } else { vec![] };
    Ok(Fetched { title, kind: kind.into(), text, warnings, ..Default::default() })
}

const WALL: &[&str] = &[
    "cookie", "consent", "datenschutz", "privacy", "zustimmen", "alle akzeptieren", "accept all", "nutzungsbedingungen",
    "terms of service", "поверителност", "бисквитк", "условия", "confidentialité", "privacidad",
];

/// Warns when a page looks like a cookie/consent wall or footer instead of content.
pub fn consent_wall(text: &str) -> Option<String> {
    let n = text.chars().count();
    let lo = text.to_lowercase();
    let hits = WALL.iter().filter(|k| lo.contains(*k)).count();
    if n < 500 {
        Some(format!("nur {n} Zeichen Text – vermutlich Cookie-/Consent-Seite oder Skript-Seite, Inhalt prüfen"))
    } else if hits >= 2 && n < 3000 {
        Some(format!("Text sieht nach Cookie-/Datenschutz-Hinweis aus ({hits} Treffer) – Inhalt prüfen"))
    } else {
        None
    }
}

fn run_capped(mut c: std::process::Command, secs: u64) -> Result<std::process::Output> {
    use std::process::Stdio;
    let mut ch = c.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()?;
    let (mut out, mut err) = (ch.stdout.take().unwrap(), ch.stderr.take().unwrap());
    let ro = std::thread::spawn(move || {
        let mut b = vec![];
        let _ = out.read_to_end(&mut b);
        b
    });
    let re = std::thread::spawn(move || {
        let mut b = vec![];
        let _ = err.read_to_end(&mut b);
        b
    });
    let t0 = std::time::Instant::now();
    let status = loop {
        if let Some(s) = ch.try_wait()? {
            break s;
        }
        if t0.elapsed().as_secs() > secs {
            let _ = ch.kill();
            let _ = ch.wait();
            bail!("Zeitüberschreitung nach {secs} s");
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    };
    Ok(std::process::Output { status, stdout: ro.join().unwrap_or_default(), stderr: re.join().unwrap_or_default() })
}

fn ytdlp_info(bin: &Path, url: &str) -> Result<serde_json::Value> {
    let mut c = std::process::Command::new(bin);
    c.args(["--dump-single-json", "--skip-download", "--no-playlist", "--no-warnings", "-N", "1", "--", url]);
    let o = run_capped(c, 60)?;
    if !o.status.success() {
        bail!("{}", String::from_utf8_lossy(&o.stderr).lines().last().unwrap_or("Fehler"));
    }
    Ok(serde_json::from_slice(&o.stdout)?)
}

/// Picks a caption track: manual before automatic; original language > de > en > any.
/// Returns (language key, manual?).
pub fn pick_captions(info: &serde_json::Value) -> Option<(String, bool)> {
    let lang = info["language"].as_str().unwrap_or("").to_string();
    let base = lang.split(['-', '_']).next().unwrap_or("").to_string();
    for (key, manual) in [("subtitles", true), ("automatic_captions", false)] {
        let Some(m) = info[key].as_object() else { continue };
        let ok = |k: &str| m.get(k).and_then(|v| v.as_array()).is_some_and(|a| !a.is_empty()) && k != "live_chat";
        let mut prefs: Vec<String> = vec![];
        if !base.is_empty() {
            // auto: "<lang>-orig" is the untranslated ASR track
            prefs.extend([format!("{base}-orig"), format!("{lang}-orig"), lang.clone(), base.clone()]);
        }
        prefs.extend(["de", "de-orig", "en", "en-orig"].map(String::from));
        if let Some(k) = prefs.iter().find(|k| !k.is_empty() && ok(k)) {
            return Some((k.clone(), manual));
        }
        let mut keys: Vec<&String> = m.keys().filter(|k| ok(k)).collect();
        keys.sort_by_key(|k| (!k.ends_with("-orig"), k.to_string()));
        if let Some(k) = keys.first().filter(|k| manual || k.ends_with("-orig")) {
            return Some((k.to_string(), manual));
        }
    }
    None
}

fn captions(bin: &Path, info: &serde_json::Value, lang: &str, manual: bool) -> Result<Vec<Cue>> {
    let dir = std::env::temp_dir().join(format!("funkhaus-subs-{}-{}", std::process::id(), crate::util::stamp()));
    std::fs::create_dir_all(&dir)?;
    let res = (|| {
        let ij = dir.join("info.json");
        std::fs::write(&ij, serde_json::to_vec(info)?)?;
        let mut c = std::process::Command::new(bin);
        c.arg("--load-info-json").arg(&ij);
        c.args(["--skip-download", "--no-warnings", "-N", "1", "--sub-format", "vtt", "--sub-langs", lang]);
        c.arg(if manual { "--write-subs" } else { "--write-auto-subs" });
        c.arg("-o").arg(dir.join("s.%(ext)s"));
        let o = run_capped(c, 90)?;
        if !o.status.success() {
            bail!("{}", String::from_utf8_lossy(&o.stderr).lines().last().unwrap_or("Fehler"));
        }
        let f = std::fs::read_dir(&dir)?
            .flatten()
            .map(|e| e.path())
            .find(|p| p.extension().is_some_and(|x| x == "vtt"))
            .context("keine Untertiteldatei erhalten")?;
        Ok(cues(&std::fs::read_to_string(f)?))
    })();
    let _ = std::fs::remove_dir_all(&dir);
    res
}

use crate::util::mmss as hms;

fn video(bin: &Path, url: &str, info: &serde_json::Value, prog: Prog) -> Result<Fetched> {
    let g = |k: &str| info[k].as_str().unwrap_or("").trim().to_string();
    let title = Some(g("title")).filter(|t| !t.is_empty()).unwrap_or_else(|| url.to_string());
    let extractor = g("extractor_key");
    let mut warnings = vec![];
    let (transcript, cap) = match pick_captions(info) {
        Some((lang, manual)) => {
            prog(&format!("Lade Untertitel ({lang}, {})", if manual { "manuell" } else { "automatisch" }));
            match captions(bin, info, &lang, manual) {
                Ok(c) if !c.is_empty() => (Some(c), Some((lang, manual))),
                Ok(_) => (None, None),
                Err(e) => {
                    prog(&format!("Untertitel fehlgeschlagen: {e:#}"));
                    (None, None)
                }
            }
        }
        None => (None, None),
    };
    if transcript.is_none() {
        warnings.push("kein Transkript – nur Beschreibung".to_string());
    }
    let date = g("upload_date");
    let date = if date.len() == 8 { format!("{}-{}-{}", &date[..4], &date[4..6], &date[6..]) } else { date };
    let mut t = format!("# {title}

");
    let chan = Some(g("channel")).filter(|c| !c.is_empty()).unwrap_or_else(|| g("uploader"));
    for (k, v) in [("Kanal", chan), ("Datum", date), ("URL", Some(g("webpage_url")).filter(|u| !u.is_empty()).unwrap_or_else(|| url.into()))] {
        if !v.is_empty() {
            t.push_str(&format!("{k}: {v}
"));
        }
    }
    match &cap {
        Some((l, m)) => t.push_str(&format!("Untertitel: {l} ({})
", if *m { "manuell" } else { "automatisch" })),
        None => t.push_str("Untertitel: keine
"),
    }
    let desc = g("description");
    if !desc.is_empty() {
        t.push_str(&format!("
## Beschreibung

{desc}
"));
    }
    if let Some(ch) = info["chapters"].as_array().filter(|c| !c.is_empty()) {
        t.push_str("
## Kapitel

");
        for c in ch {
            t.push_str(&format!("{} {}
", hms(c["start_time"].as_f64().unwrap_or(0.0)), c["title"].as_str().unwrap_or("")));
        }
    }
    if let Some(c) = &transcript {
        t.push_str(&format!("\n## Transkript\n\n{}\n", cue_text(c)));
    }
    Ok(Fetched { title, kind: "video".into(), text: tidy(&t), extractor: Some(extractor), has_transcript: Some(transcript.is_some()), warnings, cues: transcript, outcut: vec![] })
}

fn md_title(t: &str) -> Option<String> {
    t.lines().take(20).find_map(|l| l.strip_prefix("# ")).map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

fn pdf(buf: &[u8]) -> Result<String> {
    // pdf-extract panics on some malformed files
    let pages = std::panic::catch_unwind(|| pdf_extract::extract_text_from_mem_by_pages(buf))
        .map_err(|_| anyhow::anyhow!("PDF nicht lesbar"))?
        .context("PDF nicht lesbar")?;
    let mut out = String::new();
    for (i, p) in pages.iter().enumerate() {
        if !p.trim().is_empty() {
            out.push_str(&format!("[Seite {}]\n{}\n\n", i + 1, dehyphen(p)));
        }
    }
    if out.trim().is_empty() {
        bail!("PDF enthält keinen Text (Scan?)");
    }
    Ok(out)
}

fn dehyphen(s: &str) -> String {
    s.replace("-\n", "")
}

/// One caption line with its time span (seconds) and speaker if marked (`<v Name>` or `Name:` prefix).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
pub struct Cue {
    pub t0: f64,
    pub t1: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speaker: Option<String>,
    pub text: String,
}

/// "00:01:02.500" / "01:02,500" / "1:02.5" → seconds
pub fn ts(s: &str) -> Option<f64> {
    let s = s.trim().replace(',', ".");
    let mut t = 0.0;
    for p in s.split(':') {
        t = t * 60.0 + p.parse::<f64>().ok()?;
    }
    Some(t)
}

fn speaker_of(raw: &str, text: &str) -> Option<(String, bool)> {
    if let Some(r) = raw.strip_prefix("<v") {
        let tag = r.split('>').next()?.trim();
        // <v.class Name> → Name
        let name = if tag.starts_with('.') { tag.split_once(' ').map_or("", |x| x.1) } else { tag }.trim();
        return (!name.is_empty()).then(|| (name.to_string(), true));
    }
    let (n, rest) = text.split_once(": ")?;
    let ok = !rest.trim().is_empty()
        && n.len() <= 30
        && n.split_whitespace().count() <= 3
        && n.chars().next().is_some_and(char::is_uppercase)
        && n.chars().all(|c| c.is_alphabetic() || c == ' ' || c == '-' || c == '.');
    ok.then(|| (n.to_string(), false))
}

/// SRT/VTT → cues; drops indices, cue settings and inline tags. YouTube auto-captions repeat the
/// previous line at the top of each new block ("roll"); only in that pattern are exact repeats dropped,
/// so ordinary captions keep every spoken line.
pub fn cues(s: &str) -> Vec<Cue> {
    // 1. blocks of (t0, t1, lines)
    let lines: Vec<&str> = s.lines().map(str::trim).collect();
    // (t0, t1, [(raw line, clean text)])
    type Block = (f64, f64, Vec<(String, String)>);
    let mut blocks: Vec<Block> = vec![];
    for (i, l) in lines.iter().enumerate() {
        if let Some((a, b)) = l.split_once("-->") {
            let t0 = ts(a).unwrap_or(0.0);
            blocks.push((t0, ts(b.split_whitespace().next().unwrap_or("")).unwrap_or(t0), vec![]));
            continue;
        }
        let srt_index = l.chars().all(|c| c.is_ascii_digit()) && lines.get(i + 1).is_some_and(|n| n.contains("-->"));
        if l.is_empty() || *l == "WEBVTT" || srt_index || ["NOTE", "STYLE", "Kind:", "Language:"].iter().any(|p| l.starts_with(p)) {
            continue;
        }
        let t = unescape(&strip_tags(l).replace("&nbsp;", " "));
        let t = t.split_whitespace().collect::<Vec<_>>().join(" ");
        if let (false, Some(b)) = (t.is_empty(), blocks.last_mut()) {
            b.2.push((l.to_string(), t));
        }
    }
    // 2. cues
    let mut out: Vec<Cue> = vec![];
    let mut guess: std::collections::HashMap<String, usize> = Default::default();
    let mut prev_t1: Option<f64> = None;
    // YouTube ASR marks speaker turns with ">>" (no names): stripped from the text, kept as a flag
    let mut turn: Vec<bool> = vec![];
    for (t0, t1, ls) in blocks {
        let ls: Vec<(String, String, bool)> = ls
            .into_iter()
            .map(|(raw, t)| match t.strip_prefix(">>") {
                Some(r) => (raw, r.trim().to_string(), true),
                None => (raw, t, false),
            })
            .filter(|x| !x.1.is_empty())
            .collect();
        let last = out.last().map(|c| c.text.clone());
        let roll = last.is_some()
            && (prev_t1.is_some_and(|p| (t0 - p).abs() <= 0.05) || ls.len() == 2 && last.as_deref() == Some(ls[0].1.as_str()));
        prev_t1 = Some(t1);
        let n = ls.len();
        for (k, (raw, t, tn)) in ls.into_iter().enumerate() {
            // in a rolled 2-line block only the top line repeats; the new bottom line is speech even if
            // it equals the previous cue (">> Ja." answering "Ja.")
            if roll && (n == 1 || k == 0) && out.last().is_some_and(|c| c.text == t) {
                continue;
            }
            let sp = speaker_of(&raw, &t);
            if let Some((n, false)) = &sp {
                *guess.entry(n.clone()).or_insert(0) += 1;
            }
            out.push(Cue { t0, t1, speaker: sp.map(|x| x.0), text: t });
            turn.push(tn);
        }
    }
    if turn.iter().any(|&x| x) {
        let mut who = 1;
        for (c, &tn) in out.iter_mut().zip(&turn) {
            if tn {
                who = 3 - who;
            }
            c.speaker.get_or_insert_with(|| format!("Sprecher {who}"));
        }
    }
    // "Name:" prefixes count only if the name recurs (otherwise "Also: …" would be a speaker)
    for c in &mut out {
        if c.speaker.as_ref().is_some_and(|n| guess.get(n).is_some_and(|&k| k < 2)) {
            c.speaker = None;
        }
    }
    out
}

pub fn cue_text(c: &[Cue]) -> String {
    c.iter().map(|c| c.text.as_str()).collect::<Vec<_>>().join("\n")
}

/// SRT/VTT → plain text (one cue line per line).
pub fn subs(s: &str) -> String {
    cue_text(&cues(s))
}

fn strip_tags(s: &str) -> String {
    let mut out = String::new();
    let mut tag = false;
    for c in s.chars() {
        match c {
            '<' => tag = true,
            '>' if tag => tag = false,
            c if !tag => out.push(c),
            _ => {}
        }
    }
    out
}

fn html_title(s: &str) -> Option<String> {
    let lo = s.to_ascii_lowercase();
    let a = lo.find("<title")?;
    let a = a + lo[a..].find('>')? + 1;
    let b = a + lo[a..].find("</title")?;
    let t = html2text::config::plain().string_from_read(&s.as_bytes()[a..b], 10_000).ok()?;
    let t = t.split_whitespace().collect::<Vec<_>>().join(" ");
    (!t.is_empty()).then_some(t)
}

/// Removes boilerplate elements (nav, footer, …) before conversion.
fn drop_elems(s: &str) -> String {
    let mut s = s.to_string();
    for tag in ["script", "style", "noscript", "nav", "footer", "header", "aside", "form", "svg"] {
        loop {
            let lo = s.to_ascii_lowercase();
            let Some(a) = lo.find(&format!("<{tag}")).filter(|&a| {
                lo[a + tag.len() + 1..].starts_with(|c: char| c == '>' || c.is_whitespace())
            }) else {
                break;
            };
            let close = format!("</{tag}>");
            match lo[a..].find(&close) {
                Some(b) => s.replace_range(a..a + b + close.len(), " "),
                None => break,
            }
        }
    }
    s
}

pub fn html(s: &str) -> Result<String> {
    let s = drop_elems(s);
    Ok(html2text::config::with_decorator(html2text::render::TrivialDecorator::new()).link_footnotes(false).string_from_read(s.as_bytes(), 10_000)?)
}

fn docx(p: &Path) -> Result<String> {
    let mut z = zip::ZipArchive::new(std::fs::File::open(p)?)?;
    let mut xml = String::new();
    z.by_name("word/document.xml")?.read_to_string(&mut xml)?;
    let xml = xml.replace("</w:p>", "\n").replace("<w:tab/>", "\t").replace("<w:br/>", "\n");
    Ok(unescape(&strip_tags(&xml)))
}

fn unescape(s: &str) -> String {
    s.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&apos;", "'").replace("&amp;", "&")
}

fn asr(p: &Path, cfg: &Config) -> Result<String> {
    let cmd = cfg.asr_cmd.as_deref().context("Audio/Video: kein asr_cmd in config.toml gesetzt")?;
    let q = format!("'{}'", p.to_string_lossy().replace('\'', "'\\''"));
    let o = std::process::Command::new("sh").arg("-c").arg(cmd.replace("{in}", &q)).output()?;
    if !o.status.success() {
        bail!("ASR fehlgeschlagen: {}", String::from_utf8_lossy(&o.stderr));
    }
    Ok(String::from_utf8_lossy(&o.stdout).into_owned())
}

/// Trims trailing spaces and collapses runs of blank lines.
pub fn tidy(s: &str) -> String {
    let mut out = String::new();
    let mut blank = 0;
    for l in s.lines() {
        let l = l.trim_end();
        if l.trim().is_empty() {
            blank += 1;
            if blank > 1 {
                continue;
            }
        } else {
            blank = 0;
        }
        out.push_str(l);
        out.push('\n');
    }
    out.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn srt_vtt() {
        let s = "1\n00:00:01,000 --> 00:00:02,000\nHallo <i>Welt</i>\n\n2\n00:00:02,000 --> 00:00:03,000\nHallo Welt\nZweite\n";
        assert_eq!(subs(s), "Hallo Welt\nZweite");
        let v = "WEBVTT\nKind: captions\n\n00:01.000 --> 00:02.000\n<v Max>Guten Tag\n";
        assert_eq!(subs(v), "Guten Tag");
    }

    #[test]
    fn youtube_rolling_autosubs() {
        let v = "WEBVTT\nKind: captions\nLanguage: en\n\n00:00:00.160 --> 00:00:02.990 align:start position:0%\n \nso<00:00:00.480><c> after</c><00:00:00.800><c> six</c><00:00:01.120><c> thousand</c>\n\n00:00:02.990 --> 00:00:03.000 align:start position:0%\nso after six thousand\n \n\n00:00:03.000 --> 00:00:05.110 align:start position:0%\nso after six thousand\nkilometers<00:00:03.520><c> the</c><00:00:03.840><c> boat</c>\n\n00:00:05.110 --> 00:00:05.120 align:start position:0%\nkilometers the boat\n \n\n00:00:05.120 --> 00:00:07.000 align:start position:0%\nkilometers the boat\ncapsized &amp; sank\n";
        assert_eq!(subs(v), "so after six thousand\nkilometers the boat\ncapsized & sank");
    }

    #[test]
    fn cue_times_and_speakers() {
        let v = "WEBVTT\n\n00:00:01.000 --> 00:00:03.500\n<v Anke Rahlfs>Wir sind 84 Mitglieder.\n\n01:02:03.000 --> 01:02:05.000 align:start\n<v.loud Jonas>Und heute?\n";
        let c = cues(v);
        assert_eq!(c[0], Cue { t0: 1.0, t1: 3.5, speaker: Some("Anke Rahlfs".into()), text: "Wir sind 84 Mitglieder.".into() });
        assert_eq!((c[1].t0, c[1].speaker.as_deref(), c[1].text.as_str()), (3723.0, Some("Jonas"), "Und heute?"));
        let srt = "1\n00:00:00,500 --> 00:00:02,000\nJonas Wiebe: Frau Rahlfs, wir stehen am Hafen.\n\n2\n00:00:02,000 --> 00:00:04,250\nAlso: das ist kein Name.\n\n3\n00:00:04,250 --> 00:00:05,000\nJonas Wiebe: Und heute?\n";
        let c = cues(srt);
        assert_eq!((c[0].t0, c[0].speaker.as_deref()), (0.5, Some("Jonas Wiebe")));
        assert_eq!((c[1].t1, c[1].speaker.as_deref()), (4.25, None));
    }

    #[test]
    fn plain_repeats_and_numbers_kept() {
        // ordinary captions: a shorter line contained in the previous one is real speech
        let v = "WEBVTT\n\n00:00:00.000 --> 00:00:02.000\nOkay. Yeah.\n\n00:00:02.000 --> 00:00:03.000\nYeah.\n\n00:00:04.000 --> 00:00:05.000\n2027\n\n00:00:05.000 --> 00:00:06.000\nYeah.\n";
        let c = cues(v);
        assert_eq!(c.iter().map(|c| c.text.as_str()).collect::<Vec<_>>(), ["Okay. Yeah.", "Yeah.", "2027", "Yeah."]);
        assert_eq!(c[0].t1, 2.0);
        // SRT index lines are dropped, number lines are not
        let srt = "1\n00:00:00,000 --> 00:00:01,000\n1983\n\n2\n00:00:01,000 --> 00:00:02,000\nfertig\n";
        assert_eq!(subs(srt), "1983\nfertig");
    }

    /// fixtures/asr-yt: rolling dupes, flush cues, ">>" turns, "Ja." answered by ">> Ja.", "2027" line.
    #[test]
    fn asr_yt_regression() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/asr-yt");
        let c = cues(&std::fs::read_to_string(root.join("talk.vtt")).unwrap());
        let e: Vec<serde_json::Value> = serde_json::from_str(&std::fs::read_to_string(root.join("expected.cues.json")).unwrap()).unwrap();
        assert_eq!(c.len(), e.len(), "{:#?}", c.iter().map(|c| &c.text).collect::<Vec<_>>());
        let ok = c
            .iter()
            .zip(&e)
            .filter(|(c, e)| {
                let sp = if e["speaker"] == "A" { "Sprecher 1" } else { "Sprecher 2" };
                c.text == e["text"].as_str().unwrap()
                    && (c.t0 - e["t0"].as_f64().unwrap()).abs() <= 0.2
                    && (c.t1 - e["t1"].as_f64().unwrap()).abs() <= 0.2
                    && c.speaker.as_deref() == Some(sp)
            })
            .count();
        assert_eq!(ok, 30, "{c:#?}");
    }

    #[test]
    fn outcut_parse() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/seminar/seminar.vtt");
        let raw = std::fs::read_to_string(&root).unwrap();
        let o = outcut_vtt(&raw);
        assert_eq!(o.len(), 1);
        assert_eq!((crate::util::mmss(o[0].t0), crate::util::mmss(o[0].t1.floor())), ("46:18".into(), "48:35".into()));
        let c = cues(&raw);
        let drop: Vec<bool> = c.iter().map(|x| o[0].hits(x.t0, x.t1)).collect();
        assert_eq!(drop.iter().filter(|&&d| d).count(), 25); // cues 526–550
        let t = cut(&tidy(&cue_text(&c)), &c, &drop);
        assert!(!t.contains("Tierarzt") && t.contains("Guten Morgen, zusammen.") && t.lines().count() == c.len() - 25);
        // json: plain + co-cut alias, review skipped, bad range rejected
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("x.outcut.json");
        std::fs::write(&p, r#"{"markers":[{"start":10,"end":20,"kind":"smalltalk","action":"outcut"},{"start":30,"end":40,"action":"review"},{"start":"0:15","end":"0:25","action":"outcut","label":"Pause"}]}"#).unwrap();
        let mut v = outcut_json(&p).unwrap();
        assert_eq!(v.len(), 2);
        merge(&mut v);
        assert_eq!((v.len(), v[0].t0, v[0].t1, v[0].reason.as_str()), (1, 10.0, 25.0, "smalltalk; Pause"));
        std::fs::write(&p, r#"[{"t0":5,"t1":5}]"#).unwrap();
        assert!(outcut_json(&p).is_err());
    }

    #[test]
    fn rolling_cue_times() {
        let v = "WEBVTT\n\n00:00:00.160 --> 00:00:02.990\nso<00:00:00.480><c> after</c>\n\n00:00:02.990 --> 00:00:03.000\nso after\n\n00:00:03.000 --> 00:00:05.110\nso after\nkilometers<00:00:03.520><c> the</c>\n";
        let c = cues(v);
        assert_eq!(c.len(), 2);
        // the repeated line keeps its own block's end, not the roll's
        assert_eq!((c[0].t0, c[0].t1), (0.16, 2.99));
        assert_eq!((c[1].t0, c[1].text.as_str()), (3.0, "kilometers the"));
    }

    #[test]
    fn caption_order() {
        let j = |s: &str| serde_json::from_str::<serde_json::Value>(s).unwrap();
        let f = r#"[{"ext":"vtt","url":"x"}]"#;
        // manual German beats automatic original
        let i = j(&format!(r#"{{"language":"en-US","subtitles":{{"de":{f},"live_chat":{f}}},"automatic_captions":{{"en-orig":{f},"en":{f}}}}}"#));
        assert_eq!(pick_captions(&i), Some(("de".into(), true)));
        // automatic: original "-orig" track beats translations
        let i = j(&format!(r#"{{"language":"en-US","subtitles":{{}},"automatic_captions":{{"bg":{f},"de":{f},"en":{f},"en-orig":{f}}}}}"#));
        assert_eq!(pick_captions(&i), Some(("en-orig".into(), false)));
        // unknown language: de, then en
        let i = j(&format!(r#"{{"automatic_captions":{{"fr":{f},"en":{f},"de":{f}}}}}"#));
        assert_eq!(pick_captions(&i), Some(("de".into(), false)));
        // manual original language first
        let i = j(&format!(r#"{{"language":"bg","subtitles":{{"en":{f},"bg":{f}}}}}"#));
        assert_eq!(pick_captions(&i), Some(("bg".into(), true)));
        // only translated auto tracks in other languages → none
        assert_eq!(pick_captions(&j(&format!(r#"{{"automatic_captions":{{"fr":{f}}}}}"#))), None);
        assert_eq!(pick_captions(&j("{}")), None);
    }

    #[test]
    fn consent_walls() {
        assert!(consent_wall("Поверителност Условия Политика за бисквитките © 2026 Google LLC").is_some());
        let long = "Inhalt ".repeat(200);
        assert!(consent_wall(&long).is_none());
        let wall = format!("{} Wir verwenden Cookies. Datenschutz. Alle akzeptieren.", "x ".repeat(300));
        assert!(consent_wall(&wall).is_some());
        assert!(consent_wall(&format!("{long} Datenschutz Cookie {}", "y ".repeat(2000))).is_none());
    }

    #[test]
    fn html_text() {
        let h = "<html><head><title>Seite &amp; Co</title><style>p{}</style></head><body><nav>Menü</nav><h1>Kopf</h1><p>Ein <a href='x'>Link</a> hier.</p><script>var x=1;</script></body></html>";
        assert_eq!(html_title(h).unwrap(), "Seite & Co");
        let t = html(h).unwrap();
        assert!(t.contains("Kopf") && t.contains("Ein Link hier."), "{t}");
        assert!(!t.contains("Menü") && !t.contains("var x"), "{t}");
    }

    #[test]
    fn html_non_ascii_offsets() {
        let h = "<html><head><title>İİİ ẞẞ Titel</title></head><body><nav>İİİİ Menü</nav><p>Text ẞ</p><script>x</script></body></html>";
        assert_eq!(html_title(h).unwrap(), "İİİ ẞẞ Titel");
        let t = html(h).unwrap();
        assert!(t.contains("Text ẞ") && !t.contains("Menü"), "{t}");
    }

    #[test]
    fn icloud_placeholders() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("a.md"), "x").unwrap();
        std::fs::write(d.path().join(".b.pdf.icloud"), "").unwrap();
        let (f, n) = walk_report(d.path());
        assert_eq!((f.len(), n), (1, 1));
    }

    #[test]
    fn md_and_tidy() {
        assert_eq!(md_title("intro\n# Titel\n").unwrap(), "Titel");
        assert_eq!(tidy("a  \n\n\n\nb\n"), "a\n\nb");
        assert_eq!(tidy("Chronik\n2027\nEnde"), "Chronik\n2027\nEnde"); // number-only lines in .txt stay
    }
}
