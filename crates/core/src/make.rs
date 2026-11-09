use crate::{Notebook, Prog, Store, ask, guide, llm, retrieval, util};
use anyhow::{Result, bail};
use serde::Serialize;
use std::path::PathBuf;

pub const KINDS: &[&str] = &["summary", "faq", "briefing", "studyguide", "timeline"];

#[derive(Serialize, Debug)]
pub struct Doc {
    pub kind: String,
    pub path: PathBuf,
    pub markdown: String,
    pub cites: Vec<ask::Cite>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

pub fn title(kind: &str) -> &'static str {
    match kind {
        "summary" => "Zusammenfassung",
        "faq" => "FAQ",
        "briefing" => "Briefing",
        "studyguide" => "Lernhilfe",
        "timeline" => "Zeitleiste",
        _ => "Dokument",
    }
}

fn task(kind: &str) -> &'static str {
    match kind {
        "summary" => "Schreibe eine strukturierte Zusammenfassung: ein Absatz Überblick, dann die wichtigsten Punkte als Liste, dann ein Fazit.",
        "faq" => "Schreibe eine FAQ mit acht bis zwölf Fragen, die ein neugieriger Leser stellen würde, jeweils mit knapper Antwort (Format: ### Frage, dann Antwort).",
        "briefing" => "Schreibe ein Briefing-Dokument für Entscheider: Kernaussagen, wichtigste Fakten und Zahlen, offene Fragen und Risiken, empfohlene nächste Schritte.",
        "studyguide" => "Schreibe eine Lernhilfe: zentrale Begriffe mit Erklärung, zehn Quizfragen mit Lösungen am Ende, drei Essay-Fragen zum Weiterdenken.",
        "timeline" => "Erstelle eine chronologische Zeitleiste aller datierbaren Ereignisse (Datum – Ereignis), danach eine kurze Liste der wichtigsten beteiligten Personen oder Akteure.",
        _ => "",
    }
}

pub fn make(st: &Store, nb: &Notebook, kind: &str, prog: Prog) -> Result<Doc> {
    if !KINDS.contains(&kind) {
        bail!("unbekannte Art: {kind} (erlaubt: {})", KINDS.join(", "));
    }
    let chunks = st.chunks(nb)?;
    if chunks.is_empty() {
        bail!("Keine aktiven Quellen im Notebook");
    }
    let warnings: Vec<String> = Store::thin(nb).into_iter().collect();
    for w in &warnings {
        prog(&format!("WARNUNG: {w}"));
    }
    let (ctx, idx, how) = match guided(st, nb, &chunks, st.cfg.make_ctx_chars) {
        Some((c, i)) => (c, i, "Quellenführer + "),
        None => {
            let i = retrieval::select(&chunks, None, st.cfg.make_ctx_chars);
            (format!("Quellenausschnitte:\n\n{}", ask::context(&chunks, &i)), i, "")
        }
    };
    funkhaus_sys::gpu::gpu_wait(prog);
    prog(&format!("Erzeuge {} ({how}{} Ausschnitte)", title(kind), idx.len()));
    let msgs = [
        llm::sys(
            "Du erstellst Dokumente ausschließlich aus den nummerierten Quellenausschnitten und, falls vorhanden, dem Quellenführer. \
             Belege Aussagen mit [n]. Erfinde nichts. Schreibe auf Deutsch in sauberem Markdown, beginne mit einer Überschrift der Ebene eins.",
        ),
        llm::user(format!("{ctx}\n\nAufgabe: {}", task(kind))),
    ];
    let mut md = llm::chat(&st.cfg, nb.backend, &msgs, None)?;
    let mut cites = ask::cites(&md, &chunks, &idx);
    crate::cite::refine(st, nb, &md, &mut cites);
    if !cites.is_empty() {
        md.push_str("\n\n## Quellen\n\n");
        for c in &cites {
            md.push_str(&format!("[{}] {} ({})\n", c.n, c.title, c.loc()));
        }
    }
    let dir = st.out_dir(nb);
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{kind}-{}.md", util::stamp()));
    std::fs::write(&path, &md)?;
    Ok(Doc { kind: kind.into(), path, markdown: md, cites, warnings })
}

/// Context from current source guides (overview, topics, sections with key facts citing [n]) plus the raw
/// text of the most-cited chunks within `budget`; sources without a guide contribute an even spread.
/// None if no enabled source has a current guide (→ plain even spread).
fn guided(st: &Store, nb: &Notebook, chunks: &[retrieval::Chunk], budget: usize) -> Option<(String, Vec<usize>)> {
    let gs: Vec<guide::Guide> = nb.sources.iter().filter(|s| s.enabled).filter_map(|s| guide::load(st, nb, s)).collect();
    if gs.is_empty() {
        return None;
    }
    let pos = |id: &str| chunks.iter().position(|c| c.id == id);
    // chunk index → number of key facts citing it
    let mut hits: std::collections::BTreeMap<usize, usize> = Default::default();
    for f in gs.iter().flat_map(|g| &g.sections).flat_map(|s| &s.key_facts) {
        if let Some(i) = pos(&f.chunk_id) {
            *hits.entry(i).or_default() += 1;
        }
    }
    let rest: Vec<usize> = (0..chunks.len()).filter(|&i| !gs.iter().any(|g| g.source == chunks[i].src)).collect();
    let rc: Vec<retrieval::Chunk> = rest.iter().map(|&i| chunks[i].clone()).collect();
    let spread: Vec<usize> = if rc.is_empty() { vec![] } else { retrieval::select(&rc, None, budget / 3).into_iter().map(|j| rest[j]).collect() };
    let mut idx: Vec<usize> = hits.keys().copied().chain(spread.iter().copied()).collect();
    idx.sort();
    idx.dedup();
    let n = |i: usize| idx.iter().position(|&x| x == i).map_or(0, |p| p + 1);
    let mut g = String::from("Quellenführer:\n");
    for x in &gs {
        g.push_str(&format!("\n## {}\nÜberblick: {}\nThemen:\n", x.title, x.overview));
        for t in &x.topics {
            g.push_str(&format!("- {} ({})\n", t.title, t.section_ids.join(", ")));
        }
        for s in &x.sections {
            g.push_str(&format!("\n### {} {}{}\n{}\n", s.id, s.title, s.span(), s.summary));
            for f in &s.key_facts {
                g.push_str(&format!("- {}{}\n", f.text, pos(&f.chunk_id).map(|i| format!(" [{}]", n(i))).unwrap_or_default()));
            }
        }
    }
    // raw text: unguided spread first, then guided chunks by number of key facts citing them
    let mut by: Vec<(usize, usize)> = hits.into_iter().collect();
    by.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    let mut used = g.len();
    let mut raw: Vec<usize> = spread
        .into_iter()
        .chain(by.into_iter().map(|x| x.0))
        .filter(|&i| {
            let ok = used + chunks[i].text.len() <= budget;
            used += if ok { chunks[i].text.len() } else { 0 };
            ok
        })
        .collect();
    raw.sort();
    let body: Vec<String> = raw
        .iter()
        .map(|&i| {
            let c = &chunks[i];
            let at = c.at().map(|a| format!(", ab {a}")).unwrap_or_default();
            format!("[{}] Quelle: {} ({}{at})\n{}\n", n(i), c.title, c.id, c.text)
        })
        .collect();
    Some((format!("{g}\nQuellenausschnitte (Rohtext, Auswahl; Nummern wie oben):\n\n{}", body.join("\n")), idx))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guide_context_or_fallback() {
        let t = tempfile::tempdir().unwrap();
        let st = Store::at(t.path().join("home")).unwrap();
        let mut nb = st.create("G").unwrap();
        let f = t.path().join("a.txt");
        std::fs::write(&f, (0..1500).map(|i| format!("w{i} ")).collect::<String>()).unwrap();
        st.add_source(&mut nb, f.to_str().unwrap(), &|_| {}).unwrap();
        let chunks = st.chunks(&nb).unwrap();
        assert!(guided(&st, &nb, &chunks, 20_000).is_none());
        let sec = guide::Section {
            id: "a1".into(),
            chunk_ids: chunks.iter().map(|c| c.id.clone()).collect(),
            t0: None,
            t1: None,
            title: "Ein Abschnitt Titel".into(),
            summary: "Kurz.".into(),
            key_facts: vec![guide::KeyFact { text: "Fakt zwei".into(), chunk_id: chunks[2].id.clone() }],
            speakers: vec![],
            dropped: 0,
            issues: vec![],
            retries: 0,
        };
        let g = guide::Guide {
            v: guide::GUIDE_V,
            key: guide::cache_key(&chunks),
            source: nb.sources[0].id.clone(),
            title: "a".into(),
            overview: "Überblick X".into(),
            topics: vec![guide::Topic { title: "Thema".into(), section_ids: vec!["a1".into()] }],
            suggested_questions: vec![],
            sections: vec![sec],
            issues: vec![],
            retries: 0,
            model: "m".into(),
        };
        let p = st.dir(&nb.id).join("guide").join(format!("{}.json", g.source));
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, serde_json::to_string(&g).unwrap()).unwrap();
        let (ctx, idx) = guided(&st, &nb, &chunks, 20_000).unwrap();
        assert_eq!(idx, vec![2]);
        assert!(ctx.contains("Überblick X") && ctx.contains("- Fakt zwei [1]") && ctx.contains(&format!("[1] Quelle: a ({})", chunks[2].id)));
        // stale key → fallback
        std::fs::write(&p, serde_json::to_string(&guide::Guide { key: "x".into(), ..g }).unwrap()).unwrap();
        assert!(guided(&st, &nb, &chunks, 20_000).is_none());
    }
}
