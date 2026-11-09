//! Per-chunk map (M1.3): one LLM call per chunk → German context, summary, quoted facts and questions,
//! cached under `map/<source>/<hash of chunk text>.json` so re-chunking only maps changed text.
use crate::{Notebook, Prog, Store, llm, retrieval::Chunk};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::PathBuf;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Fact {
    pub text: String,
    pub quote: String,
}

/// Bump on any prompt/schema/gate change: older maps are then ignored and re-mapped.
pub const MAP_V: u32 = 2;

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Map {
    #[serde(default)]
    pub v: u32,
    pub chunk: String,
    pub hash: String,
    pub context: String,
    pub summary: String,
    pub facts: Vec<Fact>,
    pub questions: Vec<String>,
    pub dropped: usize,
    pub model: String,
}

/// FNV-1a 64 of the chunk text: stable across runs and Rust versions.
pub fn key(text: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in text.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{h:016x}")
}

pub fn path(st: &Store, nb: &Notebook, c: &Chunk) -> PathBuf {
    st.dir(&nb.id).join("map").join(&c.src).join(format!("{}.json", key(&c.text)))
}

/// Cached map for a chunk (None if missing or stale).
pub fn load(st: &Store, nb: &Notebook, c: &Chunk) -> Option<Map> {
    let m: Map = serde_json::from_str(&std::fs::read_to_string(path(st, nb, c)).ok()?).ok()?;
    (m.hash == key(&c.text) && m.v == MAP_V).then_some(m)
}

/// Whitespace/case/quote-mark normalization for the quote gate.
pub fn norm(s: &str) -> String {
    let s: String = s
        .chars()
        .map(|c| match c {
            '’' | '‘' | '`' | '´' => '\'',
            '“' | '”' | '„' | '«' | '»' => '"',
            '–' | '—' => '-',
            c => c,
        })
        .collect();
    let s = s.trim().trim_matches(|c: char| c == '"' || c == '\'' || c == '…' || c == '.').trim_end_matches("...");
    s.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
}

/// Digit runs with thousands/decimal separators removed ("1.250" → "1250", "2,5" → "25").
fn numbers(s: &str) -> Vec<String> {
    let mut v = vec![];
    let mut cur = String::new();
    let cs: Vec<char> = s.chars().collect();
    for (i, &c) in cs.iter().enumerate() {
        if c.is_ascii_digit() {
            cur.push(c);
        } else if (c == '.' || c == ',') && !cur.is_empty() && cs.get(i + 1).is_some_and(|n| n.is_ascii_digit()) {
            continue;
        } else if !cur.is_empty() {
            v.push(std::mem::take(&mut cur));
        }
    }
    if !cur.is_empty() {
        v.push(cur);
    }
    v
}

pub const MIN_QUOTE_WORDS: usize = 5;

/// Keeps facts whose quote (≥ 5 words) occurs in the chunk text and whose numbers all occur in the
/// chunk; returns (kept, dropped).
pub fn gate(facts: Vec<Fact>, text: &str) -> (Vec<Fact>, usize) {
    let t = norm(text);
    let nums = numbers(text);
    let n = facts.len();
    let kept: Vec<Fact> = facts
        .into_iter()
        .filter(|f| f.quote.split_whitespace().count() >= MIN_QUOTE_WORDS && t.contains(&norm(&f.quote)))
        .filter(|f| numbers(&f.text).iter().all(|x| nums.contains(x)))
        .collect();
    let d = n - kept.len();
    (kept, d)
}

pub const SYSTEM: &str = "Du kartierst einen Ausschnitt aus einer Quelle für eine Suche. \
Schreibe context, summary, facts.text und questions IMMER auf Deutsch – auch wenn die Quelle englisch oder in einer anderen Sprache ist. \
Nur facts.quote bleibt in der Originalsprache: ein wörtliches Zitat (fünf bis fünfundzwanzig Wörter), Zeichen für Zeichen aus dem Ausschnitt kopiert, ohne Auslassungen. \
context: ein bis zwei Sätze (fünfzig bis hundert Wörter höchstens), die den Ausschnitt in der Quelle verorten (wer, was, wo, welcher Abschnitt). \
summary: höchstens sechzig Wörter. facts: bis zu sechs konkrete, überprüfbare Aussagen (Zahlen, Namen, Orte, Ereignisse), jede mit Zitat. \
questions: genau drei Fragen auf Deutsch, die dieser Ausschnitt beantwortet, so formuliert, wie ein Nutzer sie stellen würde. \
Nichts erfinden, nur was im Ausschnitt steht.";

fn schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "context": {"type": "string"},
            "summary": {"type": "string"},
            "facts": {"type": "array", "maxItems": 6, "items": {"type": "object",
                "properties": {"text": {"type": "string"}, "quote": {"type": "string"}},
                "required": ["text", "quote"], "additionalProperties": false}},
            "questions": {"type": "array", "minItems": 3, "maxItems": 3, "items": {"type": "string"}}
        },
        "required": ["context", "summary", "facts", "questions"],
        "additionalProperties": false
    })
}

pub fn prompt(title: &str, header: &str, prev: Option<&str>, c: &Chunk) -> Vec<llm::Msg> {
    let at = c.at().map(|a| format!(", ab {a}")).unwrap_or_default();
    vec![
        llm::sys(SYSTEM),
        llm::user(format!(
            "Quelle: {title}\nKopf der Quelle:\n{header}\n\nVorheriger Abschnitt (Zusammenfassung): {}\n\nAusschnitt ({}{at}):\n{}\n\nAntworte als JSON.",
            prev.unwrap_or("– (Anfang der Quelle)"),
            c.id,
            c.text
        )),
    ]
}

/// First lines of a source (title, channel, date …), at most ~400 chars.
fn header(text: &str) -> String {
    let mut h = String::new();
    for l in text.lines().take_while(|l| !l.starts_with("## ")) {
        if h.len() + l.len() > 400 {
            break;
        }
        h.push_str(l);
        h.push('\n');
    }
    h.trim().to_string()
}

fn restart_gemma(cmd: &str, prog: Prog) -> Result<()> {
    let bin = crate::util::which(cmd).with_context(|| format!("gemma_serve nicht gefunden: {cmd}"))?;
    prog("Gemma abgestürzt – starte gemma-serve neu");
    let _ = std::process::Command::new(&bin).arg("stop").output();
    std::thread::sleep(std::time::Duration::from_secs(3));
    std::process::Command::new(&bin).arg("start").stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn()?;
    for _ in 0..90 {
        std::thread::sleep(std::time::Duration::from_secs(2));
        if llm::models(&crate::config::Llm::default()).is_ok() {
            return Ok(());
        }
    }
    bail!("gemma-serve kam nicht wieder hoch")
}

/// (mapped, total) chunks per enabled source, from the cache (no LLM).
pub fn status(st: &Store, nb: &Notebook) -> Vec<(String, usize, usize)> {
    nb.sources
        .iter()
        .map(|s| {
            let cs = st.source_chunks(nb, s).unwrap_or_default();
            (s.id.clone(), cs.iter().filter(|c| load(st, nb, c).is_some()).count(), cs.len())
        })
        .collect()
}

/// "12/100 Abschnitte kartiert, ca. 45 min" (EMA of seconds per mapped chunk × remaining).
pub fn progress(done: usize, total: usize, rate: Option<f64>) -> String {
    match rate {
        Some(r) if done < total => format!("{done}/{total} Abschnitte kartiert, ca. {} min", ((total - done) as f64 * r / 60.0).ceil().max(1.0)),
        _ => format!("{done}/{total} Abschnitte kartiert"),
    }
}

pub fn run(st: &Store, nb: &Notebook, source: Option<&str>, fast: bool, prog: Prog) -> Result<Value> {
    run_with(st, nb, source, fast, prog, &std::sync::atomic::AtomicBool::new(false))
}

/// Background job: yields the GPU to interactive calls after every chunk; `cancel` is checked between chunks
/// (also the file `<nb>/.map.cancel`); finished chunks stay cached, so a rerun resumes.
pub fn run_with(st: &Store, nb: &Notebook, source: Option<&str>, fast: bool, prog: Prog, cancel: &std::sync::atomic::AtomicBool) -> Result<Value> {
    let _batch = funkhaus_sys::gpu::Batch::on();
    let cfile = st.dir(&nb.id).join(".map.cancel");
    let _ = std::fs::remove_file(&cfile);
    let stop = || cancel.load(std::sync::atomic::Ordering::Relaxed) || cfile.exists();
    let mut cancelled = false;
    let lf = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(st.dir(&nb.id).join(".map.lock"))?;
    if lf.try_lock().is_err() {
        bail!("map läuft bereits für dieses Notebook");
    }
    let mut cfg = st.cfg.clone();
    if fast {
        cfg.llm = cfg.map_fast.clone();
        cfg.fallback = None;
    }
    cfg.llm.max_tokens = 1200;
    cfg.llm.temperature = 0.2;
    let t0 = std::time::Instant::now();
    let (mut mapped, mut cached, mut kept, mut dropped, mut restarted, mut removed) = (0, 0, 0, 0, false, 0);
    let mut per = vec![];
    // --source: id or title (like `src`); otherwise all enabled sources
    let srcs: Vec<_> = match source {
        Some(k) => vec![nb.sources.iter().find(|s| s.id == k || s.title.to_lowercase() == k.to_lowercase()).with_context(|| format!("Quelle nicht gefunden: {k}"))?],
        None => nb.sources.iter().filter(|s| s.enabled).collect(),
    };
    // maps of removed sources
    if let Ok(rd) = std::fs::read_dir(st.dir(&nb.id).join("map")) {
        for e in rd.flatten() {
            if !nb.sources.iter().any(|s| e.file_name().to_string_lossy() == s.id) {
                let _ = std::fs::remove_dir_all(e.path());
            }
        }
    }
    let mut total = 0;
    let all: usize = srcs.iter().map(|s| st.source_chunks(nb, s).map(|c| c.len()).unwrap_or(0)).sum();
    let mut done: usize = srcs.iter().map(|s| st.source_chunks(nb, s).map(|c| c.iter().filter(|c| load(st, nb, c).is_some()).count()).unwrap_or(0)).sum();
    let mut rate: Option<f64> = None;
    for s in srcs {
        if cancelled {
            break;
        }
        let text = st.text(nb, &s.id)?;
        let head = header(&text);
        let chunks = st.source_chunks(nb, s)?;
        total += chunks.len();
        // GC: drop maps whose chunk text no longer exists or that are from an older MAP_V
        let live: Vec<String> = chunks.iter().map(|c| format!("{}.json", key(&c.text))).collect();
        let mut gc = 0;
        if let Ok(rd) = std::fs::read_dir(st.dir(&nb.id).join("map").join(&s.id)) {
            for e in rd.flatten() {
                let f = e.file_name().to_string_lossy().into_owned();
                let old = std::fs::read_to_string(e.path()).ok().and_then(|x| serde_json::from_str::<Map>(&x).ok()).is_none_or(|m| m.v != MAP_V);
                if !live.contains(&f) || old {
                    let _ = std::fs::remove_file(e.path());
                    gc += 1;
                }
            }
        }
        removed += gc;
        let mut prev: Option<String> = None;
        for (i, c) in chunks.iter().enumerate() {
            if let Some(m) = load(st, nb, c) {
                cached += 1;
                prev = Some(m.summary);
                continue;
            }
            if stop() {
                cancelled = true;
                prog("abgebrochen – bereits kartierte Abschnitte bleiben erhalten");
                break;
            }
            prog(&format!("{} · {} ({}/{})", progress(done, all, rate), c.id, i + 1, chunks.len()));
            funkhaus_sys::gpu::gpu_wait(prog);
            let ts = std::time::Instant::now();
            let msgs = prompt(&s.title, &head, prev.as_deref(), c);
            let repairs = std::cell::Cell::new(0);
            let mut events: Vec<&str> = vec![];
            let call = || {
                let out = llm::chat(&cfg, nb.backend, &msgs, Some(&schema()))?;
                if let Some(v) = llm::parse_json(&out) {
                    return Ok(v);
                }
                repairs.set(repairs.get() + 1);
                llm::json_repair(&cfg, nb.backend, &out, &schema())
            };
            let v = match call() {
                Err(e) if !fast && !restarted && e.to_string().contains("abgestürzt") => match &st.cfg.gemma_serve {
                    Some(cmd) => {
                        restarted = true;
                        restart_gemma(cmd, prog)?;
                        events.push("gemma-restart");
                        call().with_context(|| format!("{} nach Neustart", c.id))?
                    }
                    None => return Err(e).with_context(|| format!("{}: kein gemma_serve in config.toml für automatischen Neustart", c.id)),
                },
                r => r.with_context(|| c.id.clone())?,
            };
            events.extend(std::iter::repeat_n("json-repair", repairs.get()));
            let facts: Vec<Fact> = serde_json::from_value(v["facts"].clone()).unwrap_or_default();
            let (facts, d) = gate(facts, &c.text);
            let m = Map {
                v: MAP_V,
                chunk: c.id.clone(),
                hash: key(&c.text),
                context: v["context"].as_str().unwrap_or("").trim().into(),
                summary: v["summary"].as_str().unwrap_or("").trim().into(),
                questions: v["questions"].as_array().into_iter().flatten().filter_map(|q| q.as_str().map(|q| q.trim().to_string())).collect(),
                facts,
                dropped: d,
                model: cfg.llm.model.clone(),
            };
            let p = path(st, nb, c);
            std::fs::create_dir_all(p.parent().unwrap())?;
            std::fs::write(&p, serde_json::to_string_pretty(&m)?)?;
            let secs = (ts.elapsed().as_secs_f64() * 10.0).round() / 10.0;
            prog(&format!("  {secs} s, {} Fakten, {d} verworfen", m.facts.len()));
            per.push(json!({"chunk": c.id, "secs": secs, "facts_kept": m.facts.len(), "facts_dropped": d, "events": events}));
            mapped += 1;
            done += 1;
            rate = Some(rate.map_or(secs, |r| 0.7 * r + 0.3 * secs));
            kept += m.facts.len();
            dropped += d;
            prev = Some(m.summary);
        }
    }
    let _ = std::fs::remove_file(&cfile);
    Ok(json!({
        "cancelled": cancelled, "done": done, "chunks": total, "mapped": mapped, "skipped_cached": cached, "facts_kept": kept, "facts_dropped": dropped, "removed_stale": removed,
        "secs": (t0.elapsed().as_secs_f64() * 10.0).round() / 10.0, "model": cfg.llm.model, "per_chunk": per
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quote_gate() {
        let text = "So, they're going to salvage it tomorrow.  Uh, the guys are doing\ntheir best job here.";
        let f = |q: &str| Fact { text: "x".into(), quote: q.into() };
        let (k, d) = gate(
            vec![
                f("“They’re going to salvage it tomorrow.”"), // curly quotes, case, trailing period
                f("the guys are doing their best job"),          // line break in source
                f("the boat was destroyed by the ferry"),        // not in text
                f("salvage it tomorrow..."),                     // < 5 words
                f("they're going to salvage it"),
            ],
            text,
        );
        assert_eq!((k.len(), d), (3, 2));
        // numbers in the fact text must occur in the chunk ("1.250" ≙ "1250")
        let t = "Am 31. Dezember 2025 lebten 1.250 Einwohner in Kleinholm.";
        let g = |x: &str| Fact { text: x.into(), quote: "lebten 1.250 Einwohner in Kleinholm".into() };
        let (k, d) = gate(vec![g("Kleinholm hat 1250 Einwohner (2025)."), g("Kleinholm hat 1.300 Einwohner.")], t);
        assert_eq!((k.len(), d), (1, 1));
    }

    #[test]
    fn cache_key_and_idempotency() {
        let t = tempfile::tempdir().unwrap();
        let st = Store::at(t.path()).unwrap();
        let nb = st.create("M").unwrap();
        let c = Chunk { id: "s1#0".into(), src: "s1".into(), title: "T".into(), text: "Hallo Welt".into(), ..Default::default() };
        assert_eq!(key("Hallo Welt"), key("Hallo Welt"));
        assert_ne!(key("Hallo Welt"), key("Hallo Welt."));
        assert!(load(&st, &nb, &c).is_none());
        let m = Map { v: MAP_V, chunk: c.id.clone(), hash: key(&c.text), context: "k".into(), summary: "s".into(), facts: vec![], questions: vec![], dropped: 0, model: "m".into() };
        let p = path(&st, &nb, &c);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, serde_json::to_string(&m).unwrap()).unwrap();
        assert_eq!(load(&st, &nb, &c).unwrap().summary, "s");
        // re-chunked: same text under another id still hits; changed text misses
        let moved = Chunk { id: "s1#7".into(), ..c.clone() };
        assert!(load(&st, &nb, &moved).is_some());
        let changed = Chunk { text: "Hallo Welt!".into(), ..c.clone() };
        assert!(load(&st, &nb, &changed).is_none());
        // maps from an older prompt version are ignored
        std::fs::write(&p, serde_json::to_string(&Map { v: MAP_V - 1, ..m }).unwrap()).unwrap();
        assert!(load(&st, &nb, &c).is_none());
    }

    #[test]
    fn prompt_demands_german() {
        let c = Chunk { id: "s2#1".into(), src: "s2".into(), title: "Boat".into(), text: "We lost the drone.".into(), t0: Some(754.0), ..Default::default() };
        let m = prompt("Boat", "# Boat\nKanal: X", Some("Vorher"), &c);
        assert!(m[0].content.contains("IMMER auf Deutsch") && m[0].content.contains("Originalsprache"));
        assert!(m[1].content.contains("(s2#1, ab 12:34)") && m[1].content.contains("We lost the drone."));
        assert!(m.iter().map(|x| x.content.len()).sum::<usize>() < 3000);
        assert_eq!(header("# T\nKanal: X\n\n## Beschreibung\nlang"), "# T\nKanal: X");
    }
}
