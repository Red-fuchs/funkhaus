//! Stimmwerk: voice studio projects (`<name>.studio/`), headless core for CLI and GUI. No funkhaus-core dependency.
pub mod cues;
pub mod derived;
pub mod export;
pub mod import;
pub mod lint;
pub mod llm_cfg;
pub mod music;
pub mod project;
pub mod prose;
pub mod prose_score;
pub mod render;
pub mod script;
pub mod shorten;
pub mod speakers;
pub mod sync;
pub mod voice;
pub mod wdiff;

pub use project::Project;

use std::path::{Path, PathBuf};

pub const APP_DIR: &str = "Library/Application Support/stimmwerk";
pub const EXT: &str = "studio";

/// Data root: `$STIMMWERK_HOME` (non-empty) or ~/Library/Application Support/stimmwerk.
pub fn home() -> PathBuf {
    std::env::var_os("STIMMWERK_HOME").filter(|v| !v.is_empty()).map(PathBuf::from).unwrap_or_else(|| funkhaus_sys::home().join(APP_DIR))
}

/// Persona hints name `stimmwerk voice ls` (call once at program start).
pub fn set_app() {
    funkhaus_voice::persona::set_app("stimmwerk");
}

/// omnivoice's `--ref` prompt cache for Stimmwerk's TTS calls (`OMNIVOICE_CACHE`), so `voice revoke` can delete it.
pub fn omnivoice_cache() -> PathBuf {
    home().join("cache").join("omnivoice")
}

pub fn projects_dir() -> PathBuf {
    home().join("projects")
}

/// Persona dirs searched in order: own `<home>/voices`, then funkhaus' (`$FUNKHAUS_HOME` or its default).
pub fn persona_dirs() -> Vec<PathBuf> {
    vec![funkhaus_voice::voices::dir(&home()), funkhaus_voice::voices::dir(&funkhaus_sys::funkhaus_home())]
}

fn with_ext(p: &Path) -> PathBuf {
    if p.extension().is_some_and(|e| e == EXT) { p.to_path_buf() } else { p.with_file_name(format!("{}.{EXT}", p.file_name().map(|n| n.to_string_lossy()).unwrap_or_default())) }
}

/// Bare name (no `/`) -> `<projects>/<name>.studio`; path -> path with `.studio` appended if missing.
pub fn target(arg: &str) -> PathBuf {
    if arg.contains('/') { with_ext(Path::new(arg)) } else { with_ext(&projects_dir().join(arg)) }
}

/// Existing project dir for an argument: path as given, with `.studio`, or a name under the projects dir.
pub fn locate(arg: &str) -> PathBuf {
    let p = PathBuf::from(arg);
    if p.join(project::FILE).is_file() { p } else { target(arg) }
}
