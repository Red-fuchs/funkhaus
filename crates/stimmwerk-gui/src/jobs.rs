//! Background jobs: one thread each, events over mpsc, own cancel flag per job.
//! Follow-up work starts only on `Msg::Done` (the thread's Result). Closing a project cancels its jobs and detaches
//! them (`App::orphans`): their events and results are dropped, they only get reaped.
use eframe::egui;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use stimmwerk_core::{export, lint, music, prose, render, shorten, sync, voice};

#[derive(Clone, Debug, PartialEq)]
pub enum JobKind {
    Render,
    /// render one line (cache) and play it
    PlayLine(usize),
    Reroll {
        line: usize,
        undo: bool,
    },
    Export,
    Lint,
    Probe(String),
    /// voice design candidates (GPU, no project lock)
    Design,
    /// withdraw a clone's consent + delete its audio (locks every project; not cancellable)
    Revoke(String),
    /// delete a design voice with its audio (locks every project; not cancellable)
    Rm(String),
    /// `GET /models` of an LLM backend (no text leaves the Mac)
    LlmTest(String),
    /// API key into the keychain (the macOS dialog may block, so off the UI thread)
    KeySave(String),
    /// prose -> role script preview (LLM; nothing written until confirmed)
    Prose,
    /// shortening proposal for a sync cue (LLM; nothing written until applied)
    Shorten(usize),
    /// acestep render of a cue generation (GPU + project lock), value = cache key
    MusicGen(String),
}

impl JobKind {
    /// Uses the project lock: only one of these per project at a time.
    pub fn exclusive(&self) -> bool {
        !matches!(self, JobKind::Lint | JobKind::Probe(_) | JobKind::Design | JobKind::LlmTest(_) | JobKind::KeySave(_) | JobKind::Prose | JobKind::Shorten(_))
    }

    /// Has a working "Abbrechen" (revoke/rm run to the end once started; a keychain write is not interrupted). LLM jobs
    /// end at once: the GPU wait stops, a request in flight is abandoned.
    pub fn cancellable(&self) -> bool {
        (self.exclusive() && !matches!(self, JobKind::Revoke(_) | JobKind::Rm(_))) || matches!(self, JobKind::Prose | JobKind::Shorten(_) | JobKind::LlmTest(_))
    }

    /// Writes takes: a cancel leaves finished lines in the cache.
    pub fn renders(&self) -> bool {
        matches!(self, JobKind::Render | JobKind::PlayLine(_) | JobKind::Reroll { .. } | JobKind::Export | JobKind::MusicGen(_) | JobKind::Probe(_) | JobKind::Design)
    }

    pub fn label(&self) -> String {
        match self {
            JobKind::Render => "Rendern".into(),
            JobKind::PlayLine(l) => format!("Zeile {l}"),
            JobKind::Reroll { line, undo: false } => format!("Neu würfeln · Zeile {line}"),
            JobKind::Reroll { line, undo: true } => format!("Rückgängig · Zeile {line}"),
            JobKind::Export => "Exportieren".into(),
            JobKind::Lint => "Prüfen".into(),
            JobKind::Probe(s) => format!("Vorhören · {s}"),
            JobKind::Design => "Entwerfen".into(),
            JobKind::Revoke(s) => format!("Widerruf · {s}"),
            JobKind::Rm(s) => format!("Löschen · {s}"),
            JobKind::LlmTest(s) => format!("Verbindung · {s}"),
            JobKind::KeySave(s) => format!("Schlüssel · {s}"),
            JobKind::Prose => "Rollenskript".into(),
            JobKind::Shorten(c) => format!("Kürzen · Cue {c}"),
            JobKind::MusicGen(_) => "Musik erzeugen".into(),
        }
    }
}

/// Lint result plus what the Stimmen tab needs, all computed off the UI thread.
pub struct LintOut {
    pub rep: lint::Report,
    /// omnivoice builtin voices (None = omnivoice not reachable)
    pub bi: Option<Vec<String>>,
    /// persona files found in the persona dirs
    pub personas: Vec<String>,
    /// voice library (kind, provenance, consent) for the Stimmen tab
    pub lib: Vec<voice::Entry>,
    /// per script line: cached in draft / final + active take index (None: project not renderable yet)
    pub status: Option<Vec<render::Both>>,
    /// sync project: cue fit report (too_long cues get "Kürzen vorschlagen")
    pub sync: Option<sync::Report>,
}

pub enum Out {
    Render(render::Report),
    Reroll(render::Reroll),
    Export(Box<export::Report>),
    Lint(Box<LintOut>),
    Probe(PathBuf),
    Design(Box<voice::Batch>),
    Revoked(Box<voice::Revoked>),
    Removed(Box<voice::Removed>),
    /// model ids of a tested backend
    Models(Vec<String>),
    KeySaved,
    Prose(Box<prose::Outcome>),
    Shorten(Box<shorten::Suggestion>),
    Music(Box<music::Built>),
}

pub enum Msg {
    Ev(render::Event),
    Done(Result<Out, String>),
}

pub struct Job {
    pub kind: JobKind,
    pub rx: mpsc::Receiver<Msg>,
    pub cancel: Arc<AtomicBool>,
    pub t0: std::time::Instant,
    /// lines finished / lines in this job
    pub done: usize,
    pub total: usize,
    /// line currently rendering (1-based)
    pub cur: Option<usize>,
    pub stage: Option<String>,
    /// waiting for the shared GPU lock (another app or job computes)
    pub gpu_wait: bool,
    /// when the current line started rendering
    pub line_t0: Option<std::time::Instant>,
    /// lines this job actually spoke (TTS), for the model-load hint
    pub tts_done: usize,
    /// project the job works on (None: not project-bound)
    pub dir: Option<PathBuf>,
    pub handle: Option<std::thread::JoinHandle<()>>,
}

impl Job {
    /// First TTS line of this job running for a while: omnivoice probably loads its model (first time 30–60 s).
    pub fn model_loading(&self) -> bool {
        self.tts_done == 0 && !self.gpu_wait && self.line_t0.is_some_and(|t| t.elapsed() >= crate::app::MODEL_LOAD_HINT)
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
    pub fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }
    pub fn finished(&self) -> bool {
        self.handle.as_ref().is_none_or(|h| h.is_finished())
    }
}

/// Cancels `jobs` and waits until their threads ended, at most `max` in total; true if all ended.
pub fn join_all(jobs: &mut [Job], max: std::time::Duration) -> bool {
    jobs.iter().for_each(Job::cancel);
    let t0 = std::time::Instant::now();
    while !jobs.iter().all(Job::finished) {
        if t0.elapsed() >= max {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    jobs.iter_mut().filter_map(|j| j.handle.take()).for_each(|h| drop(h.join()));
    true
}

/// Runs `f(cancel, prog)` on a thread; events and the result come back through the job's channel.
pub fn spawn(ctx: &egui::Context, kind: JobKind, f: impl FnOnce(&AtomicBool, &render::Prog) -> anyhow::Result<Out> + Send + 'static) -> Job {
    let (tx, rx) = mpsc::channel();
    let cancel = Arc::new(AtomicBool::new(false));
    let (c, ctx) = (cancel.clone(), ctx.clone());
    let handle = std::thread::spawn(move || {
        let ev = {
            let (tx, ctx) = (tx.clone(), ctx.clone());
            move |e: render::Event| {
                let _ = tx.send(Msg::Ev(e));
                ctx.request_repaint();
            }
        };
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(&c, &ev)));
        let r = match r {
            Ok(r) => r.map_err(|e| format!("{e:#}")),
            Err(p) => Err(p.downcast_ref::<&str>().map(|s| s.to_string()).or_else(|| p.downcast_ref::<String>().cloned()).unwrap_or_else(|| "Absturz im Hintergrund".into())),
        };
        let _ = tx.send(Msg::Done(r));
        ctx.request_repaint();
    });
    Job { kind, rx, cancel, t0: std::time::Instant::now(), done: 0, total: 0, cur: None, stage: None, gpu_wait: false, line_t0: None, tts_done: 0, dir: None, handle: Some(handle) }
}

/// Setup problems (tool missing) belong in a global banner, not on a script line.
pub fn is_setup_error(e: &str) -> bool {
    e.contains("nicht gefunden") && (e.contains("omnivoice") || e.contains("ffmpeg"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancel_and_panic() {
        let ctx = egui::Context::default();
        let j = spawn(&ctx, JobKind::Render, |c, prog| {
            prog(render::Event::Stage { name: "render".into() });
            while !c.load(Ordering::Relaxed) {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            anyhow::bail!("abgebrochen")
        });
        assert!(matches!(j.rx.recv().unwrap(), Msg::Ev(render::Event::Stage { .. })));
        let t0 = std::time::Instant::now();
        j.cancel();
        assert!(matches!(j.rx.recv().unwrap(), Msg::Done(Err(e)) if e == "abgebrochen"));
        assert!(t0.elapsed().as_secs_f64() < 2.0);
        let j = spawn(&ctx, JobKind::Lint, |_, _| panic!("kaputt"));
        assert!(matches!(j.rx.recv().unwrap(), Msg::Done(Err(e)) if e == "kaputt"));
        assert!(is_setup_error("omnivoice nicht gefunden") && !is_setup_error("Zeile 3: Fehler"));
        // join_all cancels and waits
        let mut v = vec![spawn(&ctx, JobKind::Render, |c, _| {
            while !c.load(Ordering::Relaxed) {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            anyhow::bail!("abgebrochen")
        })];
        assert!(join_all(&mut v, std::time::Duration::from_secs(2)) && v[0].finished());
    }
}
