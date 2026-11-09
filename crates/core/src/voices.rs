//! Host voices: thin `Store` adapters over `funkhaus_voice::voices`.
use crate::{Store, Prog};
use anyhow::Result;
use funkhaus_voice::persona::VoiceSpec;
use serde_json::Value;
use std::path::PathBuf;

pub use funkhaus_voice::voices::New;

pub fn dir(st: &Store) -> PathBuf {
    funkhaus_voice::voices::dir(&st.root)
}

fn cfg(st: &Store) -> (&str, &str) {
    (&st.cfg.hosts.o.persona, &st.cfg.hosts.f.persona)
}

/// (O, F) specs from persona names; `None` = the config's hosts.
pub fn hosts(st: &Store, names: Option<(&str, &str)>) -> Result<(VoiceSpec, VoiceSpec)> {
    funkhaus_voice::voices::hosts(&st.root, names.unwrap_or_else(|| cfg(st)))
}

pub fn ls(st: &Store) -> Value {
    funkhaus_voice::voices::ls(&st.root, cfg(st))
}

pub fn show(st: &Store, name: &str) -> Result<Value> {
    funkhaus_voice::voices::show(&st.root, &st.cfg.tts, name)
}

pub fn add(st: &Store, name: &str, n: New) -> Result<Value> {
    funkhaus_voice::voices::add(&st.root, name, n)
}

pub fn test_real(st: &Store, name: &str, text: &str, prog: Prog) -> Result<PathBuf> {
    funkhaus_voice::voices::test_real(&st.root, &st.cfg.tts, name, text, prog)
}
