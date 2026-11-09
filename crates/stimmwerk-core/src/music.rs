//! acestep music for cues: `takes/music/<key>.wav` content-hash cache (key over prompt, duration, seed, bpm, key, model),
//! rendered as a child process under the shared GPU lock and cancellable like TTS (`render::run_child`); the cached file
//! is acestep's own output (24-bit WAV, its rate) – the mix resamples through ffmpeg when needed, so the key is
//! rate-independent. `manifest.json` next to it is informational (`stimmwerk music ls`, `cue add --gen <key>`).
//! Also: import of own WAVs (CC0 ambience, sfx) into `assets/<kind>/`.
use crate::cues::{Gen, Kind};
use crate::render::{self, Event, Prog};
use crate::Project;
use anyhow::{Context, Result, bail};
use funkhaus_sys::gpu;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

/// acestep command override (tests, other installs); default `acestep`.
pub const ACESTEP_ENV: &str = "STIMMWERK_ACESTEP_CMD";
/// Bump when the cached files become wrong for the mix (own tag, never collides with funkhaus' `m1`).
const CACHE_V: &str = "s1";

pub fn acestep_cmd() -> String {
    std::env::var(ACESTEP_ENV).ok().filter(|c| !c.is_empty()).unwrap_or_else(|| "acestep".into())
}

/// `acestep --version` (first line), asked once per process; empty when acestep is missing or does not answer. Part of
/// the cache key: a new acestep renders differently, so old cached music is not reused for it.
pub fn acestep_version() -> &'static str {
    static V: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    V.get_or_init(|| {
        funkhaus_sys::which(&acestep_cmd())
            .and_then(|b| std::process::Command::new(b).arg("--version").stdin(std::process::Stdio::null()).output().ok())
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).lines().next().unwrap_or_default().trim().to_string())
            .unwrap_or_default()
    })
}

/// `<proj>/takes/music`
pub fn dir(p: &Project) -> PathBuf {
    render::takes_root(p).join("music")
}

/// acestep arguments without the output path (= the cache key's content).
pub fn args(g: &Gen) -> Vec<String> {
    let mut a: Vec<String> = ["gen", g.prompt.trim(), "-i", "-d"].map(String::from).to_vec();
    a.extend([g.dur.to_string(), "--seed".into(), g.seed.to_string(), "--timesig".into(), "4".into()]);
    if g.bpm > 0 {
        a.extend(["--bpm".into(), g.bpm.to_string()]);
    }
    if let Some(k) = g.key.as_deref().map(str::trim).filter(|k| !k.is_empty()) {
        a.extend(["--key".into(), k.into()]);
    }
    if let Some(m) = g.model.as_deref().map(str::trim).filter(|m| !m.is_empty()) {
        a.extend(["--dit".into(), m.into()]);
    }
    a.extend(["-f".into(), "wav24".into()]);
    a
}

/// 16 hex chars (FNV-1a, like the TTS clip key) over the version tag (+ acestep version when known) and the arguments.
pub fn key(g: &Gen) -> String {
    let v = acestep_version();
    let tag = if v.is_empty() { CACHE_V.to_string() } else { format!("{CACHE_V}@{v}") };
    format!("{:016x}", funkhaus_voice::tts::fnv(&[&tag, &args(g).join("\u{1}")]))
}

/// Cache file of a generation (may not exist yet).
pub fn path(p: &Project, g: &Gen) -> PathBuf {
    dir(p).join(format!("{}.wav", key(g)))
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Entry {
    #[serde(rename = "gen")]
    pub gen_: Gen,
    /// ISO date of the render
    pub created: String,
    pub secs: f64,
    pub sr: u32,
}

pub fn manifest(p: &Project) -> BTreeMap<String, Entry> {
    std::fs::read_to_string(dir(p).join("manifest.json")).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}

fn save_manifest(p: &Project, m: &BTreeMap<String, Entry>) -> Result<()> {
    crate::project::write(&dir(p).join("manifest.json"), &serde_json::to_string_pretty(m)?)
}

/// Runs acestep with `args` + `-o <out>`; must leave a readable WAV at `out`. Tests inject a fake.
pub type Run<'a> = dyn FnMut(&[String], &Path) -> Result<()> + 'a;

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Built {
    pub key: String,
    pub path: PathBuf,
    /// no acestep call was needed
    pub cached: bool,
    pub secs: f64,
    pub sr: u32,
    #[serde(rename = "gen")]
    pub gen_: Gen,
}

/// Cache state and file info of `g` (no render).
pub fn lookup(p: &Project, g: &Gen) -> Option<Built> {
    let f = path(p, g);
    let (fr, sr, _) = funkhaus_audio::mix::info(&f).ok()?;
    Some(Built { key: key(g), path: f, cached: true, secs: render::r2(fr as f64 / sr.max(1) as f64), sr, gen_: g.clone() })
}

/// Renders `g` into the cache unless the file exists (or `force`). Temp file + rename, so a cancelled or failed run
/// never leaves a half file under the key; the manifest only lists successful renders.
pub fn generate_with(p: &Project, g: &Gen, force: bool, run: &mut Run) -> Result<Built> {
    g.check()?;
    if !force && let Some(b) = lookup(p, g) {
        return Ok(b);
    }
    let d = dir(p);
    std::fs::create_dir_all(&d)?;
    render::sweep(&d);
    let f = path(p, g);
    let tmp = render::part_path(&f);
    let r = run(&args(g), &tmp).and_then(|_| funkhaus_audio::mix::info(&tmp).context("acestep hat keine lesbare WAV geschrieben"));
    let (fr, sr, _) = match r {
        Ok(v) => v,
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            return Err(e);
        }
    };
    std::fs::rename(&tmp, &f).with_context(|| f.display().to_string())?;
    let secs = render::r2(fr as f64 / sr.max(1) as f64);
    let mut m = manifest(p);
    m.insert(key(g), Entry { gen_: g.clone(), created: chrono::Local::now().format("%Y-%m-%d").to_string(), secs, sr });
    save_manifest(p, &m)?;
    Ok(Built { key: key(g), path: f, cached: false, secs, sr, gen_: g.clone() })
}

/// The real runner: acestep looked up at the first call, each call under the GPU lock (wait cancellable), the child
/// stopped when `cancel` is set (`render::run_child`). Interactive GPU priority is held across calls.
pub fn with_real_run<T>(cancel: &AtomicBool, prog: &Prog, f: impl FnOnce(&mut Run) -> Result<T>) -> Result<T> {
    let txt = |m: &str| prog(Event::Msg { text: m.into() });
    let cmd = acestep_cmd();
    let mut bin: Option<PathBuf> = None;
    let mut g: Option<Option<gpu::PrioGuard>> = None;
    f(&mut |a, out| {
        if bin.is_none() {
            bin = Some(funkhaus_sys::which(&cmd).with_context(|| format!("acestep nicht gefunden ({cmd}) – `~/.claude/skills/acestep` installieren oder {ACESTEP_ENV} setzen"))?);
        }
        if let Some(g) = g.get_or_insert_with(gpu::PrioGuard::hold) {
            g.touch();
        }
        let mut args = a.to_vec();
        args.extend(["-o".into(), out.to_string_lossy().into_owned()]);
        prog(Event::Msg { text: format!("acestep {} s …", a.iter().skip_while(|x| *x != "-d").nth(1).cloned().unwrap_or_default()) });
        let log = render::part_path(&out.with_extension("log"));
        let st = gpu::with_lock_cancel(&txt, cancel, || render::run_child(bin.as_deref().expect("set above"), &args, &log, cancel)).map_err(|e| if cancel.load(Ordering::Relaxed) { anyhow::anyhow!(funkhaus_voice::tts::CANCELLED) } else { e });
        let o = std::fs::read_to_string(&log).unwrap_or_default();
        let _ = std::fs::remove_file(&log);
        if !st??.success() || !out.exists() {
            bail!("acestep: {}", o.trim());
        }
        Ok(())
    })
}

/// `music gen`: render (or hit the cache) under the project lock.
pub fn generate(p: &Project, g: &Gen, force: bool, cancel: &AtomicBool, prog: &Prog) -> Result<Built> {
    let lk = render::lock(p)?;
    let r = with_real_run(cancel, prog, |run| generate_with(p, g, force, run));
    drop(lk);
    r.map_err(|e| render::cancelled(e, cancel))
}

/// Cache listing for `music ls`: manifest entries, whether the file exists and which cues use them.
pub fn ls(p: &Project) -> Vec<serde_json::Value> {
    manifest(p)
        .iter()
        .map(|(k, e)| {
            let used: Vec<usize> = p.meta.cues.iter().enumerate().filter(|(_, c)| c.gen_.as_ref().is_some_and(|g| key(g) == *k)).map(|(i, _)| i + 1).collect();
            serde_json::json!({"key": k, "path": dir(p).join(format!("{k}.wav")), "exists": dir(p).join(format!("{k}.wav")).is_file(), "secs": e.secs, "sr": e.sr, "created": e.created, "gen": e.gen_, "cues": used})
        })
        .collect()
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Imported {
    /// `assets/<kind>/<name>.wav`
    pub file: String,
    pub path: PathBuf,
    pub secs: f64,
    pub sr: u32,
    pub channels: u16,
    /// converted through ffmpeg (not a plain mono/stereo PCM WAV)
    pub converted: bool,
}

/// File name part: umlauts transliterated (ä→ae, Ö→Oe, ß→ss), then letters, digits, `_`, `-`; runs of anything else
/// become one `-`.
pub fn safe_name(s: &str) -> String {
    let tl = |c: char| -> &'static str {
        match c {
            'ä' => "ae", 'ö' => "oe", 'ü' => "ue", 'Ä' => "Ae", 'Ö' => "Oe", 'Ü' => "Ue", 'ß' => "ss",
            _ => "",
        }
    };
    let n: String = s.chars().map(|c| if tl(c).is_empty() { if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c.to_string() } else { "-".into() } } else { tl(c).into() }).collect();
    n.split('-').filter(|x| !x.is_empty()).collect::<Vec<_>>().join("-")
}

/// Copies `src` to `assets/<kind>/<name>.wav` (mono/stereo PCM WAV verbatim, anything else through ffmpeg to mono
/// float WAV at its rate) and notes source + license in `assets/SOURCES.md`. Refuses to overwrite.
pub fn import(p: &Project, src: &Path, name: Option<&str>, kind: Kind, source: Option<&str>, license: Option<&str>, ffmpeg: &str) -> Result<Imported> {
    if !src.is_file() {
        bail!("{} nicht gefunden", src.display());
    }
    let stem = safe_name(name.unwrap_or_else(|| src.file_stem().and_then(|s| s.to_str()).unwrap_or("cue")));
    if stem.is_empty() {
        bail!("kein brauchbarer Name – --as <name>");
    }
    let rel = format!("assets/{}/{stem}.wav", kind.as_str());
    let dst = p.dir.join(&rel);
    if dst.exists() {
        bail!("{rel} existiert schon – --as <anderer Name>");
    }
    std::fs::create_dir_all(dst.parent().expect("has parent"))?;
    let plain = funkhaus_audio::mix::info(src).ok().filter(|(_, _, ch)| *ch <= 2);
    let converted = plain.is_none();
    render::atomic(&dst, |t| {
        if !converted {
            return std::fs::copy(src, t).map(drop).with_context(|| src.display().to_string());
        }
        let ff = funkhaus_sys::which(ffmpeg).context("ffmpeg nicht gefunden (nötig für Nicht-WAV-Quellen)")?;
        let o = std::process::Command::new(ff).args(["-y", "-loglevel", "error", "-i"]).arg(src).args(["-ac", "1", "-c:a", "pcm_f32le", "-f", "wav"]).arg(t).output()?;
        if !o.status.success() {
            bail!("ffmpeg: {}", String::from_utf8_lossy(&o.stderr).trim());
        }
        Ok(())
    })?;
    let (fr, sr, channels) = funkhaus_audio::mix::info(&dst)?;
    let line = format!("- {rel} ← {} ({}){}{}\n", src.display(), chrono::Local::now().format("%Y-%m-%d"), source.map(|s| format!(" · Quelle: {s}")).unwrap_or_default(), license.map(|l| format!(" · Lizenz: {l}")).unwrap_or_default());
    let sf = p.dir.join("assets/SOURCES.md");
    let mut md = std::fs::read_to_string(&sf).unwrap_or_else(|_| "# Quellen der Assets (Datei ← Herkunft, Datum, Lizenz)\n".into());
    md.push_str(&line);
    crate::project::write(&sf, &md)?;
    Ok(Imported { file: rel, path: dst, secs: render::r2(fr as f64 / sr.max(1) as f64), sr, channels, converted })
}

#[cfg(test)]
mod tests {
    use super::*;
    use funkhaus_audio::mix;
    use std::cell::Cell;

    fn spec(seed: u64) -> Gen {
        Gen { prompt: "calm maritime intro, acoustic guitar".into(), dur: 12, seed, bpm: 70, key: Some("A minor".into()), model: None }
    }

    fn proj() -> (tempfile::TempDir, Project) {
        let t = tempfile::tempdir().unwrap();
        let p = Project::create(&t.path().join("m.studio"), "M", "de").unwrap();
        (t, p)
    }

    #[test]
    fn key_stable_and_sensitive() {
        let g = spec(1);
        assert_eq!(key(&g), key(&g.clone()));
        assert_eq!(key(&g).len(), 16);
        let ks = [key(&Gen { seed: 2, ..g.clone() }), key(&Gen { prompt: "other".into(), ..g.clone() }), key(&Gen { dur: 13, ..g.clone() }), key(&Gen { bpm: 0, ..g.clone() }), key(&Gen { key: None, ..g.clone() }), key(&Gen { model: Some("xl-turbo".into()), ..g.clone() })];
        assert!(ks.iter().all(|k| *k != key(&g)));
        assert_eq!(key(&Gen { prompt: " calm maritime intro, acoustic guitar ".into(), ..g.clone() }), key(&g), "trimmed");
        // acestep version (when found) is part of the tag: a new acestep never reuses old music
        let v = acestep_version();
        let tag = if v.is_empty() { CACHE_V.to_string() } else { format!("{CACHE_V}@{v}") };
        assert_eq!(key(&g), format!("{:016x}", funkhaus_voice::tts::fnv(&[&tag, &args(&g).join("\u{1}")])));
        let a = args(&g);
        assert_eq!(a, ["gen", "calm maritime intro, acoustic guitar", "-i", "-d", "12", "--seed", "1", "--timesig", "4", "--bpm", "70", "--key", "A minor", "-f", "wav24"]);
        assert!(!args(&Gen { bpm: 0, key: None, ..g }).iter().any(|x| x == "--bpm" || x == "--key"));
    }

    #[test]
    fn generate_cache_force_failure() {
        let (_t, p) = proj();
        let n = Cell::new(0);
        let mut fake = |_: &[String], o: &Path| {
            n.set(n.get() + 1);
            mix::write(o, &vec![0.3f32; 48_000 * 12], 48_000)
        };
        let b = generate_with(&p, &spec(1), false, &mut fake).unwrap();
        assert_eq!((b.cached, b.secs, b.sr, n.get()), (false, 12.0, 48_000, 1));
        assert!(b.path.is_file() && b.path.ends_with(format!("takes/music/{}.wav", b.key)));
        let b2 = generate_with(&p, &spec(1), false, &mut fake).unwrap();
        assert_eq!((b2.cached, n.get(), b2.path), (true, 1, b.path.clone()), "cache hit: no call");
        assert_eq!(lookup(&p, &spec(1)).map(|b| b.cached), Some(true));
        assert!(generate_with(&p, &spec(2), false, &mut fake).unwrap().path != b.path);
        assert_eq!(n.get(), 2);
        generate_with(&p, &spec(1), true, &mut fake).unwrap();
        assert_eq!(n.get(), 3, "--force renders again");
        let m = manifest(&p);
        assert_eq!((m.len(), m[&b.key].gen_.seed, m[&b.key].secs), (2, 1, 12.0));
        // failure: no file under the key, no manifest entry, no temp left
        let e = generate_with(&p, &spec(3), false, &mut |_, _| bail!("boom")).unwrap_err();
        assert!(e.to_string().contains("boom"));
        let e = generate_with(&p, &spec(4), false, &mut |_, o| std::fs::write(o, b"not a wav").map_err(Into::into)).unwrap_err();
        assert!(e.to_string().contains("lesbare WAV"), "{e}");
        assert!(!path(&p, &spec(3)).exists() && !path(&p, &spec(4)).exists() && manifest(&p).len() == 2);
        assert!(!std::fs::read_dir(dir(&p)).unwrap().flatten().any(|e| e.file_name().to_string_lossy().contains(".part.")));
        assert!(generate_with(&p, &Gen { dur: 5, ..spec(1) }, false, &mut fake).unwrap_err().to_string().contains("10"));
        assert_eq!(ls(&p).len(), 2);
    }

    #[test]
    fn import_wav_and_sources() {
        let (t, p) = proj();
        let src = t.path().join("Meer Rauschen (CC0).wav");
        mix::write(&src, &vec![0.1f32; 24_000 * 3], 24_000).unwrap();
        let r = import(&p, &src, None, Kind::Ambience, Some("https://freesound.org/x"), Some("CC0"), "ffmpeg").unwrap();
        assert_eq!((r.file.as_str(), r.secs, r.sr, r.channels, r.converted), ("assets/ambience/Meer-Rauschen-CC0.wav", 3.0, 24_000, 1, false));
        assert!(r.path.is_file());
        let md = std::fs::read_to_string(p.dir.join("assets/SOURCES.md")).unwrap();
        assert!(md.contains("assets/ambience/Meer-Rauschen-CC0.wav ←") && md.contains("Quelle: https://freesound.org/x") && md.contains("Lizenz: CC0"), "{md}");
        assert!(import(&p, &src, None, Kind::Ambience, None, None, "ffmpeg").unwrap_err().to_string().contains("existiert schon"));
        let r = import(&p, &src, Some("tür knarrt"), Kind::Sfx, None, None, "ffmpeg").unwrap();
        assert_eq!(r.file, "assets/sfx/tuer-knarrt.wav");
        assert!(import(&p, &t.path().join("nope.wav"), None, Kind::Sfx, None, None, "ffmpeg").is_err());
        assert_eq!(safe_name("..//"), "");
        assert_eq!(safe_name("Ärger Öl Übung Straße größer"), "Aerger-Oel-Uebung-Strasse-groesser");
        // non-WAV source needs ffmpeg (skipped without it)
        if let Some(ff) = funkhaus_sys::which("ffmpeg") {
            let raw = t.path().join("raw.txt");
            std::fs::write(&raw, b"junk").unwrap();
            assert!(import(&p, &raw, Some("junk"), Kind::Sfx, None, None, ff.to_str().unwrap()).is_err());
            let o = std::process::Command::new(&ff).args(["-y", "-loglevel", "error", "-f", "lavfi", "-i", "sine=frequency=440:duration=1", "-ac", "2", "-ar", "44100", "-c:a", "libmp3lame"]).arg(t.path().join("s.mp3")).output().unwrap();
            if o.status.success() {
                let r = import(&p, &t.path().join("s.mp3"), None, Kind::Music, None, None, ff.to_str().unwrap()).unwrap();
                assert!(r.converted && r.channels == 1 && r.sr == 44_100 && (r.secs - 1.0).abs() < 0.1, "{r:?}");
            }
        }
    }
}
