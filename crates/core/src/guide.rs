//! M2 reduce + source guide. Sections of 10–15 consecutive chunks are reduced from map facts+quotes
//! (not summaries) into {title, summary, key_facts, speakers}; the source guide is built from the
//! sections. Post-validated per docs/m2/eval-reduce.md (R1–R5, G1–G6), one retry, invalid key facts
//! dropped. Cached as `guide/<source>.json`, keyed by (MAP_V, GUIDE_V, ordered chunk-text hashes).
use crate::{Notebook, Prog, Source, Store, llm, map, retrieval::Chunk, util};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::ops::Range;
use std::path::PathBuf;

/// Bump on any prompt/schema/validation change.
pub const GUIDE_V: u32 = 2;
/// Sources with fewer chunks get short sections (4–6 chunks ≈ 10–15 min), longer ones 10–15.
pub const LONG: usize = 60;
pub const SEC_MAX: usize = 15;
pub const SEC_SHORT: usize = 6;
/// Input budget per call (chars, ≈ 3.5 chars/token for German) → prompt + output stays < ~7k tokens.
const IN_CHARS: usize = 16_000;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct KeyFact {
    pub text: String,
    pub chunk_id: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Section {
    pub id: String,
    pub chunk_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub t0: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub t1: Option<f64>,
    pub title: String,
    pub summary: String,
    pub key_facts: Vec<KeyFact>,
    pub speakers: Vec<String>,
    /// key facts + speakers removed by validation
    #[serde(default)]
    pub dropped: usize,
    /// checks still failing after the retry (kept, reported by eval-guide)
    #[serde(default)]
    pub issues: Vec<String>,
    /// corrective LLM retries used (0/1)
    #[serde(default)]
    pub retries: usize,
}

impl Section {
    pub fn span(&self) -> String {
        match (self.t0, self.t1) {
            (Some(a), Some(b)) => format!(" ({}–{})", util::mmss(a), util::mmss(b)),
            _ => String::new(),
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Topic {
    pub title: String,
    pub section_ids: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Guide {
    pub v: u32,
    pub key: String,
    pub source: String,
    pub title: String,
    pub overview: String,
    pub topics: Vec<Topic>,
    pub suggested_questions: Vec<String>,
    pub sections: Vec<Section>,
    #[serde(default)]
    pub issues: Vec<String>,
    /// corrective retries of the guide call (0/1); per-section retries are in `sections`
    #[serde(default)]
    pub retries: usize,
    pub model: String,
}

/// Consecutive, balanced ranges: 4–6 chunks each below LONG chunks (n ≥ 8), else 10–15.
pub fn split(n: usize) -> Vec<Range<usize>> {
    let k = if n < 8 { n.min(1) } else { n.div_ceil(if n < LONG { SEC_SHORT } else { SEC_MAX }) };
    (0..k).map(|i| i * n / k..(i + 1) * n / k).collect()
}

pub fn cache_key(chunks: &[Chunk]) -> String {
    let hs: Vec<String> = chunks.iter().map(|c| map::key(&c.text)).collect();
    map::key(&format!("{}/{GUIDE_V}/{}", map::MAP_V, hs.join(",")))
}

fn path(st: &Store, nb: &Notebook, src: &str, ext: &str) -> PathBuf {
    st.dir(&nb.id).join("guide").join(format!("{src}.{ext}"))
}

/// Cached guide of a source if it matches the current chunks.
pub fn load(st: &Store, nb: &Notebook, s: &Source) -> Option<Guide> {
    let g: Guide = serde_json::from_str(&std::fs::read_to_string(path(st, nb, &s.id, "json")).ok()?).ok()?;
    let chunks = st.source_chunks(nb, s).ok()?;
    (g.v == GUIDE_V && g.key == cache_key(&chunks)).then_some(g)
}

// ---------- validation (docs/m2/eval-reduce.md) ----------

/// "1.200" → "1200", "3,50" → "3.5", "12" → "12".
pub fn num_norm(t: &str) -> String {
    let g: Vec<&str> = t.split('.').collect();
    let thousands = g.len() > 1 && (1..=3).contains(&g[0].len()) && g[1..].iter().all(|x| x.len() == 3) && !t.contains(',');
    let s = if thousands { t.replace('.', "") } else { t.replace(',', ".") };
    if s.contains('.') { s.trim_end_matches('0').trim_end_matches('.').to_string() } else { s }
}

/// All `\d+([.,]\d+)*` tokens, normalized.
pub fn numbers(s: &str) -> Vec<String> {
    let cs: Vec<char> = s.chars().collect();
    let (mut v, mut cur) = (vec![], String::new());
    for (i, &c) in cs.iter().enumerate() {
        if c.is_ascii_digit() || ((c == '.' || c == ',') && !cur.is_empty() && cs.get(i + 1).is_some_and(char::is_ascii_digit)) {
            cur.push(c);
        } else if !cur.is_empty() {
            v.push(num_norm(&std::mem::take(&mut cur)));
        }
    }
    if !cur.is_empty() {
        v.push(num_norm(&cur));
    }
    v
}

fn words(s: &str) -> usize {
    s.split_whitespace().filter(|w| w.chars().any(char::is_alphabetic)).count()
}

/// "[s1#3 12:34]" → "s1#3"
fn norm_id(s: &str) -> String {
    s.trim().trim_matches(|c| c == '[' || c == ']').split_whitespace().next().unwrap_or("").to_string()
}

/// R2 + R3 for one key fact; Err = detail.
pub fn fact_check(f: &KeyFact, sec: &[&Chunk]) -> Result<(), String> {
    let c = sec.iter().find(|c| c.id == f.chunk_id).ok_or_else(|| format!("R2 chunk_id {} not in section", f.chunk_id))?;
    let have = numbers(&c.text);
    match numbers(&f.text).into_iter().find(|n| !have.contains(n)) {
        Some(n) => Err(format!("R3 '{n}' not in {}", c.id)),
        None => Ok(()),
    }
}

/// R4 haystack: speaker labels + text of the section's chunks.
pub fn haystack(sec: &[&Chunk]) -> String {
    sec.iter().map(|c| format!("{}\n{}", c.speaker.as_deref().unwrap_or(""), c.text)).collect::<Vec<_>>().join("\n")
}

pub fn speaker_ok(s: &str, hay: &str) -> bool {
    let s = s.trim();
    !s.is_empty() && s != ">>" && hay.contains(s)
}

/// R5 bounds (on the kept facts).
pub fn bounds(title: &str, summary: &str, facts: usize) -> Vec<String> {
    let mut v = vec![];
    let tw = words(title);
    if !(3..=7).contains(&tw) {
        v.push(format!("R5 title {tw} words, want 3-7"));
    }
    let sw = words(summary);
    if sw > 80 {
        v.push(format!("R5 summary {sw} words, max 80"));
    }
    if !(3..=6).contains(&facts) {
        v.push(format!("R5 {facts} key_facts, want 3-6"));
    }
    v
}

/// G1–G6 on a guide against its section ids (in source order).
pub fn guide_checks(ids: &[String], g: &Guide) -> Vec<(&'static str, bool, String)> {
    let pos = |s: &String| ids.iter().position(|x| x == s);
    let uncovered: Vec<&String> = ids.iter().filter(|s| !g.topics.iter().any(|t| t.section_ids.contains(s))).collect();
    let unknown: Vec<&String> = g.topics.iter().flat_map(|t| &t.section_ids).filter(|s| pos(s).is_none()).collect();
    let unordered: Vec<&str> = g
        .topics
        .iter()
        .filter(|t| t.section_ids.iter().filter_map(pos).collect::<Vec<_>>().windows(2).any(|w| w[0] > w[1]))
        .map(|t| t.title.as_str())
        .collect();
    let q = &g.suggested_questions;
    let qset: std::collections::HashSet<&str> = q.iter().map(|x| x.trim()).collect();
    let q_ok = q.len() == 5 && qset.len() == 5 && !qset.contains("");
    let ow = words(&g.overview);
    vec![
        ("G1", uncovered.is_empty(), format!("uncovered={uncovered:?}")),
        ("G2", unknown.is_empty(), format!("unknown={unknown:?}")),
        ("G3", (4..=8).contains(&g.topics.len()), format!("{} topics, want 4-8", g.topics.len())),
        ("G4", q_ok, format!("{} questions ({} unique), want 5", q.len(), qset.len())),
        ("G5", unordered.is_empty(), format!("out of order: {unordered:?}")),
        ("G6", ow <= 150, format!("overview {ow} words")),
    ]
}

/// Drops unknown ids, sorts/dedups ids in source order, drops empty topics, keeps ≤ 5 unique questions.
fn repair(ids: &[String], g: &mut Guide) {
    for t in &mut g.topics {
        let mut p: Vec<usize> = t.section_ids.iter().filter_map(|s| ids.iter().position(|x| x == s)).collect();
        p.sort();
        p.dedup();
        t.section_ids = p.into_iter().map(|i| ids[i].clone()).collect();
    }
    g.topics.retain(|t| !t.section_ids.is_empty());
    let mut seen = std::collections::HashSet::new();
    g.suggested_questions.retain(|q| !q.trim().is_empty() && seen.insert(q.trim().to_string()));
    g.suggested_questions.truncate(5);
}

// ---------- prompts ----------

const SYS_REDUCE: &str = "Du fasst einen Abschnitt einer Quelle zusammen. \
EINGABE: aufeinanderfolgende Chunks; je Chunk eine Kopfzeile mit Sprechern, dann je Fakt eine Zeile [chunk_id mm:ss] text | Zitat: quote. \
text ist eine deutsche Kurzaussage, quote ein wörtliches Zitat in Originalsprache. \
REGELN: Nur Inhalte aus der Eingabe, nichts ergänzen. \
speakers: nur Namen, die in einer Sprecherzeile, einem Zitat oder Text wörtlich vorkommen, genau so geschrieben. „>>“ ist ein Sprecherwechsel, kein Name. Unklar: weglassen. \
title: drei bis sieben Wörter, Deutsch. summary: höchstens achtzig Wörter, Deutsch, Hauptaussage plus wichtigste Zahlen und Namen. \
key_facts: drei bis sechs tragende Fakten; text aus einem Eingabe-Fakt, chunk_id genau die ID aus der eckigen Klammer dieses Fakts (ohne Zeit). \
Zahlen unverändert übernehmen, keine Zeitangaben (mm:ss) in text. Nur JSON.";

const SYS_GUIDE: &str = "Du erstellst den Quellenführer einer Quelle aus ihren Abschnitten (in Reihenfolge, je id, title, summary, key_facts). \
overview: höchstens hundertfünfzig Wörter, Deutsch: worum es geht, für wen, was man erfährt. Keine Wertung, keine Floskeln. \
topics: vier bis acht Themen mit deutschem Titel; jeder Abschnitt gehört zu mindestens einem Thema; section_ids nur aus der Eingabe, in Reihenfolge; ein Abschnitt darf in mehreren Themen stehen. \
suggested_questions: genau fünf verschiedene Fragen auf Deutsch, wie ein Nutzer sie stellen würde, aus den Abschnitten beantwortbar. \
Zahlen und Namen nur aus key_facts oder summary. Nur JSON.";

fn reduce_schema() -> Value {
    json!({"type": "object", "properties": {
        "title": {"type": "string"},
        "summary": {"type": "string"},
        "key_facts": {"type": "array", "minItems": 3, "maxItems": 6, "items": {"type": "object",
            "properties": {"text": {"type": "string"}, "chunk_id": {"type": "string"}}, "required": ["text", "chunk_id"], "additionalProperties": false}},
        "speakers": {"type": "array", "items": {"type": "string"}}
    }, "required": ["title", "summary", "key_facts", "speakers"], "additionalProperties": false})
}

fn guide_schema() -> Value {
    json!({"type": "object", "properties": {
        "overview": {"type": "string"},
        "topics": {"type": "array", "minItems": 4, "maxItems": 8, "items": {"type": "object",
            "properties": {"title": {"type": "string"}, "section_ids": {"type": "array", "items": {"type": "string"}}},
            "required": ["title", "section_ids"], "additionalProperties": false}},
        "suggested_questions": {"type": "array", "minItems": 5, "maxItems": 5, "items": {"type": "string"}}
    }, "required": ["overview", "topics", "suggested_questions"], "additionalProperties": false})
}

fn clip_words(s: &str, n: usize) -> String {
    let w: Vec<&str> = s.split_whitespace().collect();
    if w.len() <= n { s.to_string() } else { format!("{} …", w[..n].join(" ")) }
}

/// Reduce input: facts + quotes per chunk, shrunk to IN_CHARS (shorter quotes, then ≤ 3 facts per chunk).
pub fn reduce_input(sec: &[&Chunk], maps: &[&map::Map]) -> String {
    let render = |qw: usize, nf: usize| {
        let mut s = String::new();
        for (c, m) in sec.iter().zip(maps) {
            let at = c.at().map(|a| format!(" {a}")).unwrap_or_default();
            s.push_str(&format!("Chunk {}{at} – Sprecher: {}\n", c.id, c.speaker.as_deref().unwrap_or("unbekannt")));
            for f in m.facts.iter().take(nf) {
                s.push_str(&format!("[{}{at}] {} | Zitat: {}\n", c.id, f.text, clip_words(&f.quote, qw)));
            }
        }
        s
    };
    [(99, 99), (12, 99), (12, 4), (8, 3), (6, 2)]
        .iter()
        .map(|&(q, n)| render(q, n))
        .find(|s| s.len() <= IN_CHARS)
        .unwrap_or_else(|| util::clip(&render(6, 2), IN_CHARS))
}

pub fn guide_input(title: &str, secs: &[Section]) -> String {
    let render = |full: bool, nf: usize| {
        let mut s = format!("Quelle: {title}\n\n");
        for x in secs {
            s.push_str(&format!("{}{} – {}\n", x.id, x.span(), x.title));
            if full {
                s.push_str(&format!("summary: {}\n", x.summary));
            }
            for f in x.key_facts.iter().take(nf) {
                s.push_str(&format!("- {}\n", f.text));
            }
            s.push('\n');
        }
        s
    };
    // > 25 sections: title + 2 key facts (docs/m2/prompts.md)
    let full = secs.len() <= 25;
    [render(full, 6), render(false, 2), render(false, 0)].into_iter().find(|s| s.len() <= IN_CHARS).unwrap_or_else(|| util::clip(&render(false, 0), IN_CHARS))
}

// ---------- LLM stages ----------

fn strs(v: &Value) -> Vec<String> {
    v.as_array().into_iter().flatten().filter_map(|x| x.as_str().map(|s| s.trim().to_string())).filter(|s| !s.is_empty()).collect()
}

/// One section: LLM reduce, validation, one corrective retry, invalid facts/speakers dropped.
fn reduce(cfg: &crate::Config, nb: &Notebook, id: &str, sec: &[&Chunk], maps: &[&map::Map]) -> Result<Section> {
    let hay = haystack(sec);
    let mut msgs = vec![llm::sys(SYS_REDUCE), llm::user(format!("{}\nAntworte als JSON.", reduce_input(sec, maps)))];
    let mut best: Option<(usize, Section)> = None;
    let mut tries = 0;
    for attempt in 0..2 {
        tries = attempt;
        let v = llm::json(cfg, nb.backend, &msgs, &reduce_schema())?;
        let facts: Vec<KeyFact> = v["key_facts"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|f| KeyFact { text: f["text"].as_str().unwrap_or("").trim().into(), chunk_id: norm_id(f["chunk_id"].as_str().unwrap_or("")) })
            .filter(|f| !f.text.is_empty())
            .collect();
        let (title, summary) = (v["title"].as_str().unwrap_or("").trim().to_string(), v["summary"].as_str().unwrap_or("").trim().to_string());
        let mut bad: Vec<String> = facts.iter().enumerate().filter_map(|(i, f)| fact_check(f, sec).err().map(|e| format!("key_facts[{i}]: {e}"))).collect();
        let sp = strs(&v["speakers"]);
        bad.extend(sp.iter().filter(|s| !speaker_ok(s, &hay)).map(|s| format!("speaker '{s}' kommt nicht wörtlich vor")));
        let keep: Vec<KeyFact> = facts.iter().filter(|f| fact_check(f, sec).is_ok()).cloned().collect();
        let issues = bounds(&title, &summary, keep.len());
        let n = bad.len() + issues.len();
        let s = Section {
            id: id.into(),
            chunk_ids: sec.iter().map(|c| c.id.clone()).collect(),
            t0: sec.iter().filter_map(|c| c.t0).reduce(f64::min),
            t1: sec.iter().filter_map(|c| c.t1).reduce(f64::max),
            title,
            summary,
            dropped: facts.len() - keep.len() + sp.iter().filter(|s| !speaker_ok(s, &hay)).count(),
            key_facts: keep,
            speakers: sp.into_iter().filter(|s| speaker_ok(s, &hay)).collect(),
            issues,
            retries: attempt,
        };
        if best.as_ref().is_none_or(|b| n < b.0) {
            best = Some((n, s));
        }
        if n == 0 || attempt == 1 {
            break;
        }
        let all: Vec<String> = bad.into_iter().chain(best.as_ref().unwrap().1.issues.clone()).collect();
        msgs.push(llm::asst(v.to_string()));
        msgs.push(llm::user(format!(
            "Korrigiere diese Fehler und antworte erneut als vollständiges JSON:\n- {}\nchunk_id nur aus den eckigen Klammern der Eingabe, Zahlen nur wie im Zitat.",
            all.join("\n- ")
        )));
    }
    let mut s = best.unwrap().1;
    s.retries = tries;
    Ok(s)
}

fn source_guide(cfg: &crate::Config, nb: &Notebook, title: &str, secs: &[Section]) -> Result<(String, Vec<Topic>, Vec<String>, Vec<String>, usize)> {
    let ids: Vec<String> = secs.iter().map(|s| s.id.clone()).collect();
    let mut msgs = vec![llm::sys(SYS_GUIDE), llm::user(format!("{}Antworte als JSON.", guide_input(title, secs)))];
    let mut best: Option<(usize, Guide)> = None;
    let mut tries = 0;
    for attempt in 0..2 {
        tries = attempt;
        let v = llm::json(cfg, nb.backend, &msgs, &guide_schema())?;
        let mut g = Guide {
            v: GUIDE_V,
            key: String::new(),
            source: String::new(),
            title: title.into(),
            overview: v["overview"].as_str().unwrap_or("").trim().into(),
            topics: v["topics"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|t| Topic {
                    title: t["title"].as_str().unwrap_or("").trim().into(),
                    section_ids: strs(&t["section_ids"]).iter().map(|s| norm_id(s)).collect(),
                })
                .collect(),
            suggested_questions: strs(&v["suggested_questions"]),
            sections: vec![],
            issues: vec![],
            retries: attempt,
            model: String::new(),
        };
        let fails: Vec<String> = guide_checks(&ids, &g).into_iter().filter(|c| !c.1).map(|c| format!("{} {}", c.0, c.2)).collect();
        repair(&ids, &mut g);
        let n = fails.len();
        if best.as_ref().is_none_or(|b| n < b.0) {
            best = Some((n, g));
        }
        if n == 0 || attempt == 1 {
            break;
        }
        msgs.push(llm::asst(v.to_string()));
        msgs.push(llm::user(format!(
            "Korrigiere diese Fehler und antworte erneut als vollständiges JSON (gültige Abschnitts-IDs: {}):\n- {}",
            ids.join(", "),
            fails.join("\n- ")
        )));
    }
    let g = best.unwrap().1;
    let issues = guide_checks(&ids, &g).into_iter().filter(|c| !c.1).map(|c| format!("{} {}", c.0, c.2)).collect();
    Ok((g.overview, g.topics, g.suggested_questions, issues, tries))
}

fn secs_since(t: std::time::Instant) -> f64 {
    (t.elapsed().as_secs_f64() * 10.0).round() / 10.0
}

pub fn run(st: &Store, nb: &Notebook, source: Option<&str>, prog: Prog) -> Result<Value> {
    run_with(st, nb, source, prog, &std::sync::atomic::AtomicBool::new(false))
}

/// Background job like `map::run_with`: yields to interactive calls, cancel between sections (partial file kept).
pub fn run_with(st: &Store, nb: &Notebook, source: Option<&str>, prog: Prog, cancel: &std::sync::atomic::AtomicBool) -> Result<Value> {
    let _batch = funkhaus_sys::gpu::Batch::on();
    let lf = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(st.dir(&nb.id).join(".guide.lock"))?;
    if lf.try_lock().is_err() {
        bail!("guide läuft bereits für dieses Notebook");
    }
    let srcs: Vec<&Source> = match source {
        Some(k) => {
            vec![nb.sources.iter().find(|s| s.id == k || s.title.to_lowercase() == k.to_lowercase()).with_context(|| format!("Quelle nicht gefunden: {k}"))?]
        }
        None => nb.sources.iter().filter(|s| s.enabled).collect(),
    };
    // all chunks must be mapped before any LLM work
    let mut work = vec![];
    for s in srcs {
        let chunks = st.source_chunks(nb, s)?;
        let maps: Vec<Option<map::Map>> = chunks.iter().map(|c| map::load(st, nb, c)).collect();
        let miss = maps.iter().filter(|m| m.is_none()).count();
        if miss > 0 {
            bail!("Quelle {} ({}): {miss}/{} Chunks nicht kartiert – erst kartieren (funkhaus map {})", s.id, s.title, chunks.len(), nb.name);
        }
        work.push((s, chunks, maps.into_iter().flatten().collect::<Vec<_>>()));
    }
    let mut cfg = st.cfg.clone();
    cfg.llm.temperature = 0.2;
    let t0 = std::time::Instant::now();
    let mut out = vec![];
    for (s, chunks, maps) in work {
        let key = cache_key(&chunks);
        if load(st, nb, s).is_some() {
            prog(&format!("guide {}: aus Cache", s.id));
            out.push(json!({"source": s.id, "cached": true}));
            continue;
        }
        let ts = std::time::Instant::now();
        // finished sections survive an abort: partial file keyed like the guide
        let part = path(st, nb, &s.id, "partial.json");
        std::fs::create_dir_all(part.parent().unwrap())?;
        let mut secs: Vec<Section> = std::fs::read_to_string(&part)
            .ok()
            .and_then(|x| serde_json::from_str::<Value>(&x).ok())
            .filter(|v| v["key"] == key.as_str())
            .and_then(|v| serde_json::from_value(v["sections"].clone()).ok())
            .unwrap_or_default();
        let rs = split(chunks.len());
        let mut per = vec![];
        cfg.llm.max_tokens = 800;
        for (k, r) in rs.iter().enumerate().skip(secs.len()) {
            let id = format!("a{}", k + 1);
            prog(&format!("reduce {} {id} ({}/{}, Chunks {}–{})", s.id, k + 1, rs.len(), chunks[r.start].id, chunks[r.end - 1].id));
            if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                bail!("abgebrochen – fertige Abschnitte bleiben erhalten");
            }
            funkhaus_sys::gpu::gpu_wait(prog);
            let tr = std::time::Instant::now();
            let sec: Vec<&Chunk> = chunks[r.clone()].iter().collect();
            let ms: Vec<&map::Map> = maps[r.clone()].iter().collect();
            let x = reduce(&cfg, nb, &id, &sec, &ms)?;
            let dt = secs_since(tr);
            prog(&format!(
                "  {dt} s, {} Fakten, {} verworfen{}",
                x.key_facts.len(),
                x.dropped,
                if x.issues.is_empty() { String::new() } else { format!(", offen: {}", x.issues.join("; ")) }
            ));
            per.push(json!({"section": id, "secs": dt, "key_facts": x.key_facts.len(), "dropped": x.dropped, "retries": x.retries, "issues": x.issues}));
            secs.push(x);
            std::fs::write(&part, serde_json::to_string(&json!({"key": key, "sections": secs}))?)?;
        }
        prog(&format!("guide {} ({} Abschnitte)", s.id, secs.len()));
        funkhaus_sys::gpu::gpu_wait(prog);
        cfg.llm.max_tokens = 900;
        let tg = std::time::Instant::now();
        let (overview, topics, qs, issues, retries) = source_guide(&cfg, nb, &s.title, &secs)?;
        let g = Guide {
            v: GUIDE_V,
            key,
            source: s.id.clone(),
            title: s.title.clone(),
            overview,
            topics,
            suggested_questions: qs,
            sections: secs,
            issues,
            retries,
            model: cfg.llm.model.clone(),
        };
        std::fs::write(path(st, nb, &s.id, "json"), serde_json::to_string_pretty(&g)?)?;
        let _ = std::fs::remove_file(&part);
        out.push(json!({
            "source": s.id, "chunks": chunks.len(), "sections": g.sections.len(), "secs": secs_since(ts), "guide_secs": secs_since(tg),
            "key_facts": g.sections.iter().map(|x| x.key_facts.len()).sum::<usize>(), "dropped": g.sections.iter().map(|x| x.dropped).sum::<usize>(),
            "topics": g.topics.len(), "guide_retries": g.retries, "issues": g.issues, "per_section": per
        }));
    }
    Ok(json!({"sources": out, "secs": secs_since(t0), "model": cfg.llm.model}))
}

// ---------- LLM-free eval ----------

#[derive(Serialize, Debug, Clone)]
pub struct Check {
    pub stage: &'static str,
    pub check: &'static str,
    pub pass: bool,
    pub detail: String,
}

/// R1–R5 + G1–G6 for one guide against the current chunks of its source.
pub fn checks(g: &Guide, chunks: &[Chunk]) -> Vec<Check> {
    let mut v = vec![];
    let mut push = |stage, check, pass, detail: String| v.push(Check { stage, check, pass, detail: format!("{} {detail}", g.source) });
    let all: Vec<&String> = g.sections.iter().flat_map(|s| &s.chunk_ids).collect();
    let uncovered: Vec<&String> = chunks.iter().map(|c| &c.id).filter(|id| !all.contains(id)).collect();
    let mut dup: Vec<&String> = all.iter().copied().filter(|id| all.iter().filter(|x| x == &id).count() > 1).collect();
    dup.dedup();
    let unknown: Vec<&String> = all.iter().copied().filter(|id| !chunks.iter().any(|c| &c.id == *id)).collect();
    push("reduce", "R1", uncovered.is_empty() && dup.is_empty() && unknown.is_empty(), format!("uncovered={uncovered:?} dup={dup:?} unknown={unknown:?}"));
    for s in &g.sections {
        let sec: Vec<&Chunk> = chunks.iter().filter(|c| s.chunk_ids.contains(&c.id)).collect();
        let errs: Vec<String> = s.key_facts.iter().enumerate().filter_map(|(i, f)| fact_check(f, &sec).err().map(|e| format!("fact[{i}] {e}"))).collect();
        let (r2, r3): (Vec<&String>, Vec<&String>) = errs.iter().partition(|e| e.contains("R2"));
        push("reduce", "R2", r2.is_empty(), format!("{} {}/{} facts cite section chunks {r2:?}", s.id, s.key_facts.len() - r2.len(), s.key_facts.len()));
        push("reduce", "R3", r3.is_empty(), format!("{} numbers {r3:?} (dropped before save: {})", s.id, s.dropped));
        let hay = haystack(&sec);
        let bad: Vec<&String> = s.speakers.iter().filter(|x| !speaker_ok(x, &hay)).collect();
        push("reduce", "R4", bad.is_empty(), format!("{} speakers {:?} bad={bad:?}", s.id, s.speakers));
        let b = bounds(&s.title, &s.summary, s.key_facts.len());
        push("reduce", "R5", b.is_empty(), format!("{} title {} words, facts {} {b:?}", s.id, words(&s.title), s.key_facts.len()));
    }
    let ids: Vec<String> = g.sections.iter().map(|s| s.id.clone()).collect();
    for (c, pass, d) in guide_checks(&ids, g) {
        push("guide", c, pass, d);
    }
    v
}

/// Eval of all enabled sources; a missing/stale guide is one failing G0 line.
pub fn eval(st: &Store, nb: &Notebook) -> Result<Vec<Check>> {
    let mut v = vec![];
    for s in nb.sources.iter().filter(|s| s.enabled) {
        match load(st, nb, s) {
            Some(g) => v.extend(checks(&g, &st.source_chunks(nb, s)?)),
            None => v.push(Check { stage: "guide", check: "G0", pass: false, detail: format!("{} kein aktueller Quellenführer (funkhaus guide)", s.id) }),
        }
    }
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ch(id: &str, spk: &str, text: &str) -> Chunk {
        Chunk { id: id.into(), src: "s1".into(), title: "T".into(), text: text.into(), speaker: Some(spk.into()), t0: Some(60.0), t1: Some(90.0) }
    }
    fn kf(t: &str, c: &str) -> KeyFact {
        KeyFact { text: t.into(), chunk_id: c.into() }
    }

    #[test]
    fn sections_split() {
        assert!(split(0).is_empty());
        assert_eq!(split(7), vec![0..7]);
        assert_eq!(split(6), vec![0..6]);
        assert_eq!(split(20), vec![0..5, 5..10, 10..15, 15..20]);
        assert_eq!(split(25).len(), 5);
        for n in 8..LONG {
            assert!(split(n).iter().all(|x| (4..=6).contains(&x.len())), "{n}: {:?}", split(n));
        }
        for n in LONG..200 {
            let r = split(n);
            assert_eq!((r[0].start, r.last().unwrap().end), (0, n));
            assert!(r.windows(2).all(|w| w[0].end == w[1].start));
            assert!(r.iter().all(|x| (10..=15).contains(&x.len())), "{n}: {r:?}");
        }
    }

    #[test]
    fn numbers_norm() {
        assert_eq!(numbers("1.200 Euro, 3,5 t, 3,50 m, 12 Wochen, am 31. Mai"), vec!["1200", "3.5", "3.5", "12", "31"]);
        assert_eq!(num_norm("2.5"), "2.5");
        assert_eq!(num_norm("10"), "10");
    }

    #[test]
    fn reduce_rules() {
        // docs/m2/eval-reduce.md example 1/2
        let c = [
            ch("c01", ">>", "Der Test lief 12 Wochen."),
            ch("c02", "Anna", "Wir hatten 3,5 Tonnen Ausbeute."),
            ch("c06", "Anna", "Der Preis stieg auf 1.200 Euro."),
        ];
        let sec: Vec<&Chunk> = c.iter().collect();
        assert!(fact_check(&kf("Testlauf dauerte 12 Wochen", "c01"), &sec).is_ok());
        assert!(fact_check(&kf("Ausbeute 3.5 Tonnen", "c02"), &sec).is_ok());
        assert!(fact_check(&kf("Preis 1200 Euro", "c06"), &sec).is_ok());
        assert!(fact_check(&kf("Anstieg um 2,5 Prozent", "c06"), &sec).unwrap_err().starts_with("R3 '2.5'"));
        assert!(fact_check(&kf("Kosten", "c05"), &sec).unwrap_err().starts_with("R2"));
        let hay = haystack(&sec);
        assert!(speaker_ok("Anna", &hay) && !speaker_ok("Max", &hay) && !speaker_ok(">>", &hay) && !speaker_ok(" ", &hay));
        assert!(bounds("Kleinholm Testlauf Ausbeute", "kurz", 3).is_empty());
        assert_eq!(bounds("Eins zwei", &"w ".repeat(81), 2).len(), 3);
        assert_eq!(norm_id("[s1#3 12:34]"), "s1#3");
    }

    #[test]
    fn guide_rules() {
        // docs/m2/eval-reduce.md example 3
        let ids: Vec<String> = (1..=6).map(|i| format!("s{i}")).collect();
        let t = |n: &str, s: &[&str]| Topic { title: n.into(), section_ids: s.iter().map(|x| x.to_string()).collect() };
        let mut g = Guide {
            v: GUIDE_V,
            key: String::new(),
            source: "x".into(),
            title: "T".into(),
            overview: "w ".repeat(42),
            topics: vec![t("Testlauf", &["s1", "s2"]), t("Kosten", &["s4", "s2", "s9"]), t("Fazit", &["s5"])],
            suggested_questions: vec!["a".into(), "b".into(), "c".into(), "d".into()],
            sections: vec![],
            issues: vec![],
            retries: 0,
            model: String::new(),
        };
        let r: Vec<(&str, bool)> = guide_checks(&ids, &g).into_iter().map(|c| (c.0, c.1)).collect();
        assert_eq!(r, vec![("G1", false), ("G2", false), ("G3", false), ("G4", false), ("G5", false), ("G6", true)]);
        repair(&ids, &mut g);
        assert_eq!(g.topics[1].section_ids, vec!["s2", "s4"]);
        let r: Vec<bool> = guide_checks(&ids, &g).into_iter().map(|c| c.1).collect();
        assert_eq!(r, vec![false, true, false, false, true, true]);
    }

    #[test]
    fn cache_key_tracks_text() {
        let a = vec![ch("s1#0", "A", "eins"), ch("s1#1", "A", "zwei")];
        let mut b = a.clone();
        assert_eq!(cache_key(&a), cache_key(&b));
        b[1].id = "s1#9".into(); // ids don't matter, text does
        assert_eq!(cache_key(&a), cache_key(&b));
        b[1].text = "drei".into();
        assert_ne!(cache_key(&a), cache_key(&b));
        b.swap(0, 1);
        b[0].text = "zwei".into();
        b[1].text = "eins".into();
        assert_ne!(cache_key(&a), cache_key(&b)); // order matters
    }

    #[test]
    fn inputs_fit_budget() {
        let c: Vec<Chunk> = (0..15).map(|i| ch(&format!("s1#{i}"), "Mara", "x")).collect();
        let f = map::Fact { text: "Ein langer deutscher Fakt mit vielen Wörtern ".repeat(3), quote: "an original quote with many many words ".repeat(5) };
        let m = map::Map {
            v: map::MAP_V,
            chunk: String::new(),
            hash: String::new(),
            context: String::new(),
            summary: String::new(),
            facts: vec![f; 6],
            questions: vec![],
            dropped: 0,
            model: String::new(),
        };
        let ms: Vec<&map::Map> = c.iter().map(|_| &m).collect();
        let s = reduce_input(&c.iter().collect::<Vec<_>>(), &ms);
        assert!(s.len() <= IN_CHARS && s.contains("[s1#14 1:00]") && s.contains("Sprecher: Mara"), "{}", s.len());
    }
}
