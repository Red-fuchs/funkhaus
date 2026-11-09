use serde::{Deserialize, Serialize};
use std::collections::HashMap;

pub const WORDS: usize = 500; // ≈ 800 tokens for German text
pub const OVERLAP: usize = 60;

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Chunk {
    pub id: String,
    pub src: String,
    pub title: String,
    pub text: String,
    /// time span (seconds) for timed sources
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub t0: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub t1: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speaker: Option<String>,
}

impl Chunk {
    /// "12:34" (or "1:02:03") for timed chunks
    pub fn at(&self) -> Option<String> {
        self.t0.map(crate::util::mmss)
    }
}

fn spans(text: &str) -> Vec<(usize, usize)> {
    let mut v = vec![];
    let mut start = None;
    for (i, c) in text.char_indices() {
        match (c.is_whitespace(), start) {
            (false, None) => start = Some(i),
            (true, Some(s)) => {
                v.push((s, i));
                start = None;
            }
            _ => {}
        }
    }
    if let Some(s) = start {
        v.push((s, text.len()));
    }
    v
}

/// Chunking for timed sources: cuts only at cue ends (preferring speaker changes) within ±20 % of
/// the target size; overlap starts at a cue start. Chunks carry t0/t1/speaker.
pub fn chunk_cued(src: &str, title: &str, text: &str, cues: &[crate::ingest::Cue]) -> Vec<Chunk> {
    let sp = spans(text);
    // locate cues in the text, in order (video sources have a header before the transcript)
    let mut cur = text.find("## Transkript").unwrap_or(0);
    let mut rng: Vec<(usize, usize, usize)> = vec![];
    for (k, c) in cues.iter().enumerate() {
        if let Some(p) = text[cur..].find(&c.text) {
            let a = cur + p;
            rng.push((a, a + c.text.len(), k));
            cur = a + c.text.len();
        }
    }
    let wc: Vec<Option<usize>> = sp
        .iter()
        .map(|&(a, _)| {
            let i = rng.partition_point(|r| r.0 <= a);
            (i > 0 && a < rng[i - 1].1).then(|| rng[i - 1].2)
        })
        .collect();
    let spk = |k: Option<usize>| k.and_then(|k| cues[k].speaker.as_deref());
    let (lo, hi) = (WORDS * 4 / 5, WORDS * 6 / 5);
    let mut out = vec![];
    // video header (title, description, chapters) is its own untimed chunk(s); transcript starts at "## Transkript"
    let h = text.find("## Transkript").map_or(0, |p| sp.partition_point(|w| w.0 < p));
    for (i, j) in windows(h) {
        out.push(Chunk { id: format!("{src}#{}", out.len()), src: src.into(), title: title.into(), text: text[sp[i].0..sp[j - 1].1].to_string(), ..Default::default() });
    }
    let mut i = h;
    while i < sp.len() {
        let mut j = if sp.len() - i <= hi {
            sp.len()
        } else {
            (i + lo..=i + hi)
                .filter(|&j| wc[j - 1].is_some() && wc[j - 1] != wc[j])
                .min_by_key(|&j| {
                    let d = (j as i64 - (i + WORDS) as i64).unsigned_abs() as usize;
                    // a speaker change anywhere in the window beats a plain cue end
                    if spk(wc[j - 1]) != spk(wc[j]) { d } else { d + hi }
                })
                .unwrap_or(i + WORDS)
        };
        if sp.len() - j < lo {
            j = sp.len();
        }
        let ks: Vec<usize> = wc[i..j].iter().flatten().copied().collect();
        let mut who: Vec<&str> = vec![];
        for &k in &ks {
            if let Some(n) = cues[k].speaker.as_deref().filter(|n| !who.contains(n)) {
                who.push(n);
            }
        }
        out.push(Chunk {
            id: format!("{src}#{}", out.len()),
            src: src.into(),
            title: title.into(),
            text: text[sp[i].0..sp[j - 1].1].to_string(),
            t0: ks.iter().map(|&k| cues[k].t0).reduce(f64::min),
            t1: ks.iter().map(|&k| cues[k].t1).reduce(f64::max),
            speaker: (!who.is_empty()).then(|| who.iter().take(3).copied().collect::<Vec<_>>().join(", ")),
        });
        if j == sp.len() {
            break;
        }
        let mut s = j.saturating_sub(OVERLAP).max(i + 1);
        while s > i + 1 && wc[s].is_some() && wc[s - 1] == wc[s] {
            s -= 1;
        }
        i = s;
    }
    out
}

/// Word windows [i, j) of ~WORDS with OVERLAP; a tail shorter than 80 % of WORDS is merged into
/// the last window instead of becoming a tiny chunk.
fn windows(n: usize) -> Vec<(usize, usize)> {
    let lo = WORDS * 4 / 5;
    let mut v = vec![];
    let mut i = 0;
    while i < n {
        let mut j = (i + WORDS).min(n);
        if n - j < lo {
            j = n;
        }
        v.push((i, j));
        if j == n {
            break;
        }
        i = j - OVERLAP;
    }
    v
}

/// Word windows with overlap; ids `<src>#<n>` are stable for unchanged text.
pub fn chunk(src: &str, title: &str, text: &str) -> Vec<Chunk> {
    let sp = spans(text);
    windows(sp.len())
        .into_iter()
        .enumerate()
        .map(|(n, (i, j))| Chunk {
            id: format!("{src}#{n}"),
            src: src.into(),
            title: title.into(),
            text: text[sp[i].0..sp[j - 1].1].to_string(),
            ..Default::default()
        })
        .collect()
}

const STOP: &[&str] = &[
    "der", "die", "das", "und", "oder", "ist", "sind", "ein", "eine", "einer", "eines", "einem", "einen", "den", "dem", "des",
    "zu", "im", "in", "an", "am", "auf", "mit", "von", "für", "fuer", "es", "sie", "er", "wir", "ich", "du", "ihr", "nicht",
    "auch", "als", "wie", "was", "wer", "wo", "bei", "aus", "noch", "nur", "so", "dass", "da", "hat", "haben", "wird", "werden",
    "war", "sich", "man", "the", "a", "an", "of", "to", "and", "is", "are", "in", "on", "for", "it", "what", "how",
];

/// Lowercase alphanumeric tokens, stop words removed, crude German/English suffix stemming.
pub fn tokens(s: &str) -> Vec<String> {
    s.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.chars().count() > 1 && !STOP.contains(w))
        .map(stem)
        .collect()
}

fn stem(w: &str) -> String {
    if w.chars().count() > 5 {
        for suf in ["ungen", "ung", "en", "er", "es", "em", "e", "n", "s"] {
            if let Some(r) = w.strip_suffix(suf) {
                return r.to_string();
            }
        }
    }
    w.to_string()
}

pub struct Bm25 {
    docs: Vec<HashMap<String, f32>>,
    lens: Vec<f32>,
    df: HashMap<String, f32>,
    avg: f32,
}

impl Bm25 {
    pub fn new<'a>(texts: impl IntoIterator<Item = &'a str>) -> Self {
        let mut docs = vec![];
        let mut lens = vec![];
        let mut df: HashMap<String, f32> = HashMap::new();
        for t in texts {
            let mut tf: HashMap<String, f32> = HashMap::new();
            let toks = tokens(t);
            lens.push(toks.len() as f32);
            for w in toks {
                *tf.entry(w).or_default() += 1.0;
            }
            for w in tf.keys() {
                *df.entry(w.clone()).or_default() += 1.0;
            }
            docs.push(tf);
        }
        let avg = lens.iter().sum::<f32>() / lens.len().max(1) as f32;
        Self { docs, lens, df, avg: avg.max(1.0) }
    }

    /// Indices with score > 0, best first.
    pub fn search(&self, q: &str, k: usize) -> Vec<(usize, f32)> {
        let (k1, b) = (1.5, 0.75);
        let n = self.docs.len() as f32;
        let mut qt = tokens(q);
        qt.sort();
        qt.dedup();
        let mut res: Vec<(usize, f32)> = self
            .docs
            .iter()
            .enumerate()
            .map(|(i, tf)| {
                let s = qt
                    .iter()
                    .filter_map(|w| {
                        let f = *tf.get(w)?;
                        let df = self.df[w];
                        let idf = ((n - df + 0.5) / (df + 0.5) + 1.0).ln();
                        Some(idf * f * (k1 + 1.0) / (f + k1 * (1.0 - b + b * self.lens[i] / self.avg)))
                    })
                    .sum();
                (i, s)
            })
            .filter(|&(_, s)| s > 0.0)
            .collect();
        res.sort_by(|a, b| b.1.total_cmp(&a.1));
        res.truncate(k);
        res
    }
}

/// Picks chunk indices within `budget` chars: all if they fit, else BM25 top hits for `q`,
/// else an even spread over the notebook. Returned indices are in document order.
/// German map fields per chunk (from map.rs): (context + summary + fact texts, questions).
/// Empty slice or `None` entries = no map → raw text only.
#[derive(Clone, Debug, Default)]
pub struct Aux {
    pub about: Option<String>,
    pub questions: Option<String>,
}

pub const RRF_K: f64 = 60.0;

/// Reciprocal-rank fusion: Σ w / (k + rank), rank 1-based. Returns (index, score), best first.
pub fn rrf(lists: &[(Vec<usize>, f64)]) -> Vec<(usize, f64)> {
    let mut sc: HashMap<usize, f64> = HashMap::new();
    for (l, w) in lists {
        for (r, &i) in l.iter().enumerate() {
            *sc.entry(i).or_default() += w / (RRF_K + r as f64 + 1.0);
        }
    }
    let mut v: Vec<(usize, f64)> = sc.into_iter().collect();
    v.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    v
}

/// Field weights (raw text, German about, German questions).
pub const WEIGHTS: [f64; 3] = [1.0, 1.0, 1.0];

/// Multi-field BM25 + RRF over chunk indices. Without aux this is plain BM25 on the raw text.
pub fn rank(chunks: &[Chunk], aux: &[Aux], q: &str, k: usize, w: [f64; 3]) -> Vec<usize> {
    let field = |get: &dyn Fn(usize) -> Option<String>| -> Vec<usize> {
        let ids: Vec<usize> = (0..chunks.len()).filter(|&i| get(i).is_some()).collect();
        if ids.is_empty() {
            return vec![];
        }
        let texts: Vec<String> = ids.iter().map(|&i| get(i).unwrap_or_default()).collect();
        Bm25::new(texts.iter().map(String::as_str)).search(q, ids.len()).into_iter().map(|(j, _)| ids[j]).collect()
    };
    let raw = field(&|i| Some(chunks[i].text.clone()));
    if aux.len() != chunks.len() {
        return raw.into_iter().take(k).collect();
    }
    let about = field(&|i| aux[i].about.clone());
    let qs = field(&|i| aux[i].questions.clone());
    rrf(&[(raw, w[0]), (about, w[1]), (qs, w[2])]).into_iter().take(k).map(|(i, _)| i).collect()
}

pub fn select(chunks: &[Chunk], q: Option<&str>, budget: usize) -> Vec<usize> {
    select_with(chunks, &[], q, budget)
}

/// Like `select`, ranking with the map fields when available. Citations always use chunk ids/raw text.
pub fn select_with(chunks: &[Chunk], aux: &[Aux], q: Option<&str>, budget: usize) -> Vec<usize> {
    let total: usize = chunks.iter().map(|c| c.text.len()).sum();
    if total <= budget {
        return (0..chunks.len()).collect();
    }
    let per = (total / chunks.len().max(1)).max(1);
    let n = (budget / per).max(1);
    let mut pick: Vec<usize> = match q.filter(|q| !q.trim().is_empty()) {
        Some(q) => rank(chunks, aux, q, n, WEIGHTS),
        // even spread incl. first and last chunk (beginning, middle, end)
        None if n > 1 => (0..n).map(|i| i * (chunks.len() - 1) / (n - 1)).collect(),
        None => vec![0],
    };
    if q.is_none() && pick.len() > 2 {
        // spread: secure beginning and end first, then fill the middle
        let last = pick.pop().unwrap();
        pick.insert(1, last);
    }
    let mut used = 0;
    pick.retain(|&i| {
        let n = chunks[i].text.len();
        let ok = used == 0 || used + n <= budget;
        if ok {
            used += n;
        }
        ok
    });
    pick.sort();
    pick.dedup();
    pick
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunks_overlap() {
        let text: String = (0..1200).map(|i| format!("w{i} ")).collect();
        let c = chunk("s1", "T", &text);
        assert_eq!(c.len(), 2);
        assert_eq!(c[0].id, "s1#0");
        assert!(c[0].text.starts_with("w0 ") && c[0].text.ends_with("w499"));
        // the 260-word tail is merged instead of becoming a third, tiny chunk
        assert!(c[1].text.starts_with("w440 ") && c[1].text.ends_with("w1199"));
        let c = chunk("s1", "T", &(0..1500).map(|i| format!("w{i} ")).collect::<String>());
        assert_eq!(c.len(), 3);
        assert_eq!(chunk("s1", "T", "  ").len(), 0);
    }

    #[test]
    fn cued_chunks_cut_at_cue_ends() {
        use crate::ingest::Cue;
        // 300 cues of 7 words, speaker changes every 40 cues
        let cues: Vec<Cue> = (0..300)
            .map(|k| Cue {
                t0: k as f64 * 3.0,
                t1: k as f64 * 3.0 + 2.5,
                speaker: Some(format!("S{}", k / 40)),
                text: (0..7).map(|w| format!("c{k}w{w}")).collect::<Vec<_>>().join(" "),
            })
            .collect();
        let text = format!("# Titel\nKopfzeile\n\n## Transkript\n\n{}", crate::ingest::cue_text(&cues));
        let ch = chunk_cued("s1", "T", &text, &cues);
        assert!(ch.len() >= 4);
        // header is its own untimed chunk, transcript chunks start at "## Transkript"
        assert_eq!((ch[0].text.as_str(), ch[0].t0), ("# Titel\nKopfzeile", None));
        let ch: Vec<Chunk> = ch[1..].to_vec();
        assert!(ch[0].text.starts_with("## Transkript"));
        assert_eq!(ch[0].t0, Some(0.0));
        for (k, c) in ch.iter().enumerate() {
            let n = c.text.split_whitespace().count();
            assert!(c.text.ends_with("w6"), "cut inside a cue: {}", &c.text[c.text.len() - 20..]);
            assert!(c.t0.is_some() && c.t1 > c.t0);
            // last chunk may absorb a short tail
            let max = if k + 1 == ch.len() { WORDS * 2 } else { WORDS * 6 / 5 + 4 };
            assert!(n <= max && n >= WORDS * 4 / 5 - 10, "{n}");
        }
        // second chunk starts at a cue start and its time follows the first chunk's start
        assert!(ch[1].text.starts_with('c') && ch[1].text.split_whitespace().next().unwrap().ends_with("w0"));
        assert!(ch[1].t0 > ch[0].t0);
        // a speaker change (every 280 words) near the 500-word target wins
        assert_eq!(ch[0].text.split_whitespace().last(), Some("c79w6"));
        assert_eq!(ch[0].at().as_deref(), Some("0:00"));
        assert_eq!(ch[0].speaker.as_deref(), Some("S0, S1"));
    }

    #[test]
    fn rrf_math() {
        let v = rrf(&[(vec![2, 0], 1.0), (vec![0, 1], 1.0), (vec![1], 0.5)]);
        let s = |i: usize| v.iter().find(|x| x.0 == i).unwrap().1;
        assert!((s(0) - (1.0 / 62.0 + 1.0 / 61.0)).abs() < 1e-12);
        assert!((s(1) - (1.0 / 62.0 + 0.5 / 61.0)).abs() < 1e-12);
        assert!((s(2) - 1.0 / 61.0).abs() < 1e-12);
        assert_eq!(v.iter().map(|x| x.0).collect::<Vec<_>>(), vec![0, 1, 2]);
    }

    #[test]
    fn fields_rank_and_fallback() {
        let c = |t: &str| Chunk { text: t.into(), ..Default::default() };
        let ch = vec![c("We lost the camera and the drone."), c("The ferry made big waves in the harbor."), c("Hallo Fähre")];
        // no map: identical to raw-text BM25
        assert_eq!(rank(&ch, &[], "ferry waves", 5, WEIGHTS), Bm25::new(ch.iter().map(|c| c.text.as_str())).search("ferry waves", 5).into_iter().map(|x| x.0).collect::<Vec<_>>());
        // German question only matches via the map fields
        assert!(rank(&ch, &[], "Welche Ausrüstung ging verloren?", 5, WEIGHTS).is_empty());
        let aux = vec![
            Aux { about: Some("Kamera und Drohne gingen verloren.".into()), questions: Some("Welche Ausrüstung ging verloren?".into()) },
            Aux { about: Some("Die Fähre erzeugte Wellen.".into()), questions: None },
            Aux::default(),
        ];
        assert_eq!(rank(&ch, &aux, "Welche Ausrüstung ging verloren?", 5, WEIGHTS)[0], 0);
        // partial maps: chunk without map still found via raw text
        assert_eq!(rank(&ch, &aux, "Hallo", 5, WEIGHTS), vec![2]);
        // selection returns chunk indices only (citations stay on raw text)
        let big: Vec<Chunk> = (0..3).map(|i| Chunk { text: "x ".repeat(2000) + &ch[i].text, ..Default::default() }).collect();
        assert_eq!(select_with(&big, &aux, Some("Welche Ausrüstung ging verloren?"), 4100), vec![0]);
    }

    #[test]
    fn bm25_ranks() {
        let docs = ["Der Hund bellt laut im Garten.", "Katzen schlafen viel.", "Hunde und Katzen spielen. Hund Hund."];
        let b = Bm25::new(docs);
        let r = b.search("Hund", 5);
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].0, 2);
        assert!(b.search("Elefant", 5).is_empty());
        assert_eq!(b.search("Katzen schlafen", 1)[0].0, 1);
    }

    #[test]
    fn select_budget() {
        let c: Vec<Chunk> = (0..10)
            .map(|i| Chunk { id: format!("s#{i}"), src: "s".into(), title: "t".into(), text: if i == 7 { "Apfel Birne".into() } else { "Text ohne Obst".into() }, ..Default::default() })
            .collect();
        assert_eq!(select(&c, Some("x"), 10_000).len(), 10);
        assert_eq!(select(&c, Some("Apfel"), 30), vec![7]);
        assert_eq!(select(&c, None, 30), vec![0, 9]);
    }
}
