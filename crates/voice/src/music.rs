//! acestep jingle/bed rendering with cache + manifest (docs/m3/music-spec.md). The runner is injectable.
use crate::tts::fnv;
use anyhow::{Context, Result, bail};
use funkhaus_sys::which;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Bump when rendering/trim changes make cached parts wrong.
const CACHE_V: &str = "m1";

#[derive(Clone, Debug)]
pub struct Job<'a> {
    pub part: &'a str,
    pub caption: &'a str,
    pub bpm: u32,
    pub key: &'a str,
    pub seed: u64,
    /// acestep duration (min 10)
    pub dur: u32,
    /// trim to this many seconds with 1 s fade-out; 0 = keep whole
    pub trim: u32,
    pub sr: u32,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Entry {
    pub key: String,
    pub seed: u64,
    pub caption: String,
}

#[derive(Clone, Debug)]
pub struct Built {
    pub part: String,
    pub key: String,
    pub cached: bool,
    /// `<dir>/<part>.wav`
    pub path: PathBuf,
}

/// acestep arguments without the output path (part of the cache key).
pub fn args(j: &Job) -> Vec<String> {
    let mut a: Vec<String> = ["gen", j.caption, "-i", "-d"].map(String::from).to_vec();
    a.extend([j.dur.to_string(), "--bpm".into(), j.bpm.to_string(), "--key".into(), j.key.into()]);
    a.extend(["--timesig", "4", "--seed"].map(String::from));
    a.extend([j.seed.to_string(), "-f".into(), "wav24".into()]);
    a
}

/// 16 hex chars over version, part, acestep args and trim (no mix rate: `load` resamples; FNV-1a, same style as the TTS clip key).
pub fn key(j: &Job) -> String {
    let a = args(j).join("\u{1}");
    format!("{:016x}", fnv(&[CACHE_V, j.part, &a, &j.trim.to_string()]))
}

/// Real runner: acestep (under the GPU lock) → ffmpeg trim/fade, mono at `j.sr` → `out`.
pub fn render(acestep: &str, ffmpeg: &str, j: &Job, out: &Path, prog: &dyn Fn(&str)) -> Result<()> {
    let (ac, ff) = (which(acestep).context("acestep nicht gefunden")?, which(ffmpeg).context("ffmpeg nicht gefunden")?);
    let raw = out.with_extension("raw.wav");
    let mut a = args(j);
    a.extend(["-o".into(), raw.to_string_lossy().into_owned()]);
    let o = crate::gpu::with_lock(prog, || Command::new(&ac).args(&a).output())??;
    if !o.status.success() || !raw.exists() {
        bail!("acestep {}: {}{}", j.part, String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
    }
    let mut f: Vec<String> = ["-y", "-loglevel", "error", "-i"].map(String::from).to_vec();
    f.push(raw.to_string_lossy().into_owned());
    if j.trim > 0 {
        f.extend(["-t".into(), j.trim.to_string(), "-af".into(), format!("afade=t=out:st={}:d=1", j.trim - 1)]);
    }
    f.extend(["-ac".into(), "1".into(), "-ar".into(), j.sr.to_string(), out.to_string_lossy().into_owned()]);
    let o = Command::new(ff).args(&f).output()?;
    let _ = std::fs::remove_file(&raw);
    if !o.status.success() {
        bail!("ffmpeg {}: {}", j.part, String::from_utf8_lossy(&o.stderr));
    }
    Ok(())
}

pub fn manifest(dir: &Path) -> BTreeMap<String, Entry> {
    std::fs::read_to_string(dir.join("manifest.json")).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}

/// Renders each job into `<dir>/cache/<part>-<key>.wav` and copies it to `<dir>/<part>.wav`.
/// A part is skipped when manifest key, cache file and final file are all present (unless `force`).
/// Failed parts are dropped from the manifest; the error is returned after all parts were attempted.
pub fn make(dir: &Path, jobs: &[Job], force: bool, run: &dyn Fn(&Job, &Path) -> Result<()>, prog: &dyn Fn(&str)) -> Result<Vec<Built>> {
    let cache = dir.join("cache");
    std::fs::create_dir_all(&cache)?;
    let mut man = manifest(dir);
    let (mut built, mut errs) = (vec![], vec![]);
    for j in jobs {
        let k = key(j);
        let (cf, fin) = (cache.join(format!("{}-{k}.wav", j.part)), dir.join(format!("{}.wav", j.part)));
        let hit = !force && man.get(j.part).is_some_and(|e| e.key == k) && cf.exists() && fin.exists();
        let res = if hit {
            Ok(())
        } else {
            prog(&format!("acestep {} …", j.part));
            let tmp = cache.join(format!("{}.part.wav", j.part));
            let r = run(j, &tmp).and_then(|_| {
                if !tmp.exists() {
                    bail!("keine Ausgabe");
                }
                std::fs::rename(&tmp, &cf)?;
                let t2 = dir.join(format!("{}.part.wav", j.part));
                std::fs::copy(&cf, &t2)?;
                Ok(std::fs::rename(&t2, &fin)?)
            });
            let _ = std::fs::remove_file(&tmp);
            r
        };
        match res {
            Ok(()) => {
                for e in std::fs::read_dir(&cache)?.flatten() {
                    let n = e.file_name().to_string_lossy().into_owned();
                    if n.starts_with(&format!("{}-", j.part)) && n != format!("{}-{k}.wav", j.part) {
                        let _ = std::fs::remove_file(e.path());
                    }
                }
                man.insert(j.part.into(), Entry { key: k.clone(), seed: j.seed, caption: j.caption.into() });
                built.push(Built { part: j.part.into(), key: k, cached: hit, path: fin });
            }
            Err(e) => {
                man.remove(j.part);
                errs.push(format!("{}: {e:#}", j.part));
            }
        }
    }
    std::fs::write(dir.join("manifest.json"), serde_json::to_string_pretty(&man)?)?;
    if !errs.is_empty() {
        bail!("{}", errs.join("; "));
    }
    Ok(built)
}

/// Mono samples of `p` at `sr` (resampled through ffmpeg if the file's rate differs; nothing is cached).
pub fn load(ffmpeg: &str, p: &Path, sr: u32) -> Result<Vec<f32>> {
    let (x, r) = funkhaus_audio::mix::read(p)?;
    if r == sr {
        return Ok(x);
    }
    let ff = which(ffmpeg).context("ffmpeg nicht gefunden")?;
    let tmp = p.with_extension("resample.tmp.wav");
    let o = Command::new(ff).args(["-y", "-loglevel", "error", "-i"]).arg(p).args(["-ac", "1", "-ar", &sr.to_string()]).arg(&tmp).output()?;
    if !o.status.success() {
        bail!("ffmpeg resample: {}", String::from_utf8_lossy(&o.stderr));
    }
    let r = funkhaus_audio::mix::read(&tmp);
    let _ = std::fs::remove_file(&tmp);
    Ok(r?.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    fn job<'a>(part: &'a str, seed: u64) -> Job<'a> {
        Job { part, caption: "cap", bpm: 90, key: "D major", seed, dur: 10, trim: 8, sr: 24_000 }
    }
    fn run_n(d: &Path, jobs: &[Job], force: bool, n: &Cell<usize>) -> Result<Vec<Built>> {
        make(d, jobs, force, &|_, o| { n.set(n.get() + 1); std::fs::write(o, b"x")?; Ok(()) }, &|_| {})
    }

    #[test]
    fn key_stable_and_sensitive() {
        let j = job("intro", 42);
        assert_eq!(key(&j), key(&j.clone()));
        assert_eq!(key(&j).len(), 16);
        let ks = [key(&Job { seed: 43, ..j.clone() }), key(&Job { caption: "other", ..j.clone() }), key(&Job { bpm: 80, ..j.clone() }), key(&Job { trim: 6, ..j.clone() }), key(&job("outro", 42))];
        assert!(ks.iter().all(|k| *k != key(&j)));
        assert_eq!(key(&Job { sr: 48_000, ..j.clone() }), key(&j));
    }

    #[test]
    fn cache_skip_force_and_failure() {
        let t = tempfile::tempdir().unwrap();
        let d = t.path();
        let n = Cell::new(0);
        let jobs = [job("intro", 42), job("outro", 43), job("bed", 44)];
        let b = run_n(d, &jobs, false, &n).unwrap();
        assert_eq!((n.get(), b.len(), b[0].cached), (3, 3, false));
        assert!(d.join("intro.wav").exists() && d.join("manifest.json").exists());
        n.set(0);
        assert!(run_n(d, &jobs, false, &n).unwrap().iter().all(|b| b.cached));
        assert_eq!(n.get(), 0);
        run_n(d, &jobs, true, &n).unwrap();
        assert_eq!(n.get(), 3);
        n.set(0);
        let changed = [job("intro", 42), job("outro", 99), job("bed", 44)];
        run_n(d, &changed, false, &n).unwrap();
        assert_eq!(n.get(), 1);
        assert_eq!(std::fs::read_dir(d.join("cache")).unwrap().count(), 3); // old outro entry pruned
        // failing part is dropped from the manifest, others stay
        let r = make(d, &[job("intro", 1), job("bed", 44)], false, &|j, o| if j.part == "intro" { bail!("boom") } else { std::fs::write(o, b"x").map_err(Into::into) }, &|_| {});
        assert!(r.unwrap_err().to_string().contains("boom"));
        let m = manifest(d);
        assert!(!m.contains_key("intro") && m.contains_key("bed"));
    }

    #[test]
    fn render_trims_and_fades() {
        let Some(ff) = which("ffmpeg") else { return };
        let t = tempfile::tempdir().unwrap();
        let (src, out) = (t.path().join("s.wav"), t.path().join("o.wav"));
        funkhaus_audio::mix::write(&src, &vec![0.5f32; 48_000 * 10], 48_000).unwrap();
        // fake "acestep": copies the -o target from the prepared source
        let sh = t.path().join("fake-acestep");
        std::fs::write(&sh, format!("#!/bin/sh\nwhile [ $# -gt 0 ]; do [ \"$1\" = -o ] && o=\"$2\"; shift; done\ncp {} \"$o\"\n", src.display())).unwrap();
        std::fs::set_permissions(&sh, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        render(sh.to_str().unwrap(), ff.to_str().unwrap(), &job("intro", 1), &out, &|_| {}).unwrap();
        let (x, sr) = funkhaus_audio::mix::read(&out).unwrap();
        assert_eq!(sr, 24_000);
        assert!((x.len() as i64 - 192_000).abs() <= 1);
        assert!(x[x.len() - 1].abs() < 0.01 && (x[100_000] - 0.5).abs() < 0.01);
        assert!(load(ff.to_str().unwrap(), &out, 16_000).unwrap().len().abs_diff(128_000) <= 2);
    }
}
