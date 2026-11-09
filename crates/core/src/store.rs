use crate::{Config, Prog, ingest, retrieval::Chunk, util};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Backend {
    #[default]
    Local,
    /// `claude -p` — data leaves the Mac; must be enabled per notebook.
    Claude,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Notebook {
    pub id: String,
    pub name: String,
    pub created: String,
    #[serde(default)]
    pub backend: Backend,
    #[serde(default)]
    pub sources: Vec<Source>,
    #[serde(default)]
    pub next_src: u32,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Source {
    pub id: String,
    pub title: String,
    pub kind: String,
    pub origin: String,
    pub enabled: bool,
    pub chars: usize,
    pub added: String,
    /// yt-dlp extractor for video URLs ("Youtube", …)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extractor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub has_transcript: Option<bool>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
    /// excluded time ranges (sidecar `sources/<id>.outcut.json`)
    #[serde(default, skip_serializing_if = "is_zero")]
    pub outcut: usize,
    #[serde(default, skip_serializing_if = "Ctx::is_voll")]
    pub ctx: Ctx,
}

/// How much of a source's map/guide layer is used: Aus = raw text only (no map fields in retrieval, not in
/// the podcast outline), Kurz = context+summary in retrieval and map summaries instead of raw text in
/// podcast segments, Voll = everything (default).
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Ctx {
    Aus,
    Kurz,
    #[default]
    Voll,
}

impl Ctx {
    fn is_voll(&self) -> bool {
        *self == Ctx::Voll
    }
    pub fn parse(s: &str) -> Result<Self> {
        Ok(match s {
            "aus" | "off" => Ctx::Aus,
            "kurz" | "short" => Ctx::Kurz,
            "voll" | "full" => Ctx::Voll,
            _ => bail!("Kontextstufe: aus|kurz|voll"),
        })
    }
    pub fn label(self) -> &'static str {
        match self {
            Ctx::Aus => "Aus",
            Ctx::Kurz => "Kurzfassung",
            Ctx::Voll => "Voll",
        }
    }
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

/// `src outcut` operations.
pub enum OutcutOp {
    List,
    Add(f64, f64, String),
    /// 1-based index from `List`
    Rm(usize),
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Note {
    pub id: String,
    pub title: String,
    pub text: String,
}

pub struct Store {
    pub root: PathBuf,
    pub cfg: Config,
}

impl Store {
    /// `$FUNKHAUS_HOME` or ~/Library/Application Support/funkhaus.
    pub fn open() -> Result<Self> {
        Self::at(root_from(std::env::var_os("FUNKHAUS_HOME")))
    }

    pub fn at(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        fs::create_dir_all(root.join("notebooks"))?;
        let cfg = Config::load(&root.join("config.toml"))?;
        Ok(Self { root, cfg })
    }

    pub fn dir(&self, id: &str) -> PathBuf {
        self.root.join("notebooks").join(id)
    }

    pub fn list(&self) -> Result<Vec<Notebook>> {
        let mut v = vec![];
        for e in fs::read_dir(self.root.join("notebooks"))? {
            let p = e?.path().join("notebook.json");
            if let Ok(s) = fs::read_to_string(&p) {
                match serde_json::from_str::<Notebook>(&s) {
                    Ok(n) => v.push(n),
                    Err(e) => eprintln!("Warnung: {} defekt, übersprungen ({e})", p.display()),
                }
            }
        }
        v.sort_by(|a, b| b.created.cmp(&a.created));
        Ok(v)
    }

    pub fn create(&self, name: &str) -> Result<Notebook> {
        let base = util::slug(name);
        let mut id = base.clone();
        let mut i = 2;
        while self.dir(&id).exists() {
            id = format!("{base}-{i}");
            i += 1;
        }
        let d = self.dir(&id);
        for sub in ["sources", "notes", "out"] {
            fs::create_dir_all(d.join(sub))?;
        }
        let nb = Notebook { id, name: name.into(), created: util::now(), backend: Backend::Local, sources: vec![], next_src: 1 };
        self.save(&nb)?;
        Ok(nb)
    }

    /// Resolves by id, then name (case-insensitive), then unique id prefix.
    pub fn get(&self, key: &str) -> Result<Notebook> {
        let all = self.list()?;
        let k = key.to_lowercase();
        if let Some(n) = all.iter().find(|n| n.id == key) {
            return Ok(n.clone());
        }
        let mut m: Vec<_> = all.iter().filter(|n| n.name.to_lowercase() == k).collect();
        if m.is_empty() {
            m = all.iter().filter(|n| n.id.starts_with(&k)).collect();
        }
        match m.len() {
            1 => Ok(m[0].clone()),
            0 => bail!("Notebook nicht gefunden: {key}"),
            _ => bail!("Notebook mehrdeutig: {key}"),
        }
    }

    pub fn save(&self, nb: &Notebook) -> Result<()> {
        let p = self.dir(&nb.id).join("notebook.json");
        let tmp = p.with_extension("json.tmp");
        fs::write(&tmp, serde_json::to_string_pretty(nb)?)?;
        fs::rename(tmp, p)?;
        Ok(())
    }

    pub fn remove(&self, nb: &Notebook) -> Result<()> {
        Ok(fs::remove_dir_all(self.dir(&nb.id))?)
    }

    /// Re-reads the notebook under an exclusive file lock, applies `f`, saves, and returns the fresh notebook.
    /// All mutations go through here so concurrent jobs (GUI threads, CLI processes) don't overwrite each other.
    pub fn update<T>(&self, id: &str, f: impl FnOnce(&mut Notebook) -> Result<T>) -> Result<(Notebook, T)> {
        let lf = fs::OpenOptions::new().create(true).truncate(false).write(true).open(self.dir(id).join(".lock"))?;
        lf.lock()?;
        let p = self.dir(id).join("notebook.json");
        let mut nb: Notebook = serde_json::from_str(&fs::read_to_string(&p)?).with_context(|| p.display().to_string())?;
        let r = f(&mut nb)?;
        self.save(&nb)?;
        Ok((nb, r))
    }

    /// Adds a file, folder (recursive) or URL. Returns the new sources; skips duplicates, empty and unsupported files.
    /// `nb` is refreshed from disk afterwards.
    pub fn add_source(&self, nb: &mut Notebook, input: &str, prog: Prog) -> Result<Vec<Source>> {
        let mut added = vec![];
        let mut add = |origin: &str, f: ingest::Fetched| -> Result<()> {
            if f.text.trim().is_empty() {
                prog(&format!("übersprungen: {origin} (kein Text)"));
                return Ok(());
            }
            for w in &f.warnings {
                prog(&format!("WARNUNG: {origin}: {w}"));
            }
            let (_, s) = self.update(&nb.id, |n| self.put(n, origin, f))?;
            added.extend(s);
            Ok(())
        };
        let res = if input.starts_with("http://") || input.starts_with("https://") {
            prog(&format!("Lade {input}"));
            ingest::fetch_url(input, &self.cfg, prog).and_then(|f| add(input, f))
        } else {
            (|| {
                let p = Path::new(input).canonicalize().with_context(|| format!("Pfad nicht gefunden: {input}"))?;
                let dir = p.is_dir();
                let (files, icloud) = if dir { ingest::walk_report(&p) } else { (vec![p], 0) };
                if icloud > 0 {
                    prog(&format!("{icloud} Dateien nicht lokal (iCloud) – erst herunterladen"));
                }
                for f in files {
                    prog(&format!("Lese {}", f.display()));
                    match ingest::extract(&f, &self.cfg) {
                        Ok(x) => add(&f.to_string_lossy(), x)?,
                        Err(e) if !dir => return Err(e),
                        Err(e) => prog(&format!("übersprungen: {} ({e})", f.display())),
                    }
                }
                Ok(())
            })()
        };
        if let Ok(n) = self.get(&nb.id) {
            *nb = n;
        }
        res.map(|_| added)
    }

    fn put(&self, nb: &mut Notebook, origin: &str, f: ingest::Fetched) -> Result<Option<Source>> {
        let text = &f.text;
        if nb.sources.iter().any(|s| s.origin == origin) {
            return Ok(None);
        }
        let id = format!("s{}", nb.next_src.max(1));
        nb.next_src = nb.next_src.max(1) + 1;
        fs::create_dir_all(self.dir(&nb.id).join("sources"))?;
        fs::write(self.dir(&nb.id).join("sources").join(format!("{id}.txt")), text)?;
        if let Some(c) = f.cues.as_ref().filter(|c| !c.is_empty()) {
            fs::write(self.dir(&nb.id).join("sources").join(format!("{id}.cues.json")), serde_json::to_string(c)?)?;
        }
        if !f.outcut.is_empty() {
            if f.cues.as_ref().is_none_or(|c| c.is_empty()) {
                bail!("{origin}: Ausblenden (outcut) braucht eine Quelle mit Zeitstempeln");
            }
            fs::write(self.dir(&nb.id).join("sources").join(format!("{id}.outcut.json")), serde_json::to_string_pretty(&f.outcut)?)?;
        }
        let s = Source {
            id,
            title: f.title,
            kind: f.kind,
            origin: origin.into(),
            enabled: true,
            chars: text.chars().count(),
            added: util::now(),
            extractor: f.extractor,
            has_transcript: f.has_transcript,
            warnings: f.warnings,
            outcut: f.outcut.len(),
            ctx: Ctx::default(),
        };
        nb.sources.push(s.clone());
        Ok(Some(s))
    }

    pub fn source_mut<'a>(nb: &'a mut Notebook, key: &str) -> Result<&'a mut Source> {
        let k = key.to_lowercase();
        let i = nb
            .sources
            .iter()
            .position(|s| s.id == key || s.title.to_lowercase() == k)
            .with_context(|| format!("Quelle nicht gefunden: {key}"))?;
        Ok(&mut nb.sources[i])
    }

    pub fn rm_source(&self, nb: &mut Notebook, key: &str) -> Result<Source> {
        let (n, s) = self.update(&nb.id, |n| {
            let id = Self::source_mut(n, key)?.id.clone();
            let i = n.sources.iter().position(|s| s.id == id).unwrap();
            Ok(n.sources.remove(i))
        })?;
        let _ = fs::remove_file(self.dir(&n.id).join("sources").join(format!("{}.txt", s.id)));
        let _ = fs::remove_file(self.dir(&n.id).join("sources").join(format!("{}.cues.json", s.id)));
        let _ = fs::remove_file(self.dir(&n.id).join("sources").join(format!("{}.outcut.json", s.id)));
        let _ = fs::remove_dir_all(self.dir(&n.id).join("map").join(&s.id));
        *nb = n;
        Ok(s)
    }

    pub fn toggle(&self, nb: &mut Notebook, key: &str, on: bool) -> Result<Source> {
        let (n, s) = self.update(&nb.id, |n| {
            let s = Self::source_mut(n, key)?;
            s.enabled = on;
            Ok(s.clone())
        })?;
        *nb = n;
        Ok(s)
    }

    pub fn set_ctx(&self, nb: &mut Notebook, key: &str, ctx: Ctx) -> Result<Source> {
        let (n, s) = self.update(&nb.id, |n| {
            let s = Self::source_mut(n, key)?;
            s.ctx = ctx;
            Ok(s.clone())
        })?;
        *nb = n;
        Ok(s)
    }

    pub fn set_backend(&self, nb: &mut Notebook, be: Backend) -> Result<()> {
        *nb = self.update(&nb.id, |n| {
            n.backend = be;
            Ok(())
        })?
        .0;
        Ok(())
    }

    /// Warning when the enabled sources are too thin for a podcast or document.
    pub fn thin(nb: &Notebook) -> Option<String> {
        let act: Vec<&Source> = nb.sources.iter().filter(|s| s.enabled).collect();
        let chars: usize = act.iter().map(|s| s.chars).sum();
        if act.is_empty() || chars >= 1500 {
            return None;
        }
        let no_tr = act.iter().any(|s| s.has_transcript == Some(false));
        Some(format!(
            "Quellen sehr dünn ({chars} Zeichen){} – Ergebnis beruht auf wenig Material",
            if no_tr { ", Video ohne Transkript" } else { "" }
        ))
    }

    pub fn text(&self, nb: &Notebook, src: &str) -> Result<String> {
        Ok(fs::read_to_string(self.dir(&nb.id).join("sources").join(format!("{src}.txt")))?)
    }

    /// Timed cues of a source (sidecar `sources/<id>.cues.json`), if any.
    pub fn cues(&self, nb: &Notebook, src: &str) -> Option<Vec<ingest::Cue>> {
        let s = fs::read_to_string(self.dir(&nb.id).join("sources").join(format!("{src}.cues.json"))).ok()?;
        serde_json::from_str(&s).ok()
    }

    /// Excluded ranges of a source (empty if none).
    pub fn outcuts(&self, nb: &Notebook, src: &str) -> Vec<ingest::Outcut> {
        fs::read_to_string(self.dir(&nb.id).join("sources").join(format!("{src}.outcut.json"))).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
    }

    /// The single choke point for retrieval, map, guide, ask, make and podcast: cues overlapping an
    /// outcut range are removed from text and cues before chunking.
    pub fn source_chunks(&self, nb: &Notebook, s: &Source) -> Result<Vec<Chunk>> {
        let text = self.text(nb, &s.id)?;
        Ok(match self.cues(nb, &s.id) {
            Some(c) => {
                let oc = self.outcuts(nb, &s.id);
                let drop: Vec<bool> = c.iter().map(|x| oc.iter().any(|o| o.hits(x.t0, x.t1))).collect();
                if drop.contains(&true) {
                    let kept: Vec<ingest::Cue> = c.iter().zip(&drop).filter(|x| !x.1).map(|x| x.0.clone()).collect();
                    crate::retrieval::chunk_cued(&s.id, &s.title, &ingest::cut(&text, &c, &drop), &kept)
                } else {
                    crate::retrieval::chunk_cued(&s.id, &s.title, &text, &c)
                }
            }
            None => crate::retrieval::chunk(&s.id, &s.title, &text),
        })
    }

    /// List/add/remove excluded ranges of a timed source; returns the ranges after the change.
    pub fn outcut(&self, nb: &mut Notebook, key: &str, op: OutcutOp) -> Result<Vec<ingest::Outcut>> {
        let s = Self::source_mut(&mut nb.clone(), key)?.clone();
        if self.cues(nb, &s.id).is_none() {
            bail!("Ausblenden (outcut) braucht eine Quelle mit Zeitstempeln (Untertitel/Transkript)");
        }
        let mut v = self.outcuts(nb, &s.id);
        match op {
            OutcutOp::List => return Ok(v),
            OutcutOp::Add(t0, t1, reason) => {
                if t1 <= t0 {
                    bail!("Ende muss nach dem Anfang liegen");
                }
                v.push(ingest::Outcut { t0, t1, reason, src: "cli".into() });
                ingest::merge(&mut v);
            }
            OutcutOp::Rm(i) => {
                if i == 0 || i > v.len() {
                    bail!("kein Bereich Nr. {i} (vorhanden: {})", v.len());
                }
                v.remove(i - 1);
            }
        }
        let p = self.dir(&nb.id).join("sources").join(format!("{}.outcut.json", s.id));
        if v.is_empty() {
            let _ = fs::remove_file(&p);
        } else {
            fs::write(&p, serde_json::to_string_pretty(&v)?)?;
        }
        let n = v.len();
        *nb = self.update(&nb.id, |x| {
            Self::source_mut(x, &s.id)?.outcut = n;
            Ok(())
        })?
        .0;
        Ok(v)
    }

    /// Map fields for `chunks` (index-aligned); chunks without a cached map get empty fields.
    /// Gated by the source's context level (Aus: none, Kurz: context+summary, Voll: + facts and questions).
    pub fn aux(&self, nb: &Notebook, chunks: &[Chunk]) -> Vec<crate::retrieval::Aux> {
        let lvl = |src: &str| nb.sources.iter().find(|s| s.id == src).map(|s| s.ctx).unwrap_or_default();
        chunks
            .iter()
            .map(|c| match (lvl(&c.src), crate::map::load(self, nb, c)) {
                (Ctx::Aus, _) | (_, None) => Default::default(),
                (Ctx::Kurz, Some(m)) => crate::retrieval::Aux { about: Some(format!("{}\n{}", m.context, m.summary)), questions: None },
                (Ctx::Voll, Some(m)) => crate::retrieval::Aux {
                    about: Some(
                        [m.context, m.summary].into_iter().chain(m.facts.into_iter().map(|f| f.text)).collect::<Vec<_>>().join("\n"),
                    ),
                    questions: Some(m.questions.join("\n")).filter(|q| !q.is_empty()),
                },
            })
            .collect()
    }

    /// Chunks of all enabled sources, in source order.
    pub fn chunks(&self, nb: &Notebook) -> Result<Vec<Chunk>> {
        let mut v = vec![];
        for s in nb.sources.iter().filter(|s| s.enabled) {
            v.extend(self.source_chunks(nb, s)?);
        }
        Ok(v)
    }

    /// Looks up a chunk by id (`s1#3`), regardless of enabled state.
    pub fn chunk(&self, nb: &Notebook, id: &str) -> Result<Chunk> {
        let (sid, _) = id.split_once('#').context("ungültige Chunk-ID")?;
        let s = nb.sources.iter().find(|s| s.id == sid).context("Quelle fehlt")?;
        self.source_chunks(nb, s)?
            .into_iter()
            .find(|c| c.id == id)
            .context("Chunk fehlt")
    }

    pub fn append_chat(&self, nb: &Notebook, v: &impl Serialize) -> Result<()> {
        use std::io::Write;
        let mut f = fs::OpenOptions::new().create(true).append(true).open(self.dir(&nb.id).join("chat.jsonl"))?;
        writeln!(f, "{}", serde_json::to_string(v)?)?;
        Ok(())
    }

    pub fn chat<T: for<'de> Deserialize<'de>>(&self, nb: &Notebook) -> Result<Vec<T>> {
        let s = fs::read_to_string(self.dir(&nb.id).join("chat.jsonl")).unwrap_or_default();
        Ok(s.lines().filter_map(|l| serde_json::from_str(l).ok()).collect())
    }

    pub fn clear_chat(&self, nb: &Notebook) -> Result<()> {
        let _ = fs::remove_file(self.dir(&nb.id).join("chat.jsonl"));
        Ok(())
    }

    pub fn add_note(&self, nb: &Notebook, title: &str, text: &str) -> Result<Note> {
        let d = self.dir(&nb.id).join("notes");
        fs::create_dir_all(&d)?;
        let base = format!("{}-{}", util::stamp(), util::slug(title));
        let mut id = base.clone();
        let mut i = 2;
        while d.join(format!("{id}.md")).exists() {
            id = format!("{base}-{i}");
            i += 1;
        }
        fs::write(d.join(format!("{id}.md")), format!("# {title}\n\n{text}\n"))?;
        Ok(Note { id, title: title.into(), text: text.into() })
    }

    pub fn notes(&self, nb: &Notebook) -> Result<Vec<Note>> {
        let mut v = vec![];
        let Ok(rd) = fs::read_dir(self.dir(&nb.id).join("notes")) else { return Ok(v) };
        for e in rd {
            let p = e?.path();
            if p.extension().is_some_and(|x| x == "md") {
                let s = fs::read_to_string(&p)?;
                let (title, text) = match s.strip_prefix("# ").and_then(|r| r.split_once('\n')) {
                    Some((t, b)) => (t.trim().to_string(), b.trim().to_string()),
                    None => (String::new(), s.trim().to_string()),
                };
                v.push(Note { id: p.file_stem().unwrap().to_string_lossy().into(), title, text });
            }
        }
        v.sort_by(|a, b| b.id.cmp(&a.id));
        Ok(v)
    }

    pub fn rm_note(&self, nb: &Notebook, id: &str) -> Result<()> {
        Ok(fs::remove_file(self.dir(&nb.id).join("notes").join(format!("{id}.md")))?)
    }

    pub fn out_dir(&self, nb: &Notebook) -> PathBuf {
        self.dir(&nb.id).join("out")
    }
}

/// Data root; an unset or empty `FUNKHAUS_HOME` means the default location.
pub fn root_from(env: Option<std::ffi::OsString>) -> PathBuf {
    env.filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| util::home().join(funkhaus_sys::gpu::APP_DIR))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ctx_gates_aux() {
        let t = tempfile::tempdir().unwrap();
        let st = Store::at(t.path().join("home")).unwrap();
        let mut nb = st.create("C").unwrap();
        let f = t.path().join("a.md");
        fs::write(&f, "Ein kurzer Text.").unwrap();
        st.add_source(&mut nb, f.to_str().unwrap(), &|_| {}).unwrap();
        let c = st.chunks(&nb).unwrap();
        let m = crate::map::Map {
            v: crate::map::MAP_V,
            chunk: c[0].id.clone(),
            hash: crate::map::key(&c[0].text),
            context: "K".into(),
            summary: "S".into(),
            facts: vec![crate::map::Fact { text: "F".into(), quote: "q".into() }],
            questions: vec!["Q?".into()],
            dropped: 0,
            model: String::new(),
        };
        let p = crate::map::path(&st, &nb, &c[0]);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, serde_json::to_string(&m).unwrap()).unwrap();
        let a = |nb: &Notebook| st.aux(nb, &c).remove(0);
        assert_eq!((a(&nb).about.as_deref(), a(&nb).questions.as_deref()), (Some("K\nS\nF"), Some("Q?")));
        st.set_ctx(&mut nb, "s1", Ctx::Kurz).unwrap();
        assert_eq!((a(&nb).about.as_deref(), a(&nb).questions), (Some("K\nS"), None));
        st.set_ctx(&mut nb, "s1", Ctx::Aus).unwrap();
        assert!(a(&nb).about.is_none());
        assert_eq!(st.get("c").unwrap().sources[0].ctx, Ctx::Aus);
    }

    #[test]
    fn outcut_never_chunked() {
        let t = tempfile::tempdir().unwrap();
        let st = Store::at(t.path().join("home")).unwrap();
        let mut nb = st.create("O").unwrap();
        let f = t.path().join("talk.vtt");
        let cue = |k: usize, txt: &str| format!("00:00:{:02}.000 --> 00:00:{:02}.500\n<v A>{txt}\n\n", k * 2, k * 2 + 1);
        let body: String = (0..10).map(|k| cue(k, &format!("Satz {k} öffentlich."))).collect();
        let private = cue(10, "Mein Hund muss zum Tierarzt.");
        fs::write(&f, format!("WEBVTT\n\n{body}NOTE outcut:start privat\n\n{private}NOTE outcut:end\n\n{}", cue(11, "Weiter im Text."))).unwrap();
        st.add_source(&mut nb, f.to_str().unwrap(), &|_| {}).unwrap();
        assert_eq!(nb.sources[0].outcut, 1);
        let c = st.chunks(&nb).unwrap();
        assert!(c.iter().all(|c| !c.text.contains("Tierarzt")) && c.iter().any(|c| c.text.contains("Weiter im Text.")));
        assert_eq!(st.cues(&nb, "s1").unwrap().len(), 12); // full cues kept for audit
        assert!(st.text(&nb, "s1").unwrap().contains("Tierarzt"));
        // cli ops: add is merged/idempotent, rm by number, bad number errors, remove → text back
        let v = st.outcut(&mut nb, "s1", OutcutOp::Add(0.0, 1.0, "x".into())).unwrap();
        assert_eq!(v.len(), 2);
        assert_eq!(st.outcut(&mut nb, "s1", OutcutOp::Add(0.2, 0.8, "x".into())).unwrap().len(), 2);
        assert!(!st.chunks(&nb).unwrap()[0].text.contains("Satz 0"));
        assert!(st.outcut(&mut nb, "s1", OutcutOp::Rm(5)).is_err());
        st.outcut(&mut nb, "s1", OutcutOp::Rm(1)).unwrap();
        st.outcut(&mut nb, "s1", OutcutOp::Rm(1)).unwrap();
        assert_eq!(nb.sources[0].outcut, 0);
        assert!(st.chunks(&nb).unwrap().iter().any(|c| c.text.contains("Tierarzt")));
        st.outcut(&mut nb, "s1", OutcutOp::Add(20.0, 21.0, String::new())).unwrap();
        st.rm_source(&mut nb, "s1").unwrap();
        assert!(!st.dir("o").join("sources/s1.outcut.json").exists());
        // untimed sources can't be cut
        let g = t.path().join("a.md");
        fs::write(&g, "Text").unwrap();
        st.add_source(&mut nb, g.to_str().unwrap(), &|_| {}).unwrap();
        assert!(st.outcut(&mut nb, "s2", OutcutOp::List).is_err());
    }

    #[test]
    fn vtt_source_chunks_carry_time() {
        let t = tempfile::tempdir().unwrap();
        let st = Store::at(t.path().join("home")).unwrap();
        let mut nb = st.create("V").unwrap();
        let f = t.path().join("talk.vtt");
        fs::write(&f, "WEBVTT\n\n00:01:05.000 --> 00:01:07.000\n<v Anke>Wir sind 84 Mitglieder.\n\n00:01:07.000 --> 00:01:09.000\n<v Jonas>Und heute?\n").unwrap();
        st.add_source(&mut nb, f.to_str().unwrap(), &|_| {}).unwrap();
        assert!(st.dir("v").join("sources/s1.cues.json").exists());
        let c = st.chunks(&nb).unwrap();
        assert_eq!((c[0].t0, c[0].at().as_deref(), c[0].speaker.as_deref()), (Some(65.0), Some("1:05"), Some("Anke, Jonas")));
        assert_eq!(st.text(&nb, "s1").unwrap(), "Wir sind 84 Mitglieder.\nUnd heute?");
        assert_eq!(st.chunk(&nb, "s1#0").unwrap().t1, Some(69.0));
    }

    #[test]
    fn home_env() {
        assert!(root_from(Some("".into())).ends_with("Library/Application Support/funkhaus"));
        assert_eq!(root_from(Some("/x".into())), PathBuf::from("/x"));
    }

    #[test]
    fn empty_files_and_corrupt_notebooks() {
        let t = tempfile::tempdir().unwrap();
        let st = Store::at(t.path().join("home")).unwrap();
        let mut nb = st.create("A").unwrap();
        let d = t.path().join("src");
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join("a.md"), "Inhalt").unwrap();
        fs::write(d.join("b.md"), "   \n").unwrap();
        fs::write(d.join("c.txt"), "Mehr").unwrap();
        let v = st.add_source(&mut nb, d.to_str().unwrap(), &|_| {}).unwrap();
        assert_eq!(v.len(), 2);
        assert_eq!(st.get("a").unwrap().sources.len(), 2);
        assert_eq!(fs::read_dir(st.dir("a").join("sources")).unwrap().count(), 2);
        fs::create_dir_all(st.dir("kaputt")).unwrap();
        fs::write(st.dir("kaputt").join("notebook.json"), "{nope").unwrap();
        assert_eq!(st.list().unwrap().len(), 1);
    }

    #[test]
    fn concurrent_adds() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path().join("home");
        Store::at(&root).unwrap().create("B").unwrap();
        let hs: Vec<_> = (0..4)
            .map(|i| {
                let (root, f) = (root.clone(), t.path().join(format!("f{i}.md")));
                fs::write(&f, format!("Text {i}")).unwrap();
                std::thread::spawn(move || {
                    let st = Store::at(root).unwrap();
                    let mut nb = st.get("b").unwrap();
                    st.add_source(&mut nb, f.to_str().unwrap(), &|_| {}).unwrap();
                })
            })
            .collect();
        hs.into_iter().for_each(|h| h.join().unwrap());
        let nb = Store::at(&root).unwrap().get("b").unwrap();
        let mut ids: Vec<_> = nb.sources.iter().map(|s| s.id.clone()).collect();
        ids.sort();
        assert_eq!(ids, ["s1", "s2", "s3", "s4"]);
    }

    #[test]
    fn crud() {
        let t = tempfile::tempdir().unwrap();
        let st = Store::at(t.path()).unwrap();
        let mut nb = st.create("Test Buch").unwrap();
        assert_eq!(nb.id, "test-buch");
        assert_eq!(st.create("Test Buch").unwrap().id, "test-buch-2");
        let f = t.path().join("a.md");
        fs::write(&f, "# Titel A\n\nHallo Welt.").unwrap();
        let s = st.add_source(&mut nb, f.to_str().unwrap(), &|_| {}).unwrap();
        assert_eq!(s[0].id, "s1");
        assert_eq!(s[0].title, "Titel A");
        assert!(st.add_source(&mut nb, f.to_str().unwrap(), &|_| {}).unwrap().is_empty());
        st.toggle(&mut nb, "s1", false).unwrap();
        assert!(st.chunks(&st.get("test-buch").unwrap()).unwrap().is_empty());
        assert!(st.get("Test Buch").is_err());
        assert_eq!(st.get("test-buch").unwrap().sources.len(), 1);
        assert!(st.get("test").is_err());
        assert!(Store::thin(&st.get("test-buch").unwrap()).is_none()); // source disabled
        st.add_note(&nb, "Notiz", "Text").unwrap();
        assert_eq!(st.notes(&nb).unwrap()[0].title, "Notiz");
        st.rm_source(&mut nb, "s1").unwrap();
        st.remove(&nb).unwrap();
        assert_eq!(st.list().unwrap().len(), 1);
    }
}
