use funkhaus_audio::{chapters, mix};
use funkhaus_voice::tts;
pub mod draft;
pub mod numbers;
pub mod reroll;
pub mod script;
pub mod staged;

use crate::{Notebook, Prog, Store, util};
use anyhow::{Context, Result};
pub use draft::Len;
use serde::Serialize;
use std::path::PathBuf;

const ARTIST: &str = "Funkhaus";
const FOOTER: &str = "Erstellt mit Funkhaus, lokale Pipeline. Stimmen synthetisch (OmniVoice).";

#[derive(Clone, Debug)]
pub struct Opts {
    pub focus: Option<String>,
    pub len: Len,
    /// (O, F) persona names; None = config `[hosts]`.
    pub voices: Option<(String, String)>,
    pub lang: String,
    pub script_only: bool,
    pub from_script: Option<PathBuf>,
    /// None = config
    pub loudness: Option<mix::Loudness>,
    pub no_disclosure: bool,
    /// ignore cached TTS clips (re-render all)
    pub fresh: bool,
    /// None = config `[music] jingle`; Some(true) errors if the files are missing
    pub jingle: Option<bool>,
    /// None = config `[music] bed`
    pub bed: Option<bool>,
}

impl Default for Opts {
    fn default() -> Self {
        Self { focus: None, len: Len::Mittel, voices: None, lang: "de".into(), script_only: false, from_script: None, loudness: None, no_disclosure: false, fresh: false, jingle: None, bed: None }
    }
}

#[derive(Serialize, Debug, Clone)]
pub struct Pod {
    pub title: String,
    pub dir: PathBuf,
    pub script: PathBuf,
    pub lines: usize,
    pub mp3: Option<PathBuf>,
    pub secs: Option<f64>,
    pub warnings: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub chapters: Vec<chapters::Chapter>,
    /// seconds per stage (outline, segments, tts, mix, mp3)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timing: Option<serde_json::Value>,
}

/// The script's parent dir if it is a `podcast-*` dir directly under this notebook's out dir.
fn reuse_dir(st: &Store, nb: &Notebook, script: &std::path::Path) -> Option<PathBuf> {
    let d = std::fs::canonicalize(script.parent()?).ok()?;
    let out = std::fs::canonicalize(st.out_dir(nb)).ok()?;
    (d.parent() == Some(out.as_path()) && d.file_name()?.to_string_lossy().starts_with("podcast-")).then_some(d)
}

pub fn run(st: &Store, nb: &Notebook, o: &Opts, prog: Prog) -> Result<Pod> {
    // fail before the (slow) LLM call if rendering can't work anyway
    if !o.script_only {
        util::which(&st.cfg.tts.cmd).with_context(|| format!("omnivoice nicht gefunden ({})", st.cfg.tts.cmd))?;
        util::which(&st.cfg.ffmpeg).with_context(|| format!("ffmpeg nicht gefunden ({})", st.cfg.ffmpeg))?;
    }
    // editing a script inside an existing podcast dir re-renders only changed lines there
    let reuse = o.from_script.as_ref().and_then(|p| reuse_dir(st, nb, p));
    let reuse_dir = reuse.is_some();
    let dir = reuse.unwrap_or_else(|| st.out_dir(nb).join(format!("podcast-{}", util::stamp())));
    let mut timing = serde_json::Map::new();
    let mut staged_out = None;
    let (title, lines, txt) = match &o.from_script {
        Some(p) => {
            let title = std::fs::read_to_string(dir.join("title.txt")).unwrap_or_else(|_| nb.name.clone());
            let txt = std::fs::read_to_string(p)?;
            (title.trim().to_string(), script::parse(&txt)?, txt)
        }
        None => match staged::guides(st, nb).filter(|_| o.lang.starts_with("de")) {
            // long notebook with source guides: outline → per-segment scripts
            Some(gs) => {
                let x = staged::generate(st, nb, &gs, o.len, o.focus.as_deref(), prog)?;
                timing.insert("outline".into(), serde_json::json!(x.outline.secs));
                timing.insert("segments".into(), serde_json::json!(x.scripts.iter().map(|s| s.secs).collect::<Vec<_>>()));
                let (txt, lines) = staged::join(&x.outline, &x.scripts);
                let t = x.title.clone();
                staged_out = Some((x, gs));
                (t, lines, txt)
            }
            None => {
                if st.chunks(nb)?.iter().map(|c| c.text.len()).sum::<usize>() > st.cfg.podcast_ctx_chars {
                    prog("Hinweis: lange Quellen – mit `funkhaus map` + `funkhaus guide` entsteht ein 3-Stufen-Podcast mit Kapiteln");
                }
                let t0 = std::time::Instant::now();
                let (t, lines) = draft::generate(st, nb, o.len, o.focus.as_deref(), &o.lang, prog)?;
                timing.insert("script".into(), serde_json::json!(t0.elapsed().as_secs_f64().round()));
                let txt = script::format(&lines);
                (t, lines, txt)
            }
        },
    };
    // created only once a script exists, so failed LLM runs leave no empty "kein Audio" entries
    std::fs::create_dir_all(&dir)?;
    let sp = dir.join("script.txt");
    std::fs::write(&sp, &txt)?;
    std::fs::write(dir.join("title.txt"), &title)?;
    if let Some((x, gs)) = &staged_out {
        std::fs::write(dir.join("outline.json"), serde_json::to_string_pretty(&x.outline)?)?;
        std::fs::write(dir.join("segments.json"), serde_json::to_string_pretty(&x.scripts)?)?;
        std::fs::write(dir.join("overview.txt"), gs.iter().map(|g| g.overview.as_str()).collect::<Vec<_>>().join(" "))?;
    }
    let mut pod = Pod { title, dir: dir.clone(), script: sp, lines: lines.len(), mp3: None, secs: None, warnings: vec![], chapters: vec![], timing: None };
    let src: String = st.chunks(nb)?.iter().map(|c| c.text.as_str()).collect::<Vec<_>>().join("\n");
    let seg: Vec<String> = chapters::seg_issues(&txt).into_iter().map(|(n, s, r)| format!("Zeile {n}: {}", chapters::seg_warning(&s, r))).collect();
    pod.warnings = [script::problems(&lines), numbers::unsupported(&lines, &src), seg].concat();
    if let Some(w) = Store::thin(nb) {
        prog(&format!("WARNUNG: {w}"));
        pod.warnings.insert(0, w);
    }
    std::fs::write(dir.join("warnings.txt"), pod.warnings.join("\n"))?;
    prog(&format!("Skript: {} Zeilen → {}", lines.len(), pod.script.display()));
    if o.script_only {
        pod.timing = Some(timing.into());
        std::fs::write(dir.join("timing.json"), serde_json::to_string_pretty(&pod.timing)?)?;
        return Ok(pod);
    }
    let lap = |t: &mut serde_json::Map<String, serde_json::Value>, k: &str, c: &mut std::time::Instant| {
        t.insert(k.into(), serde_json::json!((c.elapsed().as_secs_f64() * 10.0).round() / 10.0));
        *c = std::time::Instant::now();
    };
    let mut c = std::time::Instant::now();
    let t = &st.cfg.tts;
    // a reused dir keeps the voices it was made with, so config changes don't silently miss the cache
    let saved = reuse_dir.then(|| reroll::Render::load(&dir)).flatten();
    // explicit options win; otherwise a reused dir keeps what it was rendered with (loudness/jingle/bed/disclosure)
    let loudness = o.loudness.or(saved.as_ref().and_then(|v| v.loudness));
    let (jingle, bed) = (o.jingle.or(saved.as_ref().and_then(|v| v.jingle)), o.bed.or(saved.as_ref().and_then(|v| v.bed)));
    let no_disclosure = o.no_disclosure || saved.as_ref().is_some_and(|v| v.no_disclosure);
    let names = o.voices.clone().or_else(|| saved.as_ref().map(|v| (v.o.clone(), v.f.clone())));
    let (vo, vf) = crate::voices::hosts(st, names.as_ref().map(|(a, b)| (a.as_str(), b.as_str())))?;
    let vs = tts::hosts(vo, vf);
    let (no, nf) = names.unwrap_or_else(|| (st.cfg.hosts.o.persona.clone(), st.cfg.hosts.f.persona.clone()));
    if saved.as_ref().is_some_and(|v| (&v.o, &v.f) != (&no, &nf)) {
        prog("Hinweis: andere Stimmen als im Ordner gespeichert (render.json) – Zeilen werden neu gerendert");
    }
    reroll::Render { o: no, f: nf, lang: o.lang.clone(), loudness: Some(loudness.unwrap_or(st.cfg.loudness)), jingle, bed, no_disclosure, fresh: false }.save(&dir)?;
    let lex = funkhaus_voice::lexicon::Lexicon::load(&st.dir(&nb.id).join("lexicon.tsv"));
    // spoken AI disclosure as last line; not part of script.txt, skipped if the script already ends with it
    let mut lines = lines;
    let dis = st.cfg.disclosure_text.trim();
    if st.cfg.disclosure && !no_disclosure && !dis.is_empty() && lines.last().is_none_or(|l| l.text.trim() != dis) {
        lines.push(funkhaus_audio::script::Line::new("O", 0.4, dis));
    }
    // takes of lines that no longer exist are dropped before they can keep orphan clips alive
    let mut rf = reroll::File::load(&dir)?;
    let mut valid = std::collections::HashSet::new();
    for i in 0..lines.len() {
        valid.extend(tts::line_keys(t, &lines, &vs, &lex, &o.lang, i)?);
    }
    if rf.prune(&valid) {
        rf.save(&dir)?;
    }
    let rr = rf.rerolls();
    let wavs = tts::render(&tts::RenderReq { t, lines: &lines, voices: &vs, lex: &lex, lang: &o.lang, dir: &dir, fresh: o.fresh, rr: &rr, prog, cancel: None, on_line: None })?;
    lap(&mut timing, "tts", &mut c);
    prog("Mische");
    // one clip per line: trimmed speech parts joined with exact silence for [pause]
    let (clips, sr) = tts::clips(&wavs)?;
    let out = mix::layout(&lines, &clips.iter().map(Vec::as_slice).collect::<Vec<_>>(), sr);
    let (out, lead) = match crate::music::parts(st, jingle, bed, sr, prog)? {
        Some((i, u, b)) => funkhaus_audio::music::place(&out, i.as_deref(), u.as_deref(), b.as_deref(), &st.cfg.music, sr)?,
        None => (out, 0),
    };
    let wav = dir.join("mix.wav");
    mix::write(&wav, &out, sr)?;
    let mp3 = dir.join("podcast.mp3");
    let ld = loudness.unwrap_or(st.cfg.loudness);
    let (li, ..) = ld.target();
    prog(&format!("MP3 (loudnorm {li} LUFS)"));
    lap(&mut timing, "mix", &mut c);
    mix::mp3(&st.cfg.ffmpeg, &wav, &mp3, &pod.title, ARTIST, ld, Some(dis).filter(|d| !d.is_empty()))?;
    lap(&mut timing, "mp3", &mut c);
    let mk = chapters::marks(&txt);
    if !mk.is_empty() {
        let lens: Vec<usize> = clips.iter().map(Vec::len).collect();
        let mut chs = chapters::spans(&mk, &lines, &lens, sr, lead);
        let chunks = st.chunks(nb)?;
        let outline: Option<staged::Outline> = std::fs::read_to_string(dir.join("outline.json")).ok().and_then(|s| serde_json::from_str(&s).ok());
        // match by marker text (as `staged::join` wrote it) so a dropped empty SEG does not shift the refs: the segment at
        // the chapter's position wins when it matches (duplicate titles), else the first text match, else the position
        let segs: Vec<&staged::Seg> = outline.iter().flat_map(|o| &o.segments).collect();
        let norm = |s: &str| s.replace(['|', '\n'], " ").trim().to_string();
        for (k, ch) in chs.iter_mut().enumerate() {
            let same = |s: &&&staged::Seg| norm(&s.title) == ch.title && norm(&s.goal) == ch.goal;
            let Some(sg) = segs.get(k).filter(|s| same(&s)).or_else(|| segs.iter().find(same)).or(segs.get(k)) else { break };
            // cue-precise time: the cue of the chunk that best matches the segment's goal + beats
            let claim = format!("{} {}", sg.goal, sg.beats.join(" "));
            ch.refs = sg
                .chunk_ids
                .iter()
                .map(|id| {
                    let c = chunks.iter().find(|c| &c.id == id);
                    let cue = c.map(|c| crate::cite::chunk_cues(st, nb, c)).and_then(|cs| crate::cite::best_cue(&cs, &claim).map(|x| x.t0));
                    match cue.or(c.and_then(|c| c.t0)) {
                        Some(t) => format!("{id} {}", util::mmss(t)),
                        None => id.clone(),
                    }
                })
                .collect();
        }
        let meta = dir.join("chapters.ffmeta");
        std::fs::write(&meta, chapters::ffmeta(&pod.title, ARTIST, &chs))?;
        std::fs::write(dir.join("chapters.json"), serde_json::to_string_pretty(&chs)?)?;
        let ov = std::fs::read_to_string(dir.join("overview.txt")).unwrap_or_default();
        let srcs: Vec<(String, String, String)> = nb.sources.iter().map(|s| (s.id.clone(), s.title.clone(), s.origin.clone())).collect();
        std::fs::write(dir.join("shownotes.md"), chapters::shownotes(&pod.title, &ov, &chs, &srcs, FOOTER))?;
        std::fs::write(dir.join("chapters.txt"), chapters::cue_sheet(&chs))?;
        prog(&format!("Kapitel: {}", chs.len()));
        if let Err(e) = chapters::embed(&st.cfg.ffmpeg, &mp3, &meta, &chs) {
            prog(&format!("WARNUNG: {e:#}"));
            pod.warnings.push(format!("{e:#}"));
        }
        pod.chapters = chs;
    }
    lap(&mut timing, "chapters", &mut c);
    pod.timing = Some(timing.into());
    std::fs::write(dir.join("timing.json"), serde_json::to_string_pretty(&pod.timing)?)?;
    pod.secs = Some((out.len() as f64 / sr as f64 * 10.0).round() / 10.0);
    pod.mp3 = Some(mp3);
    Ok(pod)
}

/// Podcast output dirs of a notebook, newest first.
pub fn list(st: &Store, nb: &Notebook) -> Vec<Pod> {
    let mut v: Vec<Pod> = std::fs::read_dir(st.out_dir(nb))
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.join("script.txt").is_file() && p.file_name().is_some_and(|n| n.to_string_lossy().starts_with("podcast-")))
        .map(|d| {
            let script = d.join("script.txt");
            let mp3 = d.join("podcast.mp3");
            Pod {
                title: std::fs::read_to_string(d.join("title.txt")).unwrap_or_else(|_| d.file_name().unwrap().to_string_lossy().into()),
                lines: std::fs::read_to_string(&script).map(|s| s.lines().count()).unwrap_or(0),
                mp3: mp3.exists().then_some(mp3),
                secs: None,
                warnings: std::fs::read_to_string(d.join("warnings.txt"))
                    .map(|s| s.lines().filter(|l| !l.is_empty()).map(String::from).collect())
                    .unwrap_or_default(),
                chapters: std::fs::read_to_string(d.join("chapters.json")).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default(),
                timing: None,
                script,
                dir: d,
            }
        })
        .collect();
    v.sort_by(|a, b| b.dir.cmp(&a.dir));
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_tools_fail_before_llm() {
        let t = tempfile::tempdir().unwrap();
        let mut st = Store::at(t.path()).unwrap();
        st.cfg.tts.cmd = "/nonexistent/omnivoice".into();
        st.cfg.llm.url = "http://127.0.0.1:9/v1".into(); // would fail differently if called
        st.cfg.fallback = None;
        let mut nb = st.create("P").unwrap();
        let f = t.path().join("a.md");
        std::fs::write(&f, "Inhalt").unwrap();
        st.add_source(&mut nb, f.to_str().unwrap(), &|_| {}).unwrap();
        let e = run(&st, &nb, &Opts::default(), &|_| {}).unwrap_err().to_string();
        assert!(e.contains("omnivoice nicht gefunden"), "{e}");
        assert!(std::fs::read_dir(st.out_dir(&nb)).unwrap().next().is_none());
    }

    #[test]
    fn llm_failure_leaves_no_dir_and_list_skips_empty() {
        let t = tempfile::tempdir().unwrap();
        let mut st = Store::at(t.path()).unwrap();
        (st.cfg.tts.cmd, st.cfg.ffmpeg) = ("/bin/echo".into(), "/bin/echo".into());
        st.cfg.llm.url = "http://127.0.0.1:9/v1".into();
        st.cfg.fallback = None;
        let mut nb = st.create("P").unwrap();
        let f = t.path().join("a.md");
        std::fs::write(&f, "Inhalt").unwrap();
        st.add_source(&mut nb, f.to_str().unwrap(), &|_| {}).unwrap();
        assert!(run(&st, &nb, &Opts::default(), &|_| {}).is_err());
        assert!(std::fs::read_dir(st.out_dir(&nb)).unwrap().next().is_none());
        std::fs::create_dir_all(st.out_dir(&nb).join("podcast-20260101-000000")).unwrap();
        assert!(list(&st, &nb).is_empty());
    }

    #[test]
    fn reuse_only_same_notebook() {
        let t = tempfile::tempdir().unwrap();
        let st = Store::at(t.path().join("home")).unwrap();
        let (a, b) = (st.create("A").unwrap(), st.create("B").unwrap());
        let mk = |nb: &Notebook, n: &str| {
            let d = st.out_dir(nb).join(n);
            std::fs::create_dir_all(&d).unwrap();
            d.join("script.txt")
        };
        let own = mk(&a, "podcast-1");
        assert!(reuse_dir(&st, &a, &own).is_some());
        assert!(reuse_dir(&st, &b, &own).is_none());
        assert!(reuse_dir(&st, &a, &mk(&a, "other")).is_none());
        assert!(reuse_dir(&st, &a, &t.path().join("podcast-9/script.txt")).is_none());
    }
}
