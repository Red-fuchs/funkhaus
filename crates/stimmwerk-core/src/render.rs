//! `render` / `reroll`: script lines -> omnivoice clips in `takes/<quality>/` (content-hash cache) -> `out/mix.wav`.
//! TTS per line under the shared GPU lock, whole render with interactive priority (as funkhaus). Loudness/export: Etappe 3.
//! Music cues are mixed in like the export does (`cues::mix_cues`), but from the cache only: a gen cue without its file,
//! a lint error or an unreadable source is a warning and skipped – render never calls acestep and never fails on music.
//!
//! Seeds: clip key and seed are computed WITHOUT the quality (`key_cfg`), so draft and final of a line use the same seed
//! and the same takes; quality only picks the folder `takes/<quality>/` and omnivoice's `--quality`. Takes live in
//! one project-wide `takes/rerolls.json` (a reroll in draft is the same take in final). funkhaus keeps its own keys.
//!
//! Jobs: one render/reroll/export per project at a time (`<proj>/.lock`). Cancel: the caller passes a fresh
//! `AtomicBool` per job (the GUI creates one per job; the CLI's is set by Ctrl-C); it is checked before each line.
//! Progress: structured `Event`s through `&Prog` (the CLI prints them as text or, with `--events`, as JSON lines).
use crate::{Project, cues, speakers};
use anyhow::{Context, Result, bail};
use funkhaus_audio::{mix, script::Line};
use funkhaus_sys::gpu;
use funkhaus_voice::{Tts, lexicon::Lexicon, persona::VoiceSpec, reroll, tts};
use serde::Serialize;
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

/// TTS command override (tests, other installs); default `omnivoice`.
pub const TTS_ENV: &str = "STIMMWERK_TTS_CMD";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Quality {
    Draft,
    #[default]
    Final,
}

impl Quality {
    pub fn as_str(self) -> &'static str {
        match self {
            Quality::Draft => "draft",
            Quality::Final => "final",
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Opts {
    /// 1-based script line: render only this line (-> out/line.wav), no mix.
    pub line: Option<usize>,
    pub fresh: bool,
    pub quality: Quality,
}

/// State of a script line in a job.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    Queued,
    Rendering,
    Cached,
    Done,
    Failed,
}

/// Structured progress. JSON (`--events`): `{"ev":"line","i":3,"n":7,"state":"done","tts_secs":2.9}`.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "ev", rename_all = "lowercase")]
pub enum Event {
    /// job phase: `render`, `disclosure`, `export`, `parts`
    Stage { name: String },
    /// `i`: 1-based script line, `n`: script lines; `tts_secs`: TTS wall time (`done`), not audio length
    Line {
        i: usize,
        n: usize,
        state: State,
        #[serde(skip_serializing_if = "Option::is_none")]
        tts_secs: Option<f64>,
    },
    Mix,
    Master,
    /// free text (TTS line preview, "wartet auf GPU …" / "GPU frei", warnings)
    Msg { text: String },
    /// job finished and the project lock is released
    Done,
}

/// Progress sink; `Sync` so a GUI can forward from its worker thread.
pub type Prog<'a> = dyn Fn(Event) + Sync + 'a;

/// One script line of a job.
#[derive(Serialize, Debug, Clone, PartialEq)]
pub struct LineRep {
    /// 1-based script line
    pub i: usize,
    pub who: String,
    /// clip files in `takes/<quality>/` (one per text segment)
    pub clips: Vec<PathBuf>,
    /// trimmed clip incl. `[pause]` (s)
    pub duration: f64,
    /// timeline start in `wav` (s)
    pub start: f64,
    /// no TTS needed in this job
    pub cached: bool,
    /// active take index (0 = base take)
    pub take: usize,
}

#[derive(Serialize, Debug, Clone)]
pub struct Report {
    pub quality: Quality,
    pub lines: usize,
    /// 1-based lines that needed TTS
    pub rendered: Vec<usize>,
    pub cached: usize,
    pub tts_calls: usize,
    /// audio length of `wav`
    pub secs: f64,
    pub elapsed: f64,
    /// out/mix.wav, or out/line.wav with `--line`
    pub wav: PathBuf,
    pub takes: PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<usize>,
    /// per rendered line, script order
    pub items: Vec<LineRep>,
    /// music/ambience cues as placed in the mix (cached sources only)
    pub cues: Vec<cues::Placed>,
    /// cues skipped (not generated yet, lint error, unreadable) or cut at the mix end
    pub warnings: Vec<String>,
}

pub type Say<'a> = dyn FnMut(&Path, &str, &VoiceSpec, u64) -> Result<()> + 'a;

pub fn tts_cfg(p: &Project, q: Quality) -> Tts {
    Tts { cmd: tts_cmd(), lang: p.meta.lang.clone(), quality: q.as_str().into(), ..Tts::default() }
}

/// Config for clip keys/seeds: quality-independent (draft and final share seeds and takes).
pub fn key_cfg(p: &Project) -> Tts {
    Tts { quality: String::new(), ..tts_cfg(p, Quality::Final) }
}

pub fn takes_dir(p: &Project, q: Quality) -> PathBuf {
    takes_root(p).join(q.as_str())
}

/// `<proj>/takes`: holds the project-wide `rerolls.json` and one folder per quality.
pub fn takes_root(p: &Project) -> PathBuf {
    p.dir.join("takes")
}

pub const BUSY: &str = "anderes Programm rendert gerade";
/// Lock holder is a `voice revoke|rm` purge (role marker in the lock file).
pub const PURGING: &str = "Widerruf läuft gerade – Aufnahmen einer Stimme werden gelöscht";

/// Held `<dir>/.lock`. The file names the holder's role (`render <pid>` / `purge <pid>`) so a waiting program can say
/// why it waits; emptied again before the lock is released.
#[derive(Debug)]
pub struct Held(std::fs::File);

impl Drop for Held {
    fn drop(&mut self) {
        let _ = self.0.set_len(0);
    }
}

/// Exclusive `<proj>/.lock` for a whole job (takes, rerolls.json, out/ are shared between qualities).
/// Only `WouldBlock` means "someone else renders"; other failures are reported as what they are.
pub(crate) fn lock(p: &Project) -> Result<Held> {
    // short retry: the previous job of this process may still be closing its lock file
    lock_dir(&p.dir, std::time::Duration::from_millis(300), "render", None)
}

/// `<dir>/.lock` for `role`, waiting up to `wait` while another job holds it (`voice revoke|rm` waits for a running
/// render); `on_wait` is called once before waiting.
pub(crate) fn lock_dir(dir: &Path, wait: std::time::Duration, role: &str, on_wait: Option<&dyn Fn()>) -> Result<Held> {
    use std::io::Write;
    let path = dir.join(".lock");
    let f = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(&path).with_context(|| format!("Render-Sperre {} nicht anlegbar", path.display()))?;
    let t0 = std::time::Instant::now();
    let mut told = false;
    loop {
        match f.try_lock() {
            Ok(()) => {
                let _ = f.set_len(0);
                let _ = (&f).write_all(format!("{role} {}\n", std::process::id()).as_bytes());
                return Ok(Held(f));
            }
            Err(std::fs::TryLockError::WouldBlock) if t0.elapsed() < wait => {
                if !std::mem::replace(&mut told, true)
                    && let Some(w) = on_wait
                {
                    w();
                }
                std::thread::sleep(std::time::Duration::from_millis(30))
            }
            Err(std::fs::TryLockError::WouldBlock) => {
                let purging = std::fs::read_to_string(&path).is_ok_and(|s| s.starts_with("purge"));
                bail!("{}: {}", if purging { PURGING } else { BUSY }, dir.display())
            }
            Err(std::fs::TryLockError::Error(e)) => return Err(anyhow::Error::new(e).context(format!("Render-Sperre {} fehlgeschlagen", path.display()))),
        }
    }
}

/// Leftovers of a crashed job in a folder this project owns (`takes/…`, `out/`): `*.part.wav` clips and hidden temp
/// outputs (`part_path`) of dead processes. Call under the project lock.
pub(crate) fn sweep(dir: &Path) {
    sweep_in(dir, None);
}

/// `sweep` for a folder that is not the project's own (custom `--out`, shared caches): only hidden temps of dead
/// processes, and with `stems` only those whose target stem is in the list; no `*.part.wav`.
pub(crate) fn sweep_in(dir: &Path, stems: Option<&[String]>) {
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let n = e.file_name().to_string_lossy().into_owned();
        let gone = match part_owner(&n) {
            Some((stem, pid)) => pid.is_none_or(|p| !funkhaus_sys::gpu::alive(p)) && stems.is_none_or(|s| s.iter().any(|x| x == stem)),
            None => stems.is_none() && n.ends_with(".part.wav"),
        };
        if gone {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

/// `.<stem>.part.<pid>-<n>.<ext>` -> (stem, pid); legacy `.<stem>.part.<ext>` -> (stem, None).
fn part_owner(name: &str) -> Option<(&str, Option<i32>)> {
    let rest = name.strip_prefix('.')?;
    let k = rest.rfind(".part.")?;
    let tail = &rest[k + 6..];
    let pid = tail.split_once('.').and_then(|(tag, _)| tag.split_once('-')).and_then(|(p, _)| p.parse().ok());
    Some((&rest[..k], pid))
}

/// Writes via `<dir>/.<name>.part.<ext>` + rename, so readers never see a half-written file.
pub(crate) fn atomic(path: &Path, write: impl FnOnce(&Path) -> Result<()>) -> Result<()> {
    let tmp = part_path(path);
    write(&tmp).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })?;
    std::fs::rename(&tmp, path).with_context(|| path.display().to_string())
}

static PART_N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Hidden temp name next to `path`, unique per process and call: `.<stem>.part.<pid>-<n>.<ext>` (matched by `sweep`).
pub(crate) fn part_path(path: &Path) -> PathBuf {
    let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let ext = path.extension().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "tmp".into());
    path.with_file_name(format!(".{stem}.part.{}-{}.{ext}", std::process::id(), PART_N.fetch_add(1, Ordering::Relaxed)))
}

/// `takes/rerolls.json` (or an old per-quality one); an unreadable file is renamed to `*.broken` with a warning
/// instead of blocking every render. Call under the project lock.
pub(crate) fn load_rerolls(dir: &Path, prog: &Prog) -> Result<reroll::File> {
    match reroll::File::load(dir) {
        Ok(f) => Ok(f),
        Err(e) => {
            let f = dir.join("rerolls.json");
            if !f.is_file() {
                return Err(e);
            }
            let b = (0..).map(|k| dir.join(if k == 0 { "rerolls.json.broken".into() } else { format!("rerolls.json.broken.{k}") })).find(|b| !b.exists()).expect("free name");
            std::fs::rename(&f, &b).with_context(|| f.display().to_string())?;
            prog(Event::Msg { text: format!("Warnung: {} war unlesbar ({e:#}) – umbenannt in {}, frühere Takes zurückgesetzt", f.display(), b.display()) });
            Ok(reroll::File::default())
        }
    }
}

/// A cancelled job ends with `CANCELLED` on top; a real error that happened while cancelling stays attached as cause.
pub(crate) fn cancelled(e: anyhow::Error, cancel: &AtomicBool) -> anyhow::Error {
    if cancel.load(Ordering::Relaxed) && e.to_string() != tts::CANCELLED { e.context(tts::CANCELLED) } else { e }
}

pub(crate) struct Ctx {
    pub t: Tts,
    pub lines: Vec<Line>,
    pub voices: tts::Voices,
    pub lex: Lexicon,
    pub dir: PathBuf,
}

pub(crate) fn ctx(p: &Project, dirs: &[PathBuf], q: Quality) -> Result<Ctx> {
    ctx_lines(p, p.script.lines().cloned().collect(), takes_dir(p, q), dirs)
}

/// `ctx` for given lines (sync cues) and takes folder; same checks as for the script.
pub(crate) fn ctx_lines(p: &Project, lines: Vec<Line>, dir: PathBuf, dirs: &[PathBuf]) -> Result<Ctx> {
    if let Some(n) = p.script.bad().first() {
        bail!("script.txt Zeile {n} fehlerhaft ({} insgesamt) – `stimmwerk lint`", p.script.bad().len());
    }
    if lines.is_empty() {
        bail!("script.txt enthält keine Zeilen");
    }
    Ok(Ctx { t: key_cfg(p), voices: speakers::voices(p, dirs)?, lex: Lexicon::load(&p.lexicon_path()), dir, lines })
}

/// Sync clips (cue takes and their atempo versions): `takes/<quality>/sync/`, own folder so a script render never prunes them.
pub fn sync_takes_dir(p: &Project, q: Quality) -> PathBuf {
    takes_dir(p, q).join("sync")
}

/// Own reroll namespace of sync cues: `takes/sync/rerolls.json` (a script render prunes only `takes/rerolls.json`).
pub fn sync_rerolls_dir(p: &Project) -> PathBuf {
    takes_root(p).join("sync")
}

impl Ctx {
    pub(crate) fn keys(&self, i: usize) -> Result<Vec<u64>> {
        self.keys_with(&self.t, i)
    }
    fn keys_with(&self, t: &Tts, i: usize) -> Result<Vec<u64>> {
        tts::line_keys(t, &self.lines, &self.voices, &self.lex, &t.lang, i)
    }
}

/// Old layout (before quality-free keys): `takes/<quality>/rerolls.json` keyed with the quality in the hash.
/// Takes of current script lines move to the project-wide file (final wins over draft); the old file is renamed
/// to `rerolls.json.migrated`. Salts are kept, so take indices survive; the clips render once with the new seeds.
pub(crate) fn migrate_rerolls(p: &Project, c: &Ctx, prog: &Prog) -> Result<bool> {
    let root = takes_root(p);
    let mut new = load_rerolls(&root, prog)?;
    let mut moved = false;
    for q in [Quality::Final, Quality::Draft] {
        let d = takes_dir(p, q);
        if !d.join("rerolls.json").is_file() {
            continue;
        }
        let old = load_rerolls(&d, prog)?;
        if !d.join("rerolls.json").is_file() {
            continue; // was broken: renamed, nothing to migrate
        }
        let t = Tts { quality: q.as_str().into(), ..c.t.clone() };
        for i in 0..c.lines.len() {
            for (o, n) in c.keys_with(&t, i)?.into_iter().zip(c.keys(i)?) {
                if let Some(tk) = old.takes.get(&format!("{o:016x}")) {
                    new.takes.entry(format!("{n:016x}")).or_insert_with(|| tk.clone());
                }
            }
        }
        new.save(&root)?;
        std::fs::rename(d.join("rerolls.json"), d.join("rerolls.json.migrated"))?;
        moved = true;
    }
    Ok(moved)
}

/// TTS command: `$STIMMWERK_TTS_CMD` or `omnivoice`.
pub fn tts_cmd() -> String {
    std::env::var(TTS_ENV).ok().filter(|c| !c.is_empty()).unwrap_or_else(|| "omnivoice".into())
}

/// Real omnivoice for project `p` (`$STIMMWERK_TTS_CMD`), see `with_say`.
pub(crate) fn with_real_say<T>(p: &Project, q: Quality, cancel: &AtomicBool, prog: &Prog, f: impl FnOnce(&mut Say) -> Result<T>) -> Result<T> {
    with_say(tts_cfg(p, q), cancel, prog, f)
}

/// `omnivoice say` per call, as a child process that is killed when `cancel` is set (GUI cancel < 2 s; the CLI's Ctrl-C
/// reaches the child anyway). Looked up, and interactive GPU priority taken, only at the first TTS call:
/// project/persona errors come first and a fully cached job needs neither omnivoice nor the GPU.
pub(crate) fn with_say<T>(t: Tts, cancel: &AtomicBool, prog: &Prog, f: impl FnOnce(&mut Say) -> Result<T>) -> Result<T> {
    let txt = |m: &str| prog(Event::Msg { text: m.into() });
    let mut bin: Option<PathBuf> = None;
    let mut g: Option<Option<gpu::PrioGuard>> = None;
    f(&mut |w, x, v, s| {
        // last gate before audio is made: consent / anchor checked (ref_ok), whoever built the spec
        v.validate()?;
        if bin.is_none() {
            bin = Some(funkhaus_sys::which(&t.cmd).context("omnivoice nicht gefunden")?);
        }
        if let Some(g) = g.get_or_insert_with(gpu::PrioGuard::hold) {
            g.touch();
        }
        let log = part_path(&w.with_extension("log"));
        let args = tts::say_args(x, w, v, s, &t.lang, &t.quality);
        // the GPU wait itself is cancellable (another app may hold the GPU for minutes)
        let st = gpu::with_lock_cancel(&txt, cancel, || run_child(bin.as_deref().expect("set above"), &args, &log, cancel)).map_err(|e| if cancel.load(Ordering::Relaxed) { anyhow::anyhow!(tts::CANCELLED) } else { e });
        let out = std::fs::read_to_string(&log).unwrap_or_default();
        let _ = std::fs::remove_file(&log);
        if !st??.success() || !w.exists() {
            bail!("{}", out.trim());
        }
        Ok(())
    })
}

unsafe extern "C" {
    fn kill(pid: i32, sig: i32) -> i32;
}

/// Ends our own child: SIGTERM (omnivoice can clean up), SIGKILL after 500 ms.
pub(crate) fn stop_child(ch: &mut std::process::Child) {
    // SAFETY: signals only our own, not yet reaped child (pid cannot be reused before wait)
    unsafe { kill(ch.id() as i32, 15) };
    let t0 = std::time::Instant::now();
    while t0.elapsed() < std::time::Duration::from_millis(500) {
        if matches!(ch.try_wait(), Ok(Some(_))) {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let _ = ch.kill();
    let _ = ch.wait();
}

/// Runs `bin args`, output into `log`; polls `cancel` every 50 ms and stops the child (`stop_child`) when it is set.
pub(crate) fn run_child(bin: &Path, args: &[String], log: &Path, cancel: &AtomicBool) -> Result<std::process::ExitStatus> {
    if cancel.load(Ordering::Relaxed) {
        bail!("{}", tts::CANCELLED);
    }
    let f = std::fs::File::create(log).with_context(|| log.display().to_string())?;
    // omnivoice's --ref prompt cache lives with Stimmwerk's data, so revoke can delete it
    let mut ch = std::process::Command::new(bin).args(args).env("OMNIVOICE_CACHE", crate::omnivoice_cache()).stdin(std::process::Stdio::null()).stdout(f.try_clone()?).stderr(f).spawn().with_context(|| bin.display().to_string())?;
    loop {
        if let Some(s) = ch.try_wait()? {
            return Ok(s);
        }
        if cancel.load(Ordering::Relaxed) {
            stop_child(&mut ch);
            bail!("{}", tts::CANCELLED);
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

pub fn render(p: &Project, dirs: &[PathBuf], o: &Opts, cancel: &AtomicBool, prog: &Prog) -> Result<Report> {
    with_real_say(p, o.quality, cancel, prog, |say| render_with(p, dirs, o, cancel, prog, say))
}

/// `render` with an injectable `say(wav, text, voice, seed)`.
pub fn render_with(p: &Project, dirs: &[PathBuf], o: &Opts, cancel: &AtomicBool, prog: &Prog, say: &mut Say) -> Result<Report> {
    let c = ctx(p, dirs, o.quality)?;
    let l = lock(p)?;
    let r = run(p, &c, o, cancel, prog, say)?.0;
    drop(l);
    prog(Event::Done);
    Ok(r)
}

/// Assumes the project lock is held. Also returns the clips (trimmed, `[pause]` as silence) of the rendered range and their rate.
pub(crate) fn run(p: &Project, c: &Ctx, o: &Opts, cancel: &AtomicBool, prog: &Prog, say: &mut Say) -> Result<(Report, Vec<Vec<f32>>, u32)> {
    let t0 = std::time::Instant::now();
    let n = c.lines.len();
    if let Some(l) = o.line.filter(|&l| l < 1 || l > n) {
        bail!("Zeile {l} außerhalb 1..={n}");
    }
    std::fs::create_dir_all(&c.dir)?;
    let out_dir = p.dir.join("out");
    std::fs::create_dir_all(&out_dir)?;
    for d in [c.dir.clone(), c.dir.join("ai"), out_dir.clone()] {
        sweep(&d);
    }
    // takes of lines that no longer exist go first, so they cannot keep orphan clips alive
    migrate_rerolls(p, c, prog)?;
    let root = takes_root(p);
    let mut rf = load_rerolls(&root, prog)?;
    let base: Vec<Vec<u64>> = (0..n).map(|i| c.keys(i)).collect::<Result<_>>()?;
    if rf.prune(&base.iter().flatten().copied().collect()) {
        rf.save(&root)?;
    }
    let mut rr = rf.rerolls();
    let range = o.line.map_or(0..n, |l| l - 1..l);
    if o.line.is_some() {
        // a single-line render must not prune the other lines' clips
        let eff = |b: u64| rr.active.get(&b).map_or(b, |&s| tts::salted(b, s));
        let all: Vec<u64> = base.iter().flatten().map(|&b| eff(b)).collect();
        rr.keep.extend(all);
    }
    prog(Event::Stage { name: "render".into() });
    for i in range.clone() {
        prog(Event::Line { i: i + 1, n, state: State::Queued, tts_secs: None });
    }
    // per line of the range: Some(true) = needed TTS; line state straight from the renderer (no file-name parsing)
    let states = RefCell::new(vec![None::<bool>; range.len()]);
    let started = RefCell::new(std::time::Instant::now());
    let on_line = |k: usize, s: tts::LineState| {
        let i = range.start + k + 1;
        let (state, tts_secs) = match s {
            tts::LineState::Rendering => {
                *started.borrow_mut() = std::time::Instant::now();
                (State::Rendering, None)
            }
            tts::LineState::Cached => {
                states.borrow_mut()[k] = Some(false);
                (State::Cached, None)
            }
            tts::LineState::Done => {
                states.borrow_mut()[k] = Some(true);
                (State::Done, Some(r2(started.borrow().elapsed().as_secs_f64())))
            }
            tts::LineState::Failed => (State::Failed, None),
        };
        prog(Event::Line { i, n, state, tts_secs });
    };
    let mut calls = 0;
    let mut counted = |w: &Path, x: &str, v: &VoiceSpec, s: u64| -> Result<()> {
        calls += 1;
        say(w, x, v, s)
    };
    let txt = |m: &str| prog(Event::Msg { text: m.into() });
    let lines = &c.lines[range.clone()];
    let q = tts::RenderReq { t: &c.t, lines, voices: &c.voices, lex: &c.lex, lang: &c.t.lang, dir: &c.dir, fresh: o.fresh, rr: &rr, prog: &txt, cancel: Some(cancel), on_line: Some(&on_line) };
    let parts = tts::render_with(&q, &mut counted).map_err(|e| cancelled(e, cancel))?;
    let (clips, sr) = tts::clips(&parts)?;
    let (mut placed, mut warnings) = (vec![], vec![]);
    let (wav, out, starts) = match o.line {
        Some(_) => (out_dir.join("line.wav"), clips.concat(), vec![0]),
        None => {
            prog(Event::Mix);
            let lens: Vec<usize> = clips.iter().map(Vec::len).collect();
            let (starts, total) = mix::timeline(lines, &lens, sr);
            let cl: Vec<&[f32]> = clips.iter().map(Vec::as_slice).collect();
            // cues from the cache only; the bed (if any) decides the tail length as in the export
            let (mut srcs, skipped) = if p.meta.cues.is_empty() { Default::default() } else { cues::playable(p, &crate::export::ffmpeg(), sr) };
            let ks: Vec<usize> = srcs.keys().copied().collect();
            let end = starts.iter().zip(&lens).map(|(s, n)| s + n).max().unwrap_or(0);
            let total = cues::mix_len(ks.iter().map(|&i| &p.meta.cues[i - 1]), n, end, total, sr);
            let mut m = mix::place_raw(&starts, &cl, &lines.iter().map(mix::line_gain).collect::<Vec<_>>(), total);
            placed = cues::mix_cues(&p.meta.music, ks.iter().map(|&i| (i, &p.meta.cues[i - 1])), &mut m, &starts, sr, |i, _| Ok(srcs.remove(&i).unwrap_or_default()))?;
            warnings = skipped.into_iter().chain(cues::warnings(&placed)).collect();
            mix::norm(&mut m);
            (out_dir.join("mix.wav"), m, starts)
        }
    };
    for w in &warnings {
        prog(Event::Msg { text: format!("Warnung: {w}") });
    }
    atomic(&wav, |t| mix::write(t, &out, sr))?;
    let states = states.into_inner();
    let take = |i: usize| base[i].iter().filter_map(|k| rf.takes.get(&format!("{k:016x}")).map(|t| t.active)).max().unwrap_or(0);
    let items = range
        .clone()
        .zip(&parts)
        .zip(&clips)
        .zip(&starts)
        .map(|(((i, ps), cl), &s)| LineRep {
            i: i + 1,
            who: c.lines[i].who.clone(),
            clips: ps.iter().filter_map(|p| if let tts::Part::Wav(w) = p { Some(w.clone()) } else { None }).collect(),
            duration: r2(cl.len() as f64 / sr as f64),
            start: r2(s as f64 / sr as f64),
            cached: states[i - range.start] != Some(true),
            take: take(i),
        })
        .collect::<Vec<_>>();
    record_ref_audio(p, &c.voices, items.iter().map(|it| (it.who.as_str(), it.clips.clone())), std::slice::from_ref(&wav))?;
    let rendered: Vec<usize> = items.iter().filter(|l| !l.cached).map(|l| l.i).collect();
    let r = Report {
        quality: o.quality,
        lines: lines.len(),
        cached: lines.len() - rendered.len(),
        rendered,
        tts_calls: calls,
        secs: r2(out.len() as f64 / sr as f64),
        elapsed: r2(t0.elapsed().as_secs_f64()),
        wav,
        takes: c.dir.clone(),
        line: o.line,
        items,
        cues: placed,
        warnings,
    };
    Ok((r, clips, sr))
}

/// Index (`derived`) of audio made with ref voices: each speaker's clips under its ref sha256, `shared` (mix/export
/// files) under every ref voice that took part. Lets `voice revoke|rm` delete it later.
pub(crate) fn record_ref_audio<'a>(p: &Project, voices: &tts::Voices, clips: impl Iterator<Item = (&'a str, Vec<PathBuf>)>, shared: &[PathBuf]) -> Result<()> {
    let mut by: std::collections::BTreeMap<(&str, &str), Vec<PathBuf>> = Default::default();
    for (who, cl) in clips {
        if let Some(v) = voices.get(who).filter(|v| v.ref_wav.is_some()) {
            let name = p.meta.speakers.get(who).and_then(|s| s.persona.as_deref()).unwrap_or(who);
            by.entry((v.ref_sha.as_str(), name)).or_default().extend(cl);
        }
    }
    for ((sha, name), mut f) in by {
        f.extend(shared.iter().cloned());
        crate::derived::record(&takes_root(p), sha, name, &f)?;
    }
    Ok(())
}

/// Cache state of one script line without rendering (GUI on open).
#[derive(Serialize, Debug, Clone, PartialEq)]
pub struct LineStatus {
    /// 1-based script line
    pub i: usize,
    /// every clip of the active take exists in `takes/<quality>/`
    pub cached: bool,
    /// active take index from `takes/rerolls.json` (> 0: undo possible)
    pub take: usize,
}

/// Per script line: is the active take cached for quality `q`, and which take is active. Read-only, no lock, no TTS
/// (keys computed as `render` does; old per-quality reroll files are not migrated here).
pub fn status(p: &Project, dirs: &[PathBuf], q: Quality) -> Result<Vec<LineStatus>> {
    let c = ctx(p, dirs, q)?;
    let rf = reroll::File::load(&takes_root(p)).unwrap_or_default();
    let rr = rf.rerolls();
    (0..c.lines.len())
        .map(|i| {
            let keys = c.keys(i)?;
            let cached = keys.iter().all(|&b| c.dir.join(format!("{:016x}.wav", rr.active.get(&b).map_or(b, |&s| tts::salted(b, s)))).is_file());
            let take = keys.iter().filter_map(|k| rf.takes.get(&format!("{k:016x}")).map(|t| t.active)).max().unwrap_or(0);
            Ok(LineStatus { i: i + 1, cached, take })
        })
        .collect()
}

/// Cache state of one script line in both qualities (GUI: „Entwurf ✓ · Final –“).
#[derive(Serialize, Debug, Clone, Copy, PartialEq, Eq)]
pub struct Both {
    /// 1-based script line
    pub i: usize,
    pub draft: bool,
    #[serde(rename = "final")]
    pub fin: bool,
    /// active take index (project-wide, same in both qualities)
    pub take: usize,
}

/// `status` for draft and final at once (same keys and takes; only the folder differs).
pub fn status_both(p: &Project, dirs: &[PathBuf]) -> Result<Vec<Both>> {
    let (d, f) = (status(p, dirs, Quality::Draft)?, status(p, dirs, Quality::Final)?);
    Ok(d.iter().zip(&f).map(|(d, f)| Both { i: d.i, draft: d.cached, fin: f.cached, take: f.take }).collect())
}

pub(crate) fn r2(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}

#[derive(Serialize, Debug)]
pub struct Reroll {
    pub line: usize,
    pub take: usize,
    pub salt: Option<u64>,
    pub undo: bool,
    pub keys: Vec<String>,
    pub render: Report,
}

pub fn reroll(p: &Project, dirs: &[PathBuf], line: usize, undo: bool, q: Quality, cancel: &AtomicBool, prog: &Prog) -> Result<Reroll> {
    with_real_say(p, q, cancel, prog, |say| reroll_with(p, dirs, line, undo, q, cancel, prog, say))
}

/// New take (or `undo`: previous take) of 1-based `line`, then full render + mix.
#[allow(clippy::too_many_arguments)]
pub fn reroll_with(p: &Project, dirs: &[PathBuf], line: usize, undo: bool, q: Quality, cancel: &AtomicBool, prog: &Prog, say: &mut Say) -> Result<Reroll> {
    let c = ctx(p, dirs, q)?;
    if line < 1 || line > c.lines.len() {
        bail!("Zeile {line} außerhalb 1..={}", c.lines.len());
    }
    let l = lock(p)?;
    let keys = c.keys(line - 1)?;
    if keys.is_empty() {
        bail!("Zeile {line} hat keinen gesprochenen Text");
    }
    migrate_rerolls(p, &c, prog)?;
    let root = takes_root(p);
    let mut f = load_rerolls(&root, prog)?;
    let (take, salt) = f.bump(line, &c.lines[line - 1].text, &keys, undo)?;
    f.save(&root)?;
    let render = run(p, &c, &Opts { quality: q, ..Opts::default() }, cancel, prog, say)?.0;
    drop(l);
    prog(Event::Done);
    Ok(Reroll { line, take, salt, undo, keys: keys.iter().map(|k| format!("{k:016x}")).collect(), render })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    const FIX: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/stimmwerk/project/leuchtturm.studio");

    fn fixture() -> (tempfile::TempDir, Project) {
        let t = tempfile::tempdir().unwrap();
        let d = t.path().join("l.studio");
        std::fs::create_dir_all(&d).unwrap();
        for f in ["project.toml", "script.txt", "lexicon.tsv"] {
            std::fs::copy(Path::new(FIX).join(f), d.join(f)).unwrap();
        }
        let p = Project::load(&d).unwrap();
        (t, p)
    }

    #[test]
    fn status_from_cache_and_takes() {
        let (_t, mut p) = fixture();
        let n = Cell::new(0);
        let none = AtomicBool::new(false);
        let st = |p: &Project, q| status(p, &[], q).unwrap().iter().map(|s| (s.cached, s.take)).collect::<Vec<_>>();
        assert_eq!(st(&p, Quality::Final), vec![(false, 0); 7]);
        go(&p, &Opts::default(), &n);
        assert_eq!(st(&p, Quality::Final), vec![(true, 0); 7]);
        assert_eq!(st(&p, Quality::Draft), vec![(false, 0); 7]);
        reroll_with(&p, &[], 2, false, Quality::Final, &none, &|_| {}, &mut fake(&n)).unwrap();
        let s = st(&p, Quality::Final);
        assert_eq!((s[1], s[0]), ((true, 1), (true, 0)));
        let l = p.script.items.iter_mut().filter_map(|i| if let crate::script::Item::Line(l) = i { Some(l) } else { None }).nth(4).unwrap();
        l.text = "Ganz neuer Text.".into();
        assert_eq!(st(&p, Quality::Final)[4], (false, 0));
        n.set(0);
        assert_eq!(n.get(), 0, "status never renders");
    }

    #[test]
    fn refuses_bad_lines() {
        let (_t, mut p) = fixture();
        p.script = crate::script::Script::parse("ERZ|0|Gut.\nERZ|x|Zahl.\n").unwrap();
        let e = render_with(&p, &[], &Opts::default(), &AtomicBool::new(false), &|_| {}, &mut fake(&Cell::new(0))).unwrap_err();
        assert!(e.to_string().contains("Zeile 2 fehlerhaft"), "{e}");
    }

    /// Fake TTS: a short tone whose level depends on the seed (rerolls give other audio).
    fn fake(n: &Cell<usize>) -> impl FnMut(&Path, &str, &VoiceSpec, u64) -> Result<()> + '_ {
        move |w, _, _, s| {
            n.set(n.get() + 1);
            mix::write(w, &vec![0.1 + (s % 1000) as f32 * 0.0005; 2400], 24_000)
        }
    }

    fn go(p: &Project, o: &Opts, n: &Cell<usize>) -> Report {
        render_with(p, &[], o, &AtomicBool::new(false), &|_| {}, &mut fake(n)).unwrap()
    }

    fn md5ish(p: &Path) -> Vec<u8> {
        std::fs::read(p).unwrap()
    }

    #[test]
    fn cache_line_edit_reroll_undo() {
        let (_t, mut p) = fixture();
        let n = Cell::new(0);
        let r = go(&p, &Opts::default(), &n);
        assert_eq!((r.lines, r.tts_calls, r.cached, r.rendered.len()), (7, 8, 0, 7)); // line 4 has 2 text segments
        assert!(r.secs > 0.0 && r.wav.ends_with("out/mix.wav"));
        let mix0 = md5ish(&r.wav);
        n.set(0);
        let r = go(&p, &Opts::default(), &n);
        assert_eq!((r.tts_calls, r.cached), (0, 7));
        assert_eq!(md5ish(&r.wav), mix0);
        // edit one line -> exactly 1 TTS
        let l = p.script.items.iter_mut().filter_map(|i| if let crate::script::Item::Line(l) = i { Some(l) } else { None }).nth(4).unwrap();
        l.text = "Sie steigen langsam die Wendeltreppe hinauf.".into();
        let r = go(&p, &Opts::default(), &n);
        assert_eq!((r.tts_calls, r.rendered.clone()), (1, vec![5]));
        let mix1 = md5ish(&r.wav);
        // reroll -> 1 TTS, other audio; undo -> 0 TTS, identical mix
        n.set(0);
        let st = AtomicBool::new(false);
        let rr = reroll_with(&p, &[], 2, false, Quality::Final, &st, &|_| {}, &mut fake(&n)).unwrap();
        assert_eq!((n.get(), rr.take, rr.render.rendered.clone()), (1, 1, vec![2]));
        assert_ne!(md5ish(&rr.render.wav), mix1);
        n.set(0);
        let rr = reroll_with(&p, &[], 2, true, Quality::Final, &st, &|_| {}, &mut fake(&n)).unwrap();
        assert_eq!((n.get(), rr.take), (0, 0));
        assert_eq!(md5ish(&rr.render.wav), mix1);
        assert!(reroll_with(&p, &[], 2, true, Quality::Final, &st, &|_| {}, &mut fake(&n)).is_err());
        assert!(reroll_with(&p, &[], 99, false, Quality::Final, &st, &|_| {}, &mut fake(&n)).is_err());
    }

    #[test]
    fn single_line_keeps_other_clips_and_quality_dirs() {
        let (_t, p) = fixture();
        let n = Cell::new(0);
        go(&p, &Opts::default(), &n);
        let clips = |q| std::fs::read_dir(takes_dir(&p, q)).unwrap().filter(|e| e.as_ref().unwrap().path().extension().is_some_and(|x| x == "wav")).count();
        assert_eq!(clips(Quality::Final), 8);
        n.set(0);
        let r = go(&p, &Opts { line: Some(3), quality: Quality::Draft, ..Opts::default() }, &n);
        assert_eq!((n.get(), r.lines, r.rendered.clone(), r.line), (1, 1, vec![3], Some(3)));
        assert!(r.wav.ends_with("out/line.wav"));
        assert_eq!((clips(Quality::Final), clips(Quality::Draft)), (8, 1));
        let r = go(&p, &Opts { line: Some(4), ..Opts::default() }, &n);
        assert_eq!((r.tts_calls, r.cached), (0, 1));
        assert_eq!(clips(Quality::Final), 8);
        n.set(0);
        let r = go(&p, &Opts { fresh: true, line: Some(1), ..Opts::default() }, &n);
        assert_eq!((n.get(), r.rendered.clone()), (1, vec![1]));
        assert!(render_with(&p, &[], &Opts { line: Some(8), ..Opts::default() }, &AtomicBool::new(false), &|_| {}, &mut fake(&n)).is_err());
    }

    #[test]
    fn cancel_and_errors() {
        let (_t, mut p) = fixture();
        let n = Cell::new(0);
        let stop = AtomicBool::new(false);
        let mut say = |w: &Path, _: &str, _: &VoiceSpec, _: u64| -> Result<()> {
            n.set(n.get() + 1);
            stop.store(true, Ordering::Relaxed); // Ctrl-C during line 1
            mix::write(w, &[0.2; 2400], 24_000)
        };
        let e = render_with(&p, &[], &Opts::default(), &stop, &|_| {}, &mut say).unwrap_err();
        assert_eq!((e.to_string().as_str(), n.get()), (tts::CANCELLED, 1));
        assert!(!p.dir.join("out/mix.wav").exists());
        // the finished line stays cached
        let r = go(&p, &Opts::default(), &n);
        assert_eq!(r.cached, 1);
        p.meta.speakers["Jonas"].persona = None;
        assert!(render_with(&p, &[], &Opts::default(), &AtomicBool::new(false), &|_| {}, &mut fake(&n)).unwrap_err().to_string().contains("keine Persona"));
        // a second renderer on the same takes dir is refused
        let _l = lock(&p).unwrap();
        p.meta.speakers["Jonas"].persona = Some("fable".into());
        assert!(render_with(&p, &[], &Opts { quality: Quality::Draft, ..Opts::default() }, &AtomicBool::new(false), &|_| {}, &mut fake(&n)).unwrap_err().to_string().starts_with(BUSY));
    }

    /// Seeds per quality, in say order.
    fn seeds(p: &Project, q: Quality) -> Vec<u64> {
        let got = std::cell::RefCell::new(vec![]);
        let mut say = |w: &Path, _: &str, _: &VoiceSpec, s: u64| -> Result<()> {
            got.borrow_mut().push(s);
            mix::write(w, &[0.2; 2400], 24_000)
        };
        render_with(p, &[], &Opts { quality: q, ..Opts::default() }, &AtomicBool::new(false), &|_| {}, &mut say).unwrap();
        got.into_inner()
    }

    #[test]
    fn draft_and_final_share_seeds_and_rerolls() {
        let (_t, p) = fixture();
        let (d, f) = (seeds(&p, Quality::Draft), seeds(&p, Quality::Final));
        assert_eq!((d.len(), &d), (8, &f));
        let n = Cell::new(0);
        let st = AtomicBool::new(false);
        let rr = reroll_with(&p, &[], 2, false, Quality::Draft, &st, &|_| {}, &mut fake(&n)).unwrap();
        assert_eq!((n.get(), rr.take), (1, 1));
        assert!(p.dir.join("takes/rerolls.json").is_file() && !takes_dir(&p, Quality::Draft).join("rerolls.json").exists());
        // final picks up the draft's take: exactly line 2 re-renders, with the rerolled seed
        let d2 = seeds(&p, Quality::Draft);
        assert!(d2.is_empty());
        let f2 = seeds(&p, Quality::Final);
        assert_eq!(f2.len(), 1);
        assert_eq!(rr.salt.map(|s| tts::salted(u64::from_str_radix(&rr.keys[0], 16).unwrap(), s) & 0x7fff_ffff), Some(f2[0]));
        assert!(!f.contains(&f2[0]));
        // undo in final -> draft back on the base take, no TTS
        reroll_with(&p, &[], 2, true, Quality::Final, &st, &|_| {}, &mut fake(&n)).unwrap();
        assert!(seeds(&p, Quality::Draft).is_empty());
    }

    /// CLI draft only -> (draft, not final); "Endfassung" afterwards renders exactly the missing final clips with the
    /// draft's seeds (rerolls included), lines already final come from the cache.
    #[test]
    fn status_both_and_final_after_draft() {
        let (_t, p) = fixture();
        let n = Cell::new(0);
        let st = AtomicBool::new(false);
        let d = seeds(&p, Quality::Draft);
        reroll_with(&p, &[], 4, false, Quality::Draft, &st, &|_| {}, &mut fake(&n)).unwrap();
        let b = status_both(&p, &[]).unwrap();
        assert!(b.iter().all(|x| x.draft && !x.fin), "{b:?}");
        assert_eq!(b.iter().map(|x| x.take).collect::<Vec<_>>(), [0, 0, 0, 1, 0, 0, 0]);
        // one line already final
        render_with(&p, &[], &Opts { line: Some(2), quality: Quality::Final, fresh: false }, &st, &|_| {}, &mut fake(&n)).unwrap();
        let b = status_both(&p, &[]).unwrap();
        assert_eq!(b.iter().map(|x| (x.draft, x.fin)).collect::<Vec<_>>()[..3], [(true, false), (true, true), (true, false)]);
        let draft_now = {
            let c = ctx(&p, &[], Quality::Draft).unwrap();
            let rf = reroll::File::load(&takes_root(&p)).unwrap().rerolls();
            (0..c.lines.len()).flat_map(|i| c.keys(i).unwrap()).map(|b| rr_seed(&rr_eff(&rf, b))).collect::<Vec<u64>>()
        };
        let f = seeds(&p, Quality::Final);
        // 8 clips in 7 lines; line 2 (1 clip) was final already
        assert_eq!(f.len(), 7);
        let mut want: Vec<u64> = draft_now.iter().enumerate().filter(|(k, _)| *k != 1).map(|(_, s)| *s).collect();
        let mut got = f.clone();
        want.sort();
        got.sort();
        assert_eq!(got, want);
        assert_ne!(d, draft_now, "the reroll changed one seed");
        assert!(status_both(&p, &[]).unwrap().iter().all(|x| x.draft && x.fin));
        // only final cached -> (false, true)
        std::fs::remove_dir_all(takes_dir(&p, Quality::Draft)).unwrap();
        assert!(status_both(&p, &[]).unwrap().iter().all(|x| !x.draft && x.fin));
    }

    fn rr_eff(rr: &tts::Rerolls, b: u64) -> u64 {
        rr.active.get(&b).map_or(b, |&s| tts::salted(b, s))
    }

    fn rr_seed(k: &u64) -> u64 {
        k & 0x7fff_ffff
    }

    #[test]
    fn migrates_per_quality_rerolls() {
        let (_t, p) = fixture();
        let c = ctx(&p, &[], Quality::Final).unwrap();
        let old_key = |q: Quality, i: usize| c.keys_with(&Tts { quality: q.as_str().into(), ..c.t.clone() }, i).unwrap()[0];
        for (q, line, takes) in [(Quality::Final, 2, 2), (Quality::Draft, 2, 1), (Quality::Draft, 5, 1)] {
            let d = takes_dir(&p, q);
            std::fs::create_dir_all(&d).unwrap();
            let mut f = reroll::File::load(&d).unwrap();
            for _ in 0..takes {
                f.bump(line, "x", &[old_key(q, line - 1)], false).unwrap();
            }
            f.save(&d).unwrap();
        }
        assert!(migrate_rerolls(&p, &c, &|_| {}).unwrap());
        let f = reroll::File::load(&takes_root(&p)).unwrap();
        let t2 = &f.takes[&format!("{:016x}", c.keys(1).unwrap()[0])];
        let t5 = &f.takes[&format!("{:016x}", c.keys(4).unwrap()[0])];
        assert_eq!((f.takes.len(), t2.active, t5.active), (2, 2, 1)); // final wins for line 2
        assert!(takes_dir(&p, Quality::Draft).join("rerolls.json.migrated").exists());
        assert!(!migrate_rerolls(&p, &c, &|_| {}).unwrap());
    }

    #[test]
    fn events_and_line_report() {
        let (_t, p) = fixture();
        let ev = std::sync::Mutex::new(vec![]);
        let log = |e: Event| ev.lock().unwrap().push(e);
        let n = Cell::new(0);
        let r = render_with(&p, &[], &Opts::default(), &AtomicBool::new(false), &log, &mut fake(&n)).unwrap();
        let ev = std::mem::take(&mut *ev.lock().unwrap());
        let st = |s: State| ev.iter().filter(|e| matches!(e, Event::Line { state, .. } if *state == s)).count();
        assert_eq!((st(State::Queued), st(State::Rendering), st(State::Done), st(State::Cached)), (7, 7, 7, 0));
        assert_eq!((ev.first(), ev.last()), (Some(&Event::Stage { name: "render".into() }), Some(&Event::Done)));
        assert!(ev.contains(&Event::Mix) && ev.iter().any(|e| matches!(e, Event::Msg { text } if text.starts_with("TTS 1/7"))));
        assert!(ev.iter().all(|e| !matches!(e, Event::Line { state: State::Done, tts_secs: None, .. })));
        let j = serde_json::to_string(&Event::Line { i: 3, n: 7, state: State::Done, tts_secs: Some(2.9) }).unwrap();
        assert_eq!(j, r#"{"ev":"line","i":3,"n":7,"state":"done","tts_secs":2.9}"#);
        assert_eq!(serde_json::to_string(&Event::Mix).unwrap(), r#"{"ev":"mix"}"#);
        // per-line report: timeline offsets, clips, who
        assert_eq!(r.items.len(), 7);
        let (x, sr) = mix::read(&r.wav).unwrap();
        let last = r.items.last().unwrap();
        let end = r.items.iter().map(|l| l.start + l.duration).fold(0.0, f64::max);
        assert!((end + 0.5 - x.len() as f64 / sr as f64).abs() < 0.02, "{last:?}"); // timeline ends 0.5 s after the last clip
        assert!(r.items.iter().all(|l| !l.cached && l.take == 0 && l.clips.iter().all(|c| c.is_file())));
        assert_eq!((r.items[3].clips.len(), r.items[2].who.as_str(), r.items[0].start), (2, "Jonas", 0.0));
        assert!(r.items.windows(2).all(|w| w[1].start >= w[0].start || p.script.lines().nth(w[1].i - 1).unwrap().over));
        // rerun: all cached; reroll shows the take index
        let r = go(&p, &Opts::default(), &n);
        assert!(r.items.iter().all(|l| l.cached));
        let rr = reroll_with(&p, &[], 2, false, Quality::Final, &AtomicBool::new(false), &|_| {}, &mut fake(&n)).unwrap();
        assert_eq!((rr.render.items[1].take, rr.render.items[1].cached, rr.render.rendered.clone()), (1, false, vec![2]));
    }

    /// Hörspiel fixture: the imported ambience bed (loop_until_line = n + 1) is mixed into out/mix.wav and ends BED_FADE
    /// after the last line; the gen intro is not cached → warning, skipped, no acestep; a missing file is skipped too.
    /// Same cues twice → identical mix; `--line` renders never carry music.
    #[test]
    fn render_mixes_cached_cues_and_skips_missing() {
        let t = tempfile::tempdir().unwrap();
        let fix = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/stimmwerk/hoerspiel"));
        let d = t.path().join("h.studio");
        std::fs::create_dir_all(d.join("assets/ambience")).unwrap();
        for f in ["project.toml", "script.txt"] {
            std::fs::copy(fix.join(f), d.join(f)).unwrap();
        }
        mix::write(&d.join("assets/ambience/meer_cc0.wav"), &vec![0.2f32; 24_000 * 8], 24_000).unwrap();
        let mut p = Project::load(&d).unwrap();
        p.meta.cues[0].file = None;
        p.meta.cues[0].gen_ = Some(crate::cues::Gen { prompt: "calm intro".into(), dur: 12, seed: 1, bpm: 0, key: None, model: None });
        let mut gone = crate::cues::Cue::new(crate::cues::Kind::Sfx);
        (gone.at_line, gone.file) = (Some(3), Some("assets/sfx/nicht-da.wav".into()));
        p.meta.cues.push(gone);
        let n = Cell::new(0);
        let ev = std::sync::Mutex::new(vec![]);
        let r = render_with(&p, &[], &Opts::default(), &AtomicBool::new(false), &|e| ev.lock().unwrap().push(e), &mut fake(&n)).unwrap();
        assert_eq!((r.cues.len(), r.cues[0].i, r.warnings.len(), r.tts_calls), (1, 2, 2, 25), "{:?}", r.warnings);
        assert!(r.warnings[0].contains("Cue 1") && r.warnings[0].contains("noch nicht erzeugt") && r.warnings[1].contains("Cue 3") && r.warnings[1].contains("fehlt"), "{:?}", r.warnings);
        assert_eq!(ev.lock().unwrap().iter().filter(|e| matches!(e, Event::Msg { text } if text.starts_with("Warnung: Cue"))).count(), 2);
        let end = r.items.iter().map(|l| l.start + l.duration).fold(0.0, f64::max);
        assert!((r.secs - (end + crate::cues::BED_FADE)).abs() < 0.02 && (r.cues[0].end - r.secs).abs() < 0.01, "bed to the mix end: {} {}", r.secs, r.cues[0].end);
        assert_eq!(r.cues[0].start, r.items[17].start);
        let (x, sr) = mix::read(&r.wav).unwrap();
        assert!(x[(r.items[17].start * sr as f64) as usize + 100].abs() > 0.0 && *x.last().unwrap() == 0.0);
        let m1 = std::fs::read(&r.wav).unwrap();
        n.set(0);
        let r2 = render_with(&p, &[], &Opts::default(), &AtomicBool::new(false), &|_| {}, &mut fake(&n)).unwrap();
        assert_eq!((n.get(), r2.cues.len()), (0, 1));
        assert_eq!(std::fs::read(&r2.wav).unwrap(), m1);
        // without the bed: plain speech timeline (0.5 s tail), same as before cues existed
        p.meta.cues.clear();
        let r3 = render_with(&p, &[], &Opts::default(), &AtomicBool::new(false), &|_| {}, &mut fake(&n)).unwrap();
        assert!(r3.cues.is_empty() && r3.warnings.is_empty() && (r3.secs - (end + 0.5)).abs() < 0.02, "{}", r3.secs);
        let r4 = render_with(&p, &[], &Opts { line: Some(18), ..Opts::default() }, &AtomicBool::new(false), &|_| {}, &mut fake(&n)).unwrap();
        assert!(r4.cues.is_empty() && r4.wav.ends_with("out/line.wav"));
    }

    #[test]
    fn same_text_twice_attributed_to_first_line() {
        let (_t, mut p) = fixture();
        p.script = crate::script::Script::parse("ERZ|0|Gleich.\nMara|0|Anders.\nERZ|0|Gleich.\n").unwrap();
        let n = Cell::new(0);
        let r = go(&p, &Opts::default(), &n);
        assert_eq!((r.tts_calls, r.rendered.clone(), r.cached), (2, vec![1, 2], 1));
        assert!(r.items[2].cached && r.items[0].clips == r.items[2].clips);
    }

    #[test]
    fn cancel_keeps_real_error_and_lock_errors() {
        let (_t, p) = fixture();
        let stop = AtomicBool::new(false);
        let mut say = |_: &Path, _: &str, _: &VoiceSpec, _: u64| -> Result<()> {
            stop.store(true, Ordering::Relaxed);
            bail!("omnivoice: Speicher voll")
        };
        let e = render_with(&p, &[], &Opts::default(), &stop, &|_| {}, &mut say).unwrap_err();
        assert_eq!(e.to_string(), tts::CANCELLED);
        assert!(format!("{e:#}").contains("Speicher voll"), "{e:#}");
        // without cancel the real error is on top
        let e = render_with(&p, &[], &Opts::default(), &AtomicBool::new(false), &|_| {}, &mut |_: &Path, _: &str, _: &VoiceSpec, _: u64| -> Result<()> { bail!("kaputt") }).unwrap_err();
        assert!(e.to_string().contains("omnivoice Zeile 1") && format!("{e:#}").contains("kaputt"), "{e:#}");
        // a lock that cannot be created is not "someone else renders"
        let _ = std::fs::remove_file(p.dir.join(".lock"));
        std::fs::create_dir_all(p.dir.join(".lock")).unwrap();
        let e = lock(&p).unwrap_err();
        assert!(!format!("{e:#}").contains(BUSY) && format!("{e:#}").contains("Render-Sperre"), "{e:#}");
    }

    #[test]
    fn atomic_mix_and_sweep_leftovers() {
        let (_t, p) = fixture();
        let n = Cell::new(0);
        let td = takes_dir(&p, Quality::Final);
        std::fs::create_dir_all(&td).unwrap();
        std::fs::create_dir_all(p.dir.join("out")).unwrap();
        let junk = [td.join("0123456789abcdef.part.wav"), p.dir.join("out/.mix.part.wav"), p.dir.join("out/.l.part.mp3")];
        for j in &junk {
            std::fs::write(j, b"x").unwrap();
        }
        std::fs::write(p.dir.join("out/keep.wav"), b"x").unwrap();
        let r = go(&p, &Opts::default(), &n);
        assert!(junk.iter().all(|j| !j.exists()) && p.dir.join("out/keep.wav").exists());
        let temps = || std::fs::read_dir(p.dir.join("out")).unwrap().flatten().filter(|e| e.file_name().to_string_lossy().starts_with(".mix.part.")).count();
        assert!(mix::read(&r.wav).is_ok() && part_path(&r.wav).file_name().unwrap().to_string_lossy().starts_with(".mix.part.") && temps() == 0);
        // failing write leaves the old mix untouched and no temp
        let before = std::fs::read(&r.wav).unwrap();
        assert!(atomic(&r.wav, |t| {
            std::fs::write(t, b"half")?;
            bail!("disk full")
        })
        .is_err());
        assert_eq!(std::fs::read(&r.wav).unwrap(), before);
        assert_eq!(temps(), 0);
    }

    #[test]
    fn persona_errors_before_omnivoice_lookup() {
        let (_t, mut p) = fixture();
        // SAFETY: test-only; no other test reads the TTS command (they inject `say`)
        unsafe { std::env::set_var(TTS_ENV, "/nonexistent/omnivoice") };
        p.meta.speakers["Jonas"].persona = None;
        let e = render(&p, &[], &Opts::default(), &AtomicBool::new(false), &|_| {}).unwrap_err();
        assert!(e.to_string().contains("keine Persona"), "{e:#}");
        p.meta.speakers["Jonas"].persona = Some("fable".into());
        // fully cached: no omnivoice (and no GPU priority) needed at all
        go(&p, &Opts::default(), &Cell::new(0));
        let r = render(&p, &[], &Opts::default(), &AtomicBool::new(false), &|_| {}).unwrap();
        assert_eq!(r.tts_calls, 0);
        std::fs::write(p.dir.join("script.txt"), "ERZ|0|Neu.\n").unwrap();
        let p2 = Project::load(&p.dir).unwrap();
        let e = render(&p2, &[], &Opts::default(), &AtomicBool::new(false), &|_| {}).unwrap_err();
        assert!(format!("{e:#}").contains("omnivoice nicht gefunden"), "{e:#}");
    }

    #[test]
    fn lexicon_applies() {
        let (_t, p) = fixture();
        std::fs::write(p.lexicon_path(), "Leuchtturm\tLeucht-turm\n").unwrap();
        let said = std::cell::RefCell::new(vec![]);
        let mut say = |w: &Path, x: &str, _: &VoiceSpec, _: u64| -> Result<()> {
            said.borrow_mut().push(x.to_string());
            mix::write(w, &[0.2; 2400], 24_000)
        };
        render_with(&p, &[], &Opts { line: Some(2), ..Opts::default() }, &AtomicBool::new(false), &|_| {}, &mut say).unwrap();
        assert!(said.borrow()[0].contains("Leucht-turm"), "{:?}", said.borrow());
    }

    #[test]
    fn cancel_kills_child_fast() {
        let t = tempfile::tempdir().unwrap();
        let log = t.path().join(".x.part.log");
        let c = std::sync::Arc::new(AtomicBool::new(false));
        let c2 = c.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(200));
            c2.store(true, Ordering::Relaxed);
        });
        let t0 = std::time::Instant::now();
        let e = run_child(Path::new("/bin/sleep"), &["30".into()], &log, &c).unwrap_err();
        assert_eq!(e.to_string(), tts::CANCELLED);
        assert!(t0.elapsed().as_secs_f64() < 2.0, "{:?}", t0.elapsed());
        assert!(run_child(Path::new("/bin/sh"), &["-c".into(), "echo hi; exit 3".into()], &log, &AtomicBool::new(false)).is_ok_and(|s| s.code() == Some(3)));
        assert_eq!(std::fs::read_to_string(&log).unwrap(), "hi\n");
    }

    /// Project path with spaces + umlauts; broken rerolls.json -> .broken + warning; Done only after the lock is free.
    #[test]
    fn spaces_umlauts_broken_rerolls_done_after_unlock() {
        let t = tempfile::tempdir().unwrap();
        let d = t.path().join("Mein Ordner/Hörspiel Ä.studio");
        std::fs::create_dir_all(d.join("takes")).unwrap();
        for f in ["project.toml", "script.txt", "lexicon.tsv"] {
            std::fs::copy(Path::new(FIX).join(f), d.join(f)).unwrap();
        }
        std::fs::write(d.join("takes/rerolls.json"), "{kaputt").unwrap();
        let p = Project::load(&d).unwrap();
        let ev = std::sync::Mutex::new(vec![]);
        let pp = p.clone();
        let log = |e: Event| {
            if e == Event::Done {
                assert!(lock(&pp).is_ok(), "Done while the project lock is still held");
            }
            ev.lock().unwrap().push(e);
        };
        let n = Cell::new(0);
        let r = render_with(&p, &[], &Opts::default(), &AtomicBool::new(false), &log, &mut fake(&n)).unwrap();
        assert_eq!((r.lines, n.get()), (7, 8));
        assert!(d.join("takes/rerolls.json.broken").is_file());
        let ev = ev.into_inner().unwrap();
        assert!(ev.iter().any(|e| matches!(e, Event::Msg { text } if text.starts_with("Warnung:") && text.contains("rerolls.json.broken"))), "{ev:?}");
        assert_eq!(ev.last(), Some(&Event::Done));
        // works on afterwards: reroll + undo + status
        reroll_with(&p, &[], 2, false, Quality::Final, &AtomicBool::new(false), &|_| {}, &mut fake(&n)).unwrap();
        assert_eq!(status(&p, &[], Quality::Final).unwrap()[1].take, 1);
        std::fs::write(d.join("takes/rerolls.json"), "nochmal kaputt").unwrap();
        render_with(&p, &[], &Opts::default(), &AtomicBool::new(false), &|_| {}, &mut fake(&n)).unwrap();
        assert!(d.join("takes/rerolls.json.broken.1").is_file());
    }

    #[test]
    fn temp_names_and_sweep() {
        let t = tempfile::tempdir().unwrap();
        let a = part_path(&t.path().join("Hörspiel Ä.wav"));
        let b = part_path(&t.path().join("Hörspiel Ä.wav"));
        assert_ne!(a, b);
        let me = std::process::id() as i32;
        assert_eq!(part_owner(&a.file_name().unwrap().to_string_lossy()), Some(("Hörspiel Ä", Some(me))));
        assert_eq!(part_owner(".x.part.wav"), Some(("x", None)));
        assert_eq!(part_owner("..k.part.wav.part.12-3.log"), Some((".k.part.wav", Some(12))));
        let mk = |n: &str| {
            std::fs::write(t.path().join(n), b"x").unwrap();
            n.to_string()
        };
        let (legacy, dead, live, other, clip, keep) = (mk(".l.part.wav"), mk(".l.part.999999-1.wav"), mk(&format!(".l.part.{me}-7.wav")), mk(".o.part.999999-1.wav"), mk("abc.part.wav"), mk("notes.txt"));
        let has = |n: &str| t.path().join(n).exists();
        sweep_in(t.path(), Some(&["l".into()]));
        assert!(!has(&legacy) && !has(&dead) && has(&live) && has(&other) && has(&clip) && has(&keep));
        sweep(t.path());
        assert!(has(&live) && !has(&other) && !has(&clip) && has(&keep));
    }

    /// Cancel: SIGTERM first, SIGKILL after 500 ms for a child that ignores it.
    #[test]
    fn stop_child_term_then_kill() {
        for (script, max) in [("sleep 5", 300), ("trap '' TERM; sleep 5", 900)] {
            let mut ch = std::process::Command::new("/bin/sh").args(["-c", script]).spawn().unwrap();
            std::thread::sleep(std::time::Duration::from_millis(100));
            let t0 = std::time::Instant::now();
            stop_child(&mut ch);
            let el = t0.elapsed();
            assert!(el < std::time::Duration::from_millis(max), "{script}: {el:?}");
            assert!(ch.try_wait().unwrap().is_some());
        }
    }
}
