use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct Config {
    pub llm: Llm,
    /// Tried when the primary endpoint is unreachable.
    pub fallback: Option<Llm>,
    /// small/fast model for `map --fast`
    pub map_fast: Llm,
    /// gemma-serve script for one automatic restart after a GPU crash during `map` (None = no restart)
    pub gemma_serve: Option<String>,
    pub tts: Tts,
    /// Host names + voice personas (`<home>/voices/<persona>.toml`).
    pub hosts: Hosts,
    /// Context budget in characters; smaller notebooks go in whole.
    pub ctx_chars: usize,
    /// Smaller budgets for the heavy JSON/long-output jobs (local GPU limits).
    pub podcast_ctx_chars: usize,
    pub make_ctx_chars: usize,
    /// ASR hook for audio/video sources, e.g. "whisper-cli -nt -f {in}" (stdout = transcript).
    pub asr_cmd: Option<String>,
    pub ffmpeg: String,
    pub claude: String,
    /// yt-dlp binary for video URLs (YouTube, Vimeo, Mediatheken …)
    pub ytdlp: String,
    /// GUI theme: "system" | "hell" | "dunkel"
    pub theme: String,
    /// podcast | hoerbuch | video
    pub loudness: funkhaus_audio::mix::Loudness,
    /// Spoken AI disclosure as the final line of a podcast.
    pub disclosure: bool,
    pub disclosure_text: String,
    pub music: funkhaus_audio::music::Music,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Host {
    pub name: String,
    pub persona: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct Hosts {
    pub o: Host,
    pub f: Host,
}

impl Default for Hosts {
    fn default() -> Self {
        Self { o: Host { name: "Oskar".into(), persona: "gt".into() }, f: Host { name: "Fable".into(), persona: "fable".into() } }
    }
}

/// OpenAI-compatible (or later Anthropic) endpoint; same `[llm]` TOML table as before.
pub type Llm = funkhaus_llm::Backend;

use funkhaus_voice::Tts;

impl Default for Config {
    fn default() -> Self {
        Self {
            llm: Llm::default(),
            fallback: Some(Llm { url: "http://127.0.0.1:8080/v1".into(), model: String::new(), ..Llm::default() }),
            tts: Tts::default(),
            hosts: Hosts::default(),
            map_fast: Llm { url: "http://127.0.0.1:8080/v1".into(), model: "lmstudio-community/gemma-4-E2B-it-MLX-4bit".into(), ..Llm::default() },
            ctx_chars: 24_000,
            podcast_ctx_chars: 14_000,
            make_ctx_chars: 20_000,
            asr_cmd: None,
            gemma_serve: None,
            ffmpeg: "ffmpeg".into(),
            claude: "claude".into(),
            ytdlp: "yt-dlp".into(),
            theme: "system".into(),
            loudness: Default::default(),
            music: Default::default(),
            disclosure: true,
            disclosure_text: "Zum Schluss ein Hinweis: Die Stimmen dieser Folge sind künstlich erzeugt, die Inhalte beruhen auf den Quellen dieses Notizbuchs.".into(),
        }
    }
}

impl Config {
    /// Loads `path`, writing the defaults there first if it doesn't exist.
    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            let c = Self::default();
            std::fs::write(path, toml::to_string_pretty(&c)?)?;
            return Ok(c);
        }
        let raw = std::fs::read_to_string(path)?;
        let raw_t = raw.parse::<toml::Table>().unwrap_or_default();
        let mut c: Self = toml::from_str(&raw)?;
        c.migrate(&raw_t);
        Ok(c)
    }

    /// Legacy `[tts] voice_o/voice_f/host_o/host_f` -> `[hosts]` in memory (an explicit `[hosts]` table wins); warns once.
    fn migrate(&mut self, raw: &toml::Table) {
        let old = |k: &str| raw.get("tts").and_then(|t| t.get(k)).and_then(|v| v.as_str()).map(String::from);
        let (vo, vf, ho, hf) = (old("voice_o"), old("voice_f"), old("host_o"), old("host_f"));
        let legacy = [&vo, &vf, &ho, &hf].iter().any(|x| x.is_some());
        if !raw.contains_key("hosts") {
            if let Some(v) = vo { self.hosts.o.persona = v }
            if let Some(v) = vf { self.hosts.f.persona = v }
            if let Some(v) = ho { self.hosts.o.name = v }
            if let Some(v) = hf { self.hosts.f.name = v }
        }
        for (t, b) in [("llm", Some(&self.llm)), ("fallback", self.fallback.as_ref()), ("map_fast", Some(&self.map_fast))] {
            if let Some(w) = b.and_then(|b| b.legacy_key_warning_once(t)) {
                eprintln!("WARNUNG: config.toml: {w}");
            }
        }
        if legacy {
            static ONCE: std::sync::Once = std::sync::Once::new();
            ONCE.call_once(|| eprintln!("WARNUNG: config.toml: [tts] voice_o/voice_f/host_o/host_f sind veraltet, bitte [hosts.o]/[hosts.f] (name, persona) verwenden"));
        }
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        Ok(std::fs::write(path, toml::to_string_pretty(self)?)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn load(s: &str) -> Config {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("config.toml");
        std::fs::write(&p, s).unwrap();
        Config::load(&p).unwrap()
    }

    #[test]
    fn legacy_tts_fields_migrate() {
        let c = load("[tts]\nvoice_o = \"anna\"\nvoice_f = \"kn\"\nhost_o = \"Ann\"\nhost_f = \"Ken\"\nseed = 3\n");
        assert_eq!((c.hosts.o.persona.as_str(), c.hosts.f.persona.as_str(), c.hosts.o.name.as_str(), c.hosts.f.name.as_str()), ("anna", "kn", "Ann", "Ken"));
        assert_eq!(c.tts.seed, 3);
        assert!(!toml::to_string_pretty(&c).unwrap().contains("voice_o"));
    }

    #[test]
    fn explicit_hosts_win_and_default_ok() {
        let c = load("[tts]\nvoice_o = \"anna\"\n[hosts.o]\nname = \"X\"\npersona = \"p\"\n[hosts.f]\nname = \"Y\"\npersona = \"q\"\n");
        assert_eq!((c.hosts.o.persona.as_str(), c.hosts.f.name.as_str()), ("p", "Y"));
        assert_eq!(load("").hosts.o.persona, "gt");
    }
}
