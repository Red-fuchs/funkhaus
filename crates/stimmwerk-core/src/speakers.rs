//! Cast: speaker ID -> persona (`speaker set|ls`) and the voice map for rendering.
use crate::Project;
use crate::lint::persona;
use anyhow::{Result, bail};
use funkhaus_voice::persona::{Persona, VoiceSpec, resolve};
use funkhaus_voice::Tts;
use funkhaus_voice::tts::{self, Voices};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

/// Sets (adds if missing) speaker `id` to `persona`; label only changes when given. Persona must resolve;
/// an implicit builtin must be in `bi` (omnivoice's list). Returns a warning if `bi` is None and the persona is implicit.
pub fn set(p: &mut Project, dirs: &[PathBuf], bi: Option<&[String]>, id: &str, name: &str, label: Option<&str>) -> Result<Option<String>> {
    if !crate::script::id_ok(id) {
        bail!("Sprecher-ID {id:?} ungültig (Buchstaben, Ziffern, _ -; nicht B – reserviert für Einwürfe)");
    }
    let (kind, _, r) = persona(dirs, name, bi);
    if let Err(e) = r {
        bail!("Sprecher {id}: {e}");
    }
    p.add_speaker(id, None);
    let s = &mut p.meta.speakers[id];
    s.persona = Some(name.into());
    if let Some(l) = label {
        s.label = Some(l.into()).filter(|l: &String| !l.is_empty());
    }
    Ok((kind == "implicit" && bi.is_none()).then(|| format!("omnivoice-Stimmliste nicht verfügbar: '{name}' ungeprüft")))
}

/// Speakers in cast order with persona kind (builtin|design|clone|implicit|unset|error) and line counts.
pub fn ls(p: &Project, dirs: &[PathBuf], bi: Option<&[String]>) -> Value {
    let v: Vec<Value> = p
        .meta
        .speakers
        .iter()
        .map(|(id, s)| {
            let lines = p.script.lines().filter(|l| &l.who == id).count();
            let (kind, file, err) = match &s.persona {
                None => ("unset", None, None),
                Some(n) => {
                    let (k, f, r) = persona(dirs, n, bi);
                    (k, f, r.err())
                }
            };
            json!({"id": id, "persona": s.persona, "label": s.label, "kind": kind, "file": file, "lines": lines, "error": err})
        })
        .collect();
    json!(v)
}

/// Voice spec of a persona across dirs (first file wins, else implicit builtin).
pub fn spec(dirs: &[PathBuf], name: &str) -> Result<VoiceSpec> {
    let d = dirs.iter().find(|d| Persona::path(d, name).exists()).or(dirs.first()).cloned().unwrap_or_default();
    let v = resolve(&d, name)?;
    if v.ref_wav.is_some() && v.kind == funkhaus_voice::persona::Kind::Design {
        crate::voice::check_anchor_source(&d, name)?;
    }
    Ok(v)
}

/// Voices for every speaker used in the script; unknown speaker or unset persona is an error.
pub fn voices(p: &Project, dirs: &[PathBuf]) -> Result<Voices> {
    let mut v = Voices::new();
    for l in p.script.lines() {
        if v.contains_key(&l.who) {
            continue;
        }
        let Some(s) = p.meta.speakers.get(&l.who) else { bail!("Sprecher {:?} ist im Projekt nicht angelegt – im Reiter „Stimmen“ eine Stimme zuweisen oder `stimmwerk lint`", l.who) };
        let Some(n) = &s.persona else { bail!("Sprecher {} hat keine Persona (Stimme) – im Reiter „Stimmen“ wählen oder `stimmwerk speaker set <proj> {} --persona NAME`", l.who, l.who) };
        v.insert(l.who.clone(), spec(dirs, n).map_err(|e| anyhow::anyhow!("Sprecher {}: {e:#}", l.who))?);
    }
    Ok(v)
}

/// Probe sample cache: `<home>/cache/probe`.
pub fn probe_dir() -> PathBuf {
    crate::home().join("cache").join("probe")
}

/// Short voice sample ("Vorhören"): `text` in persona `name`, draft quality, cached as `probe_dir()/<key>.wav`.
pub fn probe(dirs: &[PathBuf], name: &str, text: &str, lang: &str, cancel: &AtomicBool, prog: &crate::render::Prog) -> Result<PathBuf> {
    let t = Tts { cmd: crate::render::tts_cmd(), lang: lang.into(), quality: "draft".into(), ..Tts::default() };
    crate::render::with_say(t, cancel, prog, |say| probe_with(&probe_dir(), dirs, name, text, lang, say))
}

/// `probe` into `dir` with an injectable `say(wav, text, voice, seed)`; a cached sample needs no TTS.
pub fn probe_with(dir: &Path, dirs: &[PathBuf], name: &str, text: &str, lang: &str, say: &mut crate::render::Say) -> Result<PathBuf> {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.is_empty() {
        bail!("Probetext ist leer");
    }
    let v = spec(dirs, name).map_err(|e| anyhow::anyhow!("Persona {name}: {e:#}"))?;
    v.validate().map_err(|e| anyhow::anyhow!("Persona {name}: {e:#}"))?;
    let t = Tts { lang: lang.into(), quality: "draft".into(), ..Tts::default() };
    let wav = dir.join(format!("{:016x}.wav", tts::key(&t, &v, lang, &text)));
    if !wav.is_file() {
        std::fs::create_dir_all(dir)?;
        crate::render::sweep(dir);
        crate::render::atomic(&wav, |tmp| say(tmp, &text, &v, tts::seed_of(&t, &v, lang, &text)))?;
    }
    if v.ref_wav.is_some() {
        record_checked(dir, dirs, name, &v.ref_sha, std::slice::from_ref(&wav))?;
    }
    Ok(wav)
}

/// Indexes probe audio `files` of ref voice `name` (sha `sha`) in `dir`, under the index lock: a `voice revoke|rm`
/// that ran while the sample was rendered (it holds this lock for its whole run) is seen here, and the fresh files
/// are deleted instead of indexed – no audio of a withdrawn voice survives a concurrent probe.
pub(crate) fn record_checked(dir: &Path, dirs: &[PathBuf], name: &str, sha: &str, files: &[PathBuf]) -> Result<()> {
    let lk = crate::derived::Lock::take(dir)?;
    match spec(dirs, name).and_then(|v| v.validate().map(|_| v)) {
        Ok(v) if v.ref_sha == sha => lk.record(sha, name, files),
        r => {
            for f in files {
                let _ = crate::derived::remove(f, crate::derived::AUDIO);
            }
            let why = r.err().map(|e| format!("{e:#}")).unwrap_or_else(|| "Referenz-Aufnahme wurde ersetzt".into());
            bail!("Stimme {name} wurde während der Hörprobe widerrufen oder gelöscht – Probe verworfen ({why})")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_ls_voices() {
        let t = tempfile::tempdir().unwrap();
        let dirs = vec![t.path().join("voices")];
        let mut p = Project::create(&t.path().join("x.studio"), "X", "de").unwrap();
        p.script = crate::script::Script::parse("ERZ|0|Hallo.\nMara|0.3|Ja.\n").unwrap();
        p.add_speaker("ERZ", Some("Erzählerin"));
        assert!(voices(&p, &dirs).unwrap_err().to_string().contains("keine Persona"));
        let bi = ["gt".to_string(), "kn".to_string()];
        let bi = Some(&bi[..]);
        assert_eq!(set(&mut p, &dirs, bi, "ERZ", "gt", None).unwrap(), None);
        assert!(voices(&p, &dirs).unwrap_err().to_string().contains("Mara"));
        assert!(set(&mut p, &dirs, bi, "Mara", "anna", None).unwrap_err().to_string().contains("weder eigene Stimme noch omnivoice-Stimme"));
        assert!(set(&mut p, &dirs, None, "Mara", "anna", None).unwrap().is_some_and(|w| w.contains("ungeprüft")));
        set(&mut p, &dirs, bi, "Mara", "kn", Some("Mara Lind")).unwrap();
        assert!(set(&mut p, &dirs, bi, "B", "gt", None).is_err());
        assert!(set(&mut p, &dirs, bi, "ERZ", "Kein Name!", None).is_err());
        assert_eq!(p.meta.speakers["ERZ"].label.as_deref(), Some("Erzählerin"));
        let v = voices(&p, &dirs).unwrap();
        assert_eq!((v["ERZ"].voice.as_str(), v["Mara"].voice.as_str()), ("gt", "kn"));
        let l = ls(&p, &dirs, bi);
        assert_eq!((l[1]["id"].as_str(), l[1]["label"].as_str(), l[1]["kind"].as_str(), l[1]["lines"].as_u64()), (Some("Mara"), Some("Mara Lind"), Some("implicit"), Some(1)));
        p.save().unwrap();
        assert_eq!(Project::load(&p.dir).unwrap().meta, p.meta);
    }

    #[test]
    fn probe_cached() {
        let t = tempfile::tempdir().unwrap();
        let (dir, n) = (t.path().join("probe"), std::cell::Cell::new(0));
        let mut say = |w: &Path, x: &str, v: &VoiceSpec, _s: u64| -> Result<()> {
            n.set(n.get() + 1);
            assert_eq!((x, v.voice.as_str()), ("Hallo du.", "gt"));
            funkhaus_audio::mix::write(w, &[0.1; 2400], 24_000)
        };
        let a = probe_with(&dir, &[], "gt", " Hallo   du. ", "de", &mut say).unwrap();
        let b = probe_with(&dir, &[], "gt", "Hallo du.", "de", &mut say).unwrap();
        assert_eq!((a.clone(), n.get()), (b, 1));
        assert!(a.is_file() && a.starts_with(&dir));
        assert!(probe_with(&dir, &[], "gt", "  ", "de", &mut say).is_err());
        assert!(probe_with(&dir, &[], "Kein Name", "x", "de", &mut say).is_err());
        let mut fail = |_: &Path, _: &str, _: &VoiceSpec, _: u64| -> Result<()> { anyhow::bail!("kaputt") };
        assert!(probe_with(&dir, &[], "kn", "Neu.", "de", &mut fail).is_err());
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1, "no temp left");
    }
}
