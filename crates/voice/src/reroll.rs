//! Per-line reroll takes persisted as `rerolls.json` (by content hash).
use crate::tts::{self, Rerolls};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};
use std::path::Path;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct Take {
    pub line: usize,
    pub text: String,
    /// `None` = base take
    pub salts: Vec<Option<u64>>,
    pub active: usize,
}

#[derive(Serialize, Deserialize, Debug, Default, PartialEq)]
pub struct File {
    pub v: u32,
    pub takes: BTreeMap<String, Take>,
}

impl File {
    /// Missing file = empty; unreadable/unknown-version file is an error (never silently reset).
    pub fn load(dir: &Path) -> Result<Self> {
        let p = dir.join("rerolls.json");
        if !p.exists() {
            return Ok(Self { v: 1, ..Default::default() });
        }
        let f: Self = serde_json::from_str(&std::fs::read_to_string(&p)?).with_context(|| format!("{} ist kaputt", p.display()))?;
        if f.v != 1 {
            bail!("{}: unbekannte Version {}", p.display(), f.v);
        }
        Ok(f)
    }

    pub fn save(&self, dir: &Path) -> Result<()> {
        let (p, tmp) = (dir.join("rerolls.json"), dir.join("rerolls.json.tmp"));
        std::fs::write(&tmp, serde_json::to_string_pretty(self)?)?;
        Ok(std::fs::rename(tmp, p)?)
    }

    pub fn rerolls(&self) -> Rerolls {
        let mut r = Rerolls::default();
        for (k, t) in &self.takes {
            let Ok(base) = u64::from_str_radix(k, 16) else { continue };
            for s in &t.salts {
                r.keep.insert(s.map_or(base, |s| tts::salted(base, s)));
            }
            if let Some(Some(s)) = t.salts.get(t.active) {
                r.active.insert(base, *s);
            }
        }
        r
    }

    /// New take (or one step back) for each key of `line`; returns (active index, active salt).
    pub fn bump(&mut self, line: usize, text: &str, keys: &[u64], undo: bool) -> Result<(usize, Option<u64>)> {
        let mut res = (0, None);
        if undo && !keys.iter().any(|k| self.takes.get(&format!("{k:016x}")).is_some_and(|t| t.active > 0)) {
            bail!("Zeile {line}: kein früherer Take");
        }
        for &k in keys {
            let id = format!("{k:016x}");
            let t = self.takes.entry(id.clone()).or_insert_with(|| Take { line, text: text.into(), salts: vec![None], active: 0 });
            (t.line, t.text) = (line, text.into());
            if undo {
                t.active = t.active.saturating_sub(1);
            } else {
                t.salts.push(Some(tts::fnv(&[&id, "take", &t.salts.len().to_string()])));
                t.active = t.salts.len() - 1;
            }
            res = (t.active, t.salts[t.active]);
        }
        Ok(res)
    }
}

impl File {
    /// Drops takes whose base key is no longer produced by the script; true if anything was removed.
    pub fn prune(&mut self, valid: &HashSet<u64>) -> bool {
        let n = self.takes.len();
        self.takes.retain(|k, _| u64::from_str_radix(k, 16).is_ok_and(|b| valid.contains(&b)));
        self.takes.len() != n
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{lexicon::Lexicon, persona::VoiceSpec};
    use funkhaus_audio::script::Line;
    use std::cell::Cell;

    fn lines(n: usize) -> Vec<Line> {
        (1..=n).map(|i| Line::ofb(if i % 2 == 1 { 'O' } else { 'F' }, 0.0, format!("Satz {i}."))).collect()
    }
    fn render(d: &Path, ls: &[Line], n: &Cell<usize>) -> Vec<Vec<tts::Part>> {
        let rr = File::load(d).unwrap().rerolls();
        let say = |w: &Path, _: &str, _: &VoiceSpec, _: u64| -> Result<()> {
            n.set(n.get() + 1);
            funkhaus_audio::mix::write(w, &[0.1f32; 100], 24_000)
        };
        tts::render_with(&tts::RenderReq { t: &crate::Tts::default(), lines: ls, voices: &tts::hosts(VoiceSpec::builtin("gt"), VoiceSpec::builtin("anna")), lex: &Lexicon::default(), lang: "de", dir: d, fresh: false, rr: &rr, prog: &|_: &str| {}, cancel: None, on_line: None }, say).unwrap()
    }
    fn keys(ls: &[Line], i: usize) -> Vec<u64> {
        tts::line_keys(&crate::Tts::default(), ls, &tts::hosts(VoiceSpec::builtin("gt"), VoiceSpec::builtin("anna")), &Lexicon::default(), "de", i).unwrap()
    }
    fn files(d: &Path) -> usize {
        std::fs::read_dir(d).unwrap().flatten().filter(|e| e.file_name().to_string_lossy().ends_with(".wav")).count()
    }

    #[test]
    fn reroll_persist_undo() {
        let (d, n) = (tempfile::tempdir().unwrap(), Cell::new(0));
        let ls = lines(3);
        let base = render(d.path(), &ls, &n);
        assert_eq!(n.get(), 3);
        n.set(0);
        let mut f = File::load(d.path()).unwrap();
        f.v = 1;
        f.bump(2, &ls[1].text, &keys(&ls, 1), false).unwrap();
        f.save(d.path()).unwrap();
        let a = render(d.path(), &ls, &n);
        assert_eq!(n.get(), 1);
        assert_eq!((&a[0], &a[2]), (&base[0], &base[2]));
        assert_ne!(a[1], base[1]);
        n.set(0);
        assert_eq!(render(d.path(), &ls, &n), a); // rerun keeps the take
        assert_eq!(n.get(), 0);
        assert_eq!(files(d.path()), 4); // base take of line 2 survives pruning
        let mut f = File::load(d.path()).unwrap();
        f.bump(2, &ls[1].text, &keys(&ls, 1), true).unwrap();
        f.save(d.path()).unwrap();
        assert_eq!(render(d.path(), &ls, &n), base);
        assert_eq!(n.get(), 0);
        assert!(f.bump(2, "x", &keys(&ls, 1), true).is_err());
    }

    #[test]
    fn prune_drops_stale_takes() {
        let ls = lines(3);
        let mut f = File::default();
        f.bump(2, &ls[1].text, &keys(&ls, 1), false).unwrap();
        f.bump(3, "weg", &[0xdead_beef], false).unwrap();
        let valid: HashSet<u64> = (0..3).flat_map(|i| keys(&ls, i)).collect();
        assert!(f.prune(&valid));
        assert_eq!(f.takes.len(), 1);
        assert!(!f.prune(&valid));
        assert_eq!(f.rerolls().active.len(), 1);
    }

    #[test]
    fn salt_changes_hash_and_seed_only_for_target() {
        let b = 0x1234_5678_9abc_def0u64;
        assert_ne!(tts::salted(b, 5), b);
        assert_ne!(tts::salted(b, 5), tts::salted(b, 6));
        assert_ne!(tts::salted(b, 5) & 0x7fff_ffff, b & 0x7fff_ffff);
    }

    #[test]
    fn bad_json_errors_without_reset() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("rerolls.json"), "{kaputt").unwrap();
        assert!(File::load(d.path()).is_err());
        std::fs::write(d.path().join("rerolls.json"), r#"{"v":2,"takes":{}}"#).unwrap();
        assert!(File::load(d.path()).is_err());
        assert!(std::fs::read_to_string(d.path().join("rerolls.json")).unwrap().contains("\"v\":2"));
    }
}
