//! App state, job dispatch/poll, top bar. Tabs draw themselves (tabs/*.rs).
use crate::jobs::{self, Job, JobKind, LintOut, Msg, Out};
use crate::play::Player;
use crate::tabs::{self, Tab};
use crate::theme;
use eframe::egui::{self, RichText};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use stimmwerk_core::cues::{self, Cue, Gen};
use stimmwerk_core::render::{Event, Quality, State};
use stimmwerk_core::{Project, export, lint, llm_cfg, music, persona_dirs, prose, render, shorten, speakers, sync, voice};

/// Persisted UI settings (`<STIMMWERK_HOME>/gui.json`).
#[derive(Serialize, Deserialize, Default, Clone)]
pub struct Cfg {
    #[serde(default)]
    pub theme: String,
    #[serde(default)]
    pub recent: Vec<PathBuf>,
}

pub struct ExportUi {
    /// None = project setting
    pub preset: Option<export::Loudness>,
    pub kind: export::Kind,
    /// None = project setting
    pub disclosure: Option<bool>,
    pub res: Option<Box<export::Report>>,
}

/// "Stimme erzeugen" tab state (design + clone).
pub struct GenUi {
    /// index into `voice::PRESETS`
    pub preset: Option<usize>,
    pub instruct: String,
    pub n: usize,
    pub text: String,
    pub batch: Option<Box<voice::Batch>>,
    /// name field per candidate
    pub names: Vec<String>,
    /// persona name a candidate was kept as
    pub kept: Vec<Option<String>>,
    pub wav: String,
    pub start: String,
    pub end: String,
    pub ref_text: String,
    pub name: String,
    pub consent_open: bool,
    pub who: String,
    pub how: String,
    /// date of the consent (JJJJ-MM-TT), default today, editable
    pub consent_date: String,
    pub agreed: bool,
    /// last cloned persona (probe button)
    pub cloned: Option<String>,
    /// Stimmen tab: clone awaiting revoke confirmation
    pub revoke_ask: Option<String>,
    /// Stimmen tab: design awaiting delete confirmation
    pub rm_ask: Option<String>,
}

fn today() -> String {
    voice::today()
}

/// " Nicht erledigt: a; b" for skipped purge items (empty if none).
/// Own sentence after a partial revoke/rm: „ Nicht gelöscht (2): …“ (empty if nothing was skipped).
pub fn skipped_text(s: &[String]) -> String {
    if s.is_empty() { String::new() } else { format!(" Nicht gelöscht ({}): {}.", s.len(), s.join("; ")) }
}

fn files(n: usize) -> String {
    if n == 1 { "1 Datei".into() } else { format!("{n} Dateien") }
}

impl Default for GenUi {
    fn default() -> Self {
        GenUi {
            preset: None,
            instruct: String::new(),
            n: 3,
            text: voice::SAMPLE.into(),
            batch: None,
            names: vec![],
            kept: vec![],
            wav: String::new(),
            start: String::new(),
            end: String::new(),
            ref_text: String::new(),
            name: String::new(),
            consent_open: false,
            who: String::new(),
            how: String::new(),
            consent_date: today(),
            agreed: false,
            cloned: None,
            revoke_ask: None,
            rm_ask: None,
        }
    }
}

/// Add/edit dialog of a KI-Anbieter. `key` is typed here, handed to the keychain job and cleared – never shown again
/// (also not by `Debug`).
#[derive(Clone, Default)]
pub struct LlmDlg {
    /// name of the edited backend (None = new)
    pub orig: Option<String>,
    pub name: String,
    pub kind: funkhaus_llm::Kind,
    pub url: String,
    pub model: String,
    pub key: String,
    pub allow_online: bool,
    pub timeout_s: u64,
    pub err: Option<String>,
}

impl std::fmt::Debug for LlmDlg {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LlmDlg")
            .field("orig", &self.orig)
            .field("name", &self.name)
            .field("kind", &self.kind)
            .field("url", &self.url)
            .field("model", &self.model)
            .field("key", &if self.key.is_empty() { "" } else { "***" })
            .field("allow_online", &self.allow_online)
            .field("timeout_s", &self.timeout_s)
            .field("err", &self.err)
            .finish()
    }
}

impl LlmDlg {
    pub fn new() -> Self {
        LlmDlg { timeout_s: 900, ..Default::default() }
    }

    pub fn edit(name: &str, e: &llm_cfg::Entry) -> Self {
        LlmDlg { orig: Some(name.into()), name: name.into(), kind: e.kind, url: e.url.clone(), model: e.model.clone(), allow_online: e.allow_online, timeout_s: e.timeout_s, ..Default::default() }
    }

    pub fn preset(&mut self, p: &llm_cfg::Preset) {
        let e = p.entry();
        (self.name, self.kind, self.url, self.model, self.timeout_s, self.allow_online) = (p.name.into(), e.kind, e.url, e.model, e.timeout_s, false);
    }

    /// Non-loopback URL: text would leave the Mac.
    pub fn online(&self) -> bool {
        !funkhaus_llm::Backend { url: self.url.trim().into(), ..Default::default() }.is_local()
    }

    /// Entry from the fields; everything the dialog does not edit (`key`, `gpu`, `max_tokens`, `temperature`) stays as
    /// in the existing entry (the typed key goes through the keychain job).
    pub fn entry(&self, old: Option<&llm_cfg::Entry>) -> llm_cfg::Entry {
        llm_cfg::Entry { kind: self.kind, url: self.url.trim().into(), model: self.model.trim().into(), allow_online: self.allow_online, timeout_s: self.timeout_s, ..old.cloned().unwrap_or_default() }
    }
}

/// Einstellungen → KI-Anbieter state.
#[derive(Default)]
pub struct LlmUi {
    pub dlg: Option<LlmDlg>,
    /// backend awaiting the remove confirmation
    pub rm_ask: Option<String>,
    /// remove dialog: delete the keychain item too
    pub rm_key: bool,
    /// first "Online-Anbieter erlauben" tick awaits the confirmation dialog
    pub online_ask: bool,
    /// per backend: Ok(models) | Err((chip, detail)) of the last "Testen"
    pub status: BTreeMap<String, Result<usize, (String, String)>>,
    /// settings.toml did not load: (error, backup copy). Builtins are shown, nothing is written until it is fixed.
    pub broken: Option<(String, Option<PathBuf>)>,
}

/// "Aus Prosa erstellen …" dialog (Skript tab).
#[derive(Default)]
pub struct ProseUi {
    pub open: bool,
    pub path: String,
    pub text: String,
    /// preview; applied only with the confirm button
    pub res: Option<Box<prose::Outcome>>,
    pub err: Option<String>,
}

/// Musik tab state (generation form, import form, last results).
pub struct MusicUi {
    pub prompt: String,
    pub dur: u32,
    pub seed: u64,
    pub bpm: u32,
    pub key: String,
    pub last: Option<Box<music::Built>>,
    pub import_path: String,
    pub import_kind: cues::Kind,
    pub imported: Option<music::Imported>,
    /// placement of the next cue
    pub at_line: usize,
    pub loop_: bool,
    pub duck: bool,
    /// a running export was started as "Mix vorhören": play it when done
    pub preview: bool,
}

impl Default for MusicUi {
    fn default() -> Self {
        MusicUi { prompt: String::new(), dur: 30, seed: 1, bpm: 0, key: String::new(), last: None, import_path: String::new(), import_kind: cues::Kind::Ambience, imported: None, at_line: 1, loop_: false, duck: true, preview: false }
    }
}

impl MusicUi {
    pub fn spec(&self) -> Gen {
        Gen { prompt: self.prompt.trim().into(), dur: self.dur, seed: self.seed, bpm: self.bpm, key: Some(self.key.trim().to_string()).filter(|k| !k.is_empty()), model: None }
    }
}

/// Project list state: selected row, rename modal (dir, typed name), delete confirmations.
#[derive(Default)]
pub struct StartUi {
    pub sel: Option<PathBuf>,
    pub rename: Option<(PathBuf, String)>,
    pub rm: Option<Rm>,
}

/// Project awaiting the two delete confirmations.
#[derive(Clone, Debug, Default)]
pub struct Rm {
    pub dir: PathBuf,
    /// folder stem
    pub name: String,
    pub title: String,
    pub step: u8,
}

/// Footer numbers of the open project (computed on load, not per frame).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Summary {
    pub lines: usize,
    pub est: String,
    /// distinct speakers used in the script (incl. ones missing in the cast)
    pub speakers: usize,
}

impl Summary {
    pub fn of(p: &Project) -> Self {
        let s = p.summary();
        let who: std::collections::HashSet<&str> = p.script.lines().map(|l| l.who.as_str()).collect();
        Summary { lines: s["lines"].as_u64().unwrap_or(0) as usize, est: s["est"].as_str().unwrap_or("–").into(), speakers: who.len() }
    }
}

/// script.txt / project.toml change watch (external editor).
#[derive(Default)]
pub struct Watch {
    pub at: Option<std::time::Instant>,
    pub script: Option<std::time::SystemTime>,
    pub toml: Option<std::time::SystemTime>,
    /// projects folder mtime of the cached start list
    pub projects: Option<std::time::SystemTime>,
    /// changed mtimes (script, toml) seen first at: reload only once they stayed the same for `DEBOUNCE`
    /// (editors write in several steps; no reload in the middle of a save)
    pub pending: Option<(Option<std::time::SystemTime>, Option<std::time::SystemTime>, std::time::Instant)>,
}

/// How long changed files must stay unchanged before they are reloaded.
pub const DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(1500);
/// After this long on the first line of a job without any finished TTS line, omnivoice is probably loading its model.
pub const MODEL_LOAD_HINT: std::time::Duration = std::time::Duration::from_secs(4);

fn mtime(p: &Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(p).and_then(|m| m.modified()).ok()
}

pub struct App {
    pub tab: Tab,
    pub proj: Option<Project>,
    pub summary: Summary,
    /// start screen / "Projekt …" menu: cached; None = reload
    pub known: Option<Vec<stimmwerk_core::project::Info>>,
    pub watch: Watch,
    /// jobs of a closed project: cancelled, results ignored, only reaped
    pub orphans: Vec<Job>,
    /// a lint was asked for while one ran: Some(refresh builtins)
    pub relint_again: Option<bool>,
    /// builtins queried at least once in this session
    pub bi_known: bool,
    pub lint: Option<lint::Report>,
    pub bi: Option<Vec<String>>,
    pub personas: Vec<String>,
    /// voice library (from the last lint job)
    pub lib: Vec<voice::Entry>,
    /// persona dirs (own first); own voices folder = `dirs[0]`
    pub dirs: Vec<PathBuf>,
    pub generate: GenUi,
    /// per script line (0-based): state from the last job's events
    pub states: Vec<Option<State>>,
    /// per script line: active take from the last render report (empty = unknown)
    pub takes: Vec<usize>,
    pub jobs: Vec<Job>,
    pub player: Player,
    /// what is playing: Some(line) or None (probe/export)
    pub playing: Option<usize>,
    /// (error?, text)
    pub msg: Option<(bool, String)>,
    /// setup problem (omnivoice/ffmpeg missing)
    pub banner: Option<String>,
    pub cfg: Cfg,
    /// None: never persisted (shots, tests)
    pub cfg_path: Option<PathBuf>,
    pub new_name: String,
    pub open_path: String,
    pub show_lint: bool,
    /// selected script item (0-based index into script.items)
    pub sel: Option<usize>,
    pub draft: bool,
    /// per script line: take cached in [draft, final] (from the last lint + finished jobs)
    pub both: Vec<[bool; 2]>,
    /// project has no remembered quality: pick the one with more cached lines once the first lint is back
    pub auto_quality: bool,
    pub probe_text: String,
    pub ex: ExportUi,
    /// `<STIMMWERK_HOME>/settings.toml` (LLM backends); keys stay in the keychain
    pub settings: llm_cfg::Settings,
    pub llm: LlmUi,
    pub prose: ProseUi,
    /// shortening proposals per sync cue (1-based), shown until applied or dismissed
    pub shorten: BTreeMap<usize, shorten::Suggestion>,
    /// sync project: cue fit from the last lint
    pub sync_rep: Option<sync::Report>,
    pub music: MusicUi,
    pub start: StartUi,
}

/// ui-strings-de.md `consent.body` (the dialog text; `CONSENT_STATEMENT` is `consent.statement`).
pub const CONSENT_BODY: &str = "Klone nur Stimmen von Personen, die ausdrücklich eingewilligt haben – dafür bist du verantwortlich. \
Stimmwerk speichert Name der Person, Datum und Einwilligungstext; Aufnahme und Stimme bleiben lokal auf diesem Mac. \
Die Einwilligung ist an genau diese Aufnahme gebunden. Sie kann jederzeit widerrufen werden: Die Stimme wird danach \
nicht mehr gerendert und alle Aufnahmen werden gelöscht.";
pub const REVOKE_BODY: &str = "Die Stimme wird danach nicht mehr gerendert – auch nicht in bestehenden Projekten. Alle Aufnahmen \
mit ihr werden gelöscht: Referenz-Clip, Takes und Mischungen in deinen Projekten, Hörproben, Exporte aus Stimmwerk und der \
Stimm-Cache. Übrig bleibt nur ein Vermerk (Name, Einwilligung, Widerrufsdatum, Transkript der Aufnahme für spätere Bereinigungen). Das lässt sich nicht rückgängig machen; \
für eine neue Einwilligung klonst du die Stimme neu.";
pub const RM_BODY: &str = "Die entworfene Stimme und alle Aufnahmen mit ihr werden gelöscht: Anker-Clip, Takes und Mischungen \
in deinen Projekten, Hörproben, Exporte aus Stimmwerk und der Stimm-Cache. Projekte, die sie nutzen, brauchen danach eine \
andere Stimme. Das lässt sich nicht rückgängig machen.";
pub const CONSENT_STATEMENT: &str = "Die Person hat zugestimmt, dass ihre Stimme geklont und hier verwendet wird.";

/// Render button hint on a sync project (GUI export still goes through the sync path).
pub const SYNC_RENDER: &str = "Sync-Projekte rendern: kommt mit der Prüf-Ansicht; CLI: stimmwerk sync render";

pub const PROBE_TEXT: &str = "Hallo, so klinge ich in deinem Projekt.";

impl App {
    pub fn new(cfg_path: Option<PathBuf>) -> Self {
        Self::with_settings(cfg_path, llm_cfg::load(), &llm_cfg::path())
    }

    /// `settings`: the load result of `path`. A broken file is copied to `settings.toml.broken`, the builtins are shown
    /// and the KI-Anbieter card stays read-only until the file is fixed – the user's text is never overwritten.
    pub fn with_settings(cfg_path: Option<PathBuf>, settings: anyhow::Result<llm_cfg::Settings>, path: &Path) -> Self {
        let cfg = cfg_path.as_deref().and_then(|p| std::fs::read_to_string(p).ok()).and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
        let (settings, broken, msg) = match settings {
            Ok(s) => (s, None, None),
            Err(e) => {
                let b = llm_cfg::backup_broken(path).ok();
                let e = format!("{e:#}").split_whitespace().collect::<Vec<_>>().join(" ");
                (llm_cfg::Settings { llm: llm_cfg::AppLlm::builtin() }, Some((e.clone(), b)), Some((true, format!("KI-Anbieter: {e} – die Datei bleibt unverändert, bis du sie reparierst; bis dahin gelten die lokalen Vorlagen (nur lesen)."))))
            }
        };
        App {
            tab: Tab::Skript,
            proj: None,
            summary: Summary::default(),
            known: None,
            watch: Watch::default(),
            orphans: vec![],
            relint_again: None,
            bi_known: false,
            lint: None,
            bi: None,
            personas: vec![],
            lib: vec![],
            dirs: persona_dirs(),
            generate: GenUi::default(),
            states: vec![],
            takes: vec![],
            jobs: vec![],
            player: Player::default(),
            playing: None,
            msg,
            banner: None,
            cfg,
            cfg_path,
            new_name: String::new(),
            open_path: String::new(),
            show_lint: true,
            sel: None,
            draft: false,
            both: vec![],
            auto_quality: false,
            probe_text: PROBE_TEXT.into(),
            ex: ExportUi { preset: None, kind: export::Kind::Wav, disclosure: None, res: None },
            settings,
            llm: LlmUi { broken, ..LlmUi::default() },
            prose: ProseUi::default(),
            shorten: BTreeMap::new(),
            sync_rep: None,
            music: MusicUi::default(),
            start: StartUi::default(),
        }
    }

    /// Reads settings.toml again ("Neu laden" after a repair).
    pub fn reload_settings(&mut self) {
        match llm_cfg::load() {
            Ok(s) => {
                (self.settings, self.llm.broken) = (s, None);
                self.msg = Some((false, "KI-Anbieter neu geladen.".into()));
            }
            Err(e) => {
                let e = format!("{e:#}");
                self.llm.broken = Some((e.clone(), self.llm.broken.as_ref().and_then(|b| b.1.clone())));
                self.err(format!("KI-Anbieter: {e} – weiterhin nur lesen."));
            }
        }
    }

    pub fn save_cfg(&self) {
        if let Some(p) = &self.cfg_path {
            let _ = std::fs::create_dir_all(p.parent().unwrap_or(Path::new(".")));
            let _ = std::fs::write(p, serde_json::to_string_pretty(&self.cfg).unwrap_or_default());
        }
    }

    pub fn err(&mut self, e: impl std::fmt::Display) {
        self.msg = Some((true, e.to_string()));
    }

    pub fn quality(&self) -> Quality {
        if self.draft { Quality::Draft } else { Quality::Final }
    }

    pub fn n_lines(&self) -> usize {
        self.proj.as_ref().map_or(0, |p| p.script.lines().count())
    }

    /// Opens a project dir (adds it to the recent list) and starts linting. Jobs of a previously open project are detached.
    pub fn open(&mut self, ctx: &egui::Context, dir: &Path) {
        match Project::load(dir) {
            Ok(p) => {
                self.detach();
                let n = p.script.lines().count();
                self.cfg.recent.retain(|r| r != &p.dir);
                self.cfg.recent.insert(0, p.dir.clone());
                self.cfg.recent.truncate(8);
                self.save_cfg();
                let lq = p.meta.settings.last_quality;
                self.set_proj(p);
                (self.states, self.takes, self.lint, self.sel, self.ex.res, self.msg, self.known) = (vec![None; n], vec![], None, None, None, None, None);
                (self.draft, self.auto_quality, self.both) = (lq == Some(Quality::Draft), lq.is_none(), vec![]);
                self.tab = Tab::Skript;
                self.stamp();
                self.relint(ctx);
            }
            Err(e) => self.err(format!("{e:#}")),
        }
    }

    /// Sets the open project and its cached summary.
    pub fn set_proj(&mut self, p: Project) {
        self.summary = Summary::of(&p);
        self.proj = Some(p);
    }

    /// Remembers the current mtimes of script.txt / project.toml (after our own writes: no reload).
    pub fn stamp(&mut self) {
        self.watch.pending = None;
        if let Some(p) = &self.proj {
            self.watch.script = mtime(&p.dir.join(stimmwerk_core::project::SCRIPT));
            self.watch.toml = mtime(&p.dir.join(stimmwerk_core::project::FILE));
        }
    }

    /// After our own project.toml write: only its stamp. A script.txt change seen meanwhile (pending in the debounce
    /// window or not yet noticed) still reloads.
    pub fn stamp_toml(&mut self) {
        if let Some(p) = &self.proj {
            let t = mtime(&p.dir.join(stimmwerk_core::project::FILE));
            self.watch.toml = t;
            if let Some(pd) = self.watch.pending.as_mut() {
                pd.1 = t;
            }
        }
    }

    /// Re-reads the project from disk (external edit or "Neu laden"); line states come back from the cache via relint.
    pub fn reload(&mut self, ctx: &egui::Context, why: &str) {
        let Some(d) = self.proj.as_ref().map(|p| p.dir.clone()) else {
            return;
        };
        match Project::load(&d) {
            Ok(p) => {
                let n = p.script.lines().count();
                self.set_proj(p);
                (self.states, self.takes) = (vec![None; n], vec![]);
                self.sel = self.sel.filter(|&i| self.proj.as_ref().is_some_and(|p| i < p.script.items.len()));
                self.msg = Some((false, why.into()));
            }
            Err(e) => self.err(format!("Neu laden: {e:#}")),
        }
        self.stamp();
        self.relint(ctx);
    }

    /// Checks script.txt / project.toml twice a second; an external change reloads once the files stayed unchanged for
    /// `DEBOUNCE` (and after a running job ends).
    pub fn check_watch(&mut self, ctx: &egui::Context) {
        if self.watch.at.is_some_and(|t| t.elapsed() < std::time::Duration::from_millis(500)) {
            return;
        }
        self.watch.at = Some(std::time::Instant::now());
        let Some(d) = self.proj.as_ref().map(|p| p.dir.clone()) else {
            let m = mtime(&stimmwerk_core::projects_dir());
            if m != self.watch.projects {
                (self.watch.projects, self.known) = (m, None);
            }
            return;
        };
        let (s, t) = (mtime(&d.join(stimmwerk_core::project::SCRIPT)), mtime(&d.join(stimmwerk_core::project::FILE)));
        if s == self.watch.script && t == self.watch.toml {
            self.watch.pending = None;
            return;
        }
        match self.watch.pending {
            Some((ps, pt, at)) if (ps, pt) == (s, t) => {
                if at.elapsed() < DEBOUNCE || self.busy() {
                    return;
                }
            }
            _ => {
                self.watch.pending = Some((s, t, std::time::Instant::now()));
                return;
            }
        }
        self.watch.pending = None;
        let why = if s != self.watch.script { "Skript wurde außerhalb geändert – neu geladen." } else { "Projekt-Einstellungen wurden außerhalb geändert – neu geladen." };
        self.reload(ctx, why);
    }

    /// Opens script.txt in the default text editor (`open -t`); Stimmwerk reloads it when saved.
    pub fn edit_script(&mut self) {
        let Some(f) = self.proj.as_ref().map(|p| p.dir.join(stimmwerk_core::project::SCRIPT)) else {
            return;
        };
        if let Err(e) = std::process::Command::new("open").arg("-t").arg(&f).stdin(std::process::Stdio::null()).spawn() {
            self.err(format!("Editor ließ sich nicht öffnen: {e}"));
        }
    }

    /// Start screen list, loaded once and again only after a change (create/open/close, projects folder mtime).
    pub fn known(&mut self) -> &[stimmwerk_core::project::Info] {
        if self.known.is_none() {
            self.known = Some(tabs::start::known(self));
        }
        self.known.as_deref().unwrap_or_default()
    }

    pub fn create(&mut self, ctx: &egui::Context, name: &str) {
        let name = name.trim();
        if name.is_empty() {
            return self.err("Gib dem Projekt einen Namen.");
        }
        let dir = stimmwerk_core::target(name);
        let title = dir.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| name.into());
        match Project::create(&dir, &title, "de") {
            Ok(p) => {
                self.open(ctx, &p.dir);
                self.new_name.clear();
            }
            Err(e) => self.err(format!("{e:#}")),
        }
    }

    /// Back to the start screen: jobs cancelled + detached (no stuck busy, no events into the next project).
    pub fn close(&mut self) {
        self.detach();
        self.player.stop();
        self.playing = None;
        self.proj = None;
        (self.lint, self.states, self.takes, self.sel, self.ex.res, self.known, self.both) = (None, vec![], vec![], None, None, None, vec![]);
    }

    /// Cancels all jobs and moves them to `orphans` (their results are ignored).
    pub fn detach(&mut self) {
        for j in &self.jobs {
            j.cancel();
        }
        self.orphans.append(&mut self.jobs);
        self.relint_again = None;
    }

    pub fn busy(&self) -> bool {
        self.jobs.iter().any(|j| j.kind.exclusive())
    }

    /// A cancelled job of this project from before a close/reopen still ends (holds the project lock until then).
    pub fn ending(&self, dir: &Path) -> bool {
        self.orphans.iter().any(|j| j.kind.exclusive() && j.dir.as_deref() == Some(dir) && !j.finished())
    }

    pub fn running(&self, k: &JobKind) -> bool {
        self.jobs.iter().any(|j| &j.kind == k)
    }

    /// Lint + persona files + line states, off the UI thread; omnivoice's voice list only the first time (or while unknown).
    pub fn relint(&mut self, ctx: &egui::Context) {
        self.relint_with(ctx, false);
    }

    /// `refresh`: ask omnivoice for its voice list again ("Prüfen").
    pub fn relint_with(&mut self, ctx: &egui::Context, refresh: bool) {
        let Some(p) = &self.proj else { return };
        if self.running(&JobKind::Lint) {
            // the running lint may have read the old files: run once more when it is done
            self.relint_again = Some(refresh || self.relint_again == Some(true));
            return;
        }
        let (dir, dirs) = (p.dir.clone(), self.dirs.clone());
        let cached = (!refresh && self.bi_known).then(|| self.bi.clone()).flatten();
        self.jobs.push(jobs::spawn(ctx, JobKind::Lint, move |_, _| {
            let bi = cached.or_else(lint::query_builtins);
            let bi = bi.as_deref();
            let rep = lint::lint(&dir, &dirs, bi)?;
            // line states and undo from disk (cache + takes/rerolls.json), no rendering
            let p = Project::load(&dir).ok();
            let status = p.as_ref().and_then(|p| render::status_both(p, &dirs).ok());
            let sync = p.as_ref().and_then(|p| sync::load(p).ok().flatten().map(|s| sync::check_project(p, &s, &dirs)));
            let lib = voice::ls(&dirs);
            let personas = lib.iter().filter(|e| e.kind != "error").map(|e| e.name.clone()).collect();
            Ok(Out::Lint(Box::new(LintOut { rep, bi: bi.map(<[String]>::to_vec), personas, lib, status, sync })))
        }));
    }

    /// Starts a project job (render/reroll/export/line) unless one is already running.
    pub fn start(&mut self, ctx: &egui::Context, kind: JobKind) {
        let Some(p) = self.proj.clone() else { return };
        if kind.exclusive() && self.busy() {
            return self.err("Es läuft schon ein Auftrag – warte kurz oder brich ihn ab.");
        }
        if kind.exclusive() && self.ending(&p.dir) {
            return self.err("Der vorige Auftrag wird noch beendet – versuch es gleich noch einmal.");
        }
        if kind == JobKind::Render && self.is_sync() {
            return self.err(SYNC_RENDER);
        }
        let dir = p.dir.clone();
        self.banner = None;
        let q = Self::job_quality(self.quality(), &kind);
        let ex = (self.ex.preset, self.ex.kind, self.ex.disclosure);
        let (k, dirs) = (kind.clone(), self.dirs.clone());
        let j = jobs::spawn(ctx, kind, move |c, prog| {
            Ok(match k {
                JobKind::Render => Out::Render(render::render(&p, &dirs, &render::Opts { quality: q, ..Default::default() }, c, prog)?),
                JobKind::PlayLine(l) => Out::Render(render::render(&p, &dirs, &render::Opts { line: Some(l), quality: q, fresh: false }, c, prog)?),
                JobKind::Reroll { line, undo } => Out::Reroll(render::reroll(&p, &dirs, line, undo, q, c, prog)?),
                JobKind::Export => {
                    let o = export::Opts { kind: ex.1, loudness: ex.0, disclosure: ex.2, out: None, quality: q, allow_too_long: false };
                    Out::Export(Box::new(export::export(&p, &dirs, &o, c, prog)?))
                }
                _ => unreachable!("own spawn paths: relint() / probe() / design() / revoke() / rm_voice() / llm_test() / …"),
            })
        });
        self.jobs.push(jobs::Job { dir: Some(dir), ..j });
    }

    /// The open project has subtitle cues (`sync.json`): rendering is the CLI's `sync render` until the review view.
    pub fn is_sync(&self) -> bool {
        self.proj.as_ref().is_some_and(|p| p.dir.join(stimmwerk_core::project::SYNC).is_file())
    }

    /// Quality a job runs in: the active one, but export always final (a draft is never exported silently).
    pub fn job_quality(active: Quality, kind: &JobKind) -> Quality {
        if *kind == JobKind::Export { Quality::Final } else { active }
    }

    /// Lines whose take is cached only as draft.
    pub fn draft_only(&self) -> usize {
        self.both.iter().filter(|b| b[0] && !b[1]).count()
    }

    /// Lines without a cached final take (unknown before the first lint: 0).
    pub fn final_missing(&self) -> usize {
        self.both.iter().filter(|b| !b[1]).count()
    }

    /// "Endfassung rendern" makes sense: every line exists as draft, some are missing in final.
    pub fn offer_final(&self) -> bool {
        !self.both.is_empty() && self.both.len() == self.n_lines() && self.both.iter().all(|b| b[0]) && self.final_missing() > 0
    }

    /// Switches to final and renders the missing final clips (same seeds and takes as the draft; cached lines stay).
    pub fn render_final(&mut self, ctx: &egui::Context) {
        if self.busy() {
            return;
        }
        self.set_draft(ctx, false);
        self.start(ctx, JobKind::Render);
    }

    /// Probe: own job (no project lock), text from the Stimmen tab.
    pub fn probe(&mut self, ctx: &egui::Context, name: &str) {
        let Some(lang) = self.proj.as_ref().map(|p| p.meta.lang.clone()) else {
            return;
        };
        let (name, text) = (name.to_string(), self.probe_text.clone());
        let (n2, dirs) = (name.clone(), self.dirs.clone());
        self.jobs.push(jobs::spawn(ctx, JobKind::Probe(name), move |c, prog| Ok(Out::Probe(speakers::probe(&dirs, &n2, &text, &lang, c, prog)?))));
    }

    /// Own voices folder (designs, clones).
    pub fn vdir(&self) -> PathBuf {
        self.dirs.first().cloned().unwrap_or_else(voice::dir)
    }

    /// Design candidates from the "Stimme erzeugen" tab (own job, GPU, no project lock).
    pub fn design(&mut self, ctx: &egui::Context) {
        if self.running(&JobKind::Design) {
            return;
        }
        let g = &self.generate;
        let lang = self.proj.as_ref().map_or("de".into(), |p| p.meta.lang.clone());
        let d = voice::Design { instruct: g.instruct.trim().into(), n: g.n, seeds: None, text: g.text.clone(), lang, quality: Quality::Final };
        let vdir = self.vdir();
        self.jobs.push(jobs::spawn(ctx, JobKind::Design, move |c, prog| Ok(Out::Design(Box::new(voice::design(&vdir, &d, c, prog)?)))));
    }

    /// Keeps candidate `id` of the shown batch as persona `name`.
    pub fn keep(&mut self, ctx: &egui::Context, id: usize, name: &str) {
        let Some(b) = self.generate.batch.as_ref() else {
            return;
        };
        match voice::keep(&self.vdir(), &self.dirs, self.bi.as_deref(), &b.batch, id, name.trim(), None) {
            Ok(s) => {
                if let Some(k) = self.generate.kept.get_mut(id - 1) {
                    *k = Some(s.persona.clone());
                }
                self.msg = Some((false, format!("Stimme „{}“ gespeichert – du kannst sie jetzt im Reiter „Stimmen“ zuweisen.", s.persona)));
                self.relint(ctx);
            }
            Err(e) => self.err(format!("{e:#}")),
        }
    }

    /// Clone from the consent dialog (fs only: copy/trim + sha256, no TTS).
    pub fn clone_voice(&mut self, ctx: &egui::Context) {
        let g = &self.generate;
        let num = |s: &str| -> Result<Option<f64>, String> {
            let s = s.trim().replace(',', ".");
            if s.is_empty() { Ok(None) } else { s.parse().map(Some).map_err(|_| format!("„{s}“ ist keine Zahl (Sekunden)")) }
        };
        let (start, end) = match (num(&g.start), num(&g.end)) {
            (Ok(a), Ok(b)) => (a, b),
            (Err(e), _) | (_, Err(e)) => return self.err(e),
        };
        let consent = if g.agreed { format!("{}{}", CONSENT_STATEMENT, if g.how.trim().is_empty() { String::new() } else { format!(" ({})", g.how.trim()) }) } else { String::new() };
        let lang = self.proj.as_ref().map_or("de".into(), |p| p.meta.lang.clone());
        let req = voice::CloneReq { wav: PathBuf::from(g.wav.trim()), name: g.name.trim().into(), ref_text: g.ref_text.clone(), start, end, consent, who: g.who.trim().into(), date: Some(g.consent_date.trim().to_string()).filter(|d| !d.is_empty()), lang };
        match voice::clone(&self.vdir(), &self.dirs, self.bi.as_deref(), &req) {
            Ok(s) => {
                let w = s.warnings.first().map(|w| format!(" Hinweis: {w}")).unwrap_or_default();
                self.msg = Some((false, format!("Stimme „{}“ geklont und an die Aufnahme gebunden.{w}", s.persona)));
                self.generate.cloned = Some(s.persona);
                self.generate.consent_open = false;
                (self.generate.agreed, self.generate.who, self.generate.how, self.generate.consent_date) = (false, String::new(), String::new(), today());
                self.relint(ctx);
            }
            Err(e) => self.err(format!("{e:#}")),
        }
    }

    /// Where audio of a voice can be: data root + the open project; one total lock wait for busy projects.
    fn places(&self) -> voice::Places {
        voice::Places::new(&self.proj.iter().map(|p| p.dir.clone()).collect::<Vec<_>>())
    }

    /// Revoke / rm may start: no other job of this app runs (a probe or design of the same voice could still write
    /// audio; lint only reads).
    pub fn may_purge(&self) -> bool {
        !self.jobs.iter().any(|j| j.kind != JobKind::Lint) && !self.orphans.iter().any(|j| !j.finished())
    }

    /// Withdraws the consent of clone `name` and deletes all its audio (background job: waits for busy projects).
    pub fn revoke(&mut self, ctx: &egui::Context, name: &str) {
        if !self.may_purge() {
            return self.err("Erst den laufenden Auftrag abwarten oder abbrechen.");
        }
        let (dirs, n, pl) = (self.dirs.clone(), name.to_string(), self.places());
        self.jobs.push(jobs::spawn(ctx, JobKind::Revoke(name.into()), move |_, _| Ok(Out::Revoked(Box::new(voice::revoke(&dirs, &n, None, false, &pl)?)))));
    }

    /// Deletes design `name` (own folder) with all its audio (background job).
    pub fn rm_voice(&mut self, ctx: &egui::Context, name: &str) {
        if !self.may_purge() {
            return self.err("Erst den laufenden Auftrag abwarten oder abbrechen.");
        }
        let (vdir, n, pl) = (self.vdir(), name.to_string(), self.places());
        self.jobs.push(jobs::spawn(ctx, JobKind::Rm(name.into()), move |_, _| Ok(Out::Removed(Box::new(voice::rm(&vdir, &n, &pl)?)))));
    }

    /// Result text of a revoke: what happened, then what was not deleted as its own sentence.
    pub fn revoked_text(r: &voice::Revoked) -> (bool, String) {
        let ext = r.kept_external.as_ref().map(|p| format!(" Die Original-Aufnahme außerhalb von Stimmwerk ({}) bleibt liegen.", p.display())).unwrap_or_default();
        let done = if r.skipped.is_empty() { format!("{} gelöscht", files(r.deleted.len())) } else { format!("{} gelöscht, nicht alles", files(r.deleted.len())) };
        let rt = if r.ref_text_kept { " Das Transkript bleibt für spätere Bereinigung gespeichert." } else { "" };
        (!r.skipped.is_empty(), format!("Einwilligung für „{}“ widerrufen ({}) – die Stimme wird nicht mehr gerendert, {done}.{ext}{rt}{}", r.persona, r.revoked, skipped_text(&r.skipped)))
    }

    /// Result text of an rm (partial: the voice stays until everything is deleted).
    pub fn removed_text(r: &voice::Removed) -> (bool, String) {
        if r.removed {
            (false, format!("Stimme „{}“ gelöscht – {} entfernt.", r.persona, files(r.deleted.len())))
        } else {
            (true, format!("Stimme „{}“ nur teilweise gelöscht ({} entfernt) – sie bleibt, bis alles gelöscht ist.{}", r.persona, files(r.deleted.len()), skipped_text(&r.skipped)))
        }
    }

    pub fn cancel_all(&mut self) {
        for j in &self.jobs {
            j.cancel();
        }
    }

    // ---------- Projekte umbenennen / löschen (Startseite + Menü „Projekt …“) ----------

    /// Renames the project folder (`<name>.studio`) and its title; recent list, take index and – if it is the open
    /// project – the open project follow (closed first, reopened at the new place).
    pub fn rename_project(&mut self, ctx: &egui::Context, dir: &Path, name: &str) {
        if self.ending(dir) || (self.is_open(dir) && self.busy()) {
            return self.err(tabs::start::BUSY);
        }
        let reopen = self.is_open(dir);
        if reopen {
            self.close();
        }
        match stimmwerk_core::project::rename(dir, name) {
            Ok(new) => {
                self.cfg.recent.iter_mut().filter(|r| *r == dir).for_each(|r| *r = new.clone());
                self.save_cfg();
                (self.known, self.start.rename, self.start.sel) = (None, None, Some(new.clone()));
                if reopen {
                    self.open(ctx, &new);
                }
                if self.msg.is_none() {
                    self.msg = Some((false, format!("Projekt heißt jetzt „{}“.", name.trim())));
                }
            }
            Err(e) => {
                if reopen {
                    self.open(ctx, dir);
                }
                self.err(format!("Umbenennen: {e:#}"));
            }
        }
    }

    /// Moves the project folder to the Trash (after the two confirmations); an open project is closed first.
    pub fn delete_project(&mut self, dir: &Path, title: &str) {
        self.start.rm = None;
        if self.ending(dir) || (self.is_open(dir) && self.busy()) {
            return self.err(tabs::start::BUSY);
        }
        if self.is_open(dir) {
            self.close();
        }
        match stimmwerk_core::project::trash(dir) {
            Ok(r) => {
                self.cfg.recent.retain(|r| r != dir);
                self.save_cfg();
                (self.known, self.start.sel) = (None, None);
                let mut ext = String::new();
                if !r.moved_in.is_empty() {
                    ext += &format!(" Mit in den Papierkorb (out/{}): {} eigene Exporte.", stimmwerk_core::project::EXTERN, files(r.moved_in.len()));
                }
                if !r.outside.is_empty() {
                    ext += &format!(" Bleiben liegen (anderes Laufwerk): {}.", files(r.outside.len()));
                }
                self.msg = Some((!ext.is_empty(), format!("In den Papierkorb verschoben: „{title}“.{ext}")));
            }
            Err(e) => self.err(format!("Löschen: {e:#}")),
        }
    }

    fn is_open(&self, dir: &Path) -> bool {
        self.proj.as_ref().is_some_and(|p| p.dir == dir)
    }

    // ---------- KI-Anbieter (llm_cfg) ----------

    /// settings.toml may be written (not broken); otherwise the refusal is shown.
    pub fn writable(&mut self) -> bool {
        if let Some((e, _)) = &self.llm.broken {
            let e = e.clone();
            self.err(format!("Nicht gespeichert: settings.toml ist fehlerhaft und wird nicht überschrieben – erst reparieren ({e})."));
            return false;
        }
        true
    }

    /// Writes settings.toml; false (and a message) when that fails or the file is broken (never overwritten).
    pub fn save_settings(&mut self) -> bool {
        if !self.writable() {
            return false;
        }
        match llm_cfg::save(&self.settings) {
            Ok(()) => true,
            Err(e) => {
                self.err(format!("Anbieter konnte nicht gespeichert werden: {e:#}"));
                false
            }
        }
    }

    /// Entry of the dialog into the settings (key via `key_save`), dialog closed.
    pub fn llm_save_dlg(&mut self, ctx: &egui::Context) {
        let Some(d) = self.llm.dlg.clone() else {
            return;
        };
        let name = d.name.trim().to_string();
        let err = if !llm_cfg::name_ok(&name) {
            Some(if name.is_empty() { "Gib dem Anbieter einen Namen.".to_string() } else { "Nur Buchstaben, Ziffern, _ - . im Namen.".into() })
        } else if d.orig.is_none() && self.settings.llm.backends.contains_key(&name) {
            Some(format!("Es gibt schon einen Anbieter „{name}“."))
        } else if d.url.trim().is_empty() {
            Some("Gib eine Adresse (URL) an.".into())
        } else if d.kind == funkhaus_llm::Kind::Anthropic && d.model.trim().is_empty() {
            Some("Gib ein Modell an.".into())
        } else {
            None
        };
        if let Some(e) = err {
            if let Some(dl) = self.llm.dlg.as_mut() {
                dl.err = Some(e);
            }
            return;
        }
        let e = d.entry(self.settings.llm.backends.get(&name));
        let before = self.settings.clone();
        self.settings.llm.backends.insert(name.clone(), e);
        if self.settings.llm.default.is_none() {
            self.settings.llm.default = Some(name.clone());
        }
        if !self.save_settings() {
            self.settings = before;
            return;
        }
        self.llm.dlg = None;
        self.msg = Some((false, format!("Anbieter „{name}“ gespeichert.")));
        if !d.key.trim().is_empty() {
            self.key_save(ctx, &name, d.key);
        }
    }

    /// `GET /models` as a job; nothing but the model list is requested. Abbrechen abandons the wait.
    pub fn llm_test(&mut self, ctx: &egui::Context, name: &str) {
        let Some(e) = self.settings.llm.backends.get(name).cloned() else {
            return;
        };
        self.jobs.push(jobs::spawn(ctx, JobKind::LlmTest(name.into()), move |c, _| Ok(Out::Models(funkhaus_llm::models_cancel(&e.backend(), e.allow_online, c)?))));
    }

    /// Key into the keychain (service `stimmwerk-llm`, account = backend) off the UI thread; the key lives only in the closure.
    pub fn key_save(&mut self, ctx: &egui::Context, name: &str, key: String) {
        let n = name.to_string();
        self.jobs.push(jobs::spawn(ctx, JobKind::KeySave(name.into()), move |_, _| {
            funkhaus_llm::keychain::save(funkhaus_llm::keychain::SERVICE, &n, key.trim())?;
            Ok(Out::KeySaved)
        }));
    }

    pub fn key_clear(&mut self, name: &str) {
        if !self.writable() {
            return;
        }
        match llm_cfg::clear_key(&mut self.settings, name) {
            Ok(()) => {
                if self.save_settings() {
                    self.msg = Some((false, format!("Schlüssel für „{name}“ aus dem Schlüsselbund entfernt.")));
                }
            }
            Err(e) => self.err(format!("{e:#}")),
        }
    }

    /// Removes a backend (never the default); `rm_key` also deletes its keychain item.
    pub fn llm_remove(&mut self, name: &str, rm_key: bool) {
        if self.settings.llm.default.as_deref() == Some(name) {
            return self.err("Der Standard-Anbieter kann nicht entfernt werden. Wähle erst einen anderen Standard.");
        }
        if !self.writable() {
            return;
        }
        if rm_key && let Err(e) = llm_cfg::clear_key(&mut self.settings, name) {
            return self.err(format!("{e:#}"));
        }
        self.settings.llm.backends.remove(name);
        self.llm.status.remove(name);
        if self.save_settings() {
            self.msg = Some((false, format!("Anbieter „{name}“ entfernt.")));
        }
    }

    pub fn llm_set_default(&mut self, name: &str) {
        self.settings.llm.default = Some(name.into());
        if self.save_settings() {
            self.msg = Some((false, format!("„{name}“ ist jetzt der Standard.")));
        }
    }

    /// Backend of the open project (project `[llm]` or app default) with its consent switches.
    pub fn resolved(&self) -> anyhow::Result<llm_cfg::Resolved> {
        llm_cfg::resolve(&self.settings.llm, self.proj.as_ref().and_then(|p| p.meta.llm.as_ref()))
    }

    /// Project switch: backend name (None = app default) and/or `online_ok`; written to project.toml only.
    pub fn set_project_llm(&mut self, backend: Option<Option<String>>, online_ok: Option<bool>) {
        let Some(p) = self.proj.as_mut() else { return };
        let mut l = p.meta.llm.clone().unwrap_or_default();
        if let Some(b) = backend {
            l.backend = b;
        }
        if let Some(o) = online_ok {
            l.online_ok = o;
        }
        p.meta.llm = Some(l).filter(|l| l.backend.is_some() || l.online_ok);
        if let Err(e) = p.save_meta() {
            self.err(format!("Projekt-Einstellung nicht gespeichert: {e:#}"));
        }
        self.stamp_toml();
    }

    /// Gate before any LLM job: both switches (or loopback); the error names the missing switch.
    fn llm_ready(&self) -> Result<llm_cfg::Resolved, String> {
        let r = self.resolved().map_err(|e| format!("{e:#}"))?;
        r.allowed().map_err(|e| format!("{e:#}"))?;
        Ok(r)
    }

    // ---------- Prosa -> Rollenskript ----------

    /// LLM call as a job; the result is a preview (`prose.res`), nothing is written.
    pub fn prose_run(&mut self, ctx: &egui::Context) {
        let r = match self.llm_ready() {
            Ok(r) => r,
            Err(e) => return self.prose.err = Some(e),
        };
        let text = if self.prose.path.trim().is_empty() {
            self.prose.text.clone()
        } else {
            match std::fs::read_to_string(self.prose.path.trim()) {
                Ok(t) => t,
                Err(e) => return self.prose.err = Some(format!("Datei nicht lesbar: {e}")),
            }
        };
        if prose::plain(&text).is_empty() {
            return self.prose.err = Some("Gib einen Text an (Datei oder Textfeld).".into());
        }
        let gap = self.proj.as_ref().map_or(0.5, |p| p.meta.settings.gap);
        (self.prose.err, self.prose.res) = (None, None);
        self.jobs.push(jobs::spawn(ctx, JobKind::Prose, move |c, prog| {
            let txt = |m: &str| prog(Event::Msg { text: m.into() });
            let mut llm = |m: &[funkhaus_llm::Msg], s: &serde_json::Value| funkhaus_llm::complete_cancel(&r.backend, r.online_ok, m, Some(s), c, &txt);
            Ok(Out::Prose(Box::new(prose::convert(&mut llm, &text, gap)?)))
        }));
    }

    /// Confirmed: the preview replaces script.txt, its cast becomes speakers (persona unset).
    pub fn prose_apply(&mut self, ctx: &egui::Context) {
        let Some(o) = self.prose.res.take() else {
            return;
        };
        let Some(p) = self.proj.as_mut() else { return };
        let r = stimmwerk_core::script::Script::parse(&o.script).and_then(|s| {
            p.script = s;
            let added = o.cast.iter().filter(|c| p.add_speaker(&c.id, Some(c.name.as_str()).filter(|n| *n != c.id))).count();
            p.save().map(|_| added)
        });
        match r {
            Ok(added) => {
                self.prose.open = false;
                self.reload(ctx, &format!("Rollenskript übernommen: {} Zeilen, {added} neue Sprecher – Stimmen zuweisen im Reiter „Stimmen“.", o.lines));
            }
            Err(e) => self.err(format!("Rollenskript nicht gespeichert: {e:#}")),
        }
    }

    // ---------- Kürzen (sync) ----------

    pub fn shorten_run(&mut self, ctx: &egui::Context, cue: usize) {
        let r = match self.llm_ready() {
            Ok(r) => r,
            Err(e) => return self.err(e),
        };
        let Some(p) = self.proj.clone() else { return };
        let dirs = self.dirs.clone();
        self.jobs.push(jobs::spawn(ctx, JobKind::Shorten(cue), move |c, prog| {
            let s = sync::load(&p)?.ok_or_else(|| anyhow::anyhow!("kein Sync-Projekt"))?;
            let txt = |m: &str| prog(Event::Msg { text: m.into() });
            let mut llm = |m: &[funkhaus_llm::Msg], sc: &serde_json::Value| funkhaus_llm::complete_cancel(&r.backend, r.online_ok, m, Some(sc), c, &txt);
            let v = shorten::run_with(&p, &s, &r, Some(cue), &dirs, &mut llm)?;
            Ok(Out::Shorten(Box::new(v.into_iter().next().ok_or_else(|| anyhow::anyhow!("Cue {cue}: kein Vorschlag"))?)))
        }));
    }

    /// The proposal replaces the cue's line text (script.txt; sync.json follows on load).
    pub fn shorten_apply(&mut self, ctx: &egui::Context, cue: usize) {
        let Some(t) = self.shorten.remove(&cue).and_then(|s| s.proposal) else {
            return;
        };
        let Some(p) = self.proj.as_mut() else { return };
        let Some(stimmwerk_core::script::Item::Line(l)) = p.script.items.iter_mut().filter(|i| matches!(i, stimmwerk_core::script::Item::Line(_))).nth(cue - 1) else {
            return;
        };
        l.text = t;
        match p.save() {
            Ok(()) => {
                self.stamp();
                self.msg = Some((false, format!("Cue {cue} gekürzt – Zeile neu rendern, damit die Zeit stimmt.")));
                self.relint(ctx);
            }
            Err(e) => self.err(format!("{e:#}")),
        }
    }

    // ---------- Musik ----------

    /// acestep render (or cache hit) of the form's generation as an exclusive job.
    pub fn music_gen(&mut self, ctx: &egui::Context) {
        let g = self.music.spec();
        if let Err(e) = g.check() {
            return self.err(format!("{e}"));
        }
        let Some(p) = self.proj.clone() else { return };
        if self.busy() {
            return self.err("Es läuft schon ein Auftrag – warte kurz oder brich ihn ab.");
        }
        let dir = p.dir.clone();
        self.banner = None;
        let j = jobs::spawn(ctx, JobKind::MusicGen(music::key(&g)), move |c, prog| Ok(Out::Music(Box::new(music::generate(&p, &g, false, c, prog)?))));
        self.jobs.push(jobs::Job { dir: Some(dir), ..j });
    }

    /// Copies a WAV into `assets/<kind>/` (fs only, synchronous).
    pub fn music_import(&mut self) {
        let Some(p) = self.proj.as_ref() else { return };
        let src = PathBuf::from(self.music.import_path.trim());
        match music::import(p, &src, None, self.music.import_kind, None, None, &export::ffmpeg()) {
            Ok(r) => {
                self.msg = Some((false, format!("{} importiert ({} s, {} Hz).", r.file, r.secs, r.sr)));
                self.music.imported = Some(r);
            }
            Err(e) => self.err(format!("Import: {e:#}")),
        }
    }

    /// Appends a cue if the cue lint has no error for it; project.toml only.
    pub fn cue_add(&mut self, ctx: &egui::Context, c: Cue) {
        let Some(p) = self.proj.as_mut() else { return };
        p.meta.cues.push(c);
        let n = p.meta.cues.len();
        let bad: Vec<String> = cues::lint(p).into_iter().filter(|i| i.level == "error" && i.msg.starts_with(&format!("Cue {n}:"))).map(|i| i.msg).collect();
        if !bad.is_empty() {
            p.meta.cues.pop();
            return self.err(format!("{} – nichts geändert.", bad.join("; ")));
        }
        self.cues_changed(ctx, &format!("Cue {n} angelegt."));
    }

    pub fn cue_rm(&mut self, ctx: &egui::Context, n: usize) {
        let Some(p) = self.proj.as_mut() else { return };
        if n == 0 || n > p.meta.cues.len() {
            return;
        }
        p.meta.cues.remove(n - 1);
        self.cues_changed(ctx, &format!("Cue {n} entfernt (Dateien bleiben)."));
    }

    /// After an edit of `p.meta.cues`: project.toml written, states rechecked.
    pub fn cues_changed(&mut self, ctx: &egui::Context, msg: &str) {
        let Some(p) = self.proj.as_ref() else { return };
        match p.save_meta() {
            Ok(()) => {
                self.msg = Some((false, msg.into()));
                self.set_proj(self.proj.clone().expect("checked"));
            }
            Err(e) => self.err(format!("Cues nicht gespeichert: {e:#}")),
        }
        self.stamp_toml();
        self.relint(ctx);
    }

    /// "Mix vorhören": the export (WAV, final) renders what is missing, mixes the cues and plays the result.
    pub fn mix_preview(&mut self, ctx: &egui::Context) {
        self.ex.kind = export::Kind::Wav;
        self.music.preview = true;
        self.start(ctx, JobKind::Export);
        self.music.preview = self.running(&JobKind::Export);
    }

    /// App end: cancel everything, give the threads ≤ 2 s (omnivoice children get SIGTERM/SIGKILL), drop GPU priority files.
    pub fn shutdown(&mut self) {
        self.player.stop();
        let mut all: Vec<Job> = self.jobs.drain(..).chain(self.orphans.drain(..)).collect();
        jobs::join_all(&mut all, std::time::Duration::from_secs(2));
        funkhaus_sys::gpu::release_all();
    }

    fn on_event(&mut self, k: usize, e: Event) {
        let j = &mut self.jobs[k];
        match e {
            Event::Stage { name } => j.stage = Some(name),
            Event::Msg { text } if text.starts_with("wartet auf GPU") => j.gpu_wait = true,
            Event::Msg { text } if text.starts_with("GPU frei") => (j.gpu_wait, j.line_t0) = (false, j.line_t0.map(|_| std::time::Instant::now())),
            Event::Line { i, n, state, tts_secs } => {
                if self.states.len() != n {
                    self.states.resize(n, None);
                }
                if let Some(s) = i.checked_sub(1).and_then(|k| self.states.get_mut(k)) {
                    *s = Some(state);
                }
                match state {
                    State::Queued => j.total += 1,
                    State::Rendering => (j.cur, j.line_t0) = (Some(i), Some(std::time::Instant::now())),
                    State::Cached | State::Done => {
                        j.done += 1;
                        j.tts_done += usize::from(tts_secs.is_some());
                    }
                    State::Failed => {}
                }
            }
            Event::Mix => j.stage = Some("mix".into()),
            Event::Master => j.stage = Some("master".into()),
            Event::Msg { .. } | Event::Done => {}
        }
    }

    fn on_done(&mut self, kind: JobKind, cancelled: bool, r: Result<Out, String>) {
        match r {
            Ok(Out::Lint(l)) => {
                self.lint = Some(l.rep);
                self.bi = l.bi;
                self.bi_known = true;
                self.personas = l.personas;
                self.lib = l.lib;
                self.sync_rep = l.sync;
                if let Some(st) = l.status {
                    self.both = st.iter().map(|b| [b.draft, b.fin]).collect();
                    if std::mem::take(&mut self.auto_quality) {
                        // no remembered quality: the one with more cached lines, tie = final (not persisted)
                        let (d, f) = (st.iter().filter(|b| b.draft).count(), st.iter().filter(|b| b.fin).count());
                        self.draft = d > f;
                    }
                    if !self.busy() {
                        self.cache_status(&st);
                    }
                }
            }
            Ok(Out::Render(rep)) => {
                if let JobKind::PlayLine(l) = kind {
                    if let Some(b) = self.both.get_mut(l - 1) {
                        b[usize::from(rep.quality == Quality::Final)] = true;
                    }
                    self.play(Some(l), &rep.wav);
                } else {
                    self.took(&rep);
                    let w = if rep.warnings.is_empty() { String::new() } else { format!(" · ⚠ Musik: {}", rep.warnings.join(" · ")) };
                    self.msg = Some((!rep.warnings.is_empty(), format!("Gerendert: {} Zeilen, {} neu · {}{w}", rep.lines, rep.rendered.len(), funkhaus_sys::mmss(rep.secs))));
                }
            }
            Ok(Out::Reroll(r)) => {
                self.took(&r.render);
                let what = if r.undo { "Vorheriger Take" } else { "Neuer Take" };
                self.msg = Some((false, format!("{what} für Zeile {} (Take {})", r.line, r.take)));
                self.play(Some(r.line), &r.render.items.iter().find(|x| x.i == r.line).and_then(|x| x.clips.first().cloned()).unwrap_or(r.render.wav));
            }
            Ok(Out::Export(rep)) => {
                // the format choice shows what was written
                self.ex.kind = rep.kind;
                if std::mem::take(&mut self.music.preview) {
                    self.play(None, &rep.file);
                }
                if !rep.warnings.is_empty() {
                    self.msg = Some((true, format!("⚠ Musik: {}", rep.warnings.join(" · "))));
                }
                self.ex.res = Some(rep);
            }
            Ok(Out::Models(ms)) => {
                if let JobKind::LlmTest(n) = &kind {
                    self.msg = Some((false, format!("„{n}“ erreichbar – {} Modelle.", ms.len())));
                    self.llm.status.insert(n.clone(), Ok(ms.len()));
                }
            }
            Ok(Out::KeySaved) => {
                if let JobKind::KeySave(n) = &kind {
                    if let Some(e) = self.settings.llm.backends.get_mut(n) {
                        e.key = Some(funkhaus_llm::KeyRef::Keychain { service: funkhaus_llm::keychain::SERVICE.into(), account: n.clone() });
                    }
                    if self.save_settings() {
                        self.msg = Some((false, format!("Schlüssel für „{n}“ im Schlüsselbund gespeichert.")));
                    }
                    if let Some(d) = self.llm.dlg.as_mut().filter(|d| d.orig.as_deref() == Some(n)) {
                        d.key.clear();
                    }
                }
            }
            Ok(Out::Prose(o)) => self.prose.res = Some(o),
            Ok(Out::Shorten(s)) => {
                self.shorten.insert(s.cue, *s);
            }
            Ok(Out::Music(b)) => {
                self.msg = Some((false, if b.cached { format!("Musik aus dem Cache ({} s).", b.secs) } else { format!("Musik erzeugt ({} s) – jetzt auf eine Zeile legen.", b.secs) }));
                self.music.last = Some(b);
            }
            Err(e) if matches!(kind, JobKind::LlmTest(_) | JobKind::KeySave(_) | JobKind::Prose) => match &kind {
                JobKind::LlmTest(n) if cancelled => {
                    self.llm.status.remove(n);
                    self.msg = Some((false, format!("{} abgebrochen.", kind.label())));
                }
                JobKind::LlmTest(n) => {
                    let model = self.settings.llm.backends.get(n).map(|e| e.model.clone()).unwrap_or_default();
                    self.llm.status.insert(n.clone(), Err(tabs::llm::err_text(n, &model, &e)));
                }
                JobKind::KeySave(_) => match self.llm.dlg.as_mut() {
                    Some(d) => d.err = Some(e),
                    None => self.err(e),
                },
                _ if cancelled => self.prose.err = Some("Abgebrochen – nichts geändert.".into()),
                _ => self.prose.err = Some(e),
            },
            Ok(Out::Probe(w)) => self.play(None, &w),
            Ok(Out::Revoked(r)) => self.msg = Some(Self::revoked_text(&r)),
            Ok(Out::Removed(r)) => self.msg = Some(Self::removed_text(&r)),
            Ok(Out::Design(b)) => {
                let n = b.candidates.len();
                self.msg = Some((false, format!("{n} Kandidaten entworfen – hör sie dir an und behalte deinen Favoriten.{}", b.warnings.first().map(|w| format!(" Hinweis: {w}")).unwrap_or_default())));
                (self.generate.names, self.generate.kept) = (vec![String::new(); n], vec![None; n]);
                self.generate.batch = Some(b);
            }
            Err(e) => {
                // a failed line keeps its red state; setup errors and cancels are no line errors
                if jobs::is_setup_error(&e) || cancelled {
                    self.states.iter_mut().filter(|s| matches!(s, Some(State::Failed | State::Rendering | State::Queued))).for_each(|s| *s = None);
                }
                if jobs::is_setup_error(&e) {
                    self.banner = Some(e);
                } else if cancelled && kind.renders() {
                    self.msg = Some((false, format!("{} abgebrochen – fertige Zeilen bleiben im Cache.", kind.label())));
                } else if cancelled {
                    self.msg = Some((false, format!("{} abgebrochen.", kind.label())));
                } else {
                    self.err(format!("{}: {e}", kind.label()));
                }
            }
        }
        if kind.exclusive()
            && let Some(d) = self.proj.as_ref().map(|p| p.dir.clone())
            && let Ok(p) = Project::load(&d)
        {
            self.set_proj(p);
        }
    }

    /// Cached lines (active quality) -> "aus Cache", others open; finished/failed states of this session stay. Takes
    /// (undo) from disk.
    pub fn cache_status(&mut self, st: &[render::Both]) {
        if self.states.len() != st.len() {
            self.states.resize(st.len(), None);
        }
        let draft = self.draft;
        for s in st {
            let k = &mut self.states[s.i - 1];
            if matches!(k, None | Some(State::Cached)) {
                *k = (if draft { s.draft } else { s.fin }).then_some(State::Cached);
            }
        }
        self.takes = st.iter().map(|s| s.take).collect();
    }

    /// Draft/final switch: other cache folder, so states are recomputed; the choice is remembered in project.toml
    /// (`settings.last_quality`, only that field, script.txt untouched, own write stamped: no reload).
    pub fn set_draft(&mut self, ctx: &egui::Context, on: bool) {
        if self.draft != on {
            self.draft = on;
            self.auto_quality = false;
            self.remember_quality();
            self.states.iter_mut().for_each(|s| *s = None);
            let k = usize::from(!on);
            for (s, b) in self.states.iter_mut().zip(&self.both) {
                *s = b[k].then_some(State::Cached);
            }
            self.relint(ctx);
        }
    }

    fn remember_quality(&mut self) {
        let q = self.quality();
        let Some(dir) = self.proj.as_ref().map(|p| p.dir.clone()) else {
            return;
        };
        // project.toml fresh from disk (an external edit not reloaded yet is not overwritten); script.txt is not parsed
        let r = stimmwerk_core::project::Meta::load(&dir).and_then(|mut m| {
            m.settings.last_quality = Some(q);
            stimmwerk_core::project::save_meta_at(&dir, &m)
        });
        match r {
            Ok(()) => {
                if let Some(p) = self.proj.as_mut() {
                    p.meta.settings.last_quality = Some(q);
                }
                self.stamp_toml();
            }
            Err(e) => self.err(format!("Qualität nicht gespeichert: {e:#}")),
        }
    }

    fn took(&mut self, rep: &render::Report) {
        let n = self.n_lines();
        self.takes = vec![0; n];
        if self.both.len() != n {
            self.both.resize(n, [false; 2]);
        }
        let k = usize::from(rep.quality == Quality::Final);
        for it in &rep.items {
            if let Some(t) = self.takes.get_mut(it.i - 1) {
                *t = it.take;
            }
            if let Some(b) = self.both.get_mut(it.i - 1) {
                b[k] = true;
            }
        }
    }

    pub fn play(&mut self, line: Option<usize>, p: &Path) {
        match self.player.play(p) {
            Ok(()) => self.playing = line,
            Err(e) => self.err(format!("afplay: {e}")),
        }
    }

    pub fn poll(&mut self, ctx: &egui::Context) {
        // detached jobs: drain, drop results, reap
        self.orphans.retain(|j| {
            while let Ok(m) = j.rx.try_recv() {
                if matches!(m, Msg::Done(_)) {
                    return false;
                }
            }
            !j.finished()
        });
        let mut k = 0;
        while k < self.jobs.len() {
            let mut done = None;
            while let Ok(m) = self.jobs[k].rx.try_recv() {
                match m {
                    Msg::Ev(e) => self.on_event(k, e),
                    Msg::Done(r) => {
                        done = Some(r);
                        break;
                    }
                }
            }
            match done {
                Some(r) => {
                    let j = self.jobs.remove(k);
                    self.on_done(j.kind.clone(), j.cancelled(), r);
                    if j.kind.exclusive() {
                        // takes changed on disk (also when cancelled/failed half-way, or export rendered missing final
                        // clips): both qualities' states and takes from disk again
                        self.relint(ctx);
                    }
                    if j.kind == JobKind::Lint
                        && let Some(refresh) = self.relint_again.take()
                    {
                        self.relint_with(ctx, refresh);
                    }
                }
                None => k += 1,
            }
        }
        if self.player.playing().is_none() {
            self.playing = None;
        }
    }

    fn shortcuts(&mut self, ctx: &egui::Context) {
        let typing = ctx.wants_keyboard_input();
        let (esc, cmd_r, nums) = ctx.input(|i| {
            let n = (0..Tab::ALL.len()).find(|&k| i.modifiers.command && i.key_pressed(tabs::num_key(k)));
            (i.key_pressed(egui::Key::Escape), i.modifiers.command && i.key_pressed(egui::Key::R), n)
        });
        if esc && !typing {
            self.cancel_all();
        }
        if self.proj.is_some() {
            if cmd_r {
                self.start(ctx, JobKind::Render);
            }
            if let Some(n) = nums {
                self.tab = Tab::ALL[n];
            }
        }
    }

    fn top(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("top").show(ctx, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                if self.proj.is_some() && ui.button("⬅ Projekte").clicked() {
                    self.close();
                }
                let title = self.proj.as_ref().map_or("Stimmwerk".into(), |p| p.meta.title.clone());
                ui.label(RichText::new(title).strong().size(17.0));
                if self.proj.is_some() {
                    self.project_menu(ctx, ui);
                }
                if let Some(l) = &self.lint {
                    let pal = theme::pal(ui);
                    let (c, t) = match (l.errors, l.warnings) {
                        (0, 0) => (pal.ok, "✔ Skript ok".to_string()),
                        (0, w) => (pal.warn, format!("⚠ {w} Hinweis{}", if w == 1 { "" } else { "e" })),
                        (e, _) => (pal.err, format!("✖ {e} Fehler")),
                    };
                    if ui.add(egui::Button::new(RichText::new(t).color(c)).frame(false)).on_hover_text("Prüfergebnis – Klick zeigt die Hinweise").clicked() {
                        self.tab = Tab::Skript;
                        self.show_lint = true;
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let mut stop = None;
                    for (k, j) in self.jobs.iter().enumerate().filter(|(_, j)| j.kind != JobKind::Lint) {
                        if j.kind.cancellable() && ui.small_button("✖ Abbrechen").on_hover_text("Auftrag abbrechen (Esc); fertige Zeilen bleiben").clicked() {
                            stop = Some(k);
                        }
                        let p = if j.total > 0 { format!(" {}/{}", j.done, j.total) } else { String::new() };
                        let w = if j.gpu_wait {
                            " · wartet auf GPU"
                        } else if j.model_loading() {
                            " · lädt Sprachmodell"
                        } else {
                            ""
                        };
                        ui.label(format!("{}{p}{w}", j.kind.label()));
                        ui.spinner();
                    }
                    if let Some(k) = stop {
                        self.jobs[k].cancel();
                    }
                });
            });
            if self.proj.is_some() {
                ui.add_space(2.0);
                ui.horizontal(|ui| {
                    for t in Tab::ALL {
                        ui.selectable_value(&mut self.tab, t, RichText::new(t.label()).size(15.0)).on_hover_text(t.tip());
                    }
                });
            }
            ui.add_space(4.0);
        });
    }

    /// "Projekt …": rename / trash the open project, switch to another one.
    fn project_menu(&mut self, ctx: &egui::Context, ui: &mut egui::Ui) {
        let Some((dir, title)) = self.proj.as_ref().map(|p| (p.dir.clone(), p.meta.title.clone())) else { return };
        let can = !self.busy() && !self.ending(&dir);
        let mut act = None;
        let mut go = None;
        ui.menu_button("Projekt …", |ui| {
            act = tabs::start::menu(ui, can, true);
            let others: Vec<_> = self.known().iter().filter(|i| i.path != dir).map(|i| (i.title.clone(), i.path.clone())).collect();
            if !others.is_empty() {
                ui.separator();
                ui.menu_button("Wechseln zu", |ui| {
                    for (t, d) in others {
                        if ui.button(t).clicked() {
                            go = Some(d);
                            ui.close();
                        }
                    }
                });
            }
        })
        .response
        .on_hover_text("Projekt umbenennen, löschen oder wechseln");
        if let Some(a) = act {
            tabs::start::act(self, ctx, a, &dir, &title);
        }
        if let Some(d) = go {
            self.open(ctx, &d);
        }
    }

    fn messages(&mut self, ctx: &egui::Context) {
        if self.banner.is_none() && self.msg.is_none() {
            return;
        }
        egui::TopBottomPanel::bottom("msg").show(ctx, |ui| {
            let pal = theme::pal(ui);
            if let Some(b) = self.banner.clone() {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(format!("Einrichtung: {b}")).color(pal.err).strong());
                    if ui.small_button("Schließen").clicked() {
                        self.banner = None;
                    }
                });
            }
            if let Some((is_err, m)) = self.msg.clone() {
                ui.horizontal(|ui| {
                    ui.label(if is_err { RichText::new(m).color(pal.err) } else { RichText::new(m) });
                    if ui.small_button("Schließen").clicked() {
                        self.msg = None;
                    }
                });
            }
        });
    }

    /// One frame (eframe::App::update and the headless shots).
    pub fn frame(&mut self, ctx: &egui::Context) {
        self.poll(ctx);
        self.check_watch(ctx);
        self.shortcuts(ctx);
        self.top(ctx);
        self.messages(ctx);
        if self.proj.is_none() {
            tabs::start::show(self, ctx);
        } else {
            match self.tab {
                Tab::Skript => tabs::skript::show(self, ctx),
                Tab::Stimmen => tabs::stimmen::show(self, ctx),
                Tab::Erzeugen => tabs::erzeugen::show(self, ctx),
                Tab::Musik => tabs::musik::show(self, ctx),
                Tab::Export => tabs::export::show(self, ctx),
                Tab::Einstellungen => tabs::einstellungen::show(self, ctx),
            }
        }
        tabs::start::dialogs(self, ctx);
        if !self.jobs.is_empty() || !self.orphans.is_empty() || self.playing.is_some() || self.player.playing().is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(300));
        }
        // external edits of script.txt show up without input events
        ctx.request_repaint_after(std::time::Duration::from_secs(1));
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _: &mut eframe::Frame) {
        self.frame(ctx);
    }

    fn on_exit(&mut self, _: Option<&eframe::glow::Context>) {
        self.shutdown();
    }
}
