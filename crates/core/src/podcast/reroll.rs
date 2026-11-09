//! Per-line rerolls: `rerolls.json` (takes by content hash) and `render.json` (voices + mix options used) in the podcast dir.
use super::{Opts, Pod, run, script};
use crate::{Notebook, Prog, Store};
use anyhow::{Context, Result, bail};
pub use funkhaus_voice::reroll::{File, Take};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use funkhaus_audio::mix::Loudness;
use std::path::{Path, PathBuf};

/// What the podcast was rendered with (`render.json`): personas, language and mix options, so reroll/rerun reproduce it.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct Render {
    pub o: String,
    pub f: String,
    pub lang: String,
    #[serde(default)]
    pub loudness: Option<Loudness>,
    #[serde(default)]
    pub jingle: Option<bool>,
    #[serde(default)]
    pub bed: Option<bool>,
    #[serde(default)]
    pub no_disclosure: bool,
    /// never persisted as true: a fresh render must not repeat on every reroll
    #[serde(default)]
    pub fresh: bool,
}

impl Render {
    /// `render.json`, else legacy `voices.json` (personas + lang only).
    pub fn load(dir: &Path) -> Option<Self> {
        ["render.json", "voices.json"].iter().find_map(|n| serde_json::from_str(&std::fs::read_to_string(dir.join(n)).ok()?).ok())
    }
    pub fn save(&self, dir: &Path) -> Result<()> {
        Ok(std::fs::write(dir.join("render.json"), serde_json::to_string_pretty(&Self { fresh: false, ..self.clone() })?)?)
    }
}

/// Exclusive lock on `<dir>/.lock` while held.
pub fn lock(dir: &Path) -> Result<std::fs::File> {
    let f = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(dir.join(".lock"))?;
    if f.try_lock().is_err() {
        bail!("{} wird gerade von einem anderen Prozess bearbeitet", dir.display());
    }
    Ok(f)
}

/// `None`/"latest" = newest podcast dir of the notebook.
pub fn resolve_dir(st: &Store, nb: &Notebook, dir: Option<&str>) -> Result<PathBuf> {
    match dir {
        None | Some("latest") => Ok(super::list(st, nb).into_iter().next().context("kein Podcast vorhanden")?.dir),
        Some(d) => {
            let p = PathBuf::from(d);
            Ok(if p.is_absolute() || p.exists() { p } else { st.out_dir(nb).join(p) })
        }
    }
}

/// Rerolls (or with `undo` steps back) 1-based dialog line `line` of `dir/script.txt`, then remixes via `run`.
pub fn reroll(st: &Store, nb: &Notebook, dir: Option<&str>, line: usize, undo: bool, prog: Prog) -> Result<Value> {
    let dir = resolve_dir(st, nb, dir)?;
    let sp = dir.join("script.txt");
    let lines = script::parse(&std::fs::read_to_string(&sp).with_context(|| format!("{} fehlt", sp.display()))?)?;
    if line < 1 || line > lines.len() {
        bail!("Zeile {line} außerhalb 1..={}", lines.len());
    }
    if super::reuse_dir(st, nb, &sp).is_none() {
        bail!("{} liegt nicht in einem podcast-* Ordner dieses Notebooks", dir.display());
    }
    let _lock = lock(&dir)?;
    let v = Render::load(&dir).unwrap_or_else(|| Render { o: st.cfg.hosts.o.persona.clone(), f: st.cfg.hosts.f.persona.clone(), lang: "de".into(), loudness: None, jingle: None, bed: None, no_disclosure: false, fresh: false });
    let (vo, vf) = crate::voices::hosts(st, Some((&v.o, &v.f)))?;
    let lex = funkhaus_voice::lexicon::Lexicon::load(&st.dir(&nb.id).join("lexicon.tsv"));
    let keys = funkhaus_voice::tts::line_keys(&st.cfg.tts, &lines, &funkhaus_voice::tts::hosts(vo, vf), &lex, &v.lang, line - 1)?;
    if keys.is_empty() {
        bail!("Zeile {line} hat keinen gesprochenen Text");
    }
    let mut f = File::load(&dir)?;
    let (take, salt) = f.bump(line, &lines[line - 1].text, &keys, undo)?;
    f.save(&dir)?;
    let o = Opts { from_script: Some(sp), voices: Some((v.o, v.f)), lang: v.lang, loudness: v.loudness, jingle: v.jingle, bed: v.bed, no_disclosure: v.no_disclosure, ..Opts::default() };
    let pod: Pod = run(st, nb, &o, prog)?;
    Ok(json!({"dir": dir, "line": line, "keys": keys.iter().map(|k| format!("{k:016x}")).collect::<Vec<_>>(), "take": take, "salt": salt, "undo": undo, "pod": pod}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_json_roundtrip_and_legacy() {
        let d = tempfile::tempdir().unwrap();
        assert!(Render::load(d.path()).is_none());
        std::fs::write(d.path().join("voices.json"), r#"{"o":"a","f":"b","lang":"de"}"#).unwrap();
        let l = Render::load(d.path()).unwrap();
        assert_eq!((l.loudness, l.jingle, l.no_disclosure), (None, None, false));
        let r = Render { loudness: Some(Loudness::Hoerbuch), jingle: Some(true), bed: Some(false), no_disclosure: true, fresh: true, ..l };
        r.save(d.path()).unwrap();
        let x = Render::load(d.path()).unwrap();
        assert_eq!((x.loudness, x.jingle, x.bed, x.no_disclosure, x.fresh), (Some(Loudness::Hoerbuch), Some(true), Some(false), true, false));
    }

    #[test]
    fn lock_is_exclusive() {
        let d = tempfile::tempdir().unwrap();
        let g = lock(d.path()).unwrap();
        assert!(lock(d.path()).is_err());
        drop(g);
        assert!(lock(d.path()).is_ok());
    }

}
