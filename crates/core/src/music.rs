//! Store-aware music glue: `<FUNKHAUS_HOME>/music/{intro,outro,bed}.wav`, rendered once (never per episode).
use crate::{Prog, Store};
use anyhow::{Result, bail};
use funkhaus_voice::music::{self as vm, Built, Job};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

pub const PARTS: [&str; 3] = ["intro", "outro", "bed"];
type Samples = Option<Vec<f32>>;

pub fn dir(st: &Store) -> PathBuf {
    st.root.join("music")
}

/// Sample rate of the cached TTS clips (newest podcast dir with a clip); 24 kHz if there is none yet.
pub fn voice_sr(st: &Store) -> u32 {
    let mut dirs: Vec<PathBuf> = st.list().into_iter().flatten().flat_map(|nb| std::fs::read_dir(st.out_dir(&nb)).into_iter().flatten().flatten().map(|e| e.path())).filter(|p| p.join("script.txt").is_file()).collect();
    dirs.sort();
    let hex = |p: &Path| p.file_name().and_then(|n| n.to_str()).and_then(|n| n.strip_suffix(".wav")).is_some_and(|s| s.len() == 16 && s.bytes().all(|b| b.is_ascii_hexdigit()));
    dirs.iter().rev().flat_map(|d| std::fs::read_dir(d).into_iter().flatten().flatten().map(|e| e.path())).filter(|p| hex(p)).find_map(|p| hound::WavReader::open(p).ok().map(|r| r.spec().sample_rate)).unwrap_or(24_000)
}

fn jobs(st: &Store, seed: Option<u64>, sr: u32) -> Vec<Job<'_>> {
    let (m, s) = (&st.cfg.music, seed.unwrap_or(st.cfg.music.seed));
    // (part, caption, bpm, key, dur, trim)
    [("intro", m.captions.intro.as_str(), m.intro_bpm, m.intro_key.as_str(), 10, 8), ("outro", m.captions.outro.as_str(), m.outro_bpm, m.outro_key.as_str(), 10, 6), ("bed", m.captions.bed.as_str(), m.bed_bpm, m.bed_key.as_str(), 60, 0)]
        .into_iter()
        .enumerate()
        .map(|(i, (part, caption, bpm, key, dur, trim))| Job { part, caption, bpm, key, seed: s + i as u64, dur, trim, sr })
        .collect()
}

/// Renders missing/changed parts with acestep (GPU lock per call); unchanged parts are skipped.
pub fn make(st: &Store, seed: Option<u64>, prompt: Option<&str>, force: bool, prog: Prog) -> Result<Vec<Built>> {
    let _prio = funkhaus_sys::gpu::PrioGuard::hold(); // one interactive job across the 3 acestep calls
    make_with(st, seed, prompt, force, voice_sr(st), &|j, o| vm::render(&st.cfg.music.acestep, &st.cfg.ffmpeg, j, o, prog), prog)
}

/// `make` with an injectable runner (tests use a fake; no acestep).
pub fn make_with(st: &Store, seed: Option<u64>, prompt: Option<&str>, force: bool, sr: u32, run: &dyn Fn(&Job, &Path) -> Result<()>, prog: Prog) -> Result<Vec<Built>> {
    let mut js = jobs(st, seed, sr);
    if let Some(p) = prompt {
        js.iter_mut().for_each(|j| j.caption = p);
    }
    vm::make(&dir(st), &js, force, run, prog)
}

pub fn show(st: &Store) -> Value {
    let (d, man, sr) = (dir(st), vm::manifest(&dir(st)), voice_sr(st));
    let cur: Vec<String> = jobs(st, None, sr).iter().map(vm::key).collect();
    let files = PARTS.iter().zip(cur).map(|(p, k)| {
        let f = d.join(format!("{p}.wav"));
        let secs = hound::WavReader::open(&f).ok().map(|r| (r.duration() as f64 / r.spec().sample_rate as f64 * 100.0).round() / 100.0);
        let e = man.get(*p);
        (p.to_string(), json!({"path": f, "exists": f.is_file(), "secs": secs, "key": e.map(|e| &e.key), "seed": e.map(|e| e.seed), "current": e.is_some_and(|e| e.key == k)}))
    });
    json!({"dir": d, "enabled": st.cfg.music.enabled, "jingle": st.cfg.music.jingle, "bed": st.cfg.music.bed, "mix_sr": sr, "files": files.collect::<serde_json::Map<_, _>>()})
}

fn file(st: &Store, name: &str) -> PathBuf {
    let p = funkhaus_sys::expand(name);
    if p.is_absolute() { p } else { dir(st).join(p) }
}

/// (intro, outro, bed) samples at `sr`; None if music is disabled or nothing is selected.
/// Selection = flag or config default. A missing file is an error for an explicit flag, else a warning + skip.
pub fn parts(st: &Store, jingle: Option<bool>, bed: Option<bool>, sr: u32, prog: Prog) -> Result<Option<(Samples, Samples, Samples)>> {
    let m = &st.cfg.music;
    if !m.enabled {
        return Ok(None);
    }
    let missing = std::cell::RefCell::new(vec![]);
    let stale = std::cell::Cell::new(false);
    let (man, cur) = (vm::manifest(&dir(st)), jobs(st, None, voice_sr(st)));
    let get = |explicit: Option<bool>, def: bool, name: &str| -> Result<Samples> {
        if !explicit.unwrap_or(def) {
            return Ok(None);
        }
        let p = file(st, name);
        if !p.is_file() {
            if explicit == Some(true) {
                bail!("Musikdatei fehlt: {} (erst `funkhaus music make`)", p.display());
            }
            missing.borrow_mut().push(p.display().to_string());
            return Ok(None);
        }
        // only the files `music make` owns have a manifest key to compare
        if let Some(j) = cur.iter().find(|j| p == dir(st).join(format!("{}.wav", j.part))) {
            stale.set(stale.get() || man.get(j.part).is_none_or(|e| e.key != vm::key(j)));
        }
        Ok(Some(vm::load(&st.cfg.ffmpeg, &p, sr)?))
    };
    let (i, o, b) = (get(jingle, m.jingle, &m.intro)?, get(jingle, m.jingle, &m.outro)?, get(bed, m.bed, &m.bed_file)?);
    let miss = missing.into_inner();
    if !miss.is_empty() {
        prog(&format!("Hinweis: {} fehlt, ohne Musik (`funkhaus music make`)", miss.join(", ")));
    }
    if stale.get() {
        prog("Musik veraltet – `funkhaus music make`");
    }
    Ok((i.is_some() || o.is_some() || b.is_some()).then_some((i, o, b)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    fn st() -> (tempfile::TempDir, Store) {
        let t = tempfile::tempdir().unwrap();
        let s = Store::at(t.path()).unwrap();
        (t, s)
    }
    fn wav(p: &Path, n: usize) {
        funkhaus_audio::mix::write(p, &vec![0.1; n], 24_000).unwrap();
    }

    #[test]
    fn config_toml() {
        let c: crate::Config = toml::from_str("").unwrap();
        assert_eq!(c.music, Default::default());
        let c: crate::Config = toml::from_str("[music]\nbed = true\n").unwrap();
        assert!(c.music.bed && c.music.duck_db == -12.0 && c.music.jingle);
        let back: crate::Config = toml::from_str(&toml::to_string_pretty(&c).unwrap()).unwrap();
        assert_eq!(back.music, c.music);
    }

    #[test]
    fn make_skip_force_with_fake_runner() {
        let (_t, s) = st();
        let n = Cell::new(0);
        let run = |_: &Job, o: &Path| -> Result<()> { n.set(n.get() + 1); wav(o, 100); Ok(()) };
        let a = make_with(&s, None, None, false, 24_000, &run, &|_| {}).unwrap();
        assert_eq!((n.get(), a.len()), (3, 3));
        make_with(&s, None, None, false, 24_000, &run, &|_| {}).unwrap();
        assert_eq!(n.get(), 3);
        make_with(&s, Some(7), None, false, 24_000, &run, &|_| {}).unwrap();
        assert_eq!(n.get(), 6); // new seed = new keys
        make_with(&s, Some(7), None, true, 24_000, &run, &|_| {}).unwrap();
        assert_eq!(n.get(), 9);
        let v = show(&s);
        assert!(v["files"]["bed"]["exists"] == true && v["files"]["bed"]["current"] == false); // sr from clips = 24k fallback; seed differs from cfg
    }

    #[test]
    fn missing_files_error_only_when_explicit() {
        let (_t, s) = st();
        let w = std::cell::RefCell::new(vec![]);
        let prog = |m: &str| w.borrow_mut().push(m.to_string());
        assert!(parts(&s, None, None, 24_000, &prog).unwrap().is_none());
        assert_eq!(w.borrow().len(), 1); // one hint for intro + outro
        let e = parts(&s, None, Some(true), 24_000, &prog).unwrap_err().to_string();
        assert!(e.contains("funkhaus music make"));
        assert!(parts(&s, Some(false), None, 24_000, &prog).unwrap().is_none());
        assert!(parts(&s, Some(true), None, 24_000, &prog).is_err());
        std::fs::create_dir_all(dir(&s)).unwrap();
        for p in ["intro", "outro", "bed"] {
            wav(&dir(&s).join(format!("{p}.wav")), 1000);
        }
        let mut off = Store::at(_t.path()).unwrap();
        off.cfg.music.enabled = false;
        assert!(parts(&off, Some(true), Some(true), 24_000, &prog).unwrap().is_none());
        let (i, o, b) = parts(&s, None, None, 24_000, &prog).unwrap().unwrap();
        assert!(i.is_some() && o.is_some() && b.is_none());
        assert!(parts(&s, Some(false), Some(true), 24_000, &prog).unwrap().unwrap().2.is_some());
        // hand-placed files have no current manifest key: warn once, still use them
        w.borrow_mut().clear();
        parts(&s, None, Some(true), 24_000, &prog).unwrap().unwrap();
        assert_eq!(w.borrow().iter().filter(|m| m.contains("veraltet")).count(), 1);
        make_with(&s, None, None, true, voice_sr(&s), &|_, o| Ok(wav(o, 100)), &prog).unwrap();
        w.borrow_mut().clear();
        assert!(parts(&s, None, Some(true), 24_000, &prog).unwrap().is_some());
        assert!(w.borrow().iter().all(|m| !m.contains("veraltet")));
    }
}
