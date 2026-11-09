//! Voice library: `voice ls|presets|probe|design|keep|clone|revoke|rm`. Personas live in `<home>/voices/<name>.toml`
//! (funkhaus-voice format); design candidates in `voices/candidates/<batch>/` + `candidates.json`; clone and anchor
//! clips in `voices/clips/<name>.wav`. Nothing is written into omnivoice's own voices folder (clones render via `--ref`).
//!
//! Kept designs are always anchored: the chosen candidate WAV becomes the persona's voice prompt (`ref` + `ref_text`),
//! because omnivoice samples a new timbre per call from a bare instruct; the instruct and seed stay in the persona and
//! in `[provenance]`. `revoke` (clones) and `rm` (designs) delete every audio file of the voice (see `derived`).
use crate::render::{self, Event, Prog, Quality, Say, State};
use anyhow::{Context, Result, bail};
use funkhaus_voice::Tts;
use funkhaus_voice::persona::{ANCHOR_METHOD, Consent, LICENSE_CONSENT, LICENSE_OWN, Kind, Persona, Provenance, VoiceSpec, valid_name};
use funkhaus_voice::tts;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

/// Default sample sentence for designs and probes (~4 s: also a good clone reference length).
pub const SAMPLE: &str = "Hallo, so klinge ich. Ich lese dir gern dein Skript vor.";

/// German label -> instruct string; documented OmniVoice vocabulary only, max. one value per category
/// (docs/stimmwerk/instruct-presets.md).
pub const PRESETS: [(&str, &str); 20] = [
    ("Erzähler warm (m)", "male, middle-aged, low pitch"),
    ("Erzählerin warm (w)", "female, middle-aged, moderate pitch"),
    ("Junge Frau energisch", "female, young adult, high pitch"),
    ("Junger Mann dynamisch", "male, young adult, high pitch"),
    ("Geschichtenerzählerin (alt)", "female, elderly, low pitch"),
    ("Geschichtenerzähler Opa", "male, elderly, moderate pitch"),
    ("Radiomoderator", "male, middle-aged, moderate pitch"),
    ("Radiomoderatorin", "female, young adult, moderate pitch"),
    ("Kinderstimme (erst probehören)", "child, high pitch"),
    ("Teenager lebhaft", "teenager, high pitch"),
    ("Bösewicht", "male, middle-aged, very low pitch"),
    ("Flüstern / Geheimnis", "female, young adult, moderate pitch, whisper"),
    ("Nachrichtensprecher", "male, middle-aged, moderate pitch"),
    ("Hörbuch ruhig (m)", "male, young adult, low pitch"),
    ("Meditation sanft", "female, middle-aged, low pitch, whisper"),
    ("Dokumentar ernst", "male, middle-aged, very low pitch"),
    ("Strenge Lehrerin", "female, middle-aged, low pitch"),
    ("Podcast locker (m)", "male, young adult, moderate pitch"),
    ("Werbung Frau hell", "female, young adult, very high pitch"),
    ("Coach motivierend", "male, young adult, very high pitch"),
];

const VOCAB: [(&str, &[&str]); 4] = [
    ("Geschlecht", &["male", "female"]),
    ("Alter", &["child", "teenager", "young adult", "middle-aged", "elderly"]),
    ("Tonhöhe", &["very low pitch", "low pitch", "moderate pitch", "high pitch", "very high pitch"]),
    ("Stil", &["whisper"]),
];

/// Hints for a free instruct (never blocking): unknown words, two values of one category, accents (no effect on German).
pub fn check_instruct(s: &str) -> Vec<String> {
    let mut w = vec![];
    let mut seen: Vec<&str> = vec![];
    for a in s.split([',', '，']).map(|a| a.trim().to_lowercase()).filter(|a| !a.is_empty()) {
        match VOCAB.iter().find(|(_, v)| v.contains(&a.as_str())) {
            Some((cat, _)) if seen.contains(cat) => w.push(format!("„{a}“: zweiter Wert für {cat} – Verhalten undokumentiert")),
            Some((cat, _)) => seen.push(cat),
            None if a.ends_with(" accent") => w.push(format!("„{a}“ wirkt nur bei englischem Text")),
            None => w.push(format!("„{a}“ ist kein dokumentiertes Attribut (wird vermutlich ignoriert)")),
        }
    }
    w
}

/// Own persona folder `<home>/voices`.
pub fn dir() -> PathBuf {
    funkhaus_voice::voices::dir(&crate::home())
}

pub fn candidates_root(vdir: &Path) -> PathBuf {
    vdir.join("candidates")
}

pub fn today() -> String {
    chrono::Local::now().format("%Y-%m-%d").to_string()
}

fn kind_s(k: Kind) -> &'static str {
    match k {
        Kind::Builtin => "builtin",
        Kind::Design => "design",
        Kind::Clone => "clone",
    }
}

/// One persona file for lists (CLI `voice ls`, GUI library).
#[derive(Serialize, Clone, Debug)]
pub struct Entry {
    pub name: String,
    /// builtin|design|clone|error
    pub kind: String,
    /// `stimmwerk` (own folder) or `funkhaus`
    pub source: String,
    pub file: PathBuf,
    pub lang: Option<String>,
    pub description: Option<String>,
    pub instruct: Option<String>,
    pub seed: Option<u64>,
    /// design with candidate WAV as voice prompt
    pub anchored: bool,
    pub provenance: Option<Provenance>,
    pub consent_who: Option<String>,
    pub consent_date: Option<String>,
    pub revoked: Option<String>,
    /// renders (validation incl. sha256 binding and consent ok)
    pub valid: bool,
    pub error: Option<String>,
}

/// Persona files across `dirs` (first dir wins for a name), sorted by name.
pub fn ls(dirs: &[PathBuf]) -> Vec<Entry> {
    let mut out: Vec<Entry> = vec![];
    for (k, d) in dirs.iter().enumerate() {
        let source = if k == 0 { "stimmwerk" } else { "funkhaus" };
        for (name, r) in Persona::list(d) {
            if out.iter().any(|e| e.name == name) {
                continue;
            }
            let file = Persona::path(d, &name);
            out.push(match r {
                Ok(p) => {
                    let err = p.validate(d).err().map(|e| format!("{e:#}"));
                    let c = p.consent.as_ref();
                    Entry {
                        kind: kind_s(p.kind).into(),
                        source: source.into(),
                        file,
                        lang: Some(p.lang.clone()),
                        description: p.description.clone(),
                        instruct: p.instruct.clone(),
                        seed: p.seed,
                        anchored: p.kind == Kind::Design && p.ref_.as_deref().is_some_and(|r| !r.is_empty()),
                        provenance: p.provenance.clone(),
                        consent_who: c.map(|c| c.who.clone()),
                        consent_date: c.map(|c| c.date.clone()),
                        revoked: c.and_then(|c| c.revoked.clone()).filter(|r| !r.is_empty()),
                        valid: err.is_none(),
                        error: err,
                        name,
                    }
                }
                Err(e) => Entry {
                    name,
                    kind: "error".into(),
                    source: source.into(),
                    file,
                    lang: None,
                    description: None,
                    instruct: None,
                    seed: None,
                    anchored: false,
                    provenance: None,
                    consent_who: None,
                    consent_date: None,
                    revoked: None,
                    valid: false,
                    error: Some(format!("{e:#}")),
                },
            });
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

#[derive(Serialize, Clone, Debug)]
pub struct SpecOut {
    pub kind: String,
    pub voice: String,
    pub instruct: String,
    pub ref_sha256: Option<String>,
    pub seed: Option<u64>,
}

#[derive(Serialize, Clone, Debug)]
pub struct ProbeOut {
    pub persona: String,
    pub wav: PathBuf,
    pub dur: f64,
    pub cache_key: String,
    pub cache_hit: bool,
    pub spec: SpecOut,
}

/// Voice sample of persona `name` (draft, cached in `<home>/cache/probe`); `out`: also copied to `out/<name>.wav`.
pub fn probe(dirs: &[PathBuf], name: &str, text: &str, lang: &str, out: Option<&Path>, cancel: &AtomicBool, prog: &Prog) -> Result<ProbeOut> {
    let t = Tts { cmd: render::tts_cmd(), lang: lang.into(), quality: "draft".into(), ..Tts::default() };
    render::with_say(t, cancel, prog, |say| probe_with(&crate::speakers::probe_dir(), dirs, name, text, lang, out, say))
}

pub fn probe_with(cache: &Path, dirs: &[PathBuf], name: &str, text: &str, lang: &str, out: Option<&Path>, say: &mut Say) -> Result<ProbeOut> {
    let v = crate::speakers::spec(dirs, name).map_err(|e| anyhow::anyhow!("Persona {name}: {e:#}"))?;
    let norm = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let t = Tts { lang: lang.into(), quality: "draft".into(), ..Tts::default() };
    let key = format!("{:016x}", tts::key(&t, &v, lang, &norm));
    let hit = cache.join(format!("{key}.wav")).is_file();
    let wav = crate::speakers::probe_with(cache, dirs, name, text, lang, say)?;
    let dur = dur(&wav)?;
    let wav = match out {
        Some(o) => {
            std::fs::create_dir_all(o)?;
            let dst = o.join(format!("{name}.wav"));
            std::fs::copy(&wav, &dst)?;
            if v.ref_wav.is_some() {
                crate::speakers::record_checked(cache, dirs, name, &v.ref_sha, std::slice::from_ref(&dst))?;
            }
            dst
        }
        None => wav,
    };
    let spec = SpecOut { kind: kind_s(v.kind).into(), voice: v.voice.clone(), instruct: v.instruct.clone(), ref_sha256: Some(v.ref_sha.clone()).filter(|s| !s.is_empty()), seed: v.seed };
    Ok(ProbeOut { persona: name.into(), wav, dur, cache_key: key, cache_hit: hit, spec })
}

fn dur(p: &Path) -> Result<f64> {
    let (x, sr) = funkhaus_audio::mix::read(p)?;
    if x.is_empty() {
        bail!("{}: leere Aufnahme", p.display());
    }
    Ok(render::r2(x.len() as f64 / sr as f64))
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Candidate {
    /// 1-based
    pub id: usize,
    pub seed: u64,
    pub wav: PathBuf,
    pub dur: f64,
}

/// `candidates.json` and the `voice design` result.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Batch {
    pub batch: String,
    pub instruct: String,
    pub text: String,
    pub lang: String,
    pub quality: String,
    pub created: String,
    #[serde(default)]
    pub warnings: Vec<String>,
    pub candidates: Vec<Candidate>,
}

#[derive(Clone, Debug)]
pub struct Design {
    pub instruct: String,
    pub n: usize,
    /// explicit seeds (len == n); default 1..=n
    pub seeds: Option<Vec<u64>>,
    pub text: String,
    pub lang: String,
    pub quality: Quality,
}

pub const MAX_N: usize = 8;

/// `n` candidates of one instruct (one seed each) into `<vdir>/candidates/<batch>/`; no persona yet.
pub fn design(vdir: &Path, d: &Design, cancel: &AtomicBool, prog: &Prog) -> Result<Batch> {
    let t = Tts { cmd: render::tts_cmd(), lang: d.lang.clone(), quality: d.quality.as_str().into(), ..Tts::default() };
    render::with_say(t, cancel, prog, |say| design_with(vdir, d, cancel, prog, say))
}

pub fn design_with(vdir: &Path, d: &Design, cancel: &AtomicBool, prog: &Prog, say: &mut Say) -> Result<Batch> {
    let instruct = d.instruct.trim();
    if instruct.is_empty() {
        bail!("Beschreibung (instruct) ist leer – z. B. \"female, middle-aged, low pitch\" oder `stimmwerk voice presets`");
    }
    if !(1..=MAX_N).contains(&d.n) {
        bail!("--n muss 1 bis {MAX_N} sein");
    }
    let seeds = d.seeds.clone().unwrap_or_else(|| (1..=d.n as u64).collect());
    if seeds.len() != d.n {
        bail!("{} Seeds für --n {}", seeds.len(), d.n);
    }
    if (1..seeds.len()).any(|i| seeds[..i].contains(&seeds[i])) {
        bail!("Seeds doppelt: {seeds:?}");
    }
    let text = d.text.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.is_empty() {
        bail!("Probetext ist leer");
    }
    let root = candidates_root(vdir);
    std::fs::create_dir_all(&root)?;
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
    let (batch, bdir) = (1..)
        .map(|k| if k == 1 { stamp.clone() } else { format!("{stamp}-{k}") })
        .find_map(|b| std::fs::create_dir(root.join(&b)).ok().map(|_| (b.clone(), root.join(b))))
        .context("Kandidaten-Ordner")?;
    let mut run = || -> Result<Vec<Candidate>> {
        prog(Event::Stage { name: "design".into() });
        let mut out = vec![];
        for (k, &seed) in seeds.iter().enumerate() {
            if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                bail!("{}", tts::CANCELLED);
            }
            let (i, n) = (k + 1, seeds.len());
            prog(Event::Line { i, n, state: State::Rendering, tts_secs: None });
            let t0 = std::time::Instant::now();
            let v = VoiceSpec { kind: Kind::Design, instruct: instruct.into(), seed: Some(seed), ..Default::default() };
            let wav = bdir.join(format!("{i}.wav"));
            render::atomic(&wav, |tmp| say(tmp, &text, &v, seed)).with_context(|| format!("Kandidat {i}"))?;
            let dur = dur(&wav).with_context(|| format!("Kandidat {i}"))?;
            prog(Event::Line { i, n, state: State::Done, tts_secs: Some(render::r2(t0.elapsed().as_secs_f64())) });
            out.push(Candidate { id: i, seed, wav, dur });
        }
        Ok(out)
    };
    let candidates = match run() {
        Ok(c) => c,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&bdir);
            return Err(render::cancelled(e, cancel));
        }
    };
    let b = Batch { batch, instruct: instruct.into(), text, lang: d.lang.clone(), quality: d.quality.as_str().into(), created: today(), warnings: check_instruct(instruct), candidates };
    render::atomic(&bdir.join("candidates.json"), |t| Ok(std::fs::write(t, serde_json::to_string_pretty(&b)?)?))?;
    prog(Event::Done);
    Ok(b)
}

/// An anchored design whose provenance.source names a design candidate (`stimmwerk voice design <batch>/<id>`) must
/// match that candidate's WAV while the batch still exists. Hand-written anchors without such a source pass (local-user
/// guardrail: whoever can write the voices folder can run `voice keep` as well); the sha256 binding still applies.
pub fn check_anchor_source(vdir: &Path, name: &str) -> Result<()> {
    let p = Persona::load(vdir, name)?;
    let pv = p.provenance.unwrap_or_default();
    let Some((batch, id)) = pv.source.as_deref().and_then(|s| s.strip_prefix("stimmwerk voice design ")).and_then(|s| s.rsplit_once('/')) else { return Ok(()) };
    let Ok(b) = load_batch(vdir, batch) else { return Ok(()) }; // candidates cleaned up: nothing to compare
    let Some(c) = id.parse::<usize>().ok().and_then(|i| b.candidates.into_iter().find(|c| c.id == i)).filter(|c| c.wav.is_file()) else { return Ok(()) };
    if pv.sha256.as_deref() != Some(funkhaus_voice::persona::sha256_file(&c.wav)?.as_str()) {
        bail!("Anker von '{name}' passt nicht zum Entwurf {batch}/{id}: Rendern verweigert, bitte mit `stimmwerk voice keep` neu anlegen");
    }
    Ok(())
}

pub fn load_batch(vdir: &Path, batch: &str) -> Result<Batch> {
    if batch.is_empty() || batch.contains(['/', '\\']) || batch.starts_with('.') {
        bail!("Batch-Name {batch:?} ungültig");
    }
    let p = candidates_root(vdir).join(batch).join("candidates.json");
    let s = std::fs::read_to_string(&p).with_context(|| format!("Entwurf {batch} nicht gefunden ({})", p.display()))?;
    serde_json::from_str(&s).with_context(|| p.display().to_string())
}

/// omnivoice's builtin voices as of 2026-10 (checked even when `voices list` is unavailable).
pub const KNOWN_BUILTINS: &[&str] = &["fable", "gerstaecker", "gt", "kn"];

/// Name free in every persona dir and in `<vdir>/clips/`, and no omnivoice builtin (`bi` + `KNOWN_BUILTINS`): a persona
/// file would silently shadow that voice everywhere it is used.
fn free_name(vdir: &Path, dirs: &[PathBuf], bi: Option<&[String]>, name: &str) -> Result<PathBuf> {
    if !valid_name(name) {
        bail!("Name '{name}' ungültig: nur a bis z, 0 bis 9 und Bindestrich");
    }
    if KNOWN_BUILTINS.contains(&name) || bi.is_some_and(|b| b.iter().any(|x| x == name)) {
        bail!("Name '{name}' ist schon eine omnivoice-Standard-Stimme – bitte einen anderen Namen wählen");
    }
    if let Some(d) = std::iter::once(vdir).chain(dirs.iter().map(PathBuf::as_path)).find(|d| Persona::path(d, name).exists()) {
        bail!("Persona '{name}' existiert schon ({})", Persona::path(d, name).display());
    }
    let clip = vdir.join("clips").join(format!("{name}.wav"));
    if clip.exists() {
        bail!("{} existiert schon", clip.display());
    }
    Ok(clip)
}

#[derive(Serialize, Clone, Debug)]
pub struct Saved {
    pub persona: String,
    pub path: PathBuf,
    pub kind: String,
    /// anchor / clone reference clip
    #[serde(skip_serializing_if = "Option::is_none")]
    pub clip: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ref_sha256: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seed: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instruct: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub consent_date: Option<String>,
    pub provenance: Option<Provenance>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

/// Writes the persona; on failure the copied clip goes too (nothing half-written).
fn add(vdir: &Path, p: Persona, clip: Option<&Path>) -> Result<Saved> {
    let r = p.add(vdir).and_then(|path| Ok((path.clone(), Persona::load(vdir, &p.name)?)));
    let (path, p) = r.inspect_err(|_| {
        if let Some(c) = clip {
            let _ = std::fs::remove_file(c);
        }
    })?;
    let c = p.consent.as_ref();
    Ok(Saved {
        persona: p.name.clone(),
        path,
        kind: kind_s(p.kind).into(),
        clip: clip.map(Path::to_path_buf),
        ref_sha256: p.provenance.as_ref().and_then(|v| v.sha256.clone()),
        seed: p.seed,
        instruct: p.instruct.clone(),
        consent_date: c.map(|c| c.date.clone()),
        provenance: p.provenance.clone(),
        warnings: vec![],
    })
}

/// Candidate `id` of `batch` -> design persona `name` (instruct, seed), anchored on the candidate WAV (copied to
/// `clips/<name>.wav`, bound by sha256). A bare instruct would sample a new timbre per call, so there is no unanchored keep.
/// `bi`: omnivoice builtins (a persona must not shadow one).
pub fn keep(vdir: &Path, dirs: &[PathBuf], bi: Option<&[String]>, batch: &str, id: usize, name: &str, description: Option<&str>) -> Result<Saved> {
    let b = load_batch(vdir, batch)?;
    let c = b.candidates.iter().find(|c| c.id == id).with_context(|| format!("Kandidat {id} gibt es in {batch} nicht (1..={})", b.candidates.len()))?;
    let clip = free_name(vdir, dirs, bi, name)?;
    std::fs::create_dir_all(clip.parent().unwrap())?;
    render::atomic(&clip, |t| std::fs::copy(&c.wav, t).map(drop).with_context(|| c.wav.display().to_string()))?;
    let p = Persona {
        name: name.into(),
        kind: Kind::Design,
        lang: b.lang.clone(),
        seed: Some(c.seed),
        voice: None,
        instruct: Some(b.instruct.clone()),
        ref_: Some(format!("clips/{name}.wav")),
        ref_text: Some(b.text.clone()),
        description: description.map(String::from).filter(|d| !d.trim().is_empty()),
        provenance: Some(Provenance {
            source: Some(format!("stimmwerk voice design {batch}/{id}")),
            method: Some(ANCHOR_METHOD.into()),
            license: Some(LICENSE_OWN.into()),
            created: Some(today()),
            instruct: Some(b.instruct.clone()),
            seed: Some(c.seed),
            ..Default::default()
        }),
        consent: None,
    };
    add(vdir, p, Some(&clip))
}

#[derive(Clone, Debug, Default)]
pub struct CloneReq {
    pub wav: PathBuf,
    pub name: String,
    pub ref_text: String,
    pub start: Option<f64>,
    pub end: Option<f64>,
    /// who/when/how the person agreed (required)
    pub consent: String,
    /// name of the person whose voice it is (required)
    pub who: String,
    /// ISO date, default today
    pub date: Option<String>,
    pub lang: String,
}

pub const NO_CONSENT: &str = "Einwilligung fehlt: Klonen nur mit --consent \"wer, wann, wie zugestimmt\" – nichts geschrieben";
pub const NO_WHO: &str = "Name der Person fehlt: Klonen nur mit --who \"Name\" – nichts geschrieben";

/// Copies the (trimmed) reference to `<vdir>/clips/<name>.wav`, binds it by sha256 and writes a clone persona with consent.
/// `bi`: omnivoice builtins (a persona must not shadow one).
pub fn clone(vdir: &Path, dirs: &[PathBuf], bi: Option<&[String]>, c: &CloneReq) -> Result<Saved> {
    if c.consent.trim().is_empty() {
        bail!("{NO_CONSENT}");
    }
    if c.who.trim().is_empty() {
        bail!("{NO_WHO}");
    }
    if let Some(d) = &c.date {
        check_date(d).context("Datum der Einwilligung")?;
    }
    let clip = free_name(vdir, dirs, bi, &c.name)?;
    let ref_text = c.ref_text.split_whitespace().collect::<Vec<_>>().join(" ");
    if ref_text.is_empty() {
        bail!("--ref-text fehlt: genau das, was in der Aufnahme gesagt wird");
    }
    let (x, sr) = funkhaus_audio::mix::read(&c.wav).with_context(|| format!("{} ist keine lesbare WAV-Datei", c.wav.display()))?;
    let total = x.len() as f64 / sr as f64;
    let (s, e) = (c.start.unwrap_or(0.0), c.end.unwrap_or(total).min(total));
    if s < 0.0 || e <= s {
        bail!("Ausschnitt {s}–{e} s ungültig (Aufnahme {total:.2} s)");
    }
    let dur = render::r2(e - s);
    if dur < 1.0 {
        bail!("Ausschnitt {dur} s zu kurz (3 bis 10 s empfohlen)");
    }
    let mut warnings = vec![];
    if !(3.0..=10.0).contains(&dur) {
        warnings.push(format!("Ausschnitt {dur} s: 3 bis 10 s klingen am besten"));
    }
    std::fs::create_dir_all(clip.parent().unwrap())?;
    if c.start.is_none() && c.end.is_none() {
        render::atomic(&clip, |t| std::fs::copy(&c.wav, t).map(drop).with_context(|| c.wav.display().to_string()))?;
    } else {
        let (a, b) = ((s * sr as f64) as usize, ((e * sr as f64) as usize).min(x.len()));
        render::atomic(&clip, |t| funkhaus_audio::mix::write(t, &x[a..b], sr))?;
    }
    let consent = c.consent.trim().to_string();
    let source_sha256 = Some(funkhaus_voice::persona::sha256_file(&c.wav)?);
    let p = Persona {
        name: c.name.clone(),
        kind: Kind::Clone,
        lang: c.lang.clone(),
        seed: None,
        voice: None,
        instruct: None,
        ref_: Some(format!("clips/{}.wav", c.name)),
        ref_text: Some(ref_text),
        description: None,
        provenance: Some(Provenance {
            source: c.wav.file_name().map(|n| n.to_string_lossy().into_owned()),
            method: Some("clone".into()),
            license: Some(LICENSE_CONSENT.into()),
            created: Some(today()),
            source_sha256,
            start: c.start,
            end: c.end,
            ..Default::default()
        }),
        consent: Some(Consent {
            who: c.who.trim().into(),
            date: c.date.as_deref().map(str::trim).map_or_else(today, String::from),
            statement: consent,
            recording: None,
            revoked: None,
        }),
    };
    let mut r = add(vdir, p, Some(&clip))?;
    r.warnings = warnings;
    Ok(r)
}

#[derive(Serialize, Clone, Debug)]
pub struct Revoked {
    pub persona: String,
    pub revoked: String,
    pub file: PathBuf,
    /// was already revoked: date unchanged (audio is purged again anyway)
    pub already: bool,
    /// every audio file deleted (clip, takes, probes, exports, omnivoice prompt cache)
    pub deleted: Vec<PathBuf>,
    /// ref outside the voices folder (e.g. a funkhaus persona's original recording): not deleted
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kept_external: Option<PathBuf>,
    /// what could not be checked or deleted (unreadable project, uncomputable takes, refused index entries)
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub skipped: Vec<String>,
    /// the tombstone still holds the person's transcript (`ref_text`): take keys contain it, so a later
    /// `revoke --project` can find takes of other projects. `--forget` drops it.
    pub ref_text_kept: bool,
}

/// Note for `Revoked.ref_text_kept` (CLI + GUI).
pub const REF_TEXT_NOTE: &str = "Das Transkript der Aufnahme bleibt im Vermerk gespeichert, damit spätere Bereinigungen (weitere Projekte) Takes der Stimme noch finden; `stimmwerk voice revoke NAME --forget` löscht es.";

/// Where audio of a voice can live: the data root (probe cache, `projects/`, `derived/` for exports of trashed
/// projects), extra project dirs (the open/given one, if elsewhere) and omnivoice prompt caches. The Trash itself is
/// never scanned (`project::TRASH_NOTE`). `wait`: total time purge waits for projects that are rendering.
#[derive(Clone, Debug)]
pub struct Places {
    pub home: PathBuf,
    pub projects: Vec<PathBuf>,
    pub wait: std::time::Duration,
    /// omnivoice `--ref` prompt caches to scan (Stimmwerk's, funkhaus', omnivoice's own legacy folder)
    pub caches: Vec<PathBuf>,
    /// called once per project before waiting for its lock (CLI: "wartet auf …")
    pub on_wait: Option<fn(&Path)>,
}

/// Default total wait for busy projects' locks during `revoke|rm`.
pub const PURGE_WAIT: std::time::Duration = std::time::Duration::from_secs(30);

/// omnivoice's own default prompt cache (`$OMNIVOICE_VOICES/.cache`, `$OMNIVOICE_HOME/voices/.cache`, else
/// ~/Projects/omnivoice/voices/.cache): calls without `OMNIVOICE_CACHE` (older funkhaus/Stimmwerk, an old server) wrote there.
pub fn legacy_omnivoice_cache() -> PathBuf {
    let env = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from);
    env("OMNIVOICE_VOICES").or_else(|| env("OMNIVOICE_HOME").map(|h| h.join("voices"))).unwrap_or_else(|| funkhaus_sys::home().join("Projects/omnivoice/voices")).join(".cache")
}

impl Places {
    pub fn new(extra: &[PathBuf]) -> Self {
        let home = crate::home();
        let caches = vec![home.join("cache").join("omnivoice"), funkhaus_sys::funkhaus_omnivoice_cache(), legacy_omnivoice_cache()];
        Places { home, projects: extra.to_vec(), wait: PURGE_WAIT, caches, on_wait: None }
    }

    /// Data root `home` only (own prompt cache, no extra projects) – tests and tools.
    pub fn at(home: &Path) -> Self {
        Places { home: home.to_path_buf(), projects: vec![], wait: PURGE_WAIT, caches: vec![home.join("cache").join("omnivoice")], on_wait: None }
    }

    /// Project folders: every dir under `projects/` with a project.toml, plus the extra ones.
    fn project_dirs(&self) -> Vec<PathBuf> {
        let mut v: Vec<PathBuf> = std::fs::read_dir(self.home.join("projects")).into_iter().flatten().flatten().map(|e| e.path()).filter(|p| p.join(crate::project::FILE).is_file()).collect();
        v.sort();
        for p in &self.projects {
            if p.join(crate::project::FILE).is_file() && !v.iter().any(|x| same(x, p)) {
                v.push(p.clone());
            }
        }
        v
    }
}

fn same(a: &Path, b: &Path) -> bool {
    std::fs::canonicalize(a).ok().zip(std::fs::canonicalize(b).ok()).is_some_and(|(a, b)| a == b)
}

#[derive(Default)]
struct Purged {
    deleted: Vec<PathBuf>,
    external: Option<PathBuf>,
    skipped: Vec<String>,
}

impl Purged {
    /// Deletes `f` if it is a plain file with one of `exts` (`derived::check`); a refused file lands in `skipped`.
    fn remove(&mut self, f: &Path, exts: &[&str]) {
        match crate::derived::remove(f, exts) {
            Ok(true) => self.deleted.push(f.to_path_buf()),
            Ok(false) => {}
            Err(why) => self.skipped.push(format!("{} nicht gelöscht: {why}", f.display())),
        }
    }
}

/// Deletes all audio of persona `p` (file in `d`): its clip under `d/clips/`, kept design candidates with the same
/// clip, indexed derived audio (probe cache, takes, mixes, exports; `derived`), takes of current scripts whose keys
/// belong to it (also those rendered before the index existed), and omnivoice prompt cache entries. Every project is
/// locked first (all of them within `pl.wait` in total), so no job writes takes of the voice meanwhile; a project still
/// busy after that fails the whole purge before anything is deleted. The probe cache index stays locked until `finish`
/// (tombstone / persona file removal) ran, so a probe running meanwhile sees the result and drops its sample. What
/// could not be checked or deleted is reported in `skipped`, never silently ignored.
fn purge(pl: &Places, d: &Path, p: &Persona, finish: impl FnOnce(&mut Purged) -> Result<()>) -> Result<Purged> {
    let pds = pl.project_dirs();
    let until = std::time::Instant::now() + pl.wait;
    let _locks = pds
        .iter()
        .map(|pd| {
            let note = pl.on_wait.map(|f| move || f(pd));
            render::lock_dir(pd, until.saturating_duration_since(std::time::Instant::now()), "purge", note.as_ref().map(|n| n as &dyn Fn()))
                .with_context(|| format!("Projekt {} ist beschäftigt – nichts gelöscht; bitte erneut ausführen, wenn das Rendern fertig ist", pd.display()))
        })
        .collect::<Result<Vec<_>>>()?;
    let probe = crate::derived::Lock::take(&pl.home.join("cache").join("probe"))?;
    let mut out = Purged::default();
    let r = p.ref_.as_deref().filter(|r| !r.is_empty());
    let clip = r.map(|r| d.join(r));
    let own = r.is_some_and(|r| funkhaus_voice::persona::anchor_path(d, r).is_ok());
    let mut shas: Vec<String> = p.provenance.as_ref().and_then(|v| v.sha256.clone()).into_iter().collect();
    if let Some(c) = clip.as_ref().filter(|c| c.is_file())
        && let Ok(h) = funkhaus_voice::persona::sha256_file(c)
        && !shas.contains(&h)
    {
        shas.push(h);
    }
    // omnivoice prompt caches: exact key (needs the clip's mtime, so before deleting it) + entries with the same ref_text
    let rt = p.ref_text.clone().unwrap_or_default();
    let key = clip.as_ref().and_then(|c| crate::derived::omnivoice_key(c, &rt));
    let mut npz = vec![];
    for oc in &pl.caches {
        npz.extend(key.as_ref().map(|k| oc.join(format!("{k}.npz"))));
        if !rt.trim().is_empty() {
            npz.extend(crate::derived::npz_with_text(oc, &rt));
        } else if key.is_none() && std::fs::read_dir(oc).into_iter().flatten().flatten().any(|e| e.path().extension().is_some_and(|x| x == "npz")) {
            out.skipped.push(format!("omnivoice-Cache {}: Einträge von „{}“ nicht erkennbar (Aufnahme und Transkript fehlen) – bitte von Hand prüfen", oc.display(), p.name));
        }
    }
    npz.sort();
    npz.dedup();
    for f in npz {
        out.remove(&f, &["npz"]);
    }
    let indexed = |dir: &Path, lk: Option<&crate::derived::Lock>, out: &mut Purged| -> Result<()> {
        let (gone, refused) = match lk {
            Some(l) => l.purge(&shas, &p.name)?,
            None => crate::derived::purge(dir, &shas, &p.name)?,
        };
        out.deleted.extend(gone);
        out.skipped.extend(refused.iter().map(|(f, why)| format!("{} nicht gelöscht: {why}", f.display())));
        Ok(())
    };
    indexed(&pl.home.join("cache").join("probe"), Some(&probe), &mut out)?;
    // exports of trashed projects that could not travel with the folder (`project trash`, other volume)
    indexed(&crate::derived::home_index(&pl.home), None, &mut out)?;
    for pd in &pds {
        indexed(&pd.join("takes"), None, &mut out)?;
        match crate::Project::load(pd) {
            Ok(pr) => purge_script_takes(&pr, d, p, &shas, &mut out),
            Err(e) => out.skipped.push(format!("Projekt {}: nicht lesbar ({e:#}) – nur bekannte Aufnahmen gelöscht, Takes nicht geprüft", pd.display())),
        }
    }
    if d.parent().is_some_and(|h| same(h, &funkhaus_sys::funkhaus_home())) {
        out.skipped.push(format!("funkhaus-Podcasts (MP3s in den Notebooks unter {}) mit „{}“ werden nicht gelöscht – bitte dort selbst entfernen", funkhaus_sys::funkhaus_home().display(), p.name));
    }
    // `funkhaus voice test` samples of this persona: `<root>/cache/voices/<name>-<16 hex>.wav`
    for e in std::fs::read_dir(d.parent().unwrap_or(d).join("cache").join("voices")).into_iter().flatten().flatten() {
        let n = e.file_name().to_string_lossy().into_owned();
        if n.strip_prefix(&format!("{}-", p.name)).and_then(|r| r.strip_suffix(".wav")).is_some_and(|h| h.len() == 16 && h.bytes().all(|b| b.is_ascii_hexdigit())) {
            out.remove(&e.path(), &["wav"]);
        }
    }
    // design candidates with the same clip as the anchor (the kept one and copies)
    for b in std::fs::read_dir(candidates_root(d)).into_iter().flatten().flatten() {
        for w in std::fs::read_dir(b.path()).into_iter().flatten().flatten().map(|e| e.path()).filter(|w| w.extension().is_some_and(|x| x == "wav")) {
            if funkhaus_voice::persona::sha256_file(&w).is_ok_and(|h| shas.contains(&h)) {
                out.remove(&w, &["wav"]);
            }
        }
    }
    if let Some(c) = clip {
        if own {
            out.remove(&c, &["wav"]);
        } else if c.exists() {
            out.external = Some(c);
        }
    }
    finish(&mut out)?;
    drop(probe);
    Ok(out)
}

/// Takes (all qualities, all reroll alternates) of project lines spoken by persona `p`, keys computed as `render` does
/// from provenance.sha256 + ref_text (both kept in the tombstone; the clip is not needed, so this also works after the
/// clip was deleted, and again for another project after the revoke). Lines whose keys cannot be computed land in
/// `skipped`. Mixes are deleted via the index only (stamp-checked); an unindexed mix next to such takes is reported.
fn purge_script_takes(pr: &crate::Project, d: &Path, p: &Persona, shas: &[String], out: &mut Purged) {
    let ids: Vec<&String> = pr.meta.speakers.iter().filter(|(_, s)| s.persona.as_deref() == Some(p.name.as_str())).map(|(id, _)| id).collect();
    if ids.is_empty() {
        return;
    }
    let rt = p.ref_text.clone().unwrap_or_default();
    let (Some(sha), false) = (shas.first(), rt.trim().is_empty()) else {
        out.skipped.push(format!("Projekt {}: Takes von '{}' nicht berechenbar (sha256 oder ref_text fehlt)", pr.dir.display(), p.name));
        return;
    };
    let v = VoiceSpec { kind: p.kind, voice: p.voice.clone().unwrap_or_default(), instruct: p.instruct.clone().unwrap_or_default(), ref_wav: Some(d.join(p.ref_.as_deref().unwrap_or(""))), ref_text: rt, ref_sha: sha.clone(), seed: p.seed, ref_ok: true };
    let voices: tts::Voices = ids.iter().map(|id| ((*id).clone(), v.clone())).collect();
    let lines: Vec<funkhaus_audio::script::Line> = pr.script.lines().cloned().collect();
    let (t, lex) = (render::key_cfg(pr), funkhaus_voice::lexicon::Lexicon::load(&pr.lexicon_path()));
    let root = render::takes_root(pr);
    let rf = funkhaus_voice::reroll::File::load(&root).unwrap_or_default();
    let n0 = out.deleted.len();
    for (i, l) in lines.iter().enumerate() {
        if !ids.contains(&&l.who) {
            continue;
        }
        let keys = match tts::line_keys(&t, &lines, &voices, &lex, &t.lang, i) {
            Ok(k) => k,
            Err(e) => {
                out.skipped.push(format!("Projekt {} Zeile {}: Takes nicht berechenbar ({e:#})", pr.dir.display(), i + 1));
                continue;
            }
        };
        for b in keys {
            let salts = rf.takes.get(&format!("{b:016x}")).map(|x| x.salts.clone()).unwrap_or_default();
            for h in std::iter::once(b).chain(salts.into_iter().flatten().map(|s| tts::salted(b, s))) {
                for q in [Quality::Draft, Quality::Final] {
                    out.remove(&render::takes_dir(pr, q).join(format!("{h:016x}.wav")), &["wav"]);
                }
            }
        }
    }
    if out.deleted.len() > n0 {
        for f in ["mix.wav", "line.wav"].map(|n| pr.dir.join("out").join(n)).into_iter().filter(|f| f.exists()) {
            out.skipped.push(format!("{} nicht gelöscht: kann ältere Takes dieser Stimme enthalten, ist aber nicht im Index – bitte prüfen oder neu rendern", f.display()));
        }
    }
}

/// Rewrites the persona TOML in place (comments and layout kept): `edit` on the parsed document.
fn edit_toml(file: &Path, edit: impl FnOnce(&mut toml_edit::DocumentMut)) -> Result<()> {
    let mut doc: toml_edit::DocumentMut = std::fs::read_to_string(file)?.parse().with_context(|| file.display().to_string())?;
    edit(&mut doc);
    render::atomic(file, |t| Ok(std::fs::write(t, doc.to_string())?))
}

/// Withdraws the consent of clone `name` (first dir holding it): rendering is refused at once, then every audio file of
/// the voice is deleted (`purge`) and the persona shrinks to a tombstone (name, consent who/date/statement, revoked
/// date, provenance; no ref). Idempotent: a second call keeps the date and purges again.
///
/// The tombstone keeps `ref_text` (the person's transcript) unless `forget`: take keys and seeds hash the plain
/// transcript as their last component (funkhaus-voice `tts::key`, shared with funkhaus and its golden), so no digest
/// can stand in for it; without it a later `revoke --project` cannot find that project's takes.
pub fn revoke(dirs: &[PathBuf], name: &str, date: Option<&str>, forget: bool, pl: &Places) -> Result<Revoked> {
    let d = dirs.iter().find(|d| Persona::path(d, name).exists()).with_context(|| format!("Persona '{name}' nicht gefunden"))?;
    let p = Persona::load(d, name)?;
    if p.kind != Kind::Clone {
        bail!("'{name}' ist keine geklonte Stimme – nur Klone haben eine Einwilligung zum Widerrufen (entworfene Stimmen: `stimmwerk voice rm {name}`)");
    }
    let file = Persona::path(d, name);
    let c = p.consent.as_ref().with_context(|| format!("'{name}' hat keinen [consent]-Block"))?;
    let already = c.revoked.clone().filter(|r| !r.is_empty());
    let date = match &already {
        Some(r) => r.clone(),
        None => {
            let date = date.map_or_else(today, String::from);
            check_date(&date)?;
            edit_toml(&file, |doc| doc["consent"]["revoked"] = toml_edit::value(date.clone()))?;
            date
        }
    };
    let pg = purge(pl, d, &p, |_| {
        // tombstone: provenance.sha256 (+ ref_text unless forgotten) let a later `revoke --project` find takes by key
        edit_toml(&file, |doc| {
            doc.remove("ref");
            if forget {
                doc.remove("ref_text");
            }
        })
    })?;
    let ref_text_kept = Persona::load(d, name).is_ok_and(|t| t.ref_text.is_some_and(|r| !r.trim().is_empty()));
    Ok(Revoked { persona: name.into(), revoked: date, file, already: already.is_some(), deleted: pg.deleted, kept_external: pg.external, skipped: pg.skipped, ref_text_kept })
}

/// Consent / revoke date: `JJJJ-MM-TT`, a real day, not in the future.
pub fn check_date(s: &str) -> Result<()> {
    let d = chrono::NaiveDate::parse_from_str(s.trim(), "%Y-%m-%d").ok().filter(|_| s.trim().len() == 10).with_context(|| format!("Datum „{s}“: bitte als JJJJ-MM-TT angeben (z. B. {})", today()))?;
    if d > chrono::Local::now().date_naive() {
        bail!("Datum „{s}“ liegt in der Zukunft");
    }
    Ok(())
}

#[derive(Serialize, Clone, Debug)]
pub struct Removed {
    pub persona: String,
    pub file: PathBuf,
    pub deleted: Vec<PathBuf>,
    /// what could not be checked or deleted; the persona file is kept then (run `rm` again after fixing it)
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub skipped: Vec<String>,
    /// persona file deleted (false when something was skipped)
    pub removed: bool,
}

/// Deletes design (or builtin) persona `name` from the own voices folder `vdir` with all its audio (anchor clip,
/// candidates with the same clip, takes, probes, exports, prompt cache). Clones are revoked instead (`revoke`), so the
/// consent record stays.
pub fn rm(vdir: &Path, name: &str, pl: &Places) -> Result<Removed> {
    let file = Persona::path(vdir, name);
    if !file.exists() {
        bail!("Persona '{name}' nicht im eigenen Stimmen-Ordner ({})", vdir.display());
    }
    let p = Persona::load(vdir, name)?;
    if p.kind == Kind::Clone {
        bail!("'{name}' ist ein Klon: `stimmwerk voice revoke {name}` widerruft die Einwilligung und löscht alle Aufnahmen");
    }
    let pg = purge(pl, vdir, &p, |pg| {
        // the persona file is the only record of what to delete: keep it until the purge was complete
        if pg.skipped.is_empty() {
            match std::fs::remove_file(&file) {
                Ok(()) => pg.deleted.push(file.clone()),
                Err(e) => pg.skipped.push(format!("{} nicht gelöscht: {e}", file.display())),
            }
        }
        Ok(())
    })?;
    let removed = !file.exists();
    Ok(Removed { persona: name.into(), file, deleted: pg.deleted, skipped: pg.skipped, removed })
}

#[cfg(test)]
mod tests {
    use super::*;
    use funkhaus_voice::persona::sha256_file;
    use std::cell::RefCell;

    fn tone(p: &Path, secs: f64, lvl: f32) {
        funkhaus_audio::mix::write(p, &vec![lvl; (secs * 24_000.0) as usize], 24_000).unwrap();
    }

    fn design_req(n: usize) -> Design {
        Design { instruct: "female, low pitch".into(), n, seeds: None, text: "Hallo  du.".into(), lang: "de".into(), quality: Quality::Draft }
    }

    type Log = RefCell<Vec<(String, String, Option<PathBuf>, u64)>>;

    /// Fake TTS logging (text, instruct, ref, seed); fails for seed `fail`.
    fn fake(log: &Log, fail: u64) -> impl FnMut(&Path, &str, &VoiceSpec, u64) -> Result<()> + '_ {
        move |w, x, v, s| {
            log.borrow_mut().push((x.into(), v.instruct.clone(), v.ref_wav.clone(), s));
            if s == fail {
                bail!("kaputt");
            }
            funkhaus_audio::mix::write(w, &vec![0.1 + s as f32 * 0.01; 24_000], 24_000)
        }
    }

    #[test]
    fn instruct_check_and_presets() {
        for (_, i) in PRESETS {
            assert!(check_instruct(i).is_empty(), "{i}");
        }
        assert!(check_instruct("female, male").iter().any(|w| w.contains("zweiter Wert")));
        assert!(check_instruct("female, happy").iter().any(|w| w.contains("happy")));
        assert!(check_instruct("british accent").iter().any(|w| w.contains("englisch")));
        assert!(check_instruct("Female，Low Pitch").is_empty());
    }

    #[test]
    fn design_keep_probe() {
        let t = tempfile::tempdir().unwrap();
        let v = t.path().join("voices");
        let log = RefCell::new(vec![]);
        let no = AtomicBool::new(false);
        let b = design_with(&v, &design_req(3), &no, &|_| {}, &mut fake(&log, 99)).unwrap();
        assert_eq!(b.candidates.iter().map(|c| c.seed).collect::<Vec<_>>(), [1, 2, 3]);
        assert!(b.candidates.iter().all(|c| c.wav.is_file() && c.dur == 1.0));
        assert_eq!(log.borrow().iter().map(|l| (l.0.as_str(), l.1.as_str(), l.3)).collect::<Vec<_>>(), [("Hallo du.", "female, low pitch", 1), ("Hallo du.", "female, low pitch", 2), ("Hallo du.", "female, low pitch", 3)]);
        assert_eq!(load_batch(&v, &b.batch).unwrap(), b);
        assert!(Persona::list(&v).is_empty(), "design writes no persona");
        // keep -> anchored design persona
        let dirs = vec![v.clone()];
        let s = keep(&v, &dirs, None, &b.batch, 2, "tief", Some("Test")).unwrap();
        let p = Persona::load(&v, "tief").unwrap();
        let pv = p.provenance.clone().unwrap();
        assert_eq!((p.kind, p.seed, p.instruct.as_deref(), p.ref_.as_deref(), p.ref_text.as_deref()), (Kind::Design, Some(2), Some("female, low pitch"), Some("clips/tief.wav"), Some("Hallo du.")));
        assert_eq!((pv.instruct.as_deref(), pv.seed, pv.license.as_deref(), pv.method.as_deref()), (Some("female, low pitch"), Some(2), Some(LICENSE_OWN), Some(ANCHOR_METHOD)));
        assert_eq!(s.ref_sha256.unwrap().len(), 64);
        assert!(b.candidates[1].wav.is_file(), "candidate stays");
        assert!(keep(&v, &dirs, None, &b.batch, 2, "tief", None).unwrap_err().to_string().contains("existiert schon"));
        assert!(keep(&v, &dirs, None, &b.batch, 9, "neu", None).is_err());
        assert!(keep(&v, &dirs, None, "../x", 1, "neu", None).is_err());
        for bad in ["Tief", "a b", "a_b", "a.b", "../x", "", "säge"] {
            assert!(keep(&v, &dirs, None, &b.batch, 1, bad, None).is_err(), "{bad}");
        }
        assert!(!v.join("neu.toml").exists());
        // a persona must not shadow an omnivoice builtin (known list, or what omnivoice reports)
        let bi = ["gt".to_string(), "neu-bi".to_string()];
        for n in ["gt", "kn", "fable", "gerstaecker", "neu-bi"] {
            let e = keep(&v, &dirs, Some(&bi), &b.batch, 1, n, None).unwrap_err().to_string();
            assert!(e.contains("omnivoice-Standard-Stimme"), "{n}: {e}");
            assert!(!v.join(format!("{n}.toml")).exists() && !v.join(format!("clips/{n}.wav")).exists());
        }
        // probe: anchored design renders via --ref; cached second time
        let cache = t.path().join("cache");
        log.borrow_mut().clear();
        let a = probe_with(&cache, &dirs, "tief", "Probe eins.", "de", None, &mut fake(&log, 99)).unwrap();
        let b2 = probe_with(&cache, &dirs, "tief", "Probe eins.", "de", None, &mut fake(&log, 99)).unwrap();
        assert_eq!((a.cache_hit, b2.cache_hit, log.borrow().len(), a.cache_key.clone()), (false, true, 1, b2.cache_key));
        assert_eq!(log.borrow()[0].2.as_deref(), Some(v.join("clips/tief.wav").as_path()));
        assert_eq!((a.spec.kind.as_str(), a.spec.seed), ("design", Some(2)));
        let o = probe_with(&cache, &dirs, "tief", "Probe eins.", "de", Some(&t.path().join("o")), &mut fake(&log, 99)).unwrap();
        assert!(o.wav.ends_with("o/tief.wav") && o.wav.is_file());
        let l = ls(&dirs);
        assert_eq!(l.iter().map(|e| (e.name.as_str(), e.kind.as_str(), e.anchored, e.valid)).collect::<Vec<_>>(), [("tief", "design", true, true)]);
    }

    #[test]
    fn design_errors_leave_nothing() {
        let t = tempfile::tempdir().unwrap();
        let v = t.path().join("voices");
        let log = RefCell::new(vec![]);
        let no = AtomicBool::new(false);
        assert!(design_with(&v, &Design { instruct: " ".into(), ..design_req(3) }, &no, &|_| {}, &mut fake(&log, 99)).is_err());
        assert!(design_with(&v, &design_req(0), &no, &|_| {}, &mut fake(&log, 99)).is_err());
        assert!(design_with(&v, &Design { seeds: Some(vec![1]), ..design_req(2) }, &no, &|_| {}, &mut fake(&log, 99)).is_err());
        assert!(design_with(&v, &Design { seeds: Some(vec![4, 4]), ..design_req(2) }, &no, &|_| {}, &mut fake(&log, 99)).is_err());
        let b = design_with(&v, &Design { seeds: Some(vec![5, 7]), ..design_req(2) }, &no, &|_| {}, &mut fake(&log, 99)).unwrap();
        assert_eq!(b.candidates.iter().map(|c| c.seed).collect::<Vec<_>>(), [5, 7]);
        log.borrow_mut().clear();
        let e = design_with(&v, &design_req(3), &no, &|_| {}, &mut fake(&log, 2)).unwrap_err();
        assert!(format!("{e:#}").contains("Kandidat 2"), "{e:#}");
        let left: Vec<_> = std::fs::read_dir(candidates_root(&v)).unwrap().flatten().map(|e| e.file_name()).collect();
        assert_eq!(left, [std::ffi::OsString::from(&b.batch)], "failed batch removed, no part files");
        let stop = AtomicBool::new(true);
        assert!(design_with(&v, &design_req(3), &stop, &|_| {}, &mut fake(&log, 99)).unwrap_err().to_string().contains("abgebrochen"));
    }

    fn req(t: &Path) -> CloneReq {
        let w = t.join("in.wav");
        if !w.exists() {
            tone(&w, 6.0, 0.2);
        }
        CloneReq { wav: w, name: "eigen".into(), ref_text: "Hallo, das bin ich.".into(), consent: "Eigene synthetische Stimme, Test".into(), who: "Testperson".into(), lang: "de".into(), ..Default::default() }
    }

    #[test]
    fn clone_consent_hash_revoke() {
        let t = tempfile::tempdir().unwrap();
        let pl = Places::at(&t.path().join("home"));
        let v = t.path().join("voices");
        let dirs = vec![v.clone()];
        // no consent -> nothing written
        let e = clone(&v, &dirs, None, &CloneReq { consent: "  ".into(), ..req(t.path()) }).unwrap_err();
        assert!(e.to_string().contains("Einwilligung fehlt"));
        assert!(clone(&v, &dirs, None, &CloneReq { who: " ".into(), ..req(t.path()) }).unwrap_err().to_string().contains("--who"));
        assert!(!v.exists());
        assert!(clone(&v, &dirs, None, &CloneReq { date: Some("2999-01-01".into()), ..req(t.path()) }).is_err());
        assert!(!v.join("clips/eigen.wav").exists() && !v.join("eigen.toml").exists(), "failed add removes the clip");
        assert!(clone(&v, &dirs, None, &CloneReq { ref_text: " ".into(), ..req(t.path()) }).is_err());
        assert!(clone(&v, &dirs, None, &CloneReq { start: Some(2.0), end: Some(1.0), ..req(t.path()) }).is_err());
        // trimmed copy, bound by sha256
        let s = clone(&v, &dirs, None, &CloneReq { start: Some(1.0), end: Some(5.0), ..req(t.path()) }).unwrap();
        let clip = v.join("clips/eigen.wav");
        assert_eq!(dur(&clip).unwrap(), 4.0);
        assert_eq!(s.ref_sha256.as_deref(), Some(sha256_file(&clip).unwrap().as_str()));
        let p = Persona::load(&v, "eigen").unwrap();
        let c = p.consent.clone().unwrap();
        let pv = p.provenance.unwrap();
        assert_eq!((c.who.as_str(), c.statement.as_str(), c.date, pv.method.as_deref(), pv.license.as_deref()), ("Testperson", "Eigene synthetische Stimme, Test", today(), Some("clone"), Some(LICENSE_CONSENT)));
        assert_eq!((pv.source_sha256.as_deref(), pv.start, pv.end), (Some(sha256_file(&t.path().join("in.wav")).unwrap().as_str()), Some(1.0), Some(5.0)));
        std::fs::remove_file(t.path().join("in.wav")).unwrap();
        let spec = crate::speakers::spec(&dirs, "eigen").unwrap();
        assert_eq!((spec.ref_wav.as_deref(), spec.ref_text.as_str()), (Some(clip.as_path()), "Hallo, das bin ich."));
        assert!(clone(&v, &dirs, None, &req(t.path())).unwrap_err().to_string().contains("existiert schon"));
        // render path: --ref, never omnivoice voices add
        let args = tts::say_args("x", Path::new("o.wav"), &spec, 1, "de", "");
        assert_eq!(args[3..7], ["--ref".to_string(), clip.to_string_lossy().into(), "--ref-text".into(), "Hallo, das bin ich.".into()]);
        // consent text is not in the cache key
        let t2 = Tts::default();
        let k0 = tts::key(&t2, &spec, "de", "a");
        let txt = std::fs::read_to_string(v.join("eigen.toml")).unwrap().replace("statement = \"Eigene synthetische Stimme, Test\"", "statement = \"anders\"");
        std::fs::write(v.join("eigen.toml"), txt).unwrap();
        assert_eq!(tts::key(&t2, &crate::speakers::spec(&dirs, "eigen").unwrap(), "de", "a"), k0);
        // changed audio -> refused
        let orig = std::fs::read(&clip).unwrap();
        tone(&clip, 4.0, 0.3);
        assert!(crate::speakers::spec(&dirs, "eigen").unwrap_err().to_string().contains("Referenz-Audio geändert"));
        std::fs::write(&clip, orig).unwrap();
        // revoke: render refused, idempotent, listed
        assert!(revoke(&dirs, "eigen", Some("13.10.2026"), false, &pl).is_err());
        let r = revoke(&dirs, "eigen", None, false, &pl).unwrap();
        assert!(!r.already);
        assert!(crate::speakers::spec(&dirs, "eigen").unwrap_err().to_string().contains("widerrufen"));
        let log = RefCell::new(vec![]);
        assert!(probe_with(&t.path().join("c"), &dirs, "eigen", "Hallo.", "de", None, &mut fake(&log, 99)).is_err());
        assert!(log.borrow().is_empty());
        let r2 = revoke(&dirs, "eigen", Some("2030-01-01"), false, &pl).unwrap();
        assert_eq!((r2.already, r2.revoked), (true, r.revoked.clone()));
        let e = &ls(&dirs)[0];
        assert_eq!((e.kind.as_str(), e.revoked.clone(), e.valid), ("clone", Some(r.revoked), false));
        assert!(revoke(&dirs, "nope", None, false, &pl).is_err());
    }

    #[test]
    fn revoke_only_clones_and_full_copy() {
        let t = tempfile::tempdir().unwrap();
        let v = t.path().join("voices");
        let dirs = vec![v.clone()];
        let pl = Places::at(&t.path().join("home"));
        let s = clone(&v, &dirs, None, &CloneReq { who: "Testperson B".into(), ..req(t.path()) }).unwrap();
        assert_eq!(std::fs::read(v.join("clips/eigen.wav")).unwrap(), std::fs::read(t.path().join("in.wav")).unwrap(), "untrimmed = exact copy");
        assert!(s.warnings.is_empty());
        assert_eq!(Persona::load(&v, "eigen").unwrap().consent.unwrap().who, "Testperson B");
        std::fs::write(v.join("d.toml"), "name = \"d\"\nkind = \"design\"\ninstruct = \"female\"\n").unwrap();
        assert!(revoke(&dirs, "d", None, false, &pl).unwrap_err().to_string().contains("keine geklonte"));
        let w = clone(&v, &dirs, None, &CloneReq { name: "kurz".into(), end: Some(2.0), ..req(t.path()) }).unwrap();
        assert!(w.warnings[0].contains("3 bis 10"));
    }

    /// Project with speaker ICH = `who` (persona) and ERZ = builtin gt.
    fn proj(dir: &Path, dirs: &[PathBuf], who: &str) -> crate::Project {
        let mut p = crate::Project::create(dir, "X", "de").unwrap();
        p.script = crate::script::Script::parse("ICH|0|Hallo eins.\nERZ|0|Andere Stimme.\nICH|0.3|Hallo zwei.\n").unwrap();
        crate::speakers::set(&mut p, dirs, None, "ICH", who, None).unwrap();
        crate::speakers::set(&mut p, dirs, None, "ERZ", "gt", None).unwrap();
        p.save().unwrap();
        p
    }

    fn wavs(d: &Path) -> Vec<PathBuf> {
        let mut v: Vec<PathBuf> = std::fs::read_dir(d).into_iter().flatten().flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "wav")).collect();
        v.sort();
        v
    }

    #[test]
    fn revoke_deletes_every_audio_file() {
        let t = tempfile::tempdir().unwrap();
        let home = t.path().join("home");
        let v = home.join("voices");
        let dirs = vec![v.clone()];
        let elsewhere = t.path().join("woanders.studio");
        let pl = Places { projects: vec![elsewhere.clone()], ..Places::at(&home.clone()) };
        clone(&v, &dirs, None, &req(t.path())).unwrap();
        let f = v.join("eigen.toml");
        std::fs::write(&f, format!("# eigene Notiz\n{}", std::fs::read_to_string(&f).unwrap())).unwrap();
        let clip = v.join("clips/eigen.wav");
        // two projects (data root + elsewhere), rendered in both qualities, one line rerolled, one exported-like mix
        let no = AtomicBool::new(false);
        let n = std::cell::Cell::new(0);
        let mut say = |w: &Path, _: &str, _: &VoiceSpec, s: u64| -> Result<()> {
            n.set(n.get() + 1);
            funkhaus_audio::mix::write(w, &vec![0.1 + (s % 100) as f32 * 0.001; 2400], 24_000)
        };
        let a = proj(&home.join("projects/a.studio"), &dirs, "eigen");
        let b = proj(&elsewhere, &dirs, "eigen");
        for p in [&a, &b] {
            for q in [Quality::Draft, Quality::Final] {
                render::render_with(p, &dirs, &render::Opts { quality: q, ..Default::default() }, &no, &|_| {}, &mut say).unwrap();
            }
        }
        render::reroll_with(&a, &dirs, 1, false, Quality::Final, &no, &|_| {}, &mut say).unwrap();
        let gt_takes = |p: &crate::Project| wavs(&render::takes_dir(p, Quality::Final)).len();
        assert_eq!((wavs(&render::takes_dir(&a, Quality::Final)).len(), wavs(&render::takes_dir(&a, Quality::Draft)).len()), (4, 3));
        // project b: index lost (takes from before the index existed) -> found by key
        std::fs::remove_file(render::takes_root(&b).join(crate::derived::INDEX)).unwrap();
        // probe cache + probe copy
        let probe = home.join("cache/probe");
        probe_with(&probe, &dirs, "eigen", "Probe.", "de", Some(&t.path().join("pout")), &mut say).unwrap();
        // omnivoice prompt cache: exact key, an entry with the same ref_text under an old key, an unrelated one
        let oc = home.join("cache/omnivoice");
        std::fs::create_dir_all(&oc).unwrap();
        let k = crate::derived::omnivoice_key(&clip, "Hallo, das bin ich.").unwrap();
        let u = |s: &str| s.chars().flat_map(|c| (c as u32).to_le_bytes()).collect::<Vec<u8>>();
        let rt = "Hallo, das bin ich.";
        std::fs::write(oc.join(format!("{k}.npz")), b"tokens").unwrap();
        std::fs::write(oc.join("0000000000000001.npz"), [format!("'<U{}'", rt.chars().count()).as_bytes(), &u(rt)].concat()).unwrap();
        std::fs::write(oc.join("0000000000000002.npz"), [b"'<U3'".as_slice(), &u("gt!")].concat()).unwrap();
        // a builtin-only project is untouched
        let c = proj(&home.join("projects/c.studio"), &dirs, "kn");
        render::render_with(&c, &dirs, &render::Opts::default(), &no, &|_| {}, &mut say).unwrap();
        let c_before = wavs(&render::takes_dir(&c, Quality::Final));
        let all = || files(&[t.path().to_path_buf()]);
        let before = all();

        let r = revoke(&dirs, "eigen", None, false, &pl).unwrap();
        assert!(!r.already && r.kept_external.is_none());
        assert!(!clip.exists(), "clip deleted");
        assert!(wavs(&probe).is_empty() && !t.path().join("pout/eigen.wav").exists(), "probes deleted");
        let left: Vec<String> = std::fs::read_dir(&oc).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        assert_eq!(left, ["0000000000000002.npz"], "prompt cache entries of the voice deleted");
        // only the gt line's takes stay (1 per quality); indexed mix gone, b's unindexed (pre-index) mix only reported
        for p in [&a, &b] {
            assert_eq!((gt_takes(p), wavs(&render::takes_dir(p, Quality::Draft)).len()), (1, 1), "{}", p.dir.display());
        }
        assert!(!a.dir.join("out/mix.wav").exists() && b.dir.join("out/mix.wav").exists());
        assert_eq!(r.skipped.len(), 1, "{:?}", r.skipped);
        assert!(r.skipped[0].contains("woanders.studio/out/mix.wav") && r.skipped[0].contains("nicht im Index"), "{:?}", r.skipped);
        assert_eq!(wavs(&render::takes_dir(&c, Quality::Final)), c_before);
        // exactly the files that vanished, each reported once: clip, 2 npz, probe + copy, takes (incl. the rerolled
        // alternate), a's mix
        let after = all();
        // (emptied voices.tsv indexes vanish too; they are bookkeeping, not audio)
        let mut gone: Vec<PathBuf> = before.iter().filter(|f| !after.contains(f) && !f.ends_with(crate::derived::INDEX)).cloned().collect();
        let mut del = r.deleted.clone();
        gone.sort();
        del.sort();
        assert_eq!(del, gone);
        assert_eq!(gone.iter().filter(|f| f.extension().is_some_and(|x| x == "npz")).count(), 2);
        assert!(gone.contains(&clip) && gone.contains(&t.path().join("pout/eigen.wav")) && gone.contains(&a.dir.join("out/mix.wav")));
        assert_eq!(gone.iter().filter(|f| f.starts_with(render::takes_root(&a))).count(), 5, "a: 2 draft + 2 final + rerolled alternate: {gone:?}");
        // tombstone: comment kept, ref (audio) gone, ref_text + consent + revoked date stay
        let tomb = std::fs::read_to_string(&f).unwrap();
        assert!(tomb.starts_with("# eigene Notiz") && !tomb.contains("\nref =") && tomb.contains("ref_text = ") && tomb.contains("revoked = ") && tomb.contains("who = \"Testperson\""), "{tomb}");
        assert!(crate::speakers::spec(&dirs, "eigen").unwrap_err().to_string().contains("widerrufen"));
        assert!(render::render_with(&a, &dirs, &render::Opts::default(), &no, &|_| {}, &mut say).unwrap_err().to_string().contains("widerrufen"));
        // again: same date, nothing left to delete
        let r2 = revoke(&dirs, "eigen", Some("2030-01-01"), false, &pl).unwrap();
        assert_eq!((r2.already, r2.revoked, r2.deleted.len()), (true, r.revoked, 0));
    }

    /// `project trash` must not break the revoke guarantee for exports outside the folder (`export --out`): one that
    /// could be moved into the project travels with it (Trash – not scanned, see `TRASH_NOTE`), one on "another volume"
    /// stays and is indexed at the data root, so `revoke` still deletes it; a non-audio line there is reported.
    #[test]
    fn trashed_project_keeps_outside_export_revocable() {
        let t = tempfile::tempdir().unwrap();
        let home = t.path().join("home");
        let v = home.join("voices");
        let dirs = vec![v.clone()];
        let pl = Places::at(&home);
        clone(&v, &dirs, None, &req(t.path())).unwrap();
        let sha = funkhaus_voice::persona::sha256_file(&v.join("clips/eigen.wav")).unwrap();
        let a = proj(&home.join("projects/a.studio"), &dirs, "eigen");
        let no = AtomicBool::new(false);
        let mut say = |w: &Path, _: &str, _: &VoiceSpec, _: u64| -> Result<()> { funkhaus_audio::mix::write(w, &vec![0.1; 2400], 24_000) };
        render::render_with(&a, &dirs, &render::Opts::default(), &no, &|_| {}, &mut say).unwrap();
        // two exports elsewhere + a hand-edited non-audio line, all recorded in the project's index only
        let (near, far, doc) = (t.path().join("out/export.wav"), t.path().join("usb/export.wav"), t.path().join("usb/notiz.txt"));
        for f in [&near, &far, &doc] {
            std::fs::create_dir_all(f.parent().unwrap()).unwrap();
            std::fs::write(f, b"x").unwrap();
        }
        crate::derived::record(&render::takes_root(&a), &sha, "eigen", &[near.clone(), far.clone(), doc.clone()]).unwrap();
        let bin = t.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let usb = t.path().join("usb");
        let mv_in = |x: &Path, y: &Path| if x.starts_with(&usb) { Err(std::io::Error::other("EXDEV")) } else { std::fs::rename(x, y) };
        let r = crate::project::trash_with(&a.dir, &home, &|p| Ok(std::fs::rename(p, bin.join(p.file_name().unwrap()))?), &mv_in).unwrap();
        assert_eq!((&r.moved_in, &r.outside), (&vec![near.clone()], &vec![far.clone(), doc.clone()]));
        let gone = bin.join("a.studio");
        assert!(!a.dir.exists() && !near.exists() && gone.join("out/extern/export.wav").is_file() && far.exists());
        assert!(crate::derived::home_index(&home).join(crate::derived::INDEX).is_file());
        assert!(pl.project_dirs().is_empty(), "the trashed project is no project dir any more");

        let rv = revoke(&dirs, "eigen", None, false, &pl).unwrap();
        assert!(rv.deleted.contains(&far), "{:?}", rv.deleted);
        assert!(!far.exists() && doc.exists());
        assert!(rv.skipped.iter().any(|s| s.contains("notiz.txt") && s.contains("keine erlaubte Datei")), "{:?}", rv.skipped);
        // what went to the Trash with the folder is out of reach (the dialog/CLI say so); nothing else was touched
        assert!(gone.join("out/extern/export.wav").is_file() && gone.join("takes").is_dir());
        // the root index keeps only the refused line; a second revoke reports it again, deletes nothing
        assert_eq!(std::fs::read_to_string(crate::derived::home_index(&home).join(crate::derived::INDEX)).unwrap().lines().count(), 1);
        let rv2 = revoke(&dirs, "eigen", Some("2030-01-01"), false, &pl).unwrap();
        assert!(rv2.deleted.is_empty() && rv2.skipped.iter().any(|s| s.contains("notiz.txt")));
    }

    #[test]
    fn rm_design_with_audio_but_not_clones() {
        let t = tempfile::tempdir().unwrap();
        let home = t.path().join("home");
        let v = home.join("voices");
        let dirs = vec![v.clone()];
        let pl = Places::at(&home.clone());
        let log = RefCell::new(vec![]);
        let no = AtomicBool::new(false);
        let b = design_with(&v, &design_req(2), &no, &|_| {}, &mut fake(&log, 99)).unwrap();
        keep(&v, &dirs, None, &b.batch, 1, "tief", None).unwrap();
        let a = proj(&home.join("projects/a.studio"), &dirs, "tief");
        render::render_with(&a, &dirs, &render::Opts::default(), &no, &|_| {}, &mut fake(&log, 99)).unwrap();
        clone(&v, &dirs, None, &req(t.path())).unwrap();
        assert!(rm(&v, "eigen", &pl).unwrap_err().to_string().contains("voice revoke"));
        assert!(rm(&v, "nope", &pl).is_err());
        let fin = render::takes_dir(&a, Quality::Final);
        let mut want: Vec<PathBuf> = wavs(&fin);
        let gt = want.remove(want.iter().position(|f| is_gt_take(&a, f)).unwrap());
        want.extend([v.join("tief.toml"), v.join("clips/tief.wav"), a.dir.join("out/mix.wav")]);
        // the kept candidate (same clip) goes too, the other one stays
        let sha = sha256_file(&v.join("clips/tief.wav")).unwrap();
        want.extend(b.candidates.iter().map(|c| c.wav.clone()).filter(|w| sha256_file(w).unwrap() == sha));
        let r = rm(&v, "tief", &pl).unwrap();
        assert!(r.removed && r.skipped.is_empty() && !v.join("tief.toml").exists() && !v.join("clips/tief.wav").exists());
        assert_eq!(wavs(&fin), [gt], "only the gt line");
        let (mut d, mut w) = (r.deleted.clone(), want);
        d.sort();
        w.sort();
        assert_eq!(d, w);
        assert!(v.join("eigen.toml").exists());
    }

    /// Every file under `roots` (recursive), sorted.
    fn files(roots: &[PathBuf]) -> Vec<PathBuf> {
        let mut v = vec![];
        let mut st: Vec<PathBuf> = roots.to_vec();
        while let Some(d) = st.pop() {
            for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
                let p = e.path();
                if p.is_dir() { st.push(p) } else { v.push(p) }
            }
        }
        v.sort();
        v
    }

    /// Is `f` the take of project `a`'s line 2 (ERZ = gt)?
    fn is_gt_take(a: &crate::Project, f: &Path) -> bool {
        let lines: Vec<funkhaus_audio::script::Line> = a.script.lines().cloned().collect();
        let voices: tts::Voices = [("ERZ".to_string(), VoiceSpec::builtin("gt"))].into();
        let k = tts::line_keys(&render::key_cfg(a), &lines, &voices, &funkhaus_voice::lexicon::Lexicon::load(&a.lexicon_path()), "de", 1).unwrap();
        f.file_name().is_some_and(|n| n.to_string_lossy() == format!("{:016x}.wav", k[0]))
    }

    /// The clip of a clone was deleted (or changed): consent can still be withdrawn and its takes are found by key.
    #[test]
    fn revoke_without_clip() {
        let t = tempfile::tempdir().unwrap();
        let home = t.path().join("home");
        let v = home.join("voices");
        let dirs = vec![v.clone()];
        let pl = Places::at(&home.clone());
        clone(&v, &dirs, None, &req(t.path())).unwrap();
        let a = proj(&home.join("projects/a.studio"), &dirs, "eigen");
        let no = AtomicBool::new(false);
        let log = RefCell::new(vec![]);
        render::render_with(&a, &dirs, &render::Opts::default(), &no, &|_| {}, &mut fake(&log, 99)).unwrap();
        let fin = render::takes_dir(&a, Quality::Final);
        assert_eq!(wavs(&fin).len(), 3);
        std::fs::remove_file(render::takes_root(&a).join(crate::derived::INDEX)).unwrap();
        std::fs::remove_file(v.join("clips/eigen.wav")).unwrap();
        assert!(crate::speakers::spec(&dirs, "eigen").is_err(), "not renderable any more");
        let r = revoke(&dirs, "eigen", None, false, &pl).unwrap();
        assert_eq!(wavs(&fin).len(), 1, "only the gt take stays: {:?}", r.deleted);
        // the index was gone, so the mix is not provably this voice's: reported, not deleted
        assert!(a.dir.join("out/mix.wav").exists() && r.skipped.len() == 1 && r.skipped[0].contains("mix.wav"), "{:?}", r.skipped);
        assert!(std::fs::read_to_string(v.join("eigen.toml")).unwrap().contains("revoked = "));
    }

    /// Purge takes every project lock first: a busy project fails the purge before anything is deleted (revoke flag
    /// still set, so nothing renders the voice); unreadable projects are reported, rm keeps the persona then.
    #[test]
    fn purge_locks_projects_and_reports_skips() {
        let t = tempfile::tempdir().unwrap();
        let home = t.path().join("home");
        let v = home.join("voices");
        let dirs = vec![v.clone()];
        let pl = Places { wait: std::time::Duration::from_millis(200), ..Places::at(&home.clone()) };
        let log = RefCell::new(vec![]);
        let no = AtomicBool::new(false);
        let b = design_with(&v, &design_req(1), &no, &|_| {}, &mut fake(&log, 99)).unwrap();
        keep(&v, &dirs, None, &b.batch, 1, "tief", None).unwrap();
        let a = proj(&home.join("projects/a.studio"), &dirs, "tief");
        render::render_with(&a, &dirs, &render::Opts::default(), &no, &|_| {}, &mut fake(&log, 99)).unwrap();
        let before = files(std::slice::from_ref(&home));
        // busy: a render holds the lock the whole time
        let held = render::lock(&a).unwrap();
        let e = rm(&v, "tief", &pl).unwrap_err();
        assert!(format!("{e:#}").contains("beschäftigt"), "{e:#}");
        assert_eq!(files(std::slice::from_ref(&home)), before, "nothing deleted while busy");
        // the lock is released within the wait: purge goes ahead
        let pl2 = Places { wait: std::time::Duration::from_secs(5), ..pl.clone() };
        let h = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(300));
            drop(held);
        });
        // an unreadable project next to it: reported, persona kept
        let bad = home.join("projects/kaputt.studio");
        std::fs::create_dir_all(bad.join("takes")).unwrap();
        std::fs::write(bad.join("project.toml"), "format = 99\n").unwrap();
        let r = rm(&v, "tief", &pl2).unwrap();
        h.join().unwrap();
        assert!(!r.removed && v.join("tief.toml").exists(), "persona kept until the purge was complete");
        assert_eq!(r.skipped.len(), 1, "{:?}", r.skipped);
        assert!(r.skipped[0].contains("kaputt.studio") && r.skipped[0].contains("nicht lesbar"), "{:?}", r.skipped);
        assert_eq!(wavs(&render::takes_dir(&a, Quality::Final)).len(), 1);
        // fixed project: rm completes
        std::fs::remove_dir_all(&bad).unwrap();
        let r = rm(&v, "tief", &pl2).unwrap();
        assert!(r.removed && r.skipped.is_empty() && !v.join("tief.toml").exists());
    }

    /// After a revoke, `revoke --project <other>` still finds pre-index takes there by key (tombstone keeps ref_text).
    #[test]
    fn revoke_again_for_another_project() {
        let t = tempfile::tempdir().unwrap();
        let home = t.path().join("home");
        let v = home.join("voices");
        let dirs = vec![v.clone()];
        clone(&v, &dirs, None, &req(t.path())).unwrap();
        let other = t.path().join("extern.studio");
        let b = proj(&other, &dirs, "eigen");
        let no = AtomicBool::new(false);
        let log = RefCell::new(vec![]);
        render::render_with(&b, &dirs, &render::Opts::default(), &no, &|_| {}, &mut fake(&log, 99)).unwrap();
        std::fs::remove_file(render::takes_root(&b).join(crate::derived::INDEX)).unwrap(); // pre-index takes
        let fin = render::takes_dir(&b, Quality::Final);
        assert_eq!(wavs(&fin).len(), 3);
        // first revoke without the project: nothing there touched
        let pl = Places::at(&home.clone());
        revoke(&dirs, "eigen", None, false, &pl).unwrap();
        assert_eq!(wavs(&fin).len(), 3);
        // again with --project: found by key
        let r = revoke(&dirs, "eigen", None, false, &Places { projects: vec![other.clone()], ..pl }).unwrap();
        assert!(r.already);
        assert_eq!(wavs(&fin).len(), 1, "{:?}", r.deleted);
        assert_eq!(r.deleted.iter().filter(|f| f.starts_with(&fin)).count(), 2);
    }

    /// A hand-written anchored design (method + correct sha + clip copied to clips/) passes validation: accepted as a
    /// local-user guardrail (whoever can write the voices folder can also run `voice keep`). The anchor is bound to its
    /// sha256, so swapping the clip afterwards is refused.
    #[test]
    fn hand_written_anchored_design_is_accepted() {
        let t = tempfile::tempdir().unwrap();
        let v = t.path().join("voices");
        std::fs::create_dir_all(v.join("clips")).unwrap();
        let clip = v.join("clips/hand.wav");
        tone(&clip, 4.0, 0.2);
        let sha = sha256_file(&clip).unwrap();
        std::fs::write(v.join("hand.toml"), format!("name = \"hand\"\nkind = \"design\"\ninstruct = \"female\"\nref = \"clips/hand.wav\"\nref_text = \"Hallo.\"\n[provenance]\nmethod = \"{}\"\nsha256 = \"{sha}\"\n", funkhaus_voice::persona::ANCHOR_METHOD)).unwrap();
        let dirs = vec![v.clone()];
        assert!(crate::speakers::spec(&dirs, "hand").unwrap().ref_ok);
        tone(&clip, 4.0, 0.3);
        assert!(crate::speakers::spec(&dirs, "hand").is_err());
    }

    /// An anchor kept from a design batch must match its candidate while the batch exists.
    #[test]
    fn anchor_checked_against_candidate() {
        let t = tempfile::tempdir().unwrap();
        let v = t.path().join("voices");
        let dirs = vec![v.clone()];
        let log = RefCell::new(vec![]);
        let b = design_with(&v, &design_req(2), &AtomicBool::new(false), &|_| {}, &mut fake(&log, 99)).unwrap();
        keep(&v, &dirs, None, &b.batch, 1, "tief", None).unwrap();
        assert!(crate::speakers::spec(&dirs, "tief").is_ok());
        // hand edit: source points at candidate 2, sha/clip of candidate 1 -> refused
        let f = v.join("tief.toml");
        std::fs::write(&f, std::fs::read_to_string(&f).unwrap().replace(&format!("{}/1", b.batch), &format!("{}/2", b.batch))).unwrap();
        assert!(crate::speakers::spec(&dirs, "tief").unwrap_err().to_string().contains("passt nicht zum Entwurf"));
        // batch cleaned up: nothing to compare, the sha binding alone holds
        std::fs::remove_dir_all(candidates_root(&v)).unwrap();
        assert!(crate::speakers::spec(&dirs, "tief").is_ok());
    }

    /// A probe rendering while the voice is revoked (or deleted) does not leave its sample behind: after the TTS it
    /// re-checks the voice under the probe index lock, which the purge holds until the tombstone is written.
    #[test]
    fn probe_concurrent_with_revoke_or_rm() {
        let t = tempfile::tempdir().unwrap();
        let home = t.path().join("home");
        let v = home.join("voices");
        let dirs = vec![v.clone()];
        let cache = home.join("cache").join("probe");
        clone(&v, &dirs, None, &req(t.path())).unwrap();
        let b = design_with(&v, &design_req(1), &AtomicBool::new(false), &|_| {}, &mut fake(&RefCell::new(vec![]), 99)).unwrap();
        keep(&v, &dirs, None, &b.batch, 1, "tief", None).unwrap();
        for name in ["eigen", "tief"] {
            let (started, go) = (std::sync::mpsc::channel(), std::sync::mpsc::channel::<()>());
            let (tx, rx) = (started.0, go.1);
            let (c2, d2) = (cache.clone(), dirs.clone());
            let h = std::thread::spawn(move || {
                let mut say = |w: &Path, _: &str, _: &VoiceSpec, _: u64| {
                    tx.send(()).unwrap();
                    rx.recv().unwrap();
                    funkhaus_audio::mix::write(w, &[0.1; 2400], 24_000)
                };
                probe_with(&c2, &d2, name, "Probe während Widerruf.", "de", None, &mut say).map_err(|e| format!("{e:#}"))
            });
            started.1.recv().unwrap();
            if name == "eigen" {
                revoke(&dirs, name, None, false, &Places::at(&home)).unwrap();
            } else {
                assert!(rm(&v, name, &Places::at(&home)).unwrap().removed);
            }
            go.0.send(()).unwrap();
            let e = h.join().unwrap().unwrap_err();
            assert!(e.contains("Probe verworfen"), "{e}");
            assert!(wavs(&cache).is_empty(), "{name}: sample of a withdrawn voice left: {:?}", wavs(&cache));
            assert!(std::fs::read_to_string(cache.join(crate::derived::INDEX)).unwrap_or_default().is_empty());
        }
    }

    /// revoke keeps the transcript (take keys need it) and says so; `--forget` drops it. Legacy omnivoice caches are
    /// scanned too; without clip and transcript a cache with entries is reported, not silently skipped.
    #[test]
    fn revoke_forget_and_legacy_cache() {
        let t = tempfile::tempdir().unwrap();
        let home = t.path().join("home");
        let v = home.join("voices");
        let dirs = vec![v.clone()];
        let legacy = t.path().join("omnivoice/voices/.cache");
        std::fs::create_dir_all(&legacy).unwrap();
        clone(&v, &dirs, None, &req(t.path())).unwrap();
        let u = |s: &str| s.chars().flat_map(|c| (c as u32).to_le_bytes()).collect::<Vec<u8>>();
        let k = crate::derived::omnivoice_key(&v.join("clips/eigen.wav"), "Hallo, das bin ich.").unwrap();
        std::fs::write(legacy.join(format!("{k}.npz")), b"by key").unwrap();
        std::fs::write(legacy.join("0000000000000001.npz"), [b"{'descr': '<U19'".as_slice(), &u("Hallo, das bin ich.")].concat()).unwrap();
        std::fs::write(legacy.join("0000000000000002.npz"), b"other voice").unwrap();
        let pl = Places { caches: vec![home.join("cache/omnivoice"), legacy.clone()], ..Places::at(&home) };
        let r = revoke(&dirs, "eigen", None, false, &pl).unwrap();
        assert!(r.ref_text_kept);
        assert_eq!(r.deleted.iter().filter(|f| f.starts_with(&legacy)).count(), 2, "{:?}", r.deleted);
        assert!(legacy.join("0000000000000002.npz").exists());
        assert_eq!(Persona::load(&v, "eigen").unwrap().ref_text.as_deref(), Some("Hallo, das bin ich."));
        let r = revoke(&dirs, "eigen", None, true, &pl).unwrap();
        assert!(!r.ref_text_kept && r.already);
        assert!(Persona::load(&v, "eigen").unwrap().ref_text.is_none());
        // forgotten transcript + clip gone: a cache with entries can no longer be matched -> reported
        let r = revoke(&dirs, "eigen", None, false, &pl).unwrap();
        assert!(r.skipped.iter().any(|s| s.contains(&legacy.display().to_string()) && s.contains("nicht erkennbar")), "{:?}", r.skipped);
    }

    /// `voice test` samples (`<root>/cache/voices/<name>-<16 hex>.wav`) of exactly this persona go too.
    #[test]
    fn purge_voice_test_samples() {
        let t = tempfile::tempdir().unwrap();
        let home = t.path().join("home");
        let v = home.join("voices");
        let dirs = vec![v.clone()];
        clone(&v, &dirs, None, &req(t.path())).unwrap();
        let cv = home.join("cache/voices");
        std::fs::create_dir_all(&cv).unwrap();
        let (mine, other, near) = (cv.join("eigen-0123456789abcdef.wav"), cv.join("eigen-x-0123456789abcdef.wav"), cv.join("eigen-0123.wav"));
        for f in [&mine, &other, &near] {
            std::fs::write(f, b"x").unwrap();
        }
        let r = revoke(&dirs, "eigen", None, false, &Places::at(&home)).unwrap();
        assert!(r.deleted.contains(&mine) && !mine.exists() && other.exists() && near.exists());
    }

    /// rm also deletes design candidates with the anchor's clip; a hard-linked anchor is kept and reported (persona stays).
    #[test]
    fn rm_candidates_and_linked_anchor() {
        let t = tempfile::tempdir().unwrap();
        let home = t.path().join("home");
        let v = home.join("voices");
        let dirs = vec![v.clone()];
        let b = design_with(&v, &design_req(2), &AtomicBool::new(false), &|_| {}, &mut fake(&RefCell::new(vec![]), 99)).unwrap();
        keep(&v, &dirs, None, &b.batch, 1, "tief", None).unwrap();
        keep(&v, &dirs, None, &b.batch, 2, "hoch", None).unwrap();
        std::fs::hard_link(v.join("clips/hoch.wav"), t.path().join("kopie.wav")).unwrap();
        let r = rm(&v, "tief", &Places::at(&home)).unwrap();
        assert!(r.removed && !b.candidates[0].wav.exists() && b.candidates[1].wav.exists());
        assert!(r.deleted.contains(&b.candidates[0].wav));
        let r = rm(&v, "hoch", &Places::at(&home)).unwrap();
        assert!(!r.removed && v.join("hoch.toml").exists());
        assert!(r.skipped.iter().any(|s| s.contains("harte Links")), "{:?}", r.skipped);
    }

    /// A render waiting for a project a purge holds says so ("Widerruf läuft"); purge waits one total budget for all
    /// busy projects, telling `on_wait` once per project.
    #[test]
    fn purge_lock_role_and_total_budget() {
        let t = tempfile::tempdir().unwrap();
        let home = t.path().join("home");
        let (a, b) = (proj(&home.join("projects/a.studio"), &[], "gt"), proj(&home.join("projects/b.studio"), &[], "gt"));
        let held = render::lock_dir(&a.dir, std::time::Duration::ZERO, "purge", None).unwrap();
        let e = render::lock(&a).unwrap_err().to_string();
        assert!(e.starts_with(render::PURGING), "{e}");
        drop(held);
        let _ra = render::lock(&a).unwrap();
        let _rb = render::lock(&b).unwrap();
        assert!(render::lock(&a).unwrap_err().to_string().starts_with(render::BUSY));
        static WAITED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let pl = Places { wait: std::time::Duration::from_millis(400), on_wait: Some(|_| {
            WAITED.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }), ..Places::at(&home) };
        std::fs::create_dir_all(home.join("voices")).unwrap();
        let p = Persona { name: "x".into(), kind: Kind::Builtin, lang: "de".into(), seed: None, voice: Some("gt".into()), instruct: None, ref_: None, ref_text: None, description: None, provenance: None, consent: None };
        let t0 = std::time::Instant::now();
        assert!(purge(&pl, &home.join("voices"), &p, |_| Ok(())).is_err());
        assert!(t0.elapsed() < std::time::Duration::from_millis(1200), "{:?}", t0.elapsed());
        assert!(WAITED.load(std::sync::atomic::Ordering::SeqCst) >= 1);
    }

    #[test]
    fn dates() {
        assert!(check_date(&today()).is_ok() && check_date("2024-02-29").is_ok());
        for bad in ["13.10.2026", "2024-2-3", "2023-02-29", "2999-01-01", "", "heute"] {
            assert!(check_date(bad).is_err(), "{bad}");
        }
    }
}
