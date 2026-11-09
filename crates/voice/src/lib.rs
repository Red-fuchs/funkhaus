/// Moved to funkhaus-sys (shared with Stimmwerk); re-exported for compatibility.
pub use funkhaus_sys::gpu;
pub mod lexicon;
pub mod persona;
pub mod reroll;
pub mod music;
pub mod tts;
pub mod voices;

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct Tts {
    pub cmd: String,
    pub seed: u64,
    pub lang: String,
    pub quality: String,
    /// Part of the clip cache key: change to invalidate cached clips (e.g. after a voice/model update).
    pub cache_salt: String,
}

impl Default for Tts {
    fn default() -> Self {
        Self {
            cmd: "omnivoice".into(),
            seed: 7,
            lang: "de".into(),
            quality: String::new(),
            cache_salt: String::new(),
        }
    }
}
