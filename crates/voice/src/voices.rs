//! Host voices: persona files under `<home>/voices/`, host -> persona resolution, CLI helpers.
use crate::Tts;
use anyhow::{Context, Result, bail};
use crate::persona::{Consent, Kind, Persona, VoiceSpec, resolve};
use crate::tts;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

pub fn dir(root: &Path) -> PathBuf {
    root.join("voices")
}

/// (O, F) specs from persona names.
pub fn hosts(root: &Path, (o, f): (&str, &str)) -> Result<(VoiceSpec, VoiceSpec)> {
    let d = dir(root);
    Ok((resolve(&d, o).with_context(|| format!("Host O: Persona '{o}'"))?, resolve(&d, f).with_context(|| format!("Host F: Persona '{f}'"))?))
}

fn kind_s(k: Kind) -> &'static str {
    match k {
        Kind::Builtin => "builtin",
        Kind::Design => "design",
        Kind::Clone => "clone",
    }
}

/// `hosts` = the (O, F) persona names currently configured.
pub fn ls(root: &Path, (ho, hf): (&str, &str)) -> Value {
    let used = |n: &str| -> Vec<&str> { [("o", ho), ("f", hf)].iter().filter(|(_, p)| *p == n).map(|(h, _)| *h).collect() };
    let mut v: Vec<Value> = Persona::list(&dir(root))
        .into_iter()
        .map(|(n, p)| match p {
            Ok(p) => {
                let revoked = p.consent.as_ref().and_then(|c| c.revoked.clone()).filter(|r| !r.is_empty());
                json!({"name": n, "kind": kind_s(p.kind), "lang": p.lang, "description": p.description, "hosts": used(&n), "revoked": revoked})
            }
            Err(e) => json!({"name": n, "error": format!("{e:#}"), "hosts": used(&n)}),
        })
        .collect();
    // host personas without a file are implicit omnivoice builtins
    for (h, p) in [("o", ho), ("f", hf)] {
        if !v.iter().any(|x| x["name"] == p) {
            v.push(json!({"name": p, "kind": "builtin", "implicit": true, "hosts": [h]}));
        }
    }
    json!(v)
}

pub fn show(root: &Path, t: &Tts, name: &str) -> Result<Value> {
    let d = dir(root);
    let spec = resolve(&d, name)?;
    let file = Persona::path(&d, name);
    let persona = file.exists().then(|| Persona::load(&d, name)).transpose()?;
    Ok(json!({
        "name": name, "file": file.exists().then_some(file), "persona": persona,
        "spec": {"kind": kind_s(spec.kind), "voice": spec.voice, "instruct": spec.instruct, "ref": spec.ref_wav, "ref_text": spec.ref_text, "ref_sha256": spec.ref_sha, "seed": spec.seed.unwrap_or(t.seed)},
        "key_base": format!("{:016x}", tts::key(t, &spec, &t.lang, "")),
    }))
}

#[derive(Default)]
pub struct New {
    pub kind: Option<Kind>,
    pub voice: Option<String>,
    pub instruct: Option<String>,
    pub ref_: Option<PathBuf>,
    pub ref_text: Option<String>,
    pub lang: Option<String>,
    pub seed: Option<u64>,
    pub description: Option<String>,
    pub consent_who: Option<String>,
    pub consent_statement: Option<String>,
    pub consent_date: Option<String>,
    pub consent_recording: Option<String>,
}

pub fn add(root: &Path, name: &str, n: New) -> Result<Value> {
    let d = dir(root);
    if n.kind == Some(Kind::Design) && n.ref_.is_some() {
        bail!("design: --ref nicht erlaubt – verankerte Design-Stimmen legt nur `stimmwerk voice keep` an; eine fremde Aufnahme nur als --kind clone mit Einwilligung");
    }
    let consent = match (n.consent_who, n.consent_statement) {
        (None, None) => None,
        (w, s) => Some(Consent {
            who: w.unwrap_or_default(),
            statement: s.unwrap_or_default(),
            date: n.consent_date.unwrap_or_else(|| chrono::Local::now().format("%Y-%m-%d").to_string()),
            recording: n.consent_recording,
            revoked: None,
        }),
    };
    let p = Persona {
        name: name.into(),
        kind: n.kind.context("--kind builtin|design|clone nötig")?,
        lang: n.lang.unwrap_or_else(|| "de".into()),
        seed: n.seed,
        voice: n.voice,
        instruct: n.instruct,
        ref_: n.ref_.map(|r| r.to_string_lossy().into_owned()),
        ref_text: n.ref_text,
        description: n.description,
        provenance: None,
        consent,
    };
    let f = p.add(&d)?;
    Ok(json!({"added": name, "file": f}))
}

/// One `say` into `<home>/cache/voices/` through the injected closure; returns the wav path.
pub fn test(root: &Path, t: &Tts, name: &str, text: &str, say: impl FnOnce(&Path, &str, &VoiceSpec, u64) -> Result<()>) -> Result<PathBuf> {
    let spec = resolve(&dir(root), name)?;
    spec.validate()?;
    let out = root.join("cache").join("voices");
    std::fs::create_dir_all(&out)?;
    let wav = out.join(format!("{name}-{:016x}.wav", tts::key(t, &spec, &t.lang, text)));
    let tmp = wav.with_extension("part.wav");
    if let Err(e) = say(&tmp, text, &spec, tts::seed_of(t, &spec, &t.lang, text)) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    if !tmp.exists() {
        bail!("keine Ausgabe");
    }
    std::fs::rename(&tmp, &wav)?;
    Ok(wav)
}

/// Real `omnivoice say` for [`test`].
pub fn test_real(root: &Path, t: &Tts, name: &str, text: &str, prog: &dyn Fn(&str)) -> Result<PathBuf> {
    let say = tts::say_cmd(t, &t.lang, prog)?;
    test(root, t, name, text, say)
}

#[cfg(test)]
mod tests {
    use super::*;

    const H: (&str, &str) = ("gt", "fable");

    fn st() -> (tempfile::TempDir, PathBuf, Tts) {
        let t = tempfile::tempdir().unwrap();
        let r = t.path().join("home");
        std::fs::create_dir_all(&r).unwrap();
        (t, r, Tts::default())
    }

    #[test]
    fn add_ls_show_and_clone_refusal() {
        let (t, st, tt) = st();
        let design = New { kind: Some(Kind::Design), instruct: Some("female, low pitch".into()), ..Default::default() };
        add(&st, "tief", design).unwrap();
        assert!(add(&st, "tief", New { kind: Some(Kind::Design), instruct: Some("x".into()), ..Default::default() }).is_err());
        let r = t.path().join("r.wav");
        std::fs::write(&r, b"abc").unwrap();
        let clone = |who: Option<&str>| New { kind: Some(Kind::Clone), ref_: Some(r.clone()), ref_text: Some("Hallo".into()), consent_who: who.map(String::from), consent_statement: who.map(|_| "ja".into()), ..Default::default() };
        assert!(add(&st, "fremd", clone(None)).is_err());
        assert!(!dir(&st).join("fremd.toml").exists());
        add(&st, "fremd", clone(Some("A B"))).unwrap();
        // design + --ref would be a clone without consent: refused, nothing written
        let e = add(&st, "schein", New { kind: Some(Kind::Design), instruct: Some("female".into()), ref_: Some(r.clone()), ref_text: Some("Hallo".into()), ..Default::default() }).unwrap_err();
        assert!(e.to_string().contains("--ref nicht erlaubt") && !dir(&st).join("schein.toml").exists());
        let l = ls(&st, H);
        let names: Vec<&str> = l.as_array().unwrap().iter().map(|x| x["name"].as_str().unwrap()).collect();
        assert_eq!(names, ["fremd", "tief", "gt", "fable"]); // gt/fable implicit hosts
        let s = show(&st, &tt, "fremd").unwrap();
        assert_eq!(s["spec"]["ref_sha256"].as_str().unwrap().len(), 64);
        assert_eq!(show(&st, &tt, "gt").unwrap()["spec"]["voice"], "gt");
    }

    #[test]
    fn hosts_resolve_and_unknown_name_errors() {
        let (_t, st, _) = st();
        let (o, f) = hosts(&st, H).unwrap();
        assert_eq!((o.voice.as_str(), f.voice.as_str()), ("gt", "fable"));
        assert!(hosts(&st, ("gt", "Nicht Da")).unwrap_err().to_string().contains("Nicht Da"));
    }

    #[test]
    fn test_uses_injected_say() {
        let (_t, st, tt) = st();
        add(&st, "tief", New { kind: Some(Kind::Design), instruct: Some("low".into()), ..Default::default() }).unwrap();
        let mut seen = None;
        let w = test(&st, &tt, "tief", "Hallo", |p, t, v, _| {
            seen = Some((t.to_string(), v.instruct.clone()));
            std::fs::write(p, b"x")?;
            Ok(())
        })
        .unwrap();
        assert!(w.exists() && w.starts_with(st.join("cache/voices")));
        assert_eq!(seen.unwrap(), ("Hallo".into(), "low".into()));
        assert!(test(&st, &tt, "tief", "Hallo", |_, _, _, _| bail!("boom")).is_err());
    }
}
