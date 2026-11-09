//! `project.toml` + folder layout of a `<name>.studio/` project.
use crate::cues::{Cue, MusicCfg};
use crate::script::Script;
use anyhow::{Context, Result, bail};
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const FILE: &str = "project.toml";
pub const SCRIPT: &str = "script.txt";
pub const LEXICON: &str = "lexicon.tsv";
/// Cue times of a sync project (`crate::sync`).
pub const SYNC: &str = "sync.json";
pub const DIRS: &[&str] = &["takes", "assets", "out"];
pub const FORMAT: u32 = 1;
/// Speaking-rate estimate for `open`.
pub const CHARS_PER_S: f64 = 13.0;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(deny_unknown_fields)]
pub struct Speaker {
    /// Persona name (`<voices>/<name>.toml` or implicit omnivoice builtin); unset until cast.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub persona: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    /// podcast | hoerbuch | video
    pub loudness: String,
    /// Default GAP for imported lines (s).
    pub gap: f64,
    /// Spoken AI disclosure as closing line on export (first speaker). The ID3 tags (COMM + TXXX AI_GENERATED) are set either way.
    #[serde(skip_serializing_if = "is_true")]
    pub disclosure: bool,
    /// Disclosure text; default by `lang` (de, en), other languages need it set.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disclosure_text: Option<String>,
    /// Quality last used in the app (draft | final); the app opens the project with it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_quality: Option<crate::render::Quality>,
}

fn is_true(b: &bool) -> bool {
    *b
}

impl Default for Settings {
    fn default() -> Self {
        Self { loudness: "podcast".into(), gap: 0.5, disclosure: true, disclosure_text: None, last_quality: None }
    }
}

/// Default disclosure per language (`de`, `de-AT`, `en`, …).
pub fn default_disclosure(lang: &str) -> Option<&'static str> {
    match lang.split('-').next()? {
        "de" => Some("Diese Aufnahme wurde mit KI-Stimmen erzeugt."),
        "en" => Some("This recording was made with AI-generated voices."),
        _ => None,
    }
}

impl Meta {
    /// Configured or default disclosure text (None: language without default and nothing set).
    pub fn disclosure_text(&self) -> Option<String> {
        self.settings.disclosure_text.clone().filter(|t| !t.trim().is_empty()).or_else(|| default_disclosure(&self.lang).map(String::from))
    }
}

/// `[llm]`: which backend of the app settings (`llm_cfg`) this project uses – only the name and the consent switch.
/// `url`, `key`, `model` etc. are refused here: a project file must never redirect a key or text to its own host.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default, deny_unknown_fields)]
pub struct ProjectLlm {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backend: Option<String>,
    /// Consent: this project's text may go to an online provider (the backend also needs `allow_online` in the settings).
    pub online_ok: bool,
}

/// Unknown fields are refused so a save never drops data silently.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Meta {
    pub format: u32,
    pub title: String,
    #[serde(default = "de")]
    pub lang: String,
    #[serde(default)]
    pub settings: Settings,
    /// Speaker ID -> persona; insertion order = cast order.
    #[serde(default)]
    pub speakers: IndexMap<String, Speaker>,
    /// Ducking/loop settings of the cues.
    #[serde(default, skip_serializing_if = "MusicCfg::is_default")]
    pub music: MusicCfg,
    /// Music/ambience cues (`crate::cues`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cues: Vec<Cue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub llm: Option<ProjectLlm>,
}

fn de() -> String {
    "de".into()
}

impl Meta {
    pub fn new(title: &str, lang: &str) -> Self {
        Meta { format: FORMAT, title: title.into(), lang: lang.into(), settings: Settings::default(), speakers: IndexMap::new(), music: MusicCfg::default(), cues: vec![], llm: None }
    }

    pub fn parse(s: &str) -> Result<Self> {
        let m: Self = toml::from_str(s)?;
        if m.format != FORMAT {
            bail!("project.toml format {} nicht unterstützt (erwartet {FORMAT})", m.format);
        }
        for id in m.speakers.keys() {
            if !crate::script::id_ok(id) {
                bail!("Sprecher-ID {id:?} ungültig (Buchstaben, Ziffern, _ -; nicht B – reserviert für Einwürfe)");
            }
        }
        Ok(m)
    }

    pub fn to_toml(&self) -> Result<String> {
        Ok(toml::to_string_pretty(self)?)
    }
}

/// `old` with every value that differs from `new` replaced (tables recursively, keys missing in `new` removed), so
/// comments and formatting of unchanged entries survive. None if `old` does not parse.
pub fn merge_toml(old: &str, new: &str) -> Option<String> {
    use toml_edit::{DocumentMut, Item, Table};
    fn sem(k: &str, it: &Item) -> Option<toml::Table> {
        let mut d = DocumentMut::new();
        d.insert(k, it.clone());
        toml::from_str(&d.to_string()).ok()
    }
    fn merge(o: &mut Table, n: &Table) {
        let gone: Vec<String> = o.iter().map(|(k, _)| k.to_string()).filter(|k| !n.contains_key(k)).collect();
        gone.iter().for_each(|k| drop(o.remove(k)));
        for (k, ni) in n.iter() {
            match (o.get_mut(k), ni) {
                (Some(Item::Table(ot)), Item::Table(nt)) => merge(ot, nt),
                (Some(oi), _) if sem(k, oi).is_some() && sem(k, oi) == sem(k, ni) => {}
                (Some(oi), Item::Value(nv)) if oi.is_value() => {
                    let d = oi.as_value().map(|v| v.decor().clone());
                    *oi = Item::Value(nv.clone());
                    if let (Some(d), Some(v)) = (d, oi.as_value_mut()) {
                        *v.decor_mut() = d;
                    }
                }
                _ => {
                    o.insert(k, ni.clone());
                }
            }
        }
    }
    let mut o: DocumentMut = old.parse().ok()?;
    let n: DocumentMut = new.parse().ok()?;
    merge(o.as_table_mut(), n.as_table());
    Some(o.to_string())
}

/// Spoken length estimate of a line: words / CHARS_PER_S + `[pause]`s.
pub fn est_secs(text: &str) -> f64 {
    est_secs_at(text, CHARS_PER_S)
}

/// Spoken characters of a line (tags removed, whitespace collapsed).
pub fn spoken_chars(text: &str) -> usize {
    funkhaus_audio::script::plain(text).split_whitespace().collect::<Vec<_>>().join(" ").chars().count()
}

/// `est_secs` at `cps` spoken characters per second.
pub fn est_secs_at(text: &str, cps: f64) -> f64 {
    use funkhaus_audio::script::{Seg, segments};
    spoken_chars(text) as f64 / cps + segments(text).unwrap_or_default().iter().map(|s| if let Seg::Pause(p) = s { *p } else { 0.0 }).sum::<f64>()
}

#[derive(Clone, Debug)]
pub struct Project {
    pub dir: PathBuf,
    pub meta: Meta,
    pub script: Script,
}

/// Write via temp file + rename.
pub fn write(p: &Path, s: &str) -> Result<()> {
    let tmp = p.with_extension("tmp");
    std::fs::write(&tmp, s).with_context(|| tmp.display().to_string())?;
    Ok(std::fs::rename(&tmp, p)?)
}

/// `Project::save_meta` without loading the script: writes `meta` into `<dir>/project.toml` (comments kept).
pub fn save_meta_at(dir: &Path, meta: &Meta) -> Result<()> {
    let f = dir.join(FILE);
    let new = meta.to_toml()?;
    // the merged text must mean exactly `meta`, else plain rewrite
    let out = std::fs::read_to_string(&f).ok().and_then(|old| merge_toml(&old, &new)).filter(|m| Meta::parse(m).is_ok_and(|m| m == *meta)).unwrap_or(new);
    write(&f, &out)
}

/// A project name typed by the user: folder stem and title in one.
pub fn name_ok(n: &str) -> Result<()> {
    if n.is_empty() {
        bail!("Gib dem Projekt einen Namen.");
    }
    if n.contains('/') || n.contains('\0') || n.starts_with('.') {
        bail!("Der Name darf keinen Schrägstrich enthalten und nicht mit einem Punkt beginnen.");
    }
    Ok(())
}

/// One row of `list`.
#[derive(Serialize, Debug, Clone, PartialEq)]
pub struct Info {
    /// folder stem (`<name>.studio`)
    pub name: String,
    pub path: PathBuf,
    pub title: String,
    /// last change of project.toml / script.txt (RFC 3339, local time)
    pub modified: String,
    pub lines: usize,
    pub sync: bool,
    /// bytes of the whole folder
    pub size: u64,
}

fn dir_size(d: &Path) -> u64 {
    std::fs::read_dir(d).into_iter().flatten().flatten().map(|e| match e.metadata() { Ok(m) if m.is_dir() => dir_size(&e.path()), Ok(m) => m.len(), Err(_) => 0 }).sum()
}

/// Every loadable `<name>.studio` in the projects dir, by name.
pub fn list() -> Vec<Info> {
    let mut v: Vec<Info> = std::fs::read_dir(crate::projects_dir())
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == crate::EXT))
        .filter_map(|d| info(&d).ok())
        .collect();
    v.sort_by(|a, b| a.name.cmp(&b.name));
    v
}

/// `Info` of one project dir.
pub fn info(d: &Path) -> Result<Info> {
    let p = Project::load(d)?;
    let m = [FILE, SCRIPT].iter().filter_map(|f| std::fs::metadata(d.join(f)).and_then(|m| m.modified()).ok()).max();
    let modified = m.map(|t| chrono::DateTime::<chrono::Local>::from(t).to_rfc3339_opts(chrono::SecondsFormat::Secs, false)).unwrap_or_default();
    Ok(Info { name: stem(d), path: d.to_path_buf(), title: p.meta.title, modified, lines: p.script.lines().count(), sync: d.join(SYNC).is_file(), size: dir_size(d) })
}

/// Folder name without `.studio`.
pub fn stem(d: &Path) -> String {
    d.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
}

/// Which name `rename(dir, name)` would refuse, as the message to show while typing (None: fine).
pub fn rename_check(dir: &Path, name: &str) -> Option<String> {
    let name = name.trim();
    if let Err(e) = name_ok(name) {
        return Some(e.to_string());
    }
    let new = dir.with_file_name(format!("{name}.{}", crate::EXT));
    (new != dir && new.exists()).then(|| format!("„{name}“ gibt es schon ({})", new.display()))
}

/// Renames project `dir` to `<name>.studio` beside it and sets its title; refused while a job holds the project lock.
/// Absolute paths into the folder follow: `takes/voices.tsv` (derived audio index), the GUI's recent list (gui.json).
/// Returns the new dir.
pub fn rename(dir: &Path, name: &str) -> Result<PathBuf> {
    let name = name.trim();
    if let Some(why) = rename_check(dir, name) {
        bail!("{why}");
    }
    let new = dir.with_file_name(format!("{name}.{}", crate::EXT));
    let mut m = Meta::load(dir)?;
    let _l = crate::render::lock_dir(dir, std::time::Duration::from_millis(300), "rename", None)?;
    m.title = name.into();
    save_meta_at(dir, &m)?;
    if new != dir {
        std::fs::rename(dir, &new).with_context(|| format!("{} → {}", dir.display(), new.display()))?;
        // the index holds `std::path::absolute` paths (no symlinks resolved), gui.json whatever the GUI opened: try both spellings
        for (o, n) in [(norm(dir), norm(&new)), (canon(dir), canon(&new))] {
            crate::derived::rewrite_prefix(&new.join("takes"), &o, &n)?;
            gui_json_update(&o, Some(&n))?;
        }
    }
    Ok(new)
}

/// Absolute without resolving symlinks, `.`/`..` collapsed lexically (how `derived` records paths).
fn norm(p: &Path) -> PathBuf {
    use std::path::Component as C;
    let mut r = PathBuf::new();
    for c in std::path::absolute(p).unwrap_or(p.into()).components() {
        match c {
            C::CurDir => {}
            C::ParentDir => drop(r.pop()),
            c => r.push(c),
        }
    }
    r
}

/// Symlinks of the parent resolved (the dir itself may be gone already).
fn canon(p: &Path) -> PathBuf {
    let n = norm(p);
    n.parent().and_then(|d| std::fs::canonicalize(d).ok()).and_then(|d| n.file_name().map(|f| d.join(f))).unwrap_or(n)
}

/// `<home>/gui.json` `recent`: `old` replaced by `new` or removed (None). Other fields untouched; no file: nothing.
fn gui_json_update(old: &Path, new: Option<&Path>) -> Result<()> {
    gui_json_update_in(&crate::home(), old, new)
}

fn gui_json_update_in(home: &Path, old: &Path, new: Option<&Path>) -> Result<()> {
    let f = home.join("gui.json");
    let Ok(s) = std::fs::read_to_string(&f) else { return Ok(()) };
    let mut v: serde_json::Value = serde_json::from_str(&s).with_context(|| f.display().to_string())?;
    let Some(rec) = v.get_mut("recent").and_then(|r| r.as_array_mut()) else { return Ok(()) };
    let before = rec.clone();
    rec.retain(|r| r.as_str().is_none_or(|r| Path::new(r) != old));
    if let Some(n) = new
        && rec.len() < before.len()
    {
        rec.insert(0, serde_json::Value::String(n.display().to_string()));
    }
    if *rec != before {
        write(&f, &serde_json::to_string_pretty(&v)?)?;
    }
    Ok(())
}

/// What `trash` did.
#[derive(Serialize, Debug, Clone, PartialEq)]
pub struct Trashed {
    pub name: String,
    pub path: PathBuf,
    /// recorded ref-voice audio from outside the folder (`export --out`, original paths): moved into
    /// `<dir>/out/extern/` first, so it travels with the project (and keeps its index line for a restore)
    pub moved_in: Vec<PathBuf>,
    /// could not be moved in (other volume, not a plain audio file): stays where it is, its index line now in
    /// `<home>/derived/voices.tsv` so `voice revoke|rm` still finds it
    pub outside: Vec<PathBuf>,
}

/// Note for everything that trashes (step-1 dialog, CLI): the Trash is outside every index.
pub const TRASH_NOTE: &str = "Widerrufene Stimmen werden im Papierkorb nicht gelöscht – Papierkorb danach leeren.";

/// Subfolder of `out/` that receives the project's elsewhere-exports before the move.
pub const EXTERN: &str = "extern";

/// Moves project `dir` into the Trash (recoverable) with everything in it; refused while a job holds the project lock
/// (voices are global and stay; the folder's derived index goes with it). The GUI's recent list forgets the project.
pub fn trash(dir: &Path) -> Result<Trashed> {
    trash_with(dir, &crate::home(), &move_to_trash, &|a, b| std::fs::rename(a, b))
}

/// `trash` with the folder move done by `mv` (tests: a folder instead of the real Trash) and single-file moves by
/// `mv_in` (tests: a failing one stands in for another volume, EXDEV).
pub fn trash_with(dir: &Path, home: &Path, mv: &dyn Fn(&Path) -> Result<()>, mv_in: &dyn Fn(&Path, &Path) -> std::io::Result<()>) -> Result<Trashed> {
    Meta::load(dir)?;
    let (abs, c) = (norm(dir), canon(dir));
    let takes = dir.join("takes");
    let mut found = crate::derived::outside(&takes, &abs);
    found.retain(|p| !p.starts_with(&c));
    let (mut moved_in, mut outside) = (vec![], vec![]);
    {
        let _l = crate::render::lock_dir(dir, std::time::Duration::from_millis(300), "trash", None)?;
        let ext = dir.join("out").join(EXTERN);
        let mut moves = vec![];
        for p in found {
            let to = (crate::derived::check(&p, crate::derived::AUDIO) == crate::derived::Found::Plain)
                .then(|| std::fs::create_dir_all(&ext).ok().and_then(|_| p.file_name().map(|n| free_name(&ext, n))))
                .flatten()
                .filter(|to| mv_in(&p, to).is_ok());
            match to {
                Some(to) => {
                    moves.push((p.clone(), abs.join("out").join(EXTERN).join(to.file_name().unwrap_or_default())));
                    moved_in.push(p);
                }
                None => outside.push(p),
            }
        }
        crate::derived::relocate(&takes, &moves)?;
        crate::derived::transfer(&takes, &crate::derived::home_index(home), &outside)?;
    }
    mv(dir)?;
    gui_json_update_in(home, &abs, None)?;
    gui_json_update_in(home, &c, None)?;
    Ok(Trashed { name: stem(dir), path: abs, moved_in, outside })
}

/// `<dir>/<name>`, or `<stem> 2.<ext>`, `<stem> 3.<ext>`, … if taken (the extension stays, so a purge still
/// recognises the file as audio).
fn free_name(dir: &Path, name: &std::ffi::OsStr) -> PathBuf {
    let n = Path::new(name);
    let (stem, ext) = (n.file_stem().unwrap_or(name).to_string_lossy(), n.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default());
    let mut to = dir.join(name);
    for i in 2.. {
        if !to.exists() {
            break;
        }
        to = dir.join(format!("{stem} {i}{ext}"));
    }
    to
}

/// Set in tests: `move_to_trash` moves into this folder instead of the Trash.
pub const TRASH_DIR_ENV: &str = "STIMMWERK_TRASH_DIR";

/// macOS Trash via NSFileManager (no Finder, no prompt).
pub fn move_to_trash(p: &Path) -> Result<()> {
    if let Some(d) = std::env::var_os(TRASH_DIR_ENV).filter(|v| !v.is_empty()) {
        let d = PathBuf::from(d);
        std::fs::create_dir_all(&d)?;
        let to = free_name(&d, p.file_name().context("kein Dateiname")?);
        return std::fs::rename(p, &to).with_context(|| format!("{} → {}", p.display(), to.display()));
    }
    #[cfg(target_os = "macos")]
    {
        use objc2_foundation::{NSFileManager, NSString, NSURL};
        let s = NSString::from_str(p.to_str().context("Pfad ist kein UTF-8")?);
        let url = NSURL::fileURLWithPath(&s);
        NSFileManager::defaultManager().trashItemAtURL_resultingItemURL_error(&url, None).map_err(|e| anyhow::anyhow!("Papierkorb: {}", e.localizedDescription()))
    }
    #[cfg(not(target_os = "macos"))]
    bail!("Papierkorb nur auf macOS")
}

impl Meta {
    /// Only `<dir>/project.toml` (no script.txt).
    pub fn load(dir: &Path) -> Result<Meta> {
        let f = dir.join(FILE);
        let s = std::fs::read_to_string(&f).with_context(|| format!("kein Stimmwerk-Projekt: {} fehlt", f.display()))?;
        Meta::parse(&s).with_context(|| f.display().to_string())
    }
}

impl Project {
    /// Creates `dir` (must not exist or be empty) with project.toml, empty script.txt + lexicon.tsv and the subdirs.
    pub fn create(dir: &Path, title: &str, lang: &str) -> Result<Self> {
        if dir.exists() && std::fs::read_dir(dir)?.next().is_some() {
            bail!("{} existiert schon und ist nicht leer", dir.display());
        }
        for d in DIRS {
            std::fs::create_dir_all(dir.join(d))?;
        }
        std::fs::write(dir.join(LEXICON), "")?;
        let p = Project { dir: dir.into(), meta: Meta::new(title, lang), script: Script::default() };
        p.save()?;
        Ok(p)
    }

    pub fn load(dir: &Path) -> Result<Self> {
        let f = dir.join(FILE);
        let s = std::fs::read_to_string(&f).with_context(|| format!("kein Stimmwerk-Projekt: {} fehlt", f.display()))?;
        let meta = Meta::parse(&s).with_context(|| f.display().to_string())?;
        let script = Script::parse(&std::fs::read_to_string(dir.join(SCRIPT)).unwrap_or_default()).with_context(|| dir.join(SCRIPT).display().to_string())?;
        Ok(Project { dir: dir.into(), meta, script })
    }

    /// Writes project.toml and script.txt.
    pub fn save(&self) -> Result<()> {
        write(&self.dir.join(FILE), &self.meta.to_toml()?)?;
        write(&self.dir.join(SCRIPT), &self.script.format())
    }

    /// Writes only project.toml (cast, settings), so edits of script.txt in an open editor are never overwritten.
    /// Comments and layout of unchanged parts stay (`merge_toml`).
    pub fn save_meta(&self) -> Result<()> {
        save_meta_at(&self.dir, &self.meta)
    }

    pub fn lexicon_path(&self) -> PathBuf {
        self.dir.join(LEXICON)
    }

    /// Overview for `open`: speakers with line counts, lines, chapters, estimated duration
    /// (spoken chars / CHARS_PER_S + pauses + positive gaps of main lines).
    pub fn summary(&self) -> serde_json::Value {
        use funkhaus_audio::script::{Seg, plain, segments};
        let ls: Vec<_> = self.script.lines().collect();
        let chars: usize = ls.iter().map(|l| plain(&l.text).split_whitespace().collect::<Vec<_>>().join(" ").chars().count()).sum();
        let pauses: f64 = ls.iter().flat_map(|l| segments(&l.text).unwrap_or_default()).map(|s| if let Seg::Pause(p) = s { p } else { 0.0 }).sum();
        let gaps: f64 = ls.iter().filter(|l| !l.over).map(|l| l.gap.max(0.0) as f64).sum();
        let est = chars as f64 / CHARS_PER_S + pauses + gaps;
        let speakers: Vec<_> = self
            .meta
            .speakers
            .iter()
            .map(|(id, s)| serde_json::json!({"id": id, "persona": s.persona, "label": s.label, "lines": ls.iter().filter(|l| &l.who == id).count()}))
            .collect();
        let chapters = self.script.items.iter().filter(|i| matches!(i, crate::script::Item::Note(n) if n.starts_with("# SEG"))).count();
        serde_json::json!({
            "dir": self.dir, "title": self.meta.title, "lang": self.meta.lang, "settings": self.meta.settings, "speakers": speakers,
            "lines": ls.len(), "overlays": ls.iter().filter(|l| l.over).count(), "chapters": chapters, "chars": chars, "bad_lines": self.script.bad(),
            "est_secs": (est * 10.0).round() / 10.0, "est": funkhaus_sys::mmss(est), "cues": self.meta.cues.len(),
            "sync": self.dir.join(SYNC).is_file(),
        })
    }

    /// Adds a speaker if missing (persona unset); true if added.
    pub fn add_speaker(&mut self, id: &str, label: Option<&str>) -> bool {
        if self.meta.speakers.contains_key(id) {
            return false;
        }
        self.meta.speakers.insert(id.into(), Speaker { persona: None, label: label.map(String::from) });
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIX: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/stimmwerk/project/leuchtturm.studio");

    #[test]
    fn fixture_roundtrip_bit_identical() {
        let src = Path::new(FIX);
        let t = tempfile::tempdir().unwrap();
        let d = t.path().join("p.studio");
        std::fs::create_dir_all(&d).unwrap();
        for f in [FILE, SCRIPT] {
            std::fs::copy(src.join(f), d.join(f)).unwrap();
        }
        let p = Project::load(&d).unwrap();
        assert_eq!(p.meta.speakers.keys().collect::<Vec<_>>(), ["ERZ", "Mara", "Jonas"]);
        assert_eq!(p.meta.speakers["Mara"].label.as_deref(), Some("Mara Lind"));
        let v = p.summary();
        assert_eq!((v["lines"].as_u64(), v["overlays"].as_u64(), v["chapters"].as_u64()), (Some(7), Some(1), Some(2)));
        assert_eq!((v["speakers"][1]["lines"].as_u64(), v["speakers"][2]["lines"].as_u64()), (Some(2), Some(3)));
        p.save().unwrap();
        for f in [FILE, SCRIPT] {
            assert_eq!(std::fs::read(d.join(f)).unwrap(), std::fs::read(src.join(f)).unwrap(), "{f}");
        }
    }

    #[test]
    fn rename_follows_paths_and_trash_needs_free_lock() {
        let t = tempfile::tempdir().unwrap();
        let d = t.path().join("alt.studio");
        let p = Project::create(&d, "Alt", "de").unwrap();
        let wav = d.join("takes/x.wav");
        std::fs::write(&wav, b"x").unwrap();
        let ext = t.path().join("export.wav");
        std::fs::write(&ext, b"x").unwrap();
        crate::derived::record(&d.join("takes"), "sha1", "p", &[wav.clone(), ext.clone()]).unwrap();
        for bad in ["", "  ", "a/b", ".hidden"] {
            assert!(rename(&d, bad).is_err() && rename_check(&d, bad).is_some(), "{bad:?}");
        }
        let other = t.path().join("da.studio");
        Project::create(&other, "Da", "de").unwrap();
        assert!(rename(&d, "da").unwrap_err().to_string().contains("gibt es schon"));
        assert!(rename_check(&d, "da").unwrap().contains("gibt es schon") && rename_check(&d, "alt").is_none() && rename_check(&d, "neu").is_none());
        // busy: a render holds the lock
        let held = crate::render::lock(&p).unwrap();
        let no_mv = |_: &Path, _: &Path| -> std::io::Result<()> { panic!("must not move") };
        assert!(rename(&d, "neu").is_err() && trash_with(&d, t.path(), &|_| panic!("must not move"), &no_mv).is_err());
        drop(held);
        let n = rename(&d, " Neu Ä ").unwrap();
        assert_eq!(n, t.path().join("Neu Ä.studio"));
        assert!(!d.exists() && n.join("takes/x.wav").is_file());
        assert_eq!(Project::load(&n).unwrap().meta.title, "Neu Ä");
        // derived index: the take's line points into the new folder, the external export's line is unchanged
        let idx = std::fs::read_to_string(n.join("takes").join(crate::derived::INDEX)).unwrap();
        assert!(idx.contains(&format!("\t{}\t", n.join("takes/x.wav").display())) && !idx.contains("alt.studio") && idx.contains(&ext.display().to_string()), "{idx}");
        assert_eq!(crate::derived::outside(&n.join("takes"), &n), std::slice::from_ref(&ext));
        // same name again: title only, folder stays
        assert_eq!(rename(&n, "Neu Ä").unwrap(), n);
        assert!(trash_with(&t.path().join("nope.studio"), t.path(), &|_| Ok(()), &no_mv).is_err(), "not a project");
        let bin = t.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        // a second elsewhere-export with the same file name, one on "another volume" (rename fails), a non-audio line
        let ext2 = t.path().join("zwei/export.wav");
        std::fs::create_dir_all(ext2.parent().unwrap()).unwrap();
        std::fs::write(&ext2, b"y").unwrap();
        let (far, doc) = (t.path().join("fern.wav"), t.path().join("notiz.txt"));
        std::fs::write(&far, b"f").unwrap();
        std::fs::write(&doc, b"d").unwrap();
        crate::derived::record(&n.join("takes"), "sha1", "p", &[ext2.clone(), far.clone(), doc.clone()]).unwrap();
        let mv_in = |a: &Path, b: &Path| if a == far { Err(std::io::Error::other("EXDEV")) } else { std::fs::rename(a, b) };
        let r = trash_with(&n, t.path(), &|p| Ok(std::fs::rename(p, bin.join(p.file_name().unwrap()))?), &mv_in).unwrap();
        assert_eq!((r.name.as_str(), &r.moved_in, &r.outside), ("Neu Ä", &vec![ext.clone(), ext2.clone()], &vec![far.clone(), doc.clone()]));
        let gone = bin.join("Neu Ä.studio");
        assert!(!n.exists() && other.exists() && gone.join("takes/x.wav").is_file());
        // exports travelled with the folder (no name clash), their lines point at the pre-trash place inside the project
        assert!(!ext.exists() && !ext2.exists() && gone.join("out/extern/export.wav").is_file() && gone.join("out/extern/export 2.wav").is_file());
        let idx = std::fs::read_to_string(gone.join("takes").join(crate::derived::INDEX)).unwrap();
        assert!(idx.contains(&format!("\t{}\t", n.join("out/extern/export.wav").display())) && idx.contains(&format!("\t{}\t", n.join("out/extern/export 2.wav").display())), "{idx}");
        assert!(!idx.contains("fern.wav") && !idx.contains("notiz.txt"), "{idx}");
        // the unmovable ones stay, indexed at the data root (stamp intact: a purge there deletes the audio, refuses the doc)
        assert!(far.exists() && doc.exists());
        let hi = crate::derived::home_index(t.path());
        let h = std::fs::read_to_string(hi.join(crate::derived::INDEX)).unwrap();
        assert!(h.contains(&far.display().to_string()) && h.contains(&doc.display().to_string()) && h.lines().count() == 2, "{h}");
        let (del, refused) = crate::derived::purge(&hi, &["sha1".into()], "p").unwrap();
        assert_eq!((del, refused.len()), (vec![far.clone()], 1));
        assert!(!far.exists() && doc.exists());
    }

    #[test]
    fn free_name_keeps_extension() {
        let t = tempfile::tempdir().unwrap();
        let n = std::ffi::OsStr::new("a.wav");
        assert_eq!(free_name(t.path(), n), t.path().join("a.wav"));
        std::fs::write(t.path().join("a.wav"), b"").unwrap();
        assert_eq!(free_name(t.path(), n), t.path().join("a 2.wav"));
        std::fs::write(t.path().join("a 2.wav"), b"").unwrap();
        assert_eq!(free_name(t.path(), n), t.path().join("a 3.wav"));
        std::fs::create_dir(t.path().join("p.studio")).unwrap();
        assert_eq!(free_name(t.path(), std::ffi::OsStr::new("p.studio")), t.path().join("p 2.studio"));
    }

    /// `rename` with a relative path (CLI `stimmwerk project mv ./x neu` from inside the home) still rewrites the
    /// absolute index paths.
    #[test]
    fn rename_relative_dir_rewrites_absolute_index() {
        let t = tempfile::tempdir().unwrap();
        let d = t.path().join("rel.studio");
        Project::create(&d, "Rel", "de").unwrap();
        let wav = d.join("takes/x.wav");
        std::fs::write(&wav, b"x").unwrap();
        crate::derived::record(&d.join("takes"), "s", "p", &[wav]).unwrap();
        let home = std::env::current_dir().unwrap();
        let rel = pathdiff(&home, &d);
        let n = rename(&rel, "neu").unwrap();
        assert!(n.is_relative() == rel.is_relative(), "{n:?}");
        let idx = std::fs::read_to_string(std::path::absolute(&n).unwrap().join("takes").join(crate::derived::INDEX)).unwrap();
        assert!(idx.contains("neu.studio/takes/x.wav") && !idx.contains("rel.studio"), "{idx}");
    }

    /// `..`-relative path from `base` to `to` (both absolute), for the test above.
    fn pathdiff(base: &Path, to: &Path) -> PathBuf {
        let (b, t): (Vec<_>, Vec<_>) = (base.components().collect(), to.components().collect());
        let common = b.iter().zip(&t).take_while(|(x, y)| x == y).count();
        let mut r = PathBuf::new();
        (common..b.len()).for_each(|_| r.push(".."));
        t[common..].iter().for_each(|c| r.push(c));
        r
    }

    #[test]
    fn gui_json_recent_follows_rename_and_trash() {
        let t = tempfile::tempdir().unwrap();
        let (a, b) = (t.path().join("a.studio"), t.path().join("b.studio"));
        let f = t.path().join("gui.json");
        // no file: nothing happens, no error
        gui_json_update_in(t.path(), &a, Some(&b)).unwrap();
        assert!(!f.exists());
        std::fs::write(&f, format!("{{\"theme\": \"hell\", \"recent\": [\"{}\", \"{}\"]}}", t.path().join("x.studio").display(), a.display())).unwrap();
        gui_json_update_in(t.path(), &a, Some(&b)).unwrap();
        let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&f).unwrap()).unwrap();
        assert_eq!(v["theme"], "hell");
        assert_eq!(v["recent"][0].as_str(), b.to_str());
        assert_eq!(v["recent"][1].as_str(), t.path().join("x.studio").to_str());
        gui_json_update_in(t.path(), &b, None).unwrap();
        let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&f).unwrap()).unwrap();
        assert_eq!(v["recent"].as_array().unwrap().len(), 1);
        // unknown project: file untouched (byte for byte)
        let s = std::fs::read_to_string(&f).unwrap();
        gui_json_update_in(t.path(), &a, None).unwrap();
        assert_eq!(std::fs::read_to_string(&f).unwrap(), s);
    }

    #[test]
    fn info_and_fake_trash_dir() {
        let t = tempfile::tempdir().unwrap();
        let d = t.path().join("p.studio");
        std::fs::create_dir_all(&d).unwrap();
        for f in [FILE, SCRIPT] {
            std::fs::copy(Path::new(FIX).join(f), d.join(f)).unwrap();
        }
        let i = info(&d).unwrap();
        assert_eq!((i.name.as_str(), i.title.as_str(), i.lines, i.sync), ("p", "Der Leuchtturm", 7, false));
        assert!(i.size > 100 && i.modified.starts_with("20"), "{i:?}");
        assert!(info(t.path()).is_err());
        // STIMMWERK_TRASH_DIR is read by the CLI/GUI tests' processes; here the env-free fake path is exercised directly
        assert!(dir_size(&d) == i.size);
    }

    /// Real macOS Trash: only on request (`STIMMWERK_TEST_TRASH=1 cargo test -- --ignored`), moves a temp folder.
    #[test]
    #[ignore]
    fn move_to_trash_macos() {
        if std::env::var_os("STIMMWERK_TEST_TRASH").is_none() {
            return;
        }
        let t = tempfile::tempdir().unwrap();
        let d = t.path().join("stimmwerk-trash-test.studio");
        std::fs::create_dir_all(&d).unwrap();
        move_to_trash(&d).unwrap();
        assert!(!d.exists());
    }

    #[test]
    fn create_load_save() {
        let t = tempfile::tempdir().unwrap();
        let d = t.path().join("neu.studio");
        let mut p = Project::create(&d, "Neu", "de").unwrap();
        assert!(DIRS.iter().all(|x| d.join(x).is_dir()) && d.join(LEXICON).is_file());
        assert!(Project::create(&d, "Neu", "de").is_err());
        assert!(p.add_speaker("ERZ", None) && !p.add_speaker("ERZ", None));
        p.meta.speakers["ERZ"].persona = Some("gt".into());
        p.save().unwrap();
        let s = std::fs::read_to_string(d.join(FILE)).unwrap();
        let q = Project::load(&d).unwrap();
        assert_eq!(q.meta, p.meta);
        q.save().unwrap();
        assert_eq!(std::fs::read_to_string(d.join(FILE)).unwrap(), s);
        assert!(s.contains("format = 1") && s.contains("[speakers.ERZ]\npersona = \"gt\""), "{s}");
    }

    #[test]
    fn gap_roundtrips_exactly() {
        let mut m = Meta::new("x", "de");
        m.settings.gap = 0.3;
        let s = m.to_toml().unwrap();
        assert!(s.contains("gap = 0.3\n"), "{s}");
        assert_eq!(Meta::parse(&s).unwrap(), m);
    }

    #[test]
    fn refuses_unknown_fields_and_format() {
        assert!(Meta::parse("format = 1\ntitle = \"x\"\nfoo = 1\n").is_err());
        assert!(Meta::parse("format = 2\ntitle = \"x\"\n").is_err());
        let m = Meta::parse("format = 1\ntitle = \"x\"\nlang = \"fr\"\n[settings]\ndisclosure = false\n").unwrap();
        assert!(!m.settings.disclosure && m.disclosure_text().is_none() && m.to_toml().unwrap().contains("disclosure = false"));
        assert_eq!(Meta::new("x", "de-AT").disclosure_text().as_deref(), default_disclosure("de"));
        assert!(Meta::parse("format = 1\ntitle = \"x\"\n[speakers.B]\n").is_err());
        // cues are typed now (`crate::cues`): the old free-form `at` is refused
        assert!(Meta::parse("format = 1\ntitle = \"x\"\n[[cues]]\nat = 3\n").is_err());
        let m = Meta::parse("format = 1\ntitle = \"x\"\n[[cues]]\nat_line = 3\nfile = \"assets/a.wav\"\nkind = \"music\"\n").unwrap();
        assert_eq!((m.cues.len(), m.cues[0].at_line), (1, Some(3)));
        // last_quality: optional, roundtrips, only draft|final; unknown settings fields still refused
        assert_eq!(m.settings.last_quality, None);
        let mut m = Meta::new("x", "de");
        m.settings.last_quality = Some(crate::render::Quality::Draft);
        let s = m.to_toml().unwrap();
        assert!(s.contains("last_quality = \"draft\""), "{s}");
        assert_eq!(Meta::parse(&s).unwrap(), m);
        assert!(Meta::parse("format = 1\ntitle = \"x\"\n[settings]\nlast_quality = \"mittel\"\n").is_err());
        assert!(Meta::parse("format = 1\ntitle = \"x\"\n[settings]\nlast_qualitaet = \"final\"\n").is_err());
    }

    #[test]
    fn project_llm_names_backend_only() {
        let m = Meta::parse("format = 1\ntitle = \"x\"\n[llm]\nbackend = \"claude\"\nonline_ok = true\n").unwrap();
        assert_eq!(m.llm, Some(ProjectLlm { backend: Some("claude".into()), online_ok: true }));
        let s = m.to_toml().unwrap();
        assert!(s.contains("[llm]\nbackend = \"claude\"\nonline_ok = true\n"), "{s}");
        assert_eq!(Meta::parse(&s).unwrap(), m);
        assert_eq!(Meta::parse("format = 1\ntitle = \"x\"\n[llm]\nbackend = \"gemma\"\n").unwrap().llm.unwrap().online_ok, false);
        assert_eq!(Meta::parse("format = 1\ntitle = \"x\"\n").unwrap().llm, None);
        assert!(!Meta::new("x", "de").to_toml().unwrap().contains("llm"));
        // every backend field and secret is refused in the project file
        for bad in ["url = \"http://evil.example/v1\"", "api_key = \"sk-x\"", "key = { env = \"X\" }", "kind = \"anthropic\"", "model = \"x\"", "online = true", "timeout_s = 5", "onlin_ok = true", "allow_online = true"] {
            assert!(Meta::parse(&format!("format = 1\ntitle = \"x\"\n[llm]\n{bad}\n")).is_err(), "{bad}");
        }
        assert!(Meta::parse("format = 1\ntitle = \"x\"\napi_key = \"sk-x\"\n").is_err());
    }

    #[test]
    fn save_meta_keeps_script_and_comments() {
        let t = tempfile::tempdir().unwrap();
        let d = t.path().join("p.studio");
        std::fs::create_dir_all(&d).unwrap();
        for f in [FILE, SCRIPT] {
            std::fs::copy(Path::new(FIX).join(f), d.join(f)).unwrap();
        }
        let toml = std::fs::read_to_string(d.join(FILE)).unwrap().replace("[speakers.Mara]", "# warm, ruhig\n[speakers.Mara]").replace("gap = 0.5", "gap = 0.5 # Standard");
        std::fs::write(d.join(FILE), &toml).unwrap();
        let mut p = Project::load(&d).unwrap();
        let edited = "ERZ|0|Im Editor geändert.\n";
        std::fs::write(d.join(SCRIPT), edited).unwrap();
        p.meta.speakers["Mara"].persona = Some("fable".into());
        p.add_speaker("Nina", None);
        p.meta.speakers.shift_remove("Jonas");
        p.save_meta().unwrap();
        assert_eq!(std::fs::read_to_string(d.join(SCRIPT)).unwrap(), edited);
        let s = std::fs::read_to_string(d.join(FILE)).unwrap();
        assert!(s.contains("# warm, ruhig\n[speakers.Mara]\npersona = \"fable\"") && s.contains("gap = 0.5 # Standard") && !s.contains("Jonas"), "{s}");
        assert_eq!(Meta::parse(&s).unwrap(), p.meta);
        // unchanged meta: file unchanged byte for byte
        Project::load(&d).unwrap().save_meta().unwrap();
        assert_eq!(std::fs::read_to_string(d.join(FILE)).unwrap(), s);
    }
}
