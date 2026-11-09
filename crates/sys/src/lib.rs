pub mod gpu;


use std::path::PathBuf;

pub const EXTRA: &[&str] = &["~/.local/bin", "~/.cargo/bin", "/opt/homebrew/bin", "/usr/local/bin", "/opt/miniconda3/bin"];

pub fn expand(p: &str) -> PathBuf {
    match p.strip_prefix("~/") {
        Some(r) => home().join(r),
        None => PathBuf::from(p),
    }
}

/// Apps started from Finder get a minimal PATH; add the usual tool dirs.
/// Call once at startup before spawning threads.
pub fn fix_path() {
    let cur = std::env::var("PATH").unwrap_or_default();
    let mut parts: Vec<String> = cur.split(':').filter(|s| !s.is_empty()).map(String::from).collect();
    for e in EXTRA.iter().rev() {
        let s = expand(e).to_string_lossy().into_owned();
        if !parts.contains(&s) {
            parts.insert(0, s);
        }
    }
    unsafe { std::env::set_var("PATH", parts.join(":")) };
}

pub fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| "/".into())
}

/// funkhaus data root: `$FUNKHAUS_HOME` (non-empty) or ~/Library/Application Support/funkhaus.
pub fn funkhaus_home() -> PathBuf {
    std::env::var_os("FUNKHAUS_HOME").filter(|v| !v.is_empty()).map(PathBuf::from).unwrap_or_else(|| home().join(gpu::APP_DIR))
}

/// omnivoice's `--ref` prompt cache for funkhaus' TTS calls (`OMNIVOICE_CACHE`), instead of omnivoice's own repo folder,
/// so a voice can be purged from it. Not part of any cache key.
pub fn funkhaus_omnivoice_cache() -> PathBuf {
    funkhaus_home().join("cache").join("omnivoice")
}

pub fn which(cmd: &str) -> Option<PathBuf> {
    if cmd.contains('/') {
        let p = expand(cmd);
        return p.is_file().then_some(p);
    }
    let path = std::env::var("PATH").unwrap_or_default();
    path.split(':')
        .map(PathBuf::from)
        .chain(EXTRA.iter().map(|e| expand(e)))
        .map(|d| d.join(cmd))
        .find(|p| p.is_file())
}

pub fn mmss(t: f64) -> String {
    let t = t.max(0.0) as u64;
    if t >= 3600 { format!("{}:{:02}:{:02}", t / 3600, t / 60 % 60, t % 60) } else { format!("{}:{:02}", t / 60, t % 60) }
}

pub fn clip(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        return s.to_string();
    }
    let mut t: String = s.chars().take(n).collect();
    t.push('…');
    t
}
