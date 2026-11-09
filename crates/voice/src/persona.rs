//! Voice personas: `<home>/voices/<name>.toml` -> resolved `VoiceSpec` (the only thing the TTS layer sees).
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    #[default]
    Builtin,
    Design,
    Clone,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Provenance {
    pub source: Option<String>,
    pub method: Option<String>,
    pub sha256: Option<String>,
    /// SPDX id
    pub license: Option<String>,
    pub created: Option<String>,
    /// design: the exact instruct string the voice was generated from
    pub instruct: Option<String>,
    /// design: seed of the kept candidate
    pub seed: Option<u64>,
    /// clone: sha256 of the original recording the clip was cut from
    pub source_sha256: Option<String>,
    /// clone: cut start/end in the original (s)
    pub start: Option<f64>,
    pub end: Option<f64>,
}

/// The only provenance method that makes `kind = "design"` + `ref` valid (written by `stimmwerk voice keep`).
pub const ANCHOR_METHOD: &str = "omnivoice --instruct";
/// SPDX-style license ids written by Stimmwerk.
pub const LICENSE_OWN: &str = "LicenseRef-own";
pub const LICENSE_CONSENT: &str = "LicenseRef-consent";

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Consent {
    pub who: String,
    pub date: String,
    pub statement: String,
    pub recording: Option<String>,
    /// ISO date; set = consent withdrawn, rendering refused
    pub revoked: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Persona {
    pub name: String,
    pub kind: Kind,
    #[serde(default = "de")]
    pub lang: String,
    pub seed: Option<u64>,
    pub voice: Option<String>,
    pub instruct: Option<String>,
    #[serde(rename = "ref")]
    pub ref_: Option<String>,
    pub ref_text: Option<String>,
    pub description: Option<String>,
    pub provenance: Option<Provenance>,
    pub consent: Option<Consent>,
}

static APP: std::sync::OnceLock<&'static str> = std::sync::OnceLock::new();

/// CLI name used in hints (`<app> voice ls`); default `funkhaus`. Set once at program start.
pub fn set_app(name: &'static str) {
    let _ = APP.set(name);
}

fn app() -> &'static str {
    APP.get().copied().unwrap_or("funkhaus")
}

fn de() -> String {
    "de".into()
}

/// What omnivoice needs for one voice. Provenance, consent text and persona name are deliberately absent (not in the cache key).
#[derive(Clone, Debug, PartialEq, Default)]
pub struct VoiceSpec {
    pub kind: Kind,
    pub voice: String,
    pub instruct: String,
    pub ref_wav: Option<PathBuf>,
    pub ref_text: String,
    /// sha256 (hex) of the ref file content
    pub ref_sha: String,
    pub seed: Option<u64>,
    /// ref use was checked when the spec was built: clone = valid, non-revoked consent bound by sha256;
    /// design = anchor written by `voice keep` (method, sha256, clip under `clips/`)
    pub ref_ok: bool,
}

impl VoiceSpec {
    pub fn builtin(voice: &str) -> Self {
        Self { voice: voice.into(), ..Default::default() }
    }

    pub fn label(&self) -> &str {
        match self.kind {
            Kind::Builtin => &self.voice,
            Kind::Design => "design",
            Kind::Clone => "clone",
        }
    }

    /// Re-checked at render time (spec could be built by hand).
    pub fn validate(&self) -> Result<()> {
        match self.kind {
            Kind::Builtin if self.voice.is_empty() => bail!("Stimme ohne Namen"),
            Kind::Design if self.instruct.is_empty() => bail!("Design-Stimme ohne instruct"),
            Kind::Builtin if self.ref_wav.is_some() => bail!("Standard-Stimme mit Referenz-Audio: Rendern verweigert"),
            Kind::Clone if !self.ref_ok => bail!("Clone-Stimme ohne gültige Einwilligung: Rendern verweigert"),
            Kind::Clone if self.ref_wav.is_none() || self.ref_text.is_empty() => bail!("Clone-Stimme braucht ref + ref_text"),
            Kind::Design if self.ref_wav.is_some() && !self.ref_ok => bail!("Design-Stimme mit Referenz-Audio ohne gültigen Anker (nur `stimmwerk voice keep` legt sie an): Rendern verweigert"),
            _ => Ok(()),
        }
    }
}

pub fn sha256_file(p: &Path) -> Result<String> {
    let b = std::fs::read(p).with_context(|| format!("ref-Datei {}", p.display()))?;
    Ok(Sha256::digest(b).iter().map(|x| format!("{x:02x}")).collect())
}

fn date(s: &str, what: &str) -> Result<chrono::NaiveDate> {
    chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").with_context(|| format!("{what}: ISO-Datum JJJJ-MM-TT erwartet, nicht '{s}'"))
}

/// Anchor clip of a design: relative, inside `<dir>/clips/`, a regular file (no symlink, no `..`, no escape via a
/// symlinked `clips/`). Returns the path to render from.
pub fn anchor_path(dir: &Path, r: &str) -> Result<PathBuf> {
    use std::path::Component;
    let rel = Path::new(r);
    let mut c = rel.components();
    let ok = c.next() == Some(Component::Normal("clips".as_ref())) && c.clone().next().is_some() && c.all(|x| matches!(x, Component::Normal(_)));
    if !ok {
        bail!("Anker-ref '{r}' muss relativ unter clips/ liegen (kein absoluter Pfad, kein ..)");
    }
    let p = dir.join(rel);
    let m = std::fs::symlink_metadata(&p).with_context(|| format!("design: ref-Datei fehlt: {}", p.display()))?;
    if !m.is_file() {
        bail!("Anker-ref '{r}' ist keine normale Datei (Symlink?)");
    }
    let (cp, cc) = (std::fs::canonicalize(&p)?, std::fs::canonicalize(dir.join("clips"))?);
    if !cp.starts_with(&cc) || !std::fs::canonicalize(dir).is_ok_and(|d| cc.starts_with(d)) {
        bail!("Anker-ref '{r}' zeigt aus dem Stimmen-Ordner heraus");
    }
    Ok(p)
}

pub fn valid_name(n: &str) -> bool {
    !n.is_empty() && n.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

impl Persona {
    /// Checks the consent block of a clone (also used at `add`).
    pub fn check_consent(&self) -> Result<()> {
        let Some(c) = &self.consent else { bail!("Clone '{}' ohne [consent]: abgelehnt", self.name) };
        if c.who.trim().is_empty() || c.statement.trim().is_empty() || c.date.trim().is_empty() {
            bail!("Clone '{}': [consent] braucht who, date und statement", self.name);
        }
        if date(&c.date, "consent.date")? > chrono::Local::now().date_naive() {
            bail!("consent.date liegt in der Zukunft");
        }
        Ok(())
    }

    /// Validates against the voices dir (for relative `ref`). A `ref` is only accepted for a clone with consent bound by
    /// sha256, or for an anchored design written by `voice keep` (see [`anchor_path`]); everything else is refused.
    pub fn validate(&self, dir: &Path) -> Result<()> {
        if !valid_name(&self.name) {
            bail!("Name '{}' muss [a-z0-9-]+ sein", self.name);
        }
        // revoked first: a tombstone has no audio left, "ref-Datei fehlt" would hide the reason
        if let Some(c) = &self.consent
            && let Some(r) = c.revoked.as_deref().filter(|r| !r.is_empty())
        {
            bail!("Einwilligung für '{}' am {r} widerrufen: Rendern verweigert", self.name);
        }
        let (v, i, r) = (self.voice.as_deref().unwrap_or(""), self.instruct.as_deref().unwrap_or(""), self.ref_.as_deref().unwrap_or(""));
        let sha = self.provenance.as_ref().and_then(|p| p.sha256.as_deref()).filter(|s| !s.is_empty());
        match self.kind {
            Kind::Builtin if v.is_empty() || !i.is_empty() || !r.is_empty() => bail!("builtin: nur voice erlaubt und nötig"),
            Kind::Design if i.trim().is_empty() => bail!("design: instruct nötig"),
            // anchored design: the kept candidate as voice prompt, so every line has the same timbre
            Kind::Design if !r.is_empty() => {
                if self.ref_text.as_deref().unwrap_or("").trim().is_empty() {
                    bail!("design: ref braucht ref_text");
                }
                if self.provenance.as_ref().and_then(|p| p.method.as_deref()) != Some(ANCHOR_METHOD) {
                    bail!("Design '{}' mit ref, aber nicht von `stimmwerk voice keep` angelegt: fremde Aufnahmen nur als Klon mit Einwilligung", self.name);
                }
                let p = anchor_path(dir, r)?;
                let Some(want) = sha else { bail!("Design '{}' mit ref ohne provenance.sha256: Anker nicht gebunden, bitte neu anlegen", self.name) };
                if sha256_file(&p)? != want {
                    bail!("Anker-Audio von '{}' geändert: Rendern verweigert, bitte neu anlegen", self.name);
                }
            }
            Kind::Clone => {
                if r.is_empty() || self.ref_text.as_deref().unwrap_or("").trim().is_empty() {
                    bail!("clone: ref und ref_text nötig");
                }
                self.check_consent()?;
                if !dir.join(r).is_file() {
                    bail!("clone: ref-Datei fehlt: {}", dir.join(r).display());
                }
                let Some(want) = sha else {
                    bail!("Clone '{}' ohne provenance.sha256: Einwilligung nicht an Audio gebunden, bitte neu anlegen", self.name);
                };
                if sha256_file(&dir.join(r))? != want {
                    bail!("Referenz-Audio geändert – Einwilligung gilt nicht, bitte neu anlegen ('{}')", self.name);
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// Validated, resolved spec (ref path absolute, content hashed).
    pub fn spec(&self, dir: &Path) -> Result<VoiceSpec> {
        self.validate(dir)?;
        let mut s = VoiceSpec { kind: self.kind, voice: self.voice.clone().unwrap_or_default(), instruct: self.instruct.clone().unwrap_or_default(), seed: self.seed, ..Default::default() };
        if self.kind == Kind::Clone || (self.kind == Kind::Design && self.ref_.as_deref().is_some_and(|r| !r.is_empty())) {
            let p = dir.join(self.ref_.as_deref().unwrap_or(""));
            s.ref_sha = sha256_file(&p)?;
            s.ref_wav = Some(p);
            s.ref_text = self.ref_text.clone().unwrap_or_default();
            s.ref_ok = true; // validate() above checked consent (clone) or the anchor (design)
        }
        Ok(s)
    }

    pub fn path(dir: &Path, name: &str) -> PathBuf {
        dir.join(format!("{name}.toml"))
    }

    pub fn load(dir: &Path, name: &str) -> Result<Self> {
        if !valid_name(name) {
            bail!("Persona-Name '{name}' muss [a-z0-9-]+ sein");
        }
        let p = Self::path(dir, name);
        let s = std::fs::read_to_string(&p).with_context(|| format!("Persona '{name}' nicht gefunden ({}); `{} voice ls`", p.display(), app()))?;
        let me: Self = toml::from_str(&s).with_context(|| p.display().to_string())?;
        if me.name != name {
            bail!("{}: name '{}' != Dateiname", p.display(), me.name);
        }
        Ok(me)
    }

    pub fn list(dir: &Path) -> Vec<(String, Result<Self>)> {
        let mut v: Vec<_> = std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| e.path().file_stem().filter(|_| e.path().extension().is_some_and(|x| x == "toml")).map(|s| s.to_string_lossy().into_owned()))
            .map(|n| (n.clone(), Self::load(dir, &n)))
            .collect();
        v.sort_by(|a, b| a.0.cmp(&b.0));
        v
    }

    /// Validates first; refuses to overwrite.
    pub fn add(&self, dir: &Path) -> Result<PathBuf> {
        let mut me = self.clone();
        if let Some(r) = me.ref_.as_deref().filter(|r| !r.is_empty()) {
            let pv = me.provenance.get_or_insert_with(Default::default);
            pv.sha256 = Some(sha256_file(&dir.join(r))?);
            pv.created.get_or_insert_with(|| chrono::Local::now().format("%Y-%m-%d").to_string());
            pv.method.get_or_insert_with(|| if me.kind == Kind::Clone { "clone" } else { "design-ref" }.into());
        }
        me.validate(dir)?;
        let p = Self::path(dir, &self.name);
        if p.exists() {
            bail!("Persona '{}' existiert schon", self.name);
        }
        std::fs::create_dir_all(dir)?;
        std::fs::write(&p, toml::to_string_pretty(&me)?)?;
        Ok(p)
    }
}

/// Persona for host/CLI use: file if present, else an implicit builtin omnivoice voice of that name.
pub fn resolve(dir: &Path, name: &str) -> Result<VoiceSpec> {
    if Persona::path(dir, name).exists() {
        return Persona::load(dir, name)?.spec(dir);
    }
    if !valid_name(name) {
        bail!("Persona '{name}' unbekannt; `{} voice ls`", app());
    }
    Ok(VoiceSpec::builtin(name))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(kind: Kind) -> Persona {
        Persona { name: "x".into(), kind, lang: "de".into(), seed: None, voice: None, instruct: None, ref_: None, ref_text: None, description: None, provenance: None, consent: None }
    }
    fn consent() -> Consent {
        Consent { who: "A".into(), date: "2026-10-13".into(), statement: "ok".into(), recording: None, revoked: None }
    }
    fn clone_p(d: &Path) -> Persona {
        if !d.join("r.wav").exists() {
            std::fs::write(d.join("r.wav"), b"abc").unwrap();
        }
        let provenance = Provenance { sha256: Some(sha256_file(&d.join("r.wav")).unwrap()), ..Default::default() };
        Persona { ref_: Some("r.wav".into()), ref_text: Some("Text".into()), consent: Some(consent()), provenance: Some(provenance), ..p(Kind::Clone) }
    }

    #[test]
    fn kinds_validate() {
        let d = tempfile::tempdir().unwrap();
        let d = d.path();
        assert!(p(Kind::Builtin).validate(d).is_err());
        assert!(Persona { voice: Some("gt".into()), ..p(Kind::Builtin) }.validate(d).is_ok());
        assert!(Persona { voice: Some("gt".into()), instruct: Some("a".into()), ..p(Kind::Builtin) }.validate(d).is_err());
        assert!(p(Kind::Design).validate(d).is_err());
        assert!(Persona { instruct: Some("female".into()), ..p(Kind::Design) }.validate(d).is_ok());
        assert!(clone_p(d).validate(d).is_ok());
    }

    #[test]
    fn clone_rules() {
        let d = tempfile::tempdir().unwrap();
        let d = d.path();
        let ok = clone_p(d);
        assert!(Persona { consent: None, ..ok.clone() }.validate(d).is_err());
        assert!(Persona { consent: Some(Consent { statement: " ".into(), ..consent() }), ..ok.clone() }.validate(d).is_err());
        assert!(Persona { consent: Some(Consent { date: "2999-01-01".into(), ..consent() }), ..ok.clone() }.validate(d).is_err());
        assert!(Persona { ref_: Some("nope.wav".into()), ..ok.clone() }.validate(d).is_err());
        assert!(Persona { ref_text: None, ..ok.clone() }.validate(d).is_err());
        let rev = Persona { consent: Some(Consent { revoked: Some("2026-10-14".into()), ..consent() }), ..ok.clone() };
        assert!(rev.validate(d).unwrap_err().to_string().contains("widerrufen"));
        assert!(rev.spec(d).is_err());
    }

    #[test]
    fn consent_bound_to_audio() {
        let d = tempfile::tempdir().unwrap();
        let d = d.path();
        let ok = clone_p(d);
        assert!(ok.spec(d).is_ok());
        assert!(Persona { provenance: None, ..ok.clone() }.spec(d).is_err());
        assert!(Persona { provenance: Some(Provenance::default()), ..ok.clone() }.spec(d).is_err());
        std::fs::write(d.join("r.wav"), b"other voice").unwrap();
        assert!(ok.spec(d).unwrap_err().to_string().contains("Referenz-Audio geändert"));
        // add() records hash/method/date and the result resolves
        let v = d.join("voices");
        let fresh = Persona { provenance: None, ..ok };
        fresh.add(&v).map(|_| ()).unwrap_err(); // relative ref isn't in voices dir
        let abs = Persona { ref_: Some(d.join("r.wav").to_string_lossy().into()), ..fresh };
        abs.add(&v).unwrap();
        let l = Persona::load(&v, "x").unwrap();
        let pv = l.provenance.as_ref().unwrap();
        assert_eq!((pv.sha256.as_ref().unwrap().len(), pv.method.as_deref()), (64, Some("clone")));
        assert!(pv.created.is_some() && resolve(&v, "x").is_ok());
        std::fs::write(d.join("r.wav"), b"swapped").unwrap();
        assert!(resolve(&v, "x").unwrap_err().to_string().contains("Referenz-Audio geändert"));
    }

    #[test]
    fn add_refuses_clone_without_consent_and_writes_nothing() {
        let d = tempfile::tempdir().unwrap();
        let v = d.path().join("voices");
        let bad = Persona { consent: None, ..clone_p(d.path()) };
        let bad = Persona { ref_: Some(d.path().join("r.wav").to_string_lossy().into()), ..bad };
        assert!(bad.add(&v).is_err());
        assert!(!v.exists());
        let good = Persona { consent: Some(consent()), ..bad };
        good.add(&v).unwrap();
        assert!(good.add(&v).is_err());
        assert_eq!(Persona::load(&v, "x").unwrap().kind, Kind::Clone);
        // hand-edit to revoked: resolve refuses
        let t = std::fs::read_to_string(v.join("x.toml")).unwrap().replace("statement = \"ok\"", "statement = \"ok\"\nrevoked = \"2026-10-14\"");
        std::fs::write(v.join("x.toml"), t).unwrap();
        assert!(resolve(&v, "x").is_err());
    }

    #[test]
    fn resolve_fallback_and_hash() {
        let d = tempfile::tempdir().unwrap();
        assert_eq!(resolve(d.path(), "gt").unwrap(), VoiceSpec::builtin("gt"));
        assert!(resolve(d.path(), "Bad Name").is_err());
        let a = clone_p(d.path()).spec(d.path()).unwrap();
        std::fs::write(d.path().join("r.wav"), b"abd").unwrap();
        assert_ne!(a.ref_sha, clone_p(d.path()).spec(d.path()).unwrap().ref_sha);
    }

    /// Anchored design as `voice keep` writes it: clip under clips/, method + sha256 in provenance.
    fn anchored(d: &Path) -> Persona {
        std::fs::create_dir_all(d.join("clips")).unwrap();
        if !d.join("clips/c.wav").exists() {
            std::fs::write(d.join("clips/c.wav"), b"cand").unwrap();
        }
        let provenance = Provenance { method: Some(ANCHOR_METHOD.into()), sha256: Some(sha256_file(&d.join("clips/c.wav")).unwrap()), ..Default::default() };
        Persona { instruct: Some("female, low pitch".into()), seed: Some(2), ref_: Some("clips/c.wav".into()), ref_text: Some("Hallo.".into()), provenance: Some(provenance), ..p(Kind::Design) }
    }

    #[test]
    fn design_anchor_and_app_name() {
        let d = tempfile::tempdir().unwrap();
        let d = d.path();
        let des = anchored(d);
        let s = des.spec(d).unwrap();
        assert_eq!((s.ref_wav.as_deref(), s.ref_sha.len(), s.ref_ok), (Some(d.join("clips/c.wav").as_path()), 64, true));
        s.validate().unwrap();
        let a = crate::tts::say_args("x", Path::new("o.wav"), &s, 1, "de", "");
        assert!(a.contains(&"--ref".to_string()) && !a.contains(&"--instruct".to_string()));
        assert!(Persona { ref_text: None, ..des.clone() }.validate(d).is_err());
        assert!(Persona { ref_: Some("clips/nope.wav".into()), ..des.clone() }.validate(d).is_err());
        let plain = Persona { ref_: None, ref_text: None, ..des }.spec(d).unwrap();
        assert!(crate::tts::say_args("x", Path::new("o.wav"), &plain, 1, "de", "").contains(&"--instruct".to_string()));
        assert!(!Persona::load(d, "nope").unwrap_err().to_string().contains("stimmwerk"));
    }

    /// Every way to smuggle a foreign recording in as a "design" (or past a clone's consent) is refused.
    #[test]
    fn ref_bypasses_refused() {
        let t = tempfile::tempdir().unwrap();
        let d = t.path().join("voices");
        let ok = anchored(&d);
        ok.validate(&d).unwrap();
        let foreign = t.path().join("fremd.wav");
        std::fs::write(&foreign, b"someone else").unwrap();
        let fsha = sha256_file(&foreign).unwrap();
        let with = |r: &str, sha: &str, method: Option<&str>| Persona {
            ref_: Some(r.into()),
            provenance: Some(Provenance { method: method.map(String::from), sha256: Some(sha.into()), ..Default::default() }),
            ..ok.clone()
        };
        let err = |x: &Persona| format!("{:#}", x.validate(&d).unwrap_err());
        // absolute path (even with matching sha and method)
        assert!(err(&with(&foreign.to_string_lossy(), &fsha, Some(ANCHOR_METHOD))).contains("relativ unter clips/"));
        // `..` out of clips/, and a ref outside clips/ inside the voices dir
        std::fs::copy(&foreign, d.join("x.wav")).unwrap();
        assert!(err(&with("clips/../../fremd.wav", &fsha, Some(ANCHOR_METHOD))).contains("relativ unter clips/"));
        assert!(err(&with("x.wav", &fsha, Some(ANCHOR_METHOD))).contains("relativ unter clips/"));
        assert!(err(&with("clips", &fsha, Some(ANCHOR_METHOD))).contains("relativ unter clips/"));
        // sha mismatch / missing sha
        let good = ok.provenance.as_ref().unwrap().sha256.clone().unwrap();
        assert!(err(&with("clips/c.wav", &fsha, Some(ANCHOR_METHOD))).contains("geändert"));
        assert!(err(&Persona { provenance: Some(Provenance { method: Some(ANCHOR_METHOD.into()), ..Default::default() }), ..ok.clone() }).contains("sha256"));
        // design + ref without (or with another) method: what `funkhaus voice add --kind design --ref` used to write
        assert!(err(&with("clips/c.wav", &good, None)).contains("voice keep"));
        assert!(err(&with("clips/c.wav", &good, Some("design-ref"))).contains("voice keep"));
        assert!(err(&Persona { provenance: None, ..ok.clone() }).contains("voice keep"));
        // symlinked clip, and a symlinked clips/ dir pointing outside
        std::os::unix::fs::symlink(&foreign, d.join("clips/link.wav")).unwrap();
        assert!(err(&with("clips/link.wav", &fsha, Some(ANCHOR_METHOD))).contains("Symlink"));
        let d2 = t.path().join("v2");
        std::fs::create_dir_all(&d2).unwrap();
        let outside = t.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::copy(&foreign, outside.join("f.wav")).unwrap();
        std::os::unix::fs::symlink(&outside, d2.join("clips")).unwrap();
        assert!(format!("{:#}", with("clips/f.wav", &fsha, Some(ANCHOR_METHOD)).validate(&d2).unwrap_err()).contains("heraus"));
        // hand-written TOML files: resolve() refuses, never a spec with a ref
        let toml = |n: &str, body: &str| std::fs::write(d.join(format!("{n}.toml")), format!("name = \"{n}\"\n{body}")).unwrap();
        let abs = foreign.to_string_lossy();
        toml("h1", &format!("kind = \"design\"\ninstruct = \"female\"\nref = \"{abs}\"\nref_text = \"Hallo\"\n[provenance]\nmethod = \"{ANCHOR_METHOD}\"\nsha256 = \"{fsha}\"\n"));
        toml("h2", &format!("kind = \"design\"\ninstruct = \"female\"\nref = \"clips/c.wav\"\nref_text = \"Hallo\"\n[provenance]\nsha256 = \"{good}\"\n"));
        toml("h3", "kind = \"design\"\ninstruct = \"female\"\nref = \"clips/../x.wav\"\nref_text = \"Hallo\"\n");
        toml("h4", &format!("kind = \"clone\"\nref = \"{abs}\"\nref_text = \"Hallo\"\n[provenance]\nsha256 = \"{fsha}\"\n"));
        toml("h5", &format!("kind = \"builtin\"\nvoice = \"gt\"\nref = \"{abs}\"\nref_text = \"Hallo\"\n"));
        for n in ["h1", "h2", "h3", "h4", "h5"] {
            assert!(resolve(&d, n).is_err(), "{n}");
        }
        // a hand-built spec with a ref but unchecked is refused at render time too
        let s = VoiceSpec { kind: Kind::Design, instruct: "female".into(), ref_wav: Some(foreign.clone()), ref_text: "Hallo".into(), ..Default::default() };
        assert!(s.validate().unwrap_err().to_string().contains("Anker"));
        assert!(VoiceSpec { kind: Kind::Builtin, voice: "gt".into(), ref_wav: Some(foreign), ..Default::default() }.validate().is_err());
    }

    #[test]
    fn revoked_reported_before_missing_ref() {
        let d = tempfile::tempdir().unwrap();
        let d = d.path();
        let c = clone_p(d);
        let tomb = Persona { ref_: None, ref_text: None, consent: Some(Consent { revoked: Some("2026-10-13".into()), ..consent() }), ..c.clone() };
        assert!(tomb.validate(d).unwrap_err().to_string().contains("widerrufen"));
        std::fs::remove_file(d.join("r.wav")).unwrap();
        let rev = Persona { consent: Some(Consent { revoked: Some("2026-10-13".into()), ..consent() }), ..c };
        assert!(rev.validate(d).unwrap_err().to_string().contains("widerrufen"));
    }

    #[test]
    fn old_provenance_loads_and_new_fields_roundtrip() {
        let old = "name = \"f\"\nkind = \"design\"\ninstruct = \"female\"\n[provenance]\nmethod = \"x\"\nsha256 = \"ab\"\n";
        let x: Persona = toml::from_str(old).unwrap();
        assert_eq!(x.provenance.as_ref().map(|p| (p.instruct.clone(), p.seed)), Some((None, None)));
        assert!(!toml::to_string_pretty(&x).unwrap().contains("seed"));
        let n = Persona { provenance: Some(Provenance { instruct: Some("female".into()), seed: Some(3), ..Default::default() }), ..x };
        let back: Persona = toml::from_str(&toml::to_string_pretty(&n).unwrap()).unwrap();
        assert_eq!(back.provenance.map(|p| (p.instruct, p.seed)), Some((Some("female".into()), Some(3))));
    }

    #[test]
    fn toml_roundtrip_with_provenance() {
        let s = "name = \"f\"\nkind = \"design\"\ninstruct = \"female\"\ndescription = \"d\"\n[provenance]\nsource = \"s\"\nlicense = \"CC0-1.0\"\ncreated = \"2026-10-13\"\n";
        let x: Persona = toml::from_str(s).unwrap();
        assert_eq!((x.lang.as_str(), x.provenance.unwrap().license.as_deref()), ("de", Some("CC0-1.0")));
    }
}
