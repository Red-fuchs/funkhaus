//! Retrieval evaluation: gold = verbatim quote → chunk(s) containing it (resolved at eval time, so gold
//! survives chunker changes). recall@k, MRR@max(k), bootstrap 95 % CIs. No LLM calls.
use crate::retrieval::{Bm25, Chunk};
use crate::{Notebook, Store};
use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::Path;

#[derive(Deserialize, Clone, Debug)]
pub struct Gold {
    pub q: String,
    #[serde(default)]
    pub quote: String,
    #[serde(default)]
    pub source_hint: Option<String>,
    /// unanswerable: excluded from recall/MRR
    #[serde(default)]
    pub none: bool,
    /// no content word shared with the gold chunk or its map questions; reported as a separate subset
    #[serde(default)]
    pub oblique: bool,
    /// free subset label (e.g. direct/oblique/global); "oblique" implies `oblique`
    #[serde(default)]
    pub kind: Option<String>,
    /// start "HH:MM:SS" of the cue where `quote` begins (time-correctness ±15 s)
    #[serde(default)]
    pub ts: Option<String>,
    /// similar-sounding wrong passage; for `avoid_outcut` it lies in an excluded range
    #[serde(default)]
    pub hard_negative_quote: Option<String>,
    #[serde(default)]
    pub avoid_outcut: bool,
}

/// Optional inputs for time-correctness and outcut leaks.
#[derive(Default)]
pub struct Extra<'a> {
    /// normalized texts of excluded cues (≥ 4 words) as (source id, text)
    pub outcut: Vec<(String, String)>,
    /// cues inside chunk i (without excluded ones), for cue-level citation times
    pub cues: Option<&'a dyn Fn(usize) -> Vec<crate::ingest::Cue>>,
}

pub const TIME_TOL: f64 = 15.0;

const OBLIQUE_STOP: &[&str] = &[
    "welche", "welcher", "welches", "welchen", "viele", "vielen", "wann", "warum", "wieso", "weshalb", "gibt", "gab", "dort", "hier",
    "kann", "konnte", "muss", "musste", "wurde", "wurden", "worden", "sein", "seine", "seiner", "seinen", "ihre", "ihren", "ihrer",
    "diese", "dieser", "dieses", "beim", "also", "haben", "hatte", "hatten", "eigentlich", "genau", "etwas", "einem", "einer",
    "durch", "nach", "über", "unter", "denn", "dann", "wenn", "will", "wollte", "wollen", "soll", "sollte", "darf", "durfte",
    // stemmed forms (tokens() strips -e/-es/-en/-er/… from longer words)
    "welch", "wurd", "hatt", "konnt", "musst", "sollt", "woll", "durft", "dies", "sein", "ihr",
];

/// Content words (≥ 4 chars, stemmed) for the oblique check.
pub fn content(s: &str) -> std::collections::BTreeSet<String> {
    crate::retrieval::tokens(s).into_iter().filter(|t| t.chars().count() >= 4 && !OBLIQUE_STOP.contains(&t.as_str())).collect()
}

pub fn load(p: &Path) -> Result<Vec<Gold>> {
    let s = std::fs::read_to_string(p).with_context(|| p.display().to_string())?;
    s.lines()
        .enumerate()
        .filter(|(_, l)| !l.trim().is_empty() && !l.trim_start().starts_with("//"))
        .map(|(i, l)| {
            let mut g: Gold = serde_json::from_str(l).with_context(|| format!("{}:{}", p.display(), i + 1))?;
            g.oblique |= g.kind.as_deref() == Some("oblique");
            Ok(g)
        })
        .collect()
}

pub fn norm(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
}

/// Chunks containing the normalized quote; `source_hint` narrows to matching titles when it matches any.
pub fn gold_chunks(chunks: &[Chunk], g: &Gold) -> Vec<usize> {
    let q = norm(&g.quote);
    let mut v: Vec<usize> = (0..chunks.len()).filter(|&i| !q.is_empty() && norm(&chunks[i].text).contains(&q)).collect();
    if let Some(h) = g.source_hint.as_deref().map(str::to_lowercase).filter(|h| !h.is_empty()) {
        let f: Vec<usize> = v.iter().copied().filter(|&i| chunks[i].title.to_lowercase().contains(&h)).collect();
        if !f.is_empty() {
            v = f;
        }
    }
    v
}

/// Ranker under test: chunk indices, best first.
pub type Ranker<'a> = &'a dyn Fn(&[Chunk], &str, usize) -> Vec<usize>;

pub fn bm25(chunks: &[Chunk], q: &str, k: usize) -> Vec<usize> {
    Bm25::new(chunks.iter().map(|c| c.text.as_str())).search(q, k).into_iter().map(|(i, _)| i).collect()
}

/// Deterministic xorshift for the bootstrap.
struct Rng(u64);
impl Rng {
    fn next(&mut self, n: usize) -> usize {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 % n as u64) as usize
    }
}

/// 95 % bootstrap interval of the mean (1000 resamples, fixed seed).
pub fn ci95(xs: &[f64]) -> [f64; 2] {
    if xs.is_empty() {
        return [0.0, 0.0];
    }
    let mut r = Rng(0x9E37_79B9_7F4A_7C15);
    let mut means: Vec<f64> = (0..1000).map(|_| (0..xs.len()).map(|_| xs[r.next(xs.len())]).sum::<f64>() / xs.len() as f64).collect();
    means.sort_by(f64::total_cmp);
    [round(means[25]), round(means[974])]
}

fn round(x: f64) -> f64 {
    (x * 1000.0).round() / 1000.0
}

fn mean(xs: &[f64]) -> f64 {
    if xs.is_empty() { 0.0 } else { round(xs.iter().sum::<f64>() / xs.len() as f64) }
}

fn metrics(ks: &[usize], hits: &[Vec<f64>], rr: &[f64]) -> Value {
    let mut out = json!({"n": rr.len(), "mrr": mean(rr)});
    let mut ci = json!({"mrr": ci95(rr)});
    for (j, &k) in ks.iter().enumerate() {
        out[format!("recall@{k}")] = json!(mean(&hits[j]));
        ci[format!("recall@{k}")] = json!(ci95(&hits[j]));
    }
    out["ci95"] = ci;
    out
}

/// `aux` (map fields, index-aligned or empty) is only used to check that oblique questions really are oblique.
pub fn run(chunks: &[Chunk], aux: &[crate::retrieval::Aux], gold: &[Gold], ks: &[usize], rank: Ranker) -> Value {
    run_with(chunks, aux, gold, ks, rank, &Extra::default())
}

pub fn run_with(chunks: &[Chunk], aux: &[crate::retrieval::Aux], gold: &[Gold], ks: &[usize], rank: Ranker, ex: &Extra) -> Value {
    let kmax = ks.iter().copied().max().unwrap_or(10);
    // outcut leak: a top-k chunk overlaps an excluded range or (avoid_outcut) contains the hard negative
    let leaks = |g: &Gold, res: &[usize]| {
        res.iter().any(|&i| {
            let c = &chunks[i];
            // text, not time span: a chunk may span the cut without containing excluded speech
            let t = ex.outcut.iter().any(|(s, x)| *s == c.src && norm(&c.text).contains(x.as_str()));
            let hn = g.avoid_outcut && g.hard_negative_quote.as_deref().is_some_and(|h| norm(&c.text).contains(&norm(h)));
            t || hn
        })
    };
    let (mut leak, mut abstain, mut nnone, mut hn1, mut nhn) = (0, 0, 0, 0, 0);
    let (mut tc, mut tq, mut to, mut nts) = (0, 0, 0, 0);
    let mut all = (vec![vec![]; ks.len()], vec![]);
    let mut obl = (vec![vec![]; ks.len()], vec![]);
    let mut kinds: std::collections::BTreeMap<String, (Vec<Vec<f64>>, Vec<f64>)> = Default::default();
    let (mut misses, mut unresolved, mut overlap) = (vec![], vec![], vec![]);
    let mut excluded = 0;
    for g in gold {
        let res = rank(chunks, &g.q, kmax);
        leak += leaks(g, &res) as usize;
        if let Some(h) = g.hard_negative_quote.as_deref() {
            nhn += 1;
            hn1 += res.first().is_some_and(|&i| norm(&chunks[i].text).contains(&norm(h))) as usize;
        }
        if g.none {
            excluded += 1;
            nnone += 1;
            abstain += res.is_empty() as usize;
            continue;
        }
        let gc = gold_chunks(chunks, g);
        if gc.is_empty() {
            unresolved.push(json!({"q": g.q, "gold_quote": g.quote}));
            continue;
        }
        if g.oblique {
            let q = content(&g.q);
            let mut seen = std::collections::BTreeSet::new();
            for &i in &gc {
                seen.extend(content(&chunks[i].text));
                if let Some(x) = aux.get(i).and_then(|a| a.questions.as_ref()) {
                    seen.extend(content(x));
                }
            }
            let shared: Vec<&String> = q.intersection(&seen).collect();
            if !shared.is_empty() {
                overlap.push(json!({"q": g.q, "shared": shared}));
            }
        }
        let pos = res.iter().position(|i| gc.contains(i)).map(|p| p + 1);
        if let Some(t) = g.ts.as_deref().and_then(crate::ingest::ts) {
            nts += 1;
            if pos == Some(1) {
                let i = res[0];
                let ok = |x: Option<f64>| x.is_some_and(|a| (a - t).abs() <= TIME_TOL) as usize;
                tc += ok(chunks[i].t0);
                if let Some(f) = ex.cues {
                    let cs = f(i);
                    // without an LLM answer the claim is the question itself
                    tq += ok(crate::cite::best_cue(&cs, &g.q).map(|c| c.t0).or(chunks[i].t0));
                    to += ok(cue_time(&chunks[i], &cs, &g.quote));
                }
            }
        }
        let mut kd = g.kind.as_ref().map(|k| kinds.entry(k.clone()).or_insert_with(|| (vec![vec![]; ks.len()], vec![])));
        for set in [&mut all].into_iter().chain(g.oblique.then_some(&mut obl)).chain(kd.as_deref_mut()) {
            for (j, &k) in ks.iter().enumerate() {
                set.0[j].push(if pos.is_some_and(|p| p <= k) { 1.0 } else { 0.0 });
            }
            set.1.push(pos.map_or(0.0, |p| 1.0 / p as f64));
        }
        if pos.is_none() {
            misses.push(json!({"q": g.q, "oblique": g.oblique, "gold_quote": g.quote, "gold_chunks": gc.iter().map(|&i| &chunks[i].id).collect::<Vec<_>>()}));
        }
    }
    let mut out = metrics(ks, &all.0, &all.1);
    out["excluded"] = json!(excluded);
    out["chunks"] = json!(chunks.len());
    if !obl.1.is_empty() {
        out["oblique"] = metrics(ks, &obl.0, &obl.1);
        out["oblique_overlap"] = json!(overlap);
    }
    for (k, (h, r)) in &kinds {
        out["kinds"][k] = metrics(ks, h, r);
    }
    let rate = |a: usize, n: usize| if n == 0 { Value::Null } else { json!(round(a as f64 / n as f64)) };
    out["outcut_leak"] = json!({"rate": rate(leak, gold.len()), "n": gold.len(), "leaks": leak});
    out["abstain"] = json!({"rate": rate(abstain, nnone), "n": nnone, "note": "LLM-frei: Ranker liefert keinen Treffer"});
    out["hard_negative_top1"] = json!({"rate": rate(hn1, nhn), "n": nhn});
    out["time_ok"] = json!({"n": nts, "chunk_start": rate(tc, nts), "cue_question": rate(tq, nts), "cue_oracle": rate(to, nts), "tol_s": TIME_TOL,
        "note": "top-1 trifft und Zeit ±15 s; chunk_start = Zitatzeit bis M2.3, cue_question = bester Cue zur Frage (M2.4, ohne LLM), cue_oracle = Cue mit dem Gold-Zitat"});
    out["misses"] = json!(misses);
    out["unresolved"] = json!(unresolved);
    out
}

/// `ranker`: "bm25" | "fields" | "fields:W1,W2,W3" (raw text, German about, German questions).
pub fn eval(st: &Store, nb: &Notebook, set: &Path, ks: &[usize], ranker: &str) -> Result<Value> {
    let chunks = st.chunks(nb)?;
    let gold = load(set)?;
    let cues: std::collections::HashMap<String, Vec<crate::ingest::Cue>> = nb.sources.iter().filter_map(|s| Some((s.id.clone(), st.cues(nb, &s.id)?))).collect();
    let mut outcut = vec![];
    for s in &nb.sources {
        let oc = st.outcuts(nb, &s.id);
        for c in cues.get(&s.id).into_iter().flatten().filter(|c| oc.iter().any(|o| o.hits(c.t0, c.t1)) && c.text.split_whitespace().count() >= 4) {
            outcut.push((s.id.clone(), norm(&c.text)));
        }
    }
    let cc = |i: usize| crate::cite::chunk_cues(st, nb, &chunks[i]);
    let ex = Extra { outcut, cues: Some(&cc) };
    let mut v = match ranker.split_once(':').map_or((ranker, None), |(a, b)| (a, Some(b))) {
        ("bm25", None) => run_with(&chunks, &st.aux(nb, &chunks), &gold, ks, &bm25, &ex),
        ("fields", w) => {
            let w = match w {
                Some(s) => {
                    let p: Vec<f64> = s.split(',').map(|x| x.trim().parse()).collect::<Result<_, _>>().context("Gewichte W1,W2,W3")?;
                    <[f64; 3]>::try_from(p).map_err(|_| anyhow::anyhow!("drei Gewichte"))?
                }
                None => crate::retrieval::WEIGHTS,
            };
            let aux = st.aux(nb, &chunks);
            let mapped = aux.iter().filter(|a| a.about.is_some()).count();
            let mut v = run_with(&chunks, &aux, &gold, ks, &|c, q, k| crate::retrieval::rank(c, &aux, q, k, w), &ex);
            v["mapped_chunks"] = json!(mapped);
            v["weights"] = json!(w);
            v
        }
        _ => anyhow::bail!("--ranker bm25|fields|fields:W1,W2,W3"),
    };
    v["set"] = json!(set);
    v["ranker"] = json!(ranker);
    Ok(v)
}

/// Start time of the cue (within the chunk's span) where `quote` begins.
pub fn cue_time(c: &Chunk, cues: &[crate::ingest::Cue], quote: &str) -> Option<f64> {
    let (a, b) = (c.t0?, c.t1?);
    let span: Vec<&crate::ingest::Cue> = cues.iter().filter(|x| x.t0 >= a && x.t1 <= b).collect();
    let (mut joined, mut starts) = (String::new(), vec![]);
    for x in &span {
        starts.push(joined.len());
        joined.push_str(&norm(&x.text));
        joined.push(' ');
    }
    let p = joined.find(&norm(quote))?;
    let k = starts.partition_point(|&s| s <= p) - 1;
    Some(span[k].t0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ch(id: &str, title: &str, text: &str) -> Chunk {
        Chunk { id: id.into(), src: id.split('#').next().unwrap().into(), title: title.into(), text: text.into(), ..Default::default() }
    }

    #[test]
    fn metrics() {
        let c = vec![ch("a#0", "A", "Der Hund bellt im Garten."), ch("b#0", "B", "Die Katze schläft im  Haus."), ch("c#0", "C", "Ein Vogel singt.")];
        let g = |q: &str, quote: &str| Gold { q: q.into(), quote: quote.into(), source_hint: None, none: false, oblique: false, kind: None, ts: None, hard_negative_quote: None, avoid_outcut: false };
        let gold = vec![
            g("Wo bellt der Hund?", "hund bellt"),
            g("Wo schläft die Katze?", "Katze schläft im Haus"),
            g("Was macht das Tier draußen?", "Vogel singt"),
            Gold { none: true, ..g("Gehalt?", "") },
            g("x", "steht nirgends"),
        ];
        let v = run(&c, &[], &gold, &[1, 5], &bm25);
        assert_eq!(v["n"], 3);
        assert_eq!(v["excluded"], 1);
        assert_eq!(v["unresolved"].as_array().unwrap().len(), 1);
        assert_eq!(v["recall@1"], 0.667);
        assert_eq!(v["misses"].as_array().unwrap().len(), 1);
        let ci = v["ci95"]["recall@1"].as_array().unwrap();
        assert!(ci[0].as_f64().unwrap() <= 0.667 && ci[1].as_f64().unwrap() >= 0.667);
        assert_eq!(ci95(&[1.0, 0.0, 1.0]), ci95(&[1.0, 0.0, 1.0])); // fixed seed
    }

    #[test]
    fn leak_abstain_time() {
        let mut c = vec![ch("s1#0", "A", "Wir starten um neun Uhr."), ch("s1#1", "A", "Mein Hund muss zum Tierarzt.")];
        (c[0].t0, c[0].t1, c[1].t0, c[1].t1) = (Some(0.0), Some(60.0), Some(100.0), Some(160.0));
        let g = |q: &str, quote: &str| Gold { q: q.into(), quote: quote.into(), source_hint: None, none: false, oblique: false, kind: None, ts: None, hard_negative_quote: None, avoid_outcut: false };
        let gold = vec![
            Gold { ts: Some("00:00:40".into()), ..g("Wann starten wir um neun?", "starten um neun Uhr") },
            Gold { avoid_outcut: true, hard_negative_quote: Some("Hund muss zum Tierarzt".into()), ..g("Was ist mit dem Hund?", "starten um neun") },
            Gold { none: true, ..g("Telefonnummer?", "") },
        ];
        let cc = |i: usize| if i == 0 { vec![crate::ingest::Cue { t0: 42.0, t1: 44.0, speaker: None, text: "Wir starten um neun Uhr.".into() }] } else { vec![] };
        let v = run_with(&c, &[], &gold, &[1], &bm25, &Extra { outcut: vec![], cues: Some(&cc) });
        assert_eq!((v["outcut_leak"]["leaks"].as_u64(), v["abstain"]["rate"].as_f64()), (Some(1), Some(1.0)));
        assert_eq!((v["time_ok"]["chunk_start"].as_f64(), v["time_ok"]["cue_question"].as_f64(), v["time_ok"]["cue_oracle"].as_f64()), (Some(0.0), Some(1.0), Some(1.0)));
        // excluded chunk removed → no leak; a stored range overlapping a hit also counts
        let v = run_with(&c[..1], &[], &gold, &[1], &bm25, &Extra::default());
        assert_eq!(v["outcut_leak"]["leaks"].as_u64(), Some(0));
        let v = run_with(&c[..1], &[], &gold, &[1], &bm25, &Extra { outcut: vec![("s1".into(), norm("starten um neun"))], cues: None });
        assert_eq!(v["outcut_leak"]["leaks"].as_u64(), Some(1));
    }

    #[test]
    fn cue_time_finds_quote_start() {
        let cue = |t0: f64, t: &str| crate::ingest::Cue { t0, t1: t0 + 4.0, speaker: None, text: t.into() };
        let cues = vec![cue(0.0, "Davor."), cue(100.0, "Guten Morgen, zusammen."), cue(105.0, "Wir haben von neun Uhr"), cue(110.0, "bis sechzehn Uhr Zeit.")];
        let c = Chunk { t0: Some(100.0), t1: Some(114.0), ..ch("s1#1", "A", "x") };
        assert_eq!(cue_time(&c, &cues, "neun Uhr bis sechzehn"), Some(105.0));
        assert_eq!(cue_time(&c, &cues, "Davor"), None);
    }

    #[test]
    fn hint_narrows() {
        let c = vec![ch("a#0", "Chronik", "1783 gegründet"), ch("b#0", "Interview", "1783 gegründet")];
        let g = Gold { q: "q".into(), quote: "1783 gegründet".into(), source_hint: Some("interview".into()), none: false, oblique: false, kind: None, ts: None, hard_negative_quote: None, avoid_outcut: false };
        assert_eq!(gold_chunks(&c, &g), vec![1]);
    }

    #[test]
    fn oblique_subset_and_overlap() {
        let c = vec![ch("a#0", "A", "Die Fähre fährt viermal täglich nach Rüdersand."), ch("b#0", "B", "Der Hund bellt.")];
        let o = |q: &str| Gold { q: q.into(), quote: "fährt viermal täglich".into(), source_hint: None, none: false, oblique: true, kind: None, ts: None, hard_negative_quote: None, avoid_outcut: false };
        let gold = vec![o("Wie oft pendelt das Schiff zum Festland?"), o("Wie oft fährt die Fähre?")];
        let v = run(&c, &[], &gold, &[1], &bm25);
        assert_eq!(v["oblique"]["n"], 2);
        let ov = v["oblique_overlap"].as_array().unwrap();
        assert_eq!(ov.len(), 1);
        assert_eq!(ov[0]["q"], "Wie oft fährt die Fähre?");
        // "kind" subsets; kind "oblique" implies the oblique subset
        let p = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(p.path(), "{\"q\":\"Wie oft fährt die Fähre?\",\"quote\":\"fährt viermal\",\"kind\":\"direct\"}\n{\"q\":\"Pendelt das Schiff?\",\"quote\":\"fährt viermal\",\"kind\":\"oblique\"}\n").unwrap();
        let gold = load(p.path()).unwrap();
        assert!(gold[1].oblique && !gold[0].oblique);
        let v = run(&c, &[], &gold, &[1], &bm25);
        assert_eq!((v["kinds"]["direct"]["recall@1"].as_f64(), v["kinds"]["oblique"]["n"].as_u64(), v["oblique"]["n"].as_u64()), (Some(1.0), Some(1), Some(1)));
    }

    /// Every gold quote must occur verbatim (normalized) in the committed eval sources:
    /// kleinholm = fixtures/haiku (extracted like an import), solarboot = evals/data/solarboot.
    #[test]
    fn gold_sets_verify() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let cfg = crate::Config::default();
        let kh: String = ["kleinholm.md", "chronik.txt", "interview.srt", "presse.html", "kleinholm.pdf"]
            .iter()
            .map(|f| norm(&crate::ingest::extract(&root.join("fixtures/haiku").join(f), &cfg).unwrap().text))
            .collect::<Vec<_>>()
            .join("\n");
        let sb = norm(&std::fs::read_to_string(root.join("evals/data/solarboot/source.txt")).unwrap());
        for (set, text) in [("kleinholm", &kh), ("solarboot", &sb), ("mix", &format!("{kh}\n{sb}"))] {
            let gold = load(&root.join(format!("evals/{set}.jsonl"))).unwrap();
            assert!(gold.len() >= 20, "{set}: {}", gold.len());
            for g in gold.iter().filter(|g| !g.none) {
                assert!(text.contains(&norm(&g.quote)), "{set}: quote not found: {:?}", g.quote);
            }
        }
    }
}
