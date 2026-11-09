use super::numbers;
use super::script::{Line, check, tidy};
use crate::{Notebook, Prog, Store, ask, llm, retrieval};
use anyhow::{Result, bail};
use serde_json::{Value, json};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Len {
    Kurz,
    Mittel,
    Lang,
}

impl Len {
    pub fn parse(s: &str) -> Result<Self> {
        Ok(match s {
            "kurz" | "short" => Len::Kurz,
            "mittel" | "medium" => Len::Mittel,
            "lang" | "long" => Len::Lang,
            _ => bail!("--len: kurz|mittel|lang"),
        })
    }
    /// (target words, min lines, max lines)
    pub fn spec(self) -> (usize, usize, usize) {
        match self {
            Len::Kurz => (220, 14, 32),
            Len::Mittel => (740, 40, 90),
            Len::Lang => (1800, 90, 200),
        }
    }
    /// Output cap per length (keeps the local GPU job bounded).
    pub fn max_tokens(self) -> u32 {
        match self {
            Len::Kurz => 2048,
            Len::Mittel => 4096,
            Len::Lang => 8192,
        }
    }
    fn describe(self) -> &'static str {
        match self {
            Len::Kurz => "kurz: etwa zweihundertzwanzig Wörter, etwa anderthalb Minuten, zwei bis vier B-Zeilen.",
            Len::Mittel => "mittel: etwa siebenhundertvierzig Wörter, etwa fünf Minuten.",
            Len::Lang => "lang: etwa achtzehnhundert Wörter, etwa zwölf Minuten.",
        }
    }
}

const SYSTEM: &str = include_str!("../../assets/podcast_system.txt");
const EX: [&str; 2] = [include_str!("../../assets/example_script.txt"), include_str!("../../assets/example_sauerteig.txt")];

fn schema(min: usize, max: usize) -> Value {
    json!({
        "type": "object",
        "properties": {
            "title": {"type": "string"},
            "lines": {
                "type": "array",
                "minItems": min,
                "maxItems": max,
                "items": {
                    "type": "object",
                    "properties": {
                        "who": {"type": "string", "enum": ["O", "F", "B"]},
                        "gap": {"type": "number"},
                        "text": {"type": "string"}
                    },
                    "required": ["who", "gap", "text"],
                    "additionalProperties": false
                }
            }
        },
        "required": ["title", "lines"],
        "additionalProperties": false
    })
}

pub fn system(o: &str, f: &str, lang: &str, len: Len) -> String {
    let lang = if lang.starts_with("de") { "Deutsch".to_string() } else { format!("Sprachcode {lang}") };
    SYSTEM.replace("{O}", o)
        .replace("{F}", f)
        .replace("{LEN}", len.describe())
        .replace("{LANG}", &lang)
        .replace("{EX1}", EX[0])
        .replace("{EX2}", EX[1])
}

pub(super) fn lines_of(v: &Value) -> Vec<Line> {
    let arr = v.get("lines").unwrap_or(v);
    arr.as_array()
        .into_iter()
        .flatten()
        .filter_map(|l| {
            let who = l["who"].as_str()?.trim().chars().next()?.to_ascii_uppercase();
            "OFB".contains(who).then(|| Line::ofb(who, l["gap"].as_f64().unwrap_or(0.1) as f32, l["text"].as_str().unwrap_or("")))
        })
        .collect()
}

/// Podcast context: BM25 hits for the focus, else (or without hits) an even spread over the sources.
pub fn pick(chunks: &[crate::retrieval::Chunk], aux: &[retrieval::Aux], focus: Option<&str>, budget: usize) -> Vec<usize> {
    let idx = retrieval::select_with(chunks, aux, focus, budget);
    if idx.is_empty() { retrieval::select(chunks, None, budget) } else { idx }
}

pub fn generate(st: &Store, nb: &Notebook, len: Len, focus: Option<&str>, lang: &str, prog: Prog) -> Result<(String, Vec<Line>)> {
    let chunks = st.chunks(nb)?;
    if chunks.is_empty() {
        bail!("Keine aktiven Quellen im Notebook");
    }
    let idx = pick(&chunks, &st.aux(nb, &chunks), focus, st.cfg.podcast_ctx_chars);
    let (words, min, max) = len.spec();
    let msgs = vec![
        llm::sys(system(&st.cfg.hosts.o.name, &st.cfg.hosts.f.name, lang, len)),
        llm::user(format!(
            "Länge: {}\nFokus: {}\nSprache: {lang}\n\nQuellen:\n\n{}\n\nSchreibe jetzt das Podcast-Skript als JSON.",
            match len { Len::Kurz => "kurz", Len::Mittel => "mittel", Len::Lang => "lang" },
            focus.unwrap_or("keiner"),
            ask::context(&chunks, &idx)
        )),
    ];
    funkhaus_sys::gpu::gpu_wait(prog);
    prog(&format!("Schreibe Podcast-Skript ({} Ausschnitte, ~{words} Wörter)", idx.len()));
    let schema = schema(min, max);
    let mut cfg = st.cfg.clone();
    cfg.llm.temperature = cfg.llm.temperature.min(0.6);
    cfg.llm.max_tokens = len.max_tokens();
    let chars: usize = msgs.iter().map(|m| m.content.len()).sum();
    prog(&format!("Prompt: {chars} Zeichen, max_tokens {}", cfg.llm.max_tokens));
    let v = llm::json(&cfg, nb.backend, &msgs, &schema)?;
    let mut lines = tidy(lines_of(&v));
    let src: String = idx.iter().map(|&i| chunks[i].text.as_str()).collect::<Vec<_>>().join("\n");
    let audit = |l: &[Line]| [check(l, words), numbers::unsupported(l, &src)].concat();
    let errs = audit(&lines);
    if !errs.is_empty() {
        prog(&format!("Skript hat {} Mängel, korrigiere", errs.len()));
        cfg.llm.temperature = 0.2;
        let numbered: String = lines.iter().enumerate().map(|(i, l)| format!("{}: {}|{}\n", i + 1, l.mark(), l.text)).collect();
        let fix = [
            msgs[1].clone(),
            llm::user(format!(
                "Hier ist ein Podcast-Skript (Zeilennummer: Sprecher|Text):\n{numbered}\nMängel:\n- {}\n\n\
                 Korrigiere nur die betroffenen Zeilen anhand der Quellen (Zahlen exakt wie in den Quellen, als Wörter ausgeschrieben; \
                 im Zweifel die Zahl weglassen). Gib als JSON nur die geänderten Zeilen zurück: {{\"fixes\": [{{\"line\": Nummer, \"text\": \"neuer Text\"}}]}}.",
                errs.iter().take(25).cloned().collect::<Vec<_>>().join("\n- ")
            )),
        ];
        let fs = json!({
            "type": "object",
            "properties": {"fixes": {"type": "array", "items": {"type": "object",
                "properties": {"line": {"type": "integer"}, "text": {"type": "string"}},
                "required": ["line", "text"], "additionalProperties": false}}},
            "required": ["fixes"], "additionalProperties": false
        });
        match llm::json(&cfg, nb.backend, &fix, &fs) {
            Ok(f) => {
                let mut l2 = lines.clone();
                for x in f["fixes"].as_array().into_iter().flatten() {
                    if let (Some(n), Some(t)) = (x["line"].as_u64(), x["text"].as_str())
                        && let Some(l) = l2.get_mut((n as usize).wrapping_sub(1)) {
                            l.text = t.to_string();
                        }
                }
                let l2 = tidy(l2);
                let n2 = audit(&l2).len();
                prog(&format!("nach Korrektur: {n2} Mängel"));
                if l2.len() >= 4 && n2 < errs.len() {
                    lines = l2;
                }
            }
            Err(e) => prog(&format!("Korrektur fehlgeschlagen: {e:#}")),
        }
    }
    if lines.len() < 4 {
        bail!("LLM lieferte zu wenige Skriptzeilen ({})", lines.len());
    }
    Ok((v["title"].as_str().filter(|s| !s.trim().is_empty()).unwrap_or(&nb.name).trim().to_string(), lines))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::retrieval::Chunk;

    #[test]
    fn podcast_budget_spreads() {
        let text: String = (0..5500).map(|i| format!("wort{i} ")).collect(); // ≈ 11 chunks, ~60k chars
        let ch = crate::retrieval::chunk("s1", "T", &text);
        let idx = pick(&ch, &[], None, 14_000);
        let used: usize = idx.iter().map(|&i| ch[i].text.len()).sum();
        assert!(used <= 14_000, "{used}");
        assert_eq!(idx.first(), Some(&0));
        assert_eq!(idx.last(), Some(&(ch.len() - 1)));
        assert!(idx.len() >= 2);
        let f = pick(&ch, &[], Some("wort5400"), 14_000);
        assert_eq!(f, vec![ch.len() - 1]);
        let _: &Chunk = &ch[0];
    }

    #[test]
    fn tokens_per_len_and_two_examples() {
        assert_eq!([Len::Kurz, Len::Mittel, Len::Lang].map(Len::max_tokens), [2048, 4096, 8192]);
        let s = system("O", "F", "de", Len::Kurz);
        assert!(!s.contains("{EX") && !s.contains("Beispiel drei") && !s.contains("Tobi"));
        assert!(s.len() < 8500, "{}", s.len()); // was ≈ 9.9k bytes with three examples
    }
}
