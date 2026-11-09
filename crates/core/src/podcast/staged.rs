//! M2.2 three-stage podcast for long notebooks: OUTLINE (source guides → segments with section/chunk ids
//! and beats) → SCRIPT per segment over the segment's raw chunks → joined script with `# SEG` markers.
//! Post-validated per docs/m2/eval-reduce.md (O1–O9, S1–S11) with one retry per call.
use super::draft::{Len, lines_of};
use super::numbers;
use super::script::{Line, tidy};
use crate::guide::{self, Check, Guide};
use crate::retrieval::{Chunk, tokens};
use crate::{Notebook, Prog, Store, llm, util};
use anyhow::{Result, bail};
use funkhaus_audio::script::{count_tags, plain, segments};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashSet;

/// Raw excerpt budget per segment call (chars) → prompt + output < ~7k tokens.
pub const SEG_CHARS: usize = 10_000;
/// Pause before the first line of segment ≥ 2 (end of the previous chapter).
pub const SEG_GAP: f32 = 0.8;
/// Fallback word band per segment (outlines without `words`).
pub const SEG_WORDS: (usize, usize) = (120, 300);

/// Word band per segment by episode length.
pub fn band(len: Len) -> (usize, usize) {
    match len {
        Len::Kurz => (60, 100),
        Len::Mittel => (120, 200),
        Len::Lang => (150, 300),
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Sec {
    pub id: String,
    pub title: String,
    pub chunk_ids: Vec<String>,
    /// chunks cited by the section's key facts
    pub kf_chunks: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Seg {
    pub title: String,
    pub goal: String,
    pub section_ids: Vec<String>,
    pub chunk_ids: Vec<String>,
    pub beats: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Outline {
    pub title: String,
    pub len: String,
    pub want: usize,
    pub focus: Option<String>,
    pub sections: Vec<Sec>,
    pub topics: Vec<guide::Topic>,
    pub segments: Vec<Seg>,
    /// fewer sections than segments: sections may repeat, chunk ids stay disjoint
    #[serde(default)]
    pub shared: bool,
    /// chunk ids per segment (lower bound; 0 = 3)
    #[serde(default)]
    pub min_chunks: usize,
    /// words per segment (0,0 = SEG_WORDS)
    #[serde(default)]
    pub words: (usize, usize),
    pub retries: usize,
    pub issues: Vec<String>,
    pub secs: f64,
}

impl Outline {
    pub fn lo(&self) -> usize {
        if self.min_chunks == 0 { 3 } else { self.min_chunks }
    }
    /// max chunk ids per segment: split evenly when shared
    pub fn hi(&self) -> usize {
        if self.shared { self.lo() } else { 6 }
    }
    pub fn band(&self) -> (usize, usize) {
        if self.words.1 == 0 { SEG_WORDS } else { self.words }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct SegScript {
    pub no: usize,
    pub excerpts: String,
    pub lines: Vec<Line>,
    pub retries: usize,
    pub issues: Vec<String>,
    pub secs: f64,
}

pub fn segments_for(len: Len) -> usize {
    match len {
        Len::Kurz => 3,
        Len::Mittel => 6,
        Len::Lang => 10,
    }
}

fn len_name(len: Len) -> &'static str {
    match len {
        Len::Kurz => "kurz",
        Len::Mittel => "mittel",
        Len::Lang => "lang",
    }
}

/// Current guides of all enabled sources, if the notebook is too long for single-shot and every source has one.
pub fn guides(st: &Store, nb: &Notebook) -> Option<Vec<Guide>> {
    let act: Vec<_> = nb.sources.iter().filter(|s| s.enabled).collect();
    let chars: usize = st.chunks(nb).ok()?.iter().map(|c| c.text.len()).sum();
    if act.is_empty() || chars <= st.cfg.podcast_ctx_chars {
        return None;
    }
    let gs: Option<Vec<Guide>> = act.iter().map(|s| guide::load(st, nb, s)).collect();
    // context level Aus: the source is not offered to the outline
    gs.map(|v| v.into_iter().filter(|g| act.iter().any(|s| s.id == g.source && s.ctx != crate::store::Ctx::Aus)).collect::<Vec<_>>()).filter(|v| !v.is_empty())
}

/// Section manifest with notebook-wide ids (`<source>.<section>`).
pub fn manifest(gs: &[Guide]) -> (Vec<Sec>, Vec<guide::Topic>) {
    let gid = |g: &Guide, s: &str| format!("{}.{s}", g.source);
    let secs = gs
        .iter()
        .flat_map(|g| {
            g.sections.iter().map(move |s| {
                let mut kf: Vec<String> = vec![];
                for c in s.chunk_ids.iter().filter(|c| s.key_facts.iter().any(|f| &f.chunk_id == *c)) {
                    kf.push(c.clone());
                }
                Sec { id: gid(g, &s.id), title: s.title.clone(), chunk_ids: s.chunk_ids.clone(), kf_chunks: kf }
            })
        })
        .collect();
    let topics = gs
        .iter()
        .flat_map(|g| g.topics.iter().map(move |t| guide::Topic { title: t.title.clone(), section_ids: t.section_ids.iter().map(|s| gid(g, s)).collect() }))
        .collect();
    (secs, topics)
}

// ---------- checks ----------

fn words(s: &str) -> usize {
    s.split_whitespace().filter(|w| w.chars().any(char::is_alphabetic)).count()
}

fn sentences(s: &str) -> usize {
    let mut n = 0;
    let mut prev = false;
    for c in s.chars() {
        let p = ".!?".contains(c);
        if p && !prev {
            n += 1;
        }
        prev = p;
    }
    n.max(1)
}

/// Topics matching the focus (shared token with the title); all topics without focus.
fn focus_topics<'a>(topics: &'a [guide::Topic], focus: Option<&str>) -> Vec<&'a guide::Topic> {
    match focus.map(tokens).filter(|f| !f.is_empty()) {
        Some(f) => topics.iter().filter(|t| tokens(&t.title).iter().any(|x| f.contains(x))).collect(),
        None => topics.iter().collect(),
    }
}

/// O1–O9 (eval-reduce.md §4). O7 is informational.
pub fn outline_checks(o: &Outline) -> Vec<(&'static str, bool, String)> {
    let order: Vec<&String> = o.sections.iter().flat_map(|s| &s.chunk_ids).collect();
    let pos = |c: &String| order.iter().position(|x| *x == c);
    let sec = |id: &String| o.sections.iter().find(|s| &s.id == id);
    let segs = &o.segments;
    let mut bad2 = vec![];
    for (k, s) in segs.iter().enumerate() {
        let own: Vec<&Sec> = s.section_ids.iter().filter_map(sec).collect();
        for c in &s.chunk_ids {
            if !own.iter().any(|x| x.chunk_ids.contains(c) && (o.shared || x.kf_chunks.contains(c))) {
                bad2.push(format!("seg{} {c}", k + 1));
            }
        }
    }
    let (lo, hi) = (o.lo(), o.hi());
    let bad3: Vec<String> = segs.iter().enumerate().filter(|(_, s)| !(lo..=hi).contains(&s.chunk_ids.len())).map(|(k, s)| format!("seg{} {}", k + 1, s.chunk_ids.len())).collect();
    let dups = |f: &dyn Fn(&Seg) -> &Vec<String>| -> Vec<String> {
        let mut seen = HashSet::new();
        segs.iter().flat_map(|s| f(s).iter().collect::<HashSet<_>>()).filter(|c| !seen.insert(*c)).cloned().collect()
    };
    let bad6: Vec<String> = segs
        .iter()
        .enumerate()
        .filter(|(_, s)| s.chunk_ids.windows(2).any(|w| pos(&w[0]) >= pos(&w[1])))
        .map(|(k, _)| format!("seg{}", k + 1))
        .collect();
    let firsts: Vec<Option<usize>> = segs.iter().map(|s| s.chunk_ids.first().and_then(|c| pos(c))).collect();
    let inv = firsts.windows(2).filter(|w| w[1] < w[0]).count();
    let ft = focus_topics(&o.topics, o.focus.as_deref());
    let covered = ft.iter().filter(|t| segs.iter().any(|s| s.section_ids.iter().any(|x| t.section_ids.contains(x)))).count();
    let need = if o.focus.is_some() { ft.len() } else { (ft.len() as f64 * 0.7).ceil() as usize };
    let bad9: Vec<String> = segs.iter().enumerate().filter(|(_, s)| !(3..=5).contains(&s.beats.len())).map(|(k, s)| format!("seg{} {}", k + 1, s.beats.len())).collect();
    vec![
        ("O1", segs.len() == o.want, format!("{} segments, want {} ({})", segs.len(), o.want, o.len)),
        ("O2", bad2.is_empty(), format!("not in own {}chunks: {bad2:?}", if o.shared { "" } else { "key-fact " })),
        ("O3", bad3.is_empty(), format!("chunk count outside {lo}-{hi}: {bad3:?}")),
        ("O4", dups(&|s| &s.chunk_ids).is_empty(), format!("dup chunks {:?}", dups(&|s| &s.chunk_ids))),
        // shared: sections may repeat (fewer sections than segments), O4 keeps chunks disjoint
        ("O5", o.shared || dups(&|s| &s.section_ids).is_empty(), format!("dup sections {:?}{}", dups(&|s| &s.section_ids), if o.shared { " (shared, allowed)" } else { "" })),
        ("O6", bad6.is_empty(), format!("out of order: {bad6:?}")),
        ("O7", true, format!("inversions={inv} (info)")),
        ("O8", covered >= need, format!("{covered}/{} topics covered, need {need}", ft.len())),
        ("O9", bad9.is_empty(), format!("beats outside 3-5: {bad9:?}")),
    ]
}

/// Keeps valid, unused, ordered ids; tops up to `lo` chunks from the segment's own (key-fact) chunks; drops empty segments.
pub fn repair(o: &mut Outline) {
    if o.shared {
        return repair_shared(o);
    }
    let order: Vec<String> = o.sections.iter().flat_map(|s| s.chunk_ids.clone()).collect();
    let pos = |c: &String| order.iter().position(|x| x == c).unwrap_or(usize::MAX);
    let (mut used_s, mut used_c) = (HashSet::new(), HashSet::new());
    let (n, lo) = (o.segments.len(), o.lo());
    for (k, s) in o.segments.iter_mut().enumerate() {
        // leave at least one unused section for every later segment
        let free = o.sections.iter().filter(|x| !used_s.contains(&x.id)).count();
        let cap = free.saturating_sub(n - k - 1).max(1);
        s.section_ids.retain(|x| o.sections.iter().any(|y| &y.id == x) && !used_s.contains(x));
        s.section_ids.dedup();
        s.section_ids.truncate(cap);
        if s.section_ids.is_empty()
            && let Some(x) = o.sections.iter().find(|x| !used_s.contains(&x.id))
        {
            s.section_ids.push(x.id.clone());
        }
        used_s.extend(s.section_ids.iter().cloned());
        let own: Vec<&Sec> = o.sections.iter().filter(|x| s.section_ids.contains(&x.id)).collect();
        let ok = |c: &String| own.iter().any(|x| x.kf_chunks.contains(c));
        s.chunk_ids.retain(|c| ok(c) && !used_c.contains(c));
        s.chunk_ids.sort_by_key(|c| pos(c));
        s.chunk_ids.dedup();
        for c in own.iter().flat_map(|x| &x.kf_chunks) {
            if s.chunk_ids.len() >= lo {
                break;
            }
            if !s.chunk_ids.contains(c) && !used_c.contains(c) {
                s.chunk_ids.push(c.clone());
            }
        }
        s.chunk_ids.sort_by_key(|c| pos(c));
        s.chunk_ids.truncate(6);
        used_c.extend(s.chunk_ids.iter().cloned());
        s.beats.truncate(5);
    }
    o.segments.retain(|s| !s.chunk_ids.is_empty());
}

/// Shared mode: sections may repeat; each segment gets `lo` disjoint chunks of its own sections (key-fact chunks first).
fn repair_shared(o: &mut Outline) {
    let order: Vec<String> = o.sections.iter().flat_map(|s| s.chunk_ids.clone()).collect();
    let pos = |c: &String| order.iter().position(|x| x == c).unwrap_or(usize::MAX);
    let (lo, mut used) = (o.lo(), HashSet::new());
    for s in &mut o.segments {
        s.section_ids.retain(|x| o.sections.iter().any(|y| &y.id == x));
        s.section_ids.dedup();
        let free = |x: &Sec, u: &HashSet<String>| x.chunk_ids.iter().filter(|c| !u.contains(*c)).count();
        if s.section_ids.iter().filter_map(|id| o.sections.iter().find(|x| &x.id == id)).all(|x| free(x, &used) == 0)
            && let Some(x) = o.sections.iter().filter(|x| free(x, &used) > 0).max_by_key(|x| (free(x, &used), std::cmp::Reverse(pos(&x.chunk_ids[0]))))
        {
            s.section_ids = vec![x.id.clone()];
        }
        let own: Vec<&Sec> = o.sections.iter().filter(|x| s.section_ids.contains(&x.id)).collect();
        s.chunk_ids.retain(|c| own.iter().any(|x| x.chunk_ids.contains(c)) && !used.contains(c));
        s.chunk_ids.dedup();
        let cand = own.iter().flat_map(|x| x.kf_chunks.iter()).chain(own.iter().flat_map(|x| x.chunk_ids.iter()));
        for c in cand {
            if s.chunk_ids.len() >= lo {
                break;
            }
            if !s.chunk_ids.contains(c) && !used.contains(c) {
                s.chunk_ids.push(c.clone());
            }
        }
        s.chunk_ids.sort_by_key(|c| pos(c));
        s.chunk_ids.truncate(lo);
        used.extend(s.chunk_ids.iter().cloned());
        s.beats.truncate(5);
    }
    o.segments.retain(|s| !s.chunk_ids.is_empty());
}

const ABBR: &[&str] = &["z.B.", "z. B.", "bzw.", "usw.", "etc.", "ca.", "Nr.", "Dr."];
const GREET: &[&str] = &["hallo", "hi", "hey", "servus", "moin", "willkommen", "guten tag", "guten morgen", "guten abend"];

/// S1–S11 (eval-reduce.md §5) for one segment; `excerpts` = its source text, `last` = final segment.
pub fn script_checks(lines: &[Line], excerpts: &str, last: bool, band: (usize, usize)) -> Vec<(&'static str, bool, String)> {
    let n = lines.len();
    let mut s1 = vec![];
    for (i, l) in lines.iter().enumerate() {
        let t = plain(&l.text);
        let pause = segments(&l.text).err().map(|_| "pause".to_string());
        let bad = t.chars().find(|c| c.is_ascii_digit() || "%€$&#*_/\\()[]{}<>|@=+~^`".contains(*c)).map(|c| c.to_string());
        let acr = t.split(|c: char| !c.is_alphabetic()).find(|w| w.chars().count() >= 2 && w.chars().all(|c| c.is_uppercase())).map(String::from);
        let ab = ABBR.iter().find(|a| t.contains(*a)).map(|a| a.to_string());
        let url = (t.contains("http") || t.contains("://")).then(|| "url".to_string());
        if let Some(x) = pause.or(bad).or(acr).or(ab).or(url) {
            s1.push(format!("line {} '{x}'", i + 1));
        }
    }
    let s2: Vec<String> = lines
        .iter()
        .enumerate()
        .filter(|(_, l)| l.over)
        .filter(|&(i, l)| words(&l.text) > 3 || i == 0 || i + 1 == n || lines[i - 1].over || !(0.3..=0.8).contains(&l.gap))
        .map(|(i, _)| format!("line {}", i + 1))
        .collect();
    let of: Vec<&Line> = lines.iter().skip(1).filter(|l| !l.over).collect();
    let off: Vec<String> = of
        .iter()
        .filter(|l| !((0.05..=0.15).contains(&l.gap) || (-0.20..=-0.10).contains(&l.gap)))
        .map(|l| format!("{}", l.gap))
        .collect();
    let neg = of.iter().filter(|l| l.gap < 0.0).count();
    let s3 = lines.first().is_some_and(|l| l.gap == 0.0) && off.is_empty() && neg as f64 <= 0.3 * of.len().max(1) as f64;
    let mut run = (0, (false, ""), 0usize);
    for l in lines {
        let k = (l.over, l.who.as_str());
        run.2 = if k == run.1 { run.2 + 1 } else { 1 };
        run.1 = k;
        run.0 = run.0.max(run.2);
    }
    let s5: Vec<String> = lines
        .iter()
        .enumerate()
        .filter(|(_, l)| words(&plain(&l.text)) > 35 || sentences(&plain(&l.text)) > 3)
        .map(|(i, _)| format!("line {}", i + 1))
        .collect();
    let laugh: usize = lines.iter().map(|l| l.text.matches("[laughter]").count()).sum();
    let tags: usize = lines.iter().map(|l| count_tags(&l.text)).sum();
    let total: usize = lines.iter().map(|l| words(&plain(&l.text))).sum();
    let s8 = numbers::unsupported(lines, excerpts);
    let greet: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, l)| {
            let t = l.text.trim().to_lowercase();
            GREET.iter().any(|g| t.strip_prefix(g).is_some_and(|r| !r.starts_with(|c: char| c.is_alphanumeric())))
        })
        .map(|(i, _)| i + 1)
        .collect();
    let end = lines.last().map(|l| plain(&l.text).trim().to_string()).unwrap_or_default();
    let s10 = if last { words(&end) <= 15 } else { end.ends_with('?') || end.ends_with('…') || end.ends_with(':') };
    let s11 = n > 0 && lines.iter().all(|l| (l.over || matches!(l.who.as_str(), "O" | "F")) && !l.text.trim().is_empty());
    vec![
        ("S1", s1.is_empty(), format!("forbidden {s1:?}")),
        ("S2", s2.is_empty(), format!("B rule broken {s2:?}")),
        ("S3", s3, format!("line0 gap {:?}; off-band {off:?}; negatives {neg}/{}", lines.first().map(|l| l.gap), of.len())),
        ("S4", run.0 <= 2, format!("max run {}", run.0)),
        ("S5", s5.is_empty(), format!("too long {s5:?}")),
        ("S6", laugh <= 1 && tags <= 2, format!("[laughter] count {laugh}, non-verbal tags {tags}")),
        ("S7", (band.0..=band.1).contains(&total), format!("{total} words, band {}-{}", band.0, band.1)),
        ("S8", s8.is_empty(), format!("{s8:?}")),
        ("S9", greet.is_empty(), format!("greeting lines {greet:?}")),
        ("S10", s10, format!("{} segment ends '{}'", if last { "last" } else { "non-last" }, util::clip(&end, 60))),
        ("S11", s11, format!("{n} lines")),
    ]
}

fn fails(cs: &[(&'static str, bool, String)], skip: &[&str]) -> Vec<String> {
    cs.iter().filter(|c| !c.1 && !skip.contains(&c.0)).map(|c| format!("{} {}", c.0, c.2)).collect()
}

// ---------- prompts ----------

fn outline_system(shared: bool, lo: usize, hi: usize) -> String {
    let rule = if shared {
        format!("Es gibt weniger Abschnitte als Segmente: ein Abschnitt darf in mehreren Segmenten stehen, jede chunk_id aber nur in einem Segment. chunk_ids: genau {lo}, aus den Chunks der eigenen Abschnitte, in Reihenfolge.")
    } else {
        format!("Jeder Abschnitt in höchstens einem Segment, jede chunk_id in höchstens einem Segment. chunk_ids: {lo} bis {hi}, nur aus den key_facts der eigenen Abschnitte, in Reihenfolge.")
    };
    SYS_OUTLINE.replace("{RULE}", &rule)
}

const SYS_OUTLINE: &str = "Du planst eine Podcast-Episode für zwei Hosts (O und F, Stil „Projekt-Funk“). Noch kein Skript. \
EINGABE: Quellenführer, Abschnitte (id, title, key_facts mit chunk_id), Fokus, Segmentzahl. \
Bogen: Segment eins öffnet mit einem überraschenden Fakt oder Widerspruch; die Mitte je ein Thema mit Aha-Moment; das letzte Segment führt zum Fazit. \
Ein Fokus bestimmt die Auswahl; ohne Fokus die Themen gleichmäßig abdecken. {RULE} \
title: drei bis sechs Wörter, Deutsch (Kapitelname). goal: ein Satz. section_ids: ein bis drei Abschnitte. \
beats: drei bis fünf Stichpunkte aus den key_facts bzw. Chunks des Segments. \
Keine erfundenen IDs oder Fakten. Nur JSON.";

fn outline_schema(n: usize) -> Value {
    json!({"type": "object", "properties": {
        "title": {"type": "string"},
        "segments": {"type": "array", "minItems": n, "maxItems": n, "items": {"type": "object", "properties": {
            "title": {"type": "string"}, "goal": {"type": "string"},
            "section_ids": {"type": "array", "items": {"type": "string"}},
            "chunk_ids": {"type": "array", "items": {"type": "string"}},
            "beats": {"type": "array", "items": {"type": "string"}}
        }, "required": ["title", "goal", "section_ids", "chunk_ids", "beats"], "additionalProperties": false}}
    }, "required": ["title", "segments"], "additionalProperties": false})
}

pub fn outline_input(gs: &[Guide], topics: &[guide::Topic], focus: Option<&str>, n: usize, shared: bool, chunks: &[Chunk]) -> String {
    let render = |nf: usize| {
        let mut s = String::from("Quellenführer:\n");
        for g in gs {
            s.push_str(&format!("{}: {}\n", g.title, g.overview));
        }
        s.push_str("\nThemen:\n");
        for t in topics {
            s.push_str(&format!("- {} ({})\n", t.title, t.section_ids.join(", ")));
        }
        s.push_str("\nAbschnitte:\n");
        for (g, x) in gs.iter().flat_map(|g| g.sections.iter().map(move |x| (g, x))) {
            s.push_str(&format!("{}.{}{} – {}\n", g.source, x.id, x.span(), x.title));
            if shared {
                let at = |id: &String| chunks.iter().find(|c| &c.id == id).and_then(|c| c.at()).map(|a| format!(" {a}")).unwrap_or_default();
                s.push_str(&format!("Chunks: {}\n", x.chunk_ids.iter().map(|c| format!("{c}{}", at(c))).collect::<Vec<_>>().join(", ")));
            }
            for f in x.key_facts.iter().take(nf) {
                s.push_str(&format!("- [{}] {}\n", f.chunk_id, f.text));
            }
        }
        let k: usize = gs.iter().map(|g| g.sections.len()).sum();
        let how = if shared { "Abschnitte dürfen sich wiederholen, chunk_ids nicht" } else { "jeden Abschnitt höchstens einem Segment zuordnen" };
        s.push_str(&format!("\nFokus: {}\nSegmente: genau {n} ({k} Abschnitte – {how})\n", focus.unwrap_or("keiner")));
        s
    };
    [6, 3, 2].into_iter().map(render).find(|s| s.len() <= 14_000).unwrap_or_else(|| util::clip(&render(1), 14_000))
}

fn segs_of(v: &Value) -> Vec<Seg> {
    let strs = |x: &Value| -> Vec<String> {
        x.as_array().into_iter().flatten().filter_map(|y| y.as_str()).map(|y| y.trim().trim_matches(['[', ']']).to_string()).filter(|y| !y.is_empty()).collect()
    };
    v["segments"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|s| Seg {
            title: s["title"].as_str().unwrap_or("").trim().into(),
            goal: s["goal"].as_str().unwrap_or("").trim().into(),
            section_ids: strs(&s["section_ids"]),
            chunk_ids: strs(&s["chunk_ids"]).into_iter().map(|c| c.split_whitespace().next().unwrap_or("").to_string()).collect(),
            beats: strs(&s["beats"]),
        })
        .collect()
}

/// Excerpts "[id mm:ss] text", each chunk clipped to an equal share of SEG_CHARS.
pub fn excerpts(chunks: &[&Chunk]) -> String {
    let share = SEG_CHARS / chunks.len().max(1);
    chunks
        .iter()
        .map(|c| {
            let at = c.at().map(|a| format!(" {a}")).unwrap_or_default();
            format!("[{}{at}] {}", c.id, util::clip(&c.text, share))
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

const EX: &str = include_str!("../../assets/example_sauerteig.txt");

pub fn seg_system(no: usize, total: usize, o: &str, f: &str, band: (usize, usize)) -> String {
    let words = (band.0 + band.1) / 2;
    let first = if no == 1 {
        "Zeile eins ist ein Hook: ein überraschender Fakt oder Widerspruch aus der QUELLE."
    } else {
        "Die erste Zeile knüpft an die VORHERIGEN ZEILEN an, ohne sie zu wiederholen."
    };
    let end = if no == total {
        "Vorletzte Zeile: Fazit („Also: …“). Letzte Zeile: knappe Verabschiedung, höchstens zehn Wörter."
    } else {
        "Die letzte Zeile ist eine Frage oder Überleitung zum nächsten Thema und endet mit einem Fragezeichen."
    };
    let ex: String = EX.lines().take(10).collect::<Vec<_>>().join("\n");
    format!(
        "Du schreibst Segment {no} von {total} eines Podcasts für zwei Hosts im Stil „Projekt-Funk“. \
O = {o} (ruhig, analytisch, gibt Fehler offen zu), F = {f} (scharf, direkt, witzig). B = kurzer Einwurf des anderen Hosts („Mhm.“, „Echt jetzt?“).\n\
REGELN\n\
- Nur Fakten aus der QUELLE. Erfinde keine Zahlen, Namen, Zitate oder Ursachen.\n\
- Zahlen als Wörter („sieben Minuten“). Keine Ziffern, Symbole, Abkürzungen, Akronyme in Großbuchstaben, Klammern, URLs, Markdown oder Quellen-IDs.\n\
- Kein Gruß, kein Willkommen. {first}\n\
- Keine Wiederholungsschleifen, kein Lob, keine Floskeln. Nachfragen und Widerspruch statt Zustimmung.\n\
- {end}\n\
- Turns ein bis drei Sätze, höchstens fünfunddreißig Wörter; nie mehr als zwei Zeilen desselben Sprechers in Folge.\n\
- B: höchstens drei Wörter, nie erste oder letzte Zeile, nie direkt nach einem B.\n\
- gap: O/F meist 0.1 (0.05 bis 0.15), selten -0.15 bei einer Unterbrechung; B 0.3 bis 0.8; erste Zeile 0.\n\
- [laughter] höchstens einmal.\n\
- Länge: etwa {words} Wörter (zwischen {} und {}).\n\
- Nur JSON nach Schema, Deutsch.\n\n\
STILBEISPIEL (Format WER|GAP|TEXT; nur Ton und Rhythmus übernehmen, keine Inhalte):\n{ex}",
        band.0, band.1
    )
}

fn seg_schema() -> Value {
    json!({"type": "object", "properties": {"lines": {"type": "array", "minItems": 6, "maxItems": 40, "items": {"type": "object",
        "properties": {"who": {"type": "string", "enum": ["O", "F", "B"]}, "gap": {"type": "number"}, "text": {"type": "string"}},
        "required": ["who", "gap", "text"], "additionalProperties": false}}}, "required": ["lines"], "additionalProperties": false})
}

fn secs_since(t: std::time::Instant) -> f64 {
    (t.elapsed().as_secs_f64() * 10.0).round() / 10.0
}

/// Script text with `# SEG n | title | goal` markers; first line of segment ≥ 2 gets SEG_GAP.
pub fn join(o: &Outline, scripts: &[SegScript]) -> (String, Vec<Line>) {
    let (mut txt, mut all) = (String::new(), vec![]);
    for (s, x) in o.segments.iter().zip(scripts) {
        txt.push_str(&format!("# SEG {} | {} | {}\n", x.no, s.title.replace(['|', '\n'], " "), s.goal.replace(['|', '\n'], " ")));
        for (i, l) in x.lines.iter().enumerate() {
            let mut l = l.clone();
            if i == 0 && x.no > 1 {
                l.gap = SEG_GAP;
            }
            txt.push_str(&super::script::format(std::slice::from_ref(&l)));
            all.push(l);
        }
    }
    (txt, all)
}

pub struct Staged {
    pub title: String,
    pub outline: Outline,
    pub scripts: Vec<SegScript>,
}

pub fn generate(st: &Store, nb: &Notebook, gs: &[Guide], len: Len, focus: Option<&str>, prog: Prog) -> Result<Staged> {
    let chunks = st.chunks(nb)?;
    let (secs, topics) = manifest(gs);
    let kf: HashSet<&String> = secs.iter().flat_map(|s| &s.kf_chunks).collect();
    let n = segments_for(len);
    // fewer sections than segments: share sections, split their chunks (all chunks, not only key-fact ones)
    let shared = secs.len() < n;
    let pool = if shared { secs.iter().map(|s| s.chunk_ids.len()).sum() } else { kf.len() };
    let lo = if shared { (pool / n).clamp(1, 3) } else { 3 };
    let want = n.min(pool / lo).max(1);
    if want < n {
        prog(&format!("nur {want} statt {n} Segmente möglich ({} Abschnitte, {pool} Chunks)", secs.len()));
    }
    let mut cfg = st.cfg.clone();
    cfg.llm.temperature = 0.3;
    cfg.llm.max_tokens = 1500;
    let to = std::time::Instant::now();
    let hi = if shared { lo } else { 6 };
    let mut msgs = vec![llm::sys(outline_system(shared, lo, hi)), llm::user(format!("{}\nAntworte als JSON.", outline_input(gs, &topics, focus, want, shared, &chunks)))];
    let mut best: Option<(usize, Outline)> = None;
    let mut tries = 0;
    for attempt in 0..2 {
        tries = attempt;
        funkhaus_sys::gpu::gpu_wait(prog);
        prog(&format!("Gliederung ({want} Segmente){}", if attempt > 0 { ", Korrektur" } else { "" }));
        let v = llm::json(&cfg, nb.backend, &msgs, &outline_schema(want))?;
        let mut o = Outline {
            title: v["title"].as_str().unwrap_or("").trim().to_string(),
            len: len_name(len).into(),
            want,
            focus: focus.map(String::from),
            sections: secs.clone(),
            topics: topics.clone(),
            segments: segs_of(&v),
            shared,
            min_chunks: lo,
            words: band(len),
            retries: attempt,
            issues: vec![],
            secs: 0.0,
        };
        let f = fails(&outline_checks(&o), &["O7"]);
        repair(&mut o);
        if best.as_ref().is_none_or(|b| f.len() < b.0) {
            best = Some((f.len(), o));
        }
        if f.is_empty() || attempt == 1 {
            break;
        }
        prog(&format!("  Gliederung: {}", f.join("; ")));
        msgs.push(llm::asst(v.to_string()));
        msgs.push(llm::user(format!(
            "Korrigiere diese Fehler und antworte erneut als vollständiges JSON (genau {want} Segmente, chunk_ids nur aus den eigenen Abschnitten):\n- {}",
            f.join("\n- ")
        )));
    }
    let mut o = best.unwrap().1;
    o.retries = tries;
    o.issues = fails(&outline_checks(&o), &["O7"]);
    o.secs = secs_since(to);
    if o.segments.is_empty() {
        bail!("Gliederung ohne gültige Segmente");
    }
    let total = o.segments.len();
    let bd = o.band();
    cfg.llm.max_tokens = 1200;
    let mut scripts: Vec<SegScript> = vec![];
    for (k, s) in o.segments.iter().enumerate() {
        let no = k + 1;
        let ts = std::time::Instant::now();
        // context level Kurz: German map summary + facts instead of the raw chunk text
        let short: Vec<Chunk> = s
            .chunk_ids
            .iter()
            .filter_map(|id| chunks.iter().find(|c| &c.id == id))
            .map(|c| match (nb.sources.iter().find(|x| x.id == c.src).map(|x| x.ctx), crate::map::load(st, nb, c)) {
                (Some(crate::store::Ctx::Kurz), Some(m)) => {
                    Chunk { text: format!("{} {}", m.summary, m.facts.iter().map(|f| f.text.as_str()).collect::<Vec<_>>().join(" ")), ..c.clone() }
                }
                _ => c.clone(),
            })
            .collect();
        let cs: Vec<&Chunk> = short.iter().collect();
        let ex = excerpts(&cs);
        let prev: String = scripts.last().map(|p| p.lines.iter().rev().take(4).rev().map(|l| format!("{}: {}\n", l.mark(), l.text)).collect()).unwrap_or_else(|| "– (Anfang)".into());
        let user = format!(
            "ZIEL: {}\nBEATS:\n- {}\n\nQUELLE:\n{ex}\n\nVORHERIGE ZEILEN:\n{prev}\nSchreibe jetzt Segment {no} als JSON.",
            s.goal,
            s.beats.join("\n- ")
        );
        let mut msgs = vec![llm::sys(seg_system(no, total, &st.cfg.hosts.o.name, &st.cfg.hosts.f.name, bd)), llm::user(user)];
        let mut best: Option<(usize, SegScript)> = None;
        let mut tries = 0;
        for attempt in 0..2 {
            tries = attempt;
            funkhaus_sys::gpu::gpu_wait(prog);
            prog(&format!("Segment {no}/{total}: {}{}", s.title, if attempt > 0 { " (Korrektur)" } else { "" }));
            let v = llm::json(&cfg, nb.backend, &msgs, &seg_schema())?;
            let lines = tidy(lines_of(&v));
            let f = fails(&script_checks(&lines, &ex, no == total, bd), &[]);
            let x = SegScript { no, excerpts: ex.clone(), lines, retries: attempt, issues: f.clone(), secs: 0.0 };
            if best.as_ref().is_none_or(|b| f.len() < b.0) && x.lines.len() >= 2 {
                best = Some((f.len(), x));
            }
            if f.is_empty() || attempt == 1 {
                break;
            }
            prog(&format!("  {}", f.join("; ")));
            msgs.push(llm::asst(v.to_string()));
            msgs.push(llm::user(format!("Korrigiere diese Fehler und schreibe das Segment erneut als vollständiges JSON:\n- {}", f.join("\n- "))));
        }
        let Some((_, mut x)) = best else { bail!("Segment {no}: zu wenige Skriptzeilen") };
        x.secs = secs_since(ts);
        x.retries = tries;
        scripts.push(x);
    }
    let title = Some(o.title.clone()).filter(|t| !t.is_empty()).unwrap_or_else(|| nb.name.clone());
    Ok(Staged { title, outline: o, scripts })
}

/// LLM-free O*/S* checks of a podcast dir (outline.json + segments.json).
pub fn eval(dir: &std::path::Path) -> Result<Vec<Check>> {
    let o: Outline = serde_json::from_str(&std::fs::read_to_string(dir.join("outline.json"))?)?;
    let ss: Vec<SegScript> = serde_json::from_str(&std::fs::read_to_string(dir.join("segments.json"))?)?;
    let mut v: Vec<Check> = outline_checks(&o).into_iter().map(|(c, pass, detail)| Check { stage: "outline", check: c, pass, detail }).collect();
    let n = ss.len();
    for x in &ss {
        for (c, pass, d) in script_checks(&x.lines, &x.excerpts, x.no == n, o.band()) {
            v.push(Check { stage: "script", check: c, pass, detail: format!("seg{} {d}", x.no) });
        }
    }
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sec(id: &str, cs: &[&str], kf: &[&str]) -> Sec {
        Sec { id: id.into(), title: id.into(), chunk_ids: cs.iter().map(|x| x.to_string()).collect(), kf_chunks: kf.iter().map(|x| x.to_string()).collect() }
    }
    fn seg(s: &[&str], c: &[&str], beats: usize) -> Seg {
        Seg { title: "T".into(), goal: "G".into(), section_ids: s.iter().map(|x| x.to_string()).collect(), chunk_ids: c.iter().map(|x| x.to_string()).collect(), beats: vec!["b".into(); beats] }
    }
    fn l(w: char, g: f32, t: &str) -> Line {
        Line::ofb(w, g, t)
    }

    /// eval-reduce.md example 4 (mittel, N=6)
    #[test]
    fn outline_rules() {
        let all = ["c01", "c02", "c03", "c04", "c05", "c06", "c07", "c08", "c09"];
        let mut o = Outline {
            title: "X".into(),
            len: "mittel".into(),
            want: 6,
            focus: None,
            sections: vec![sec("s1", &all[0..3], &all[0..3]), sec("s2", &all[3..6], &all[3..6]), sec("s3", &all[6..9], &all[6..9])],
            topics: vec![guide::Topic { title: "Test".into(), section_ids: vec!["s1".into(), "s2".into()] }, guide::Topic { title: "Kosten".into(), section_ids: vec!["s3".into()] }],
            segments: vec![seg(&["s1"], &["c01", "c02", "c03"], 3), seg(&["s2"], &["c05", "c04", "c06"], 3), seg(&["s1", "s3"], &["c02", "c07"], 3), seg(&["s3"], &["c08", "c09"], 3)],
            shared: false,
            min_chunks: 0,
            words: (0, 0),
            retries: 0,
            issues: vec![],
            secs: 0.0,
        };
        let r: Vec<(&str, bool)> = outline_checks(&o).into_iter().map(|c| (c.0, c.1)).collect();
        assert_eq!(
            r,
            vec![("O1", false), ("O2", true), ("O3", false), ("O4", false), ("O5", false), ("O6", false), ("O7", true), ("O8", true), ("O9", true)]
        );
        assert!(outline_checks(&o)[6].2.contains("inversions=1"));
        repair(&mut o);
        // seg3 keeps only s3 (s1 used) and is topped up from s3's chunks; seg4 (s3 again) is dropped
        assert_eq!(fails(&outline_checks(&o), &["O1", "O7"]), Vec::<String>::new());
        assert_eq!(o.segments.len(), 3);
        assert_eq!(o.segments[1].chunk_ids, vec!["c04", "c05", "c06"]);
        assert_eq!(o.segments[2].section_ids, vec!["s3"]);
        // focus: only matching topics must be covered
        o.focus = Some("Kosten".into());
        assert!(outline_checks(&o)[7].1);
    }

    /// eval-reduce.md example 5
    #[test]
    fn script_rules() {
        let ex = "[c04 01:10] Der Ertrag lag bei 3,5 Tonnen pro Lauf.\n[c05 01:22] Im Vorjahr waren es 25 Tonnen.";
        let lines = vec![
            l('O', 0.0, "Dreieinhalb Tonnen pro Lauf klingt wenig, ist es aber nicht."),
            l('F', 0.10, "Im Vorjahr waren es fünfundzwanzig Tonnen."),
            l('B', 0.5, "Genau."),
            l('F', 0.10, "Das wären zwölf Prozent mehr, und das in 2 Läufen."),
            l('O', 0.10, "Ja, und die Kosten steigen mit jeder Charge deutlich, weil die Schiffe größer werden."),
            l('O', 0.10, "Das sehe ich auch so."),
            l('O', 0.10, "Wirklich."),
            l('F', 0.10, "Und genau deshalb ist die Frage, ob der Betrieb sich noch lohnt, wenn jede Woche neue Auflagen dazukommen, Personal fehlt und die Versicherung teurer wird, eigentlich ganz entscheidend für alles andere in diesem Projekt hier wirklich."),
            l('O', 0.10, "Bleibt die Frage, wie es weitergeht?"),
        ];
        let r: Vec<(&str, bool)> = script_checks(&lines, ex, false, SEG_WORDS).into_iter().map(|c| (c.0, c.1)).collect();
        assert_eq!(
            r,
            vec![("S1", false), ("S2", true), ("S3", true), ("S4", false), ("S5", false), ("S6", true), ("S7", false), ("S8", true), ("S9", true), ("S10", true), ("S11", true)]
        );
        // invalid pause is not silently stripped
        let r = script_checks(&[l('O', 0.0, "Hallo. [pause 1,5s] Ja."), l('F', 0.1, "Echt?")], ex, false, SEG_WORDS);
        assert!(!r[0].1 && r[0].2.contains("line 1"), "{:?}", r[0]);
        // S8 via the existing guard (ignores values ≤ 12): an invented larger number fails
        let r = script_checks(&[l('O', 0.0, "Hallo, dreiundvierzig Tonnen."), l('F', 0.1, "Echt?")], ex, true, SEG_WORDS);
        assert!(!r[7].1 && !r[8].1 && r[9].1);
    }

    #[test]
    fn repair_shares_sections() {
        let mut o = Outline {
            title: "X".into(),
            len: "mittel".into(),
            want: 2,
            focus: None,
            sections: vec![sec("a", &["c1", "c2", "c3"], &["c1", "c2", "c3"]), sec("b", &["c4", "c5", "c6"], &["c4", "c5", "c6"])],
            topics: vec![],
            segments: vec![seg(&["a", "b"], &["c1", "c4", "c5"], 3), seg(&["b", "a"], &["c5", "c2"], 3)],
            shared: false,
            min_chunks: 0,
            words: (0, 0),
            retries: 0,
            issues: vec![],
            secs: 0.0,
        };
        repair(&mut o);
        assert_eq!(o.segments.len(), 2);
        assert_eq!((o.segments[0].section_ids.clone(), o.segments[0].chunk_ids.clone()), (vec!["a".to_string()], vec!["c1".into(), "c2".into(), "c3".into()]));
        assert_eq!((o.segments[1].section_ids.clone(), o.segments[1].chunk_ids.clone()), (vec!["b".to_string()], vec!["c4".into(), "c5".into(), "c6".into()]));
    }

    #[test]
    fn shared_sections_split_chunks() {
        let a = ["a0", "a1", "a2", "a3", "a4", "a5"];
        let b = ["b0", "b1", "b2", "b3", "b4", "b5"];
        let mut o = Outline {
            title: "X".into(),
            len: "mittel".into(),
            want: 6,
            focus: None,
            sections: vec![sec("a", &a, &["a1", "a4"]), sec("b", &b, &["b2"])],
            topics: vec![],
            // model reuses a1 and puts everything into section a
            segments: (0..6).map(|_| seg(&["a"], &["a1", "a2"], 3)).collect(),
            shared: true,
            min_chunks: 2,
            words: band(Len::Mittel),
            retries: 0,
            issues: vec![],
            secs: 0.0,
        };
        assert!(!outline_checks(&o)[3].1); // O4 dup chunks
        repair(&mut o);
        assert_eq!(o.segments.len(), 6);
        let r: Vec<(&str, bool)> = outline_checks(&o).into_iter().map(|c| (c.0, c.1)).collect();
        assert!(r.iter().all(|c| c.1), "{r:?} {:?}", o.segments);
        assert_eq!(o.segments[3].section_ids, vec!["b"]); // a exhausted after three segments
    }

    #[test]
    fn join_markers_and_gap() {
        let o = Outline {
            title: "X".into(),
            len: "kurz".into(),
            want: 2,
            focus: None,
            sections: vec![],
            topics: vec![],
            segments: vec![seg(&["a"], &["c1"], 3), Seg { title: "Zwei | B".into(), ..seg(&["b"], &["c2"], 3) }],
            shared: false,
            min_chunks: 0,
            words: (0, 0),
            retries: 0,
            issues: vec![],
            secs: 0.0,
        };
        let sc = |no, ls: Vec<Line>| SegScript { no, excerpts: String::new(), lines: ls, retries: 0, issues: vec![], secs: 0.0 };
        let (txt, all) = join(&o, &[sc(1, vec![l('O', 0.0, "a"), l('F', 0.1, "b?")]), sc(2, vec![l('F', 0.0, "c"), l('O', 0.1, "d")])]);
        assert_eq!(all.len(), 4);
        assert_eq!(all[2].gap, SEG_GAP);
        assert!(txt.starts_with("# SEG 1 | T | G\nO|0|a\n") && txt.contains("# SEG 2 | Zwei   B | G\nF|0.8|c\n"));
        assert_eq!(super::super::script::parse(&txt).unwrap(), all);
    }

    #[test]
    fn excerpt_budget() {
        let c: Vec<Chunk> = (0..6).map(|i| Chunk { id: format!("s1#{i}"), text: "wort ".repeat(1000), t0: Some(65.0), ..Default::default() }).collect();
        let e = excerpts(&c.iter().collect::<Vec<_>>());
        assert!(e.len() <= SEG_CHARS + 200 && e.starts_with("[s1#0 1:05] wort"));
        let s = seg_system(1, 3, "Oskar", "Fable", (60, 100));
        assert!(s.len() + e.len() < 16_000, "{}", s.len());
    }
}
