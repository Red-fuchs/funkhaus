//! Stimmwerk GUI (egui/eframe 0.33): app state, background jobs, tabs, headless shots. Depends on stimmwerk-core only (no funkhaus-core).
pub mod app;
pub mod icon;
pub mod jobs;
pub mod play;
pub mod shot;
pub mod tabs;
pub mod theme;

pub use app::App;

/// Persisted UI settings file.
pub fn cfg_path() -> std::path::PathBuf {
    stimmwerk_core::home().join("gui.json")
}
