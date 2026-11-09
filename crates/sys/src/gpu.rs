use anyhow::Result;
use std::time::Duration;

// GPU fairness (docs/m2/gpu-lock-jobs.md): one flock for all GPU calls (LLM, TTS, music) of all funkhaus processes.
// Interactive calls (ask/make/podcast) announce themselves with a priority file before blocking;
// background jobs (map/guide, marked per thread via `Batch`) poll with try_lock and yield while any
// fresh priority file exists, so an interactive call waits at most one background call.

thread_local! {
    static BATCH: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Marks the current thread as a background job until dropped.
pub struct Batch(bool);
impl Batch {
    pub fn on() -> Self {
        Batch(BATCH.replace(true))
    }
}
impl Drop for Batch {
    fn drop(&mut self) {
        BATCH.set(self.0);
    }
}

/// funkhaus app data dir relative to $HOME: funkhaus' default data root and THE GPU lock dir of every app
/// on this Mac (funkhaus, Stimmwerk, …) – independent of FUNKHAUS_HOME/STIMMWERK_HOME (one GPU per Mac).
pub const APP_DIR: &str = "Library/Application Support/funkhaus";
/// One lock for every GPU job (LLM, TTS, music) of all processes of all apps.
pub const LOCK_NAME: &str = ".gpu.lock";
pub const PRIO_PREFIX: &str = ".gpu.prio.";

/// Per-user lock dir `~/APP_DIR`, shared by every process of every app.
pub fn lock_dir() -> std::path::PathBuf {
    crate::home().join(APP_DIR)
}

pub fn lock_file(dir: &std::path::Path) -> Result<std::fs::File> {
    std::fs::create_dir_all(dir)?;
    Ok(std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(dir.join(LOCK_NAME))?)
}

/// Age after which a `.gpu.prio.*` file no longer counts (crashed process, or a waiter that never touches it).
pub const PRIO_STALE: Duration = Duration::from_secs(600);

fn age(e: &std::fs::DirEntry) -> Option<Duration> {
    e.metadata().and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok())
}

/// A fresh `.gpu.prio.*` file (< 10 min; older ones are from crashed processes).
pub fn prio_waiting(dir: &std::path::Path) -> bool {
    std::fs::read_dir(dir).into_iter().flatten().flatten().any(|e| e.file_name().to_string_lossy().starts_with(PRIO_PREFIX) && age(&e).is_some_and(|d| d < PRIO_STALE))
}

unsafe extern "C" {
    fn kill(pid: i32, sig: i32) -> i32;
}

/// Process exists (signal 0; EPERM = exists but not ours).
pub fn alive(pid: i32) -> bool {
    // SAFETY: kill(pid, 0) only checks for existence, sends nothing
    pid > 0 && (unsafe { kill(pid, 0) } == 0 || std::io::Error::last_os_error().raw_os_error() != Some(3))
}

/// Removes `.gpu.prio.<pid>-…` files that are truly stale: older than `PRIO_STALE` AND their process is gone.
/// The dir is shared with funkhaus: fresh files and files of live processes (e.g. a long waiter) are never touched.
pub fn clean_stale(dir: &std::path::Path) -> usize {
    let mut n = 0;
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let Some(rest) = name.strip_prefix(PRIO_PREFIX) else { continue };
        let pid = rest.split('-').next().and_then(|p| p.parse::<i32>().ok());
        if age(&e).is_some_and(|d| d >= PRIO_STALE) && !pid.is_some_and(alive) && std::fs::remove_file(e.path()).is_ok() {
            n += 1;
        }
    }
    n
}

static PRIO_N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Prio files this process currently holds, so a hard exit (second Ctrl-C) can still remove them.
static HELD: std::sync::Mutex<Vec<std::path::PathBuf>> = std::sync::Mutex::new(Vec::new());

fn held() -> std::sync::MutexGuard<'static, Vec<std::path::PathBuf>> {
    HELD.lock().unwrap_or_else(|e| e.into_inner())
}

fn announce(p: &std::path::Path) -> std::io::Result<()> {
    held().push(p.to_path_buf());
    std::fs::write(p, std::process::id().to_string()).inspect_err(|_| forget(p))
}

fn forget(p: &std::path::Path) {
    held().retain(|x| x != p);
    let _ = std::fs::remove_file(p);
}

/// Removes every prio file of this process. Call before `process::exit` (exit skips `Drop`).
pub fn release_all() {
    for p in std::mem::take(&mut *held()) {
        let _ = std::fs::remove_file(p);
    }
}

/// Takes the GPU lock: interactive = announce + block; batch = yield to announced interactive calls.
pub fn acquire(dir: &std::path::Path, batch: bool) -> Result<std::fs::File> {
    let f = lock_file(dir)?;
    if batch {
        loop {
            if !prio_waiting(dir) && f.try_lock().is_ok() {
                return Ok(f);
            }
            std::thread::sleep(Duration::from_millis(250));
        }
    }
    clean_stale(dir);
    let p = dir.join(format!("{PRIO_PREFIX}{}-{}", std::process::id(), PRIO_N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)));
    announce(&p)?;
    let r = f.lock();
    forget(&p);
    r?;
    Ok(f)
}

/// Interactive priority held for a whole multi-call job (e.g. a TTS render): one `.gpu.prio.*` file,
/// mtime refreshed by `touch` (rate-limited) so the 10-min staleness never triggers; removed on drop (also on error/panic)
/// and by `release_all` before a hard exit.
/// The lock itself is still taken per call. `None` for batch threads (they must keep yielding).
pub struct PrioGuard(std::path::PathBuf, std::cell::Cell<std::time::Instant>);
impl PrioGuard {
    pub fn hold() -> Option<Self> {
        if BATCH.get() {
            return None;
        }
        Self::hold_in(&lock_dir())
    }
    fn hold_in(dir: &std::path::Path) -> Option<Self> {
        std::fs::create_dir_all(dir).ok()?;
        clean_stale(dir);
        let p = dir.join(format!("{PRIO_PREFIX}{}-r{}", std::process::id(), PRIO_N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)));
        announce(&p).ok()?;
        Some(PrioGuard(p, std::cell::Cell::new(std::time::Instant::now())))
    }
    pub fn touch(&self) {
        if self.1.get().elapsed() >= Duration::from_secs(30) {
            self.1.set(std::time::Instant::now());
            let _ = std::fs::File::options().write(true).open(&self.0).and_then(|f| f.set_modified(std::time::SystemTime::now()));
        }
    }
}
impl Drop for PrioGuard {
    fn drop(&mut self) {
        forget(&self.0);
    }
}

/// True while the current thread runs as a background job (`Batch`).
pub fn is_batch() -> bool {
    BATCH.get()
}

pub fn gpu_lock() -> Result<std::fs::File> {
    acquire(&lock_dir(), BATCH.get())
}

/// Reports "wartet auf GPU …" if the GPU is busy (or, for background jobs, an interactive call waits)
/// and blocks until this thread may go; the actual lock is taken per call in `chat`.
pub fn gpu_wait(prog: &dyn Fn(&str)) {
    let dir = lock_dir();
    let Ok(f) = lock_file(&dir) else { return };
    let batch = BATCH.get();
    if f.try_lock().is_err() || (batch && prio_waiting(&dir)) {
        prog("wartet auf GPU …");
        drop(f);
        if let Ok(g) = acquire(&dir, batch) {
            drop(g);
        }
    }
}

/// Runs `f` (one GPU job, e.g. an omnivoice/acestep call) under the GPU lock. Priority follows the thread:
/// interactive unless `Batch` is on. Take it per call, not per run, so interactive LLM calls of other
/// processes wait at most one job (TTS line), not a whole render.
pub fn with_lock<T>(prog: &dyn Fn(&str), f: impl FnOnce() -> T) -> Result<T> {
    let (dir, batch) = (lock_dir(), BATCH.get());
    with_lock_in(&dir, batch, prog, f)
}

fn with_lock_in<T>(dir: &std::path::Path, batch: bool, prog: &dyn Fn(&str), f: impl FnOnce() -> T) -> Result<T> {
    let probe = lock_file(dir)?;
    if probe.try_lock().is_err() || (batch && prio_waiting(dir)) {
        prog("wartet auf GPU …");
    }
    drop(probe);
    let _g = acquire(dir, batch)?;
    Ok(f())
}

/// Error text of a cancelled `with_lock_cancel` (same word as the TTS cancel).
pub const CANCELLED: &str = "abgebrochen";

/// `with_lock` whose wait can be cancelled: polls `try_lock` every 50 ms instead of blocking, so setting `cancel`
/// ends the wait (Err `CANCELLED`, priority file removed). Reports "wartet auf GPU …" before and "GPU frei" after a wait.
/// funkhaus keeps the blocking `with_lock`.
pub fn with_lock_cancel<T>(prog: &dyn Fn(&str), cancel: &std::sync::atomic::AtomicBool, f: impl FnOnce() -> T) -> Result<T> {
    with_lock_cancel_in(&lock_dir(), BATCH.get(), prog, cancel, f)
}

/// `with_lock_cancel` on an explicit lock dir and priority (`batch`); for callers on another thread (the thread-local
/// `Batch` mark does not travel) and for tests.
pub fn with_lock_cancel_in<T>(dir: &std::path::Path, batch: bool, prog: &dyn Fn(&str), cancel: &std::sync::atomic::AtomicBool, f: impl FnOnce() -> T) -> Result<T> {
    use std::sync::atomic::Ordering;
    let lf = lock_file(dir)?;
    if !poll_lock(&lf, batch, dir) {
        prog("wartet auf GPU …");
        // interactive: announce like `acquire`, so batch jobs of other processes yield
        let p = (!batch).then(|| {
            clean_stale(dir);
            dir.join(format!("{PRIO_PREFIX}{}-c{}", std::process::id(), PRIO_N.fetch_add(1, Ordering::Relaxed)))
        });
        if let Some(p) = &p {
            announce(p)?;
        }
        let r = loop {
            if cancel.load(Ordering::Relaxed) {
                break Err(anyhow::anyhow!(CANCELLED));
            }
            if poll_lock(&lf, batch, dir) {
                break Ok(());
            }
            std::thread::sleep(Duration::from_millis(50));
        };
        if let Some(p) = &p {
            forget(p);
        }
        r?;
        prog("GPU frei");
    }
    let out = f();
    drop(lf);
    Ok(out)
}

/// One poll of the wait loop: batch callers still yield to fresh priority files of others.
fn poll_lock(f: &std::fs::File, batch: bool, dir: &std::path::Path) -> bool {
    (!batch || !prio_waiting(dir)) && f.try_lock().is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `release_all` is process-wide: tests that hold prio files must not interleave with it.
    static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());
    fn serial() -> std::sync::MutexGuard<'static, ()> {
        SERIAL.lock().unwrap_or_else(|e| e.into_inner())
    }

    #[test]
    fn batch_yields_to_interactive() {
        let _s = serial();
        let d = tempfile::tempdir().unwrap();
        let dir = d.path().to_path_buf();
        let hold = acquire(&dir, true).unwrap(); // a background call is running
        let t0 = std::time::Instant::now();
        let dd = dir.clone();
        let ask = std::thread::spawn(move || {
            let g = acquire(&dd, false).unwrap(); // interactive: announces, then blocks
            std::thread::sleep(Duration::from_millis(300));
            drop(g);
            t0.elapsed()
        });
        std::thread::sleep(Duration::from_millis(200));
        assert!(prio_waiting(&dir));
        drop(hold); // background call done
        // the next background call must not overtake the waiting interactive one
        let next = acquire(&dir, true).unwrap();
        let waited = ask.join().unwrap();
        assert!(waited < Duration::from_millis(900), "{waited:?}");
        assert!(!prio_waiting(&dir));
        drop(next);
        let _b = Batch::on();
        assert!(BATCH.get());
    }

    #[test]
    fn prio_guard_holds_touches_and_removes() {
        let _s = serial();
        let d = tempfile::tempdir().unwrap();
        let g = PrioGuard::hold_in(d.path()).unwrap();
        assert!(prio_waiting(d.path()));
        // a batch caller yields while the render holds priority, even between lines (no lock held)
        let dd = d.path().to_path_buf();
        let b = std::thread::spawn(move || acquire(&dd, true).map(|_| ()));
        std::thread::sleep(Duration::from_millis(600));
        assert!(!b.is_finished());
        // stale file is revived by touch
        let old = std::time::SystemTime::now() - Duration::from_secs(700);
        std::fs::File::options().write(true).open(&g.0).unwrap().set_modified(old).unwrap();
        assert!(!prio_waiting(d.path()));
        g.1.set(std::time::Instant::now() - Duration::from_secs(60));
        g.touch();
        assert!(prio_waiting(d.path()));
        let p = g.0.clone();
        drop(g);
        assert!(!p.exists());
        b.join().unwrap().unwrap();
    }

    #[test]
    fn release_all_and_clean_stale() {
        let _s = serial();
        let d = tempfile::tempdir().unwrap();
        let g = PrioGuard::hold_in(d.path()).unwrap();
        let p = g.0.clone();
        release_all(); // hard exit path: Drop never runs
        assert!(!p.exists());
        std::mem::forget(g);
        let old = std::time::SystemTime::now() - Duration::from_secs(700);
        let mk = |n: &str, t: Option<std::time::SystemTime>| {
            let f = d.path().join(n);
            std::fs::write(&f, "x").unwrap();
            if let Some(t) = t {
                std::fs::File::options().write(true).open(&f).unwrap().set_modified(t).unwrap();
            }
            f
        };
        let dead_old = mk(".gpu.prio.999999-r1", Some(old));
        let junk_old = mk(".gpu.prio.x", Some(old));
        let dead_fresh = mk(".gpu.prio.999998-3", None);
        let live_old = mk(&format!(".gpu.prio.{}-9", std::process::id()), Some(old));
        let other = mk("prio.999999", Some(old));
        assert_eq!(clean_stale(d.path()), 2);
        assert!(!dead_old.exists() && !junk_old.exists());
        assert!(dead_fresh.exists() && live_old.exists() && other.exists());
        // acquire cleans too
        let again = mk(".gpu.prio.999999-r2", Some(old));
        drop(acquire(d.path(), false).unwrap());
        assert!(!again.exists());
    }

    #[test]
    fn gpu_names_and_with_lock() {
        let _s = serial();
        let d = tempfile::tempdir().unwrap();
        let g = acquire(d.path(), false).unwrap();
        assert!(d.path().join(".gpu.lock").exists());
        drop(g);
        let r = with_lock_in(d.path(), false, &|_| {}, || {
            std::fs::write(d.path().join(".gpu.prio.x"), "1").unwrap();
            assert!(prio_waiting(d.path()));
            7
        });
        assert_eq!(r.unwrap(), 7);
        assert!(!d.path().join(".llm.lock").exists());
    }

    #[test]
    fn with_lock_cancel_waits_and_cancels() {
        let _s = serial();
        let d = tempfile::tempdir().unwrap();
        let no = std::sync::atomic::AtomicBool::new(false);
        let msgs = std::sync::Mutex::new(vec![]);
        let log = |m: &str| msgs.lock().unwrap().push(m.to_string());
        // free: runs at once, no wait message
        assert_eq!(with_lock_cancel_in(d.path(), false, &log, &no, || 1).unwrap(), 1);
        assert!(msgs.lock().unwrap().is_empty());
        // held by someone else: waits, announces priority, cancel ends the wait quickly
        let hold = acquire(d.path(), true).unwrap();
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let c2 = cancel.clone();
        let dd = d.path().to_path_buf();
        let t = std::thread::spawn(move || {
            let t0 = std::time::Instant::now();
            let r = with_lock_cancel_in(&dd, false, &|_| {}, &c2, || ());
            (r.map_err(|e| e.to_string()), t0.elapsed())
        });
        std::thread::sleep(Duration::from_millis(200));
        assert!(prio_waiting(d.path()), "waiter announces priority");
        cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        let (r, el) = t.join().unwrap();
        assert_eq!(r.unwrap_err(), CANCELLED);
        assert!(el < Duration::from_millis(600), "{el:?}");
        assert!(!prio_waiting(d.path()), "prio file removed on cancel");
        // released while waiting: runs, reports wait + free
        let dd = d.path().to_path_buf();
        let t = std::thread::spawn(move || {
            let m = std::sync::Mutex::new(vec![]);
            let r = with_lock_cancel_in(&dd, false, &|x: &str| m.lock().unwrap().push(x.to_string()), &std::sync::atomic::AtomicBool::new(false), || 5).unwrap();
            (r, m.into_inner().unwrap())
        });
        std::thread::sleep(Duration::from_millis(150));
        drop(hold);
        let (r, m) = t.join().unwrap();
        assert_eq!((r, m), (5, vec!["wartet auf GPU …".to_string(), "GPU frei".to_string()]));
    }
}
