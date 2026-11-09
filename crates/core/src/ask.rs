use crate::{Notebook, Prog, Store, llm, retrieval::{self, Chunk}, util};
use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

pub const NOT_FOUND: &str = "Das steht nicht in den Quellen.";

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Cite {
    pub n: usize,
    pub chunk: String,
    pub src: String,
    pub title: String,
    pub excerpt: String,
    /// start time of the cited passage in timed sources, "m:ss"
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub t0: Option<f64>,
}

impl Cite {
    /// "s1#3" or "s1#3, 12:34"
    pub fn loc(&self) -> String {
        match &self.at {
            Some(a) => format!("{}, {a}", self.chunk),
            None => self.chunk.clone(),
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Answer {
    pub ts: String,
    pub q: String,
    pub text: String,
    pub cites: Vec<Cite>,
}

/// Numbered source excerpts for the prompt; numbering is 1-based over `idx`.
pub fn context(chunks: &[Chunk], idx: &[usize]) -> String {
    idx.iter()
        .enumerate()
        .map(|(n, &i)| {
            let c = &chunks[i];
            let at = c.at().map(|a| format!(", ab {a}")).unwrap_or_default();
            format!("[{}] Quelle: {} ({}{at})\n{}\n", n + 1, c.title, c.id, c.text)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Citation numbers in order of first appearance: [1], [2, 3], [4-5], [1][2].
pub fn cite_nums(s: &str) -> Vec<usize> {
    let mut out = vec![];
    let mut rest = s;
    while let Some(a) = rest.find('[') {
        rest = &rest[a + 1..];
        let Some(b) = rest.find(']') else { break };
        let inner = &rest[..b];
        if !inner.is_empty() && inner.chars().all(|c| c.is_ascii_digit() || ", -–".contains(c)) {
            for part in inner.split(',') {
                let p: Vec<usize> = part.split(['-', '–']).filter_map(|x| x.trim().parse().ok()).collect();
                let r = match p[..] {
                    [a] => a..=a,
                    [a, b] if b >= a && b - a < 20 => a..=b,
                    _ => continue,
                };
                for n in r {
                    if !out.contains(&n) {
                        out.push(n);
                    }
                }
            }
        }
    }
    out
}

pub fn cites(text: &str, chunks: &[Chunk], idx: &[usize]) -> Vec<Cite> {
    let mut v: Vec<Cite> = cite_nums(text)
        .into_iter()
        .filter(|&n| n >= 1 && n <= idx.len())
        .map(|n| {
            let c = &chunks[idx[n - 1]];
            Cite { n, chunk: c.id.clone(), src: c.src.clone(), title: c.title.clone(), excerpt: util::clip(&c.text, 400), at: c.at(), t0: c.t0 }
        })
        .collect();
    v.sort_by_key(|c| c.n);
    v
}

/// BM25 selection; without keyword hits (synonyms, paraphrases) fall back to an even spread so the LLM still decides.
pub fn pick(chunks: &[Chunk], aux: &[retrieval::Aux], q: &str, budget: usize) -> Vec<usize> {
    let idx = retrieval::select_with(chunks, aux, Some(q), budget);
    if idx.is_empty() { retrieval::select(chunks, None, budget) } else { idx }
}

const SYS: &str = "Du bist ein sorgfältiger Recherche-Assistent. Antworte ausschließlich auf Basis der nummerierten Quellenausschnitte. \
Belege jede Aussage direkt mit der Nummer des Ausschnitts in eckigen Klammern, z. B. [1] oder [2, 3]. \
Erfinde nichts und nutze kein Vorwissen. Wenn die Ausschnitte die Frage nicht beantworten, antworte genau: «Das steht nicht in den Quellen.» \
Antworte auf Deutsch (außer die Frage ist in einer anderen Sprache), klar und knapp; Markdown ist erlaubt.";

pub fn ask(st: &Store, nb: &Notebook, q: &str, prog: Prog) -> Result<Answer> {
    let chunks = st.chunks(nb)?;
    if chunks.is_empty() {
        bail!("Keine aktiven Quellen im Notebook");
    }
    let idx = pick(&chunks, &st.aux(nb, &chunks), q, st.cfg.ctx_chars);
    let ans = if idx.is_empty() {
        Answer { ts: util::now(), q: q.into(), text: NOT_FOUND.into(), cites: vec![] }
    } else {
        funkhaus_sys::gpu::gpu_wait(prog);
        prog(&format!("Frage an LLM ({} Ausschnitte)", idx.len()));
        let mut msgs = vec![llm::sys(SYS)];
        let hist: Vec<Answer> = st.chat(nb)?;
        for a in hist.iter().rev().take(3).rev() {
            msgs.push(llm::user(a.q.clone()));
            msgs.push(llm::asst(a.text.clone()));
        }
        msgs.push(llm::user(format!("Quellenausschnitte:\n\n{}\n\nFrage: {q}", context(&chunks, &idx))));
        let text = llm::chat(&st.cfg, nb.backend, &msgs, None)?;
        let mut cites = cites(&text, &chunks, &idx);
        crate::cite::refine(st, nb, &text, &mut cites);
        Answer { ts: util::now(), q: q.into(), text, cites }
    };
    st.append_chat(nb, &ans)?;
    Ok(ans)
}

/// Three suggested questions answerable from the sources.
pub fn suggest(st: &Store, nb: &Notebook) -> Result<Vec<String>> {
    let chunks = st.chunks(nb)?;
    if chunks.is_empty() {
        bail!("Keine aktiven Quellen im Notebook");
    }
    let idx = retrieval::select(&chunks, None, st.cfg.ctx_chars.min(24_000));
    let schema = serde_json::json!({
        "type": "object",
        "properties": {"questions": {"type": "array", "minItems": 3, "maxItems": 3, "items": {"type": "string"}}},
        "required": ["questions"],
        "additionalProperties": false
    });
    let msgs = [
        llm::sys("Schlage drei kurze, konkrete Fragen auf Deutsch vor, die sich mit den Quellenausschnitten beantworten lassen und neugierig machen. Antworte als JSON."),
        llm::user(format!("Quellenausschnitte:\n\n{}", context(&chunks, &idx))),
    ];
    let v = llm::json(&st.cfg, nb.backend, &msgs, &schema)?;
    Ok(v["questions"].as_array().into_iter().flatten().filter_map(|q| q.as_str().map(|s| s.trim().to_string())).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pick_falls_back() {
        let ch: Vec<Chunk> = (0..10).map(|i| Chunk { id: format!("s1#{i}"), src: "s1".into(), title: "T".into(), text: "Wort ".repeat(20), ..Default::default() }).collect();
        assert!(retrieval::select(&ch, Some("Fahrrad"), 300).is_empty());
        assert!(!pick(&ch, &[], "Fahrrad", 300).is_empty());
    }

    #[test]
    fn nums() {
        assert_eq!(cite_nums("a [2] b [1, 3] c [4-5][2] [x] [] [10]"), vec![2, 1, 3, 4, 5, 10]);
        let ch: Vec<Chunk> = (0..3).map(|i| Chunk { id: format!("s1#{i}"), src: "s1".into(), title: "T".into(), text: format!("t{i}"), t0: (i == 2).then_some(754.0), ..Default::default() }).collect();
        let c = cites("x [2] y [9]", &ch, &[0, 2]);
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].chunk, "s1#2");
        assert_eq!(c[0].at.as_deref(), Some("12:34"));
        assert!(context(&ch, &[2]).contains("(s1#2, ab 12:34)"));
        // excerpt comes from the raw chunk text, never from map fields
        assert_eq!(c[0].excerpt, "t2");
    }
}
