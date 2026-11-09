//! Plain text / markdown -> script lines. One line per paragraph; `--detect`: `Name: text` starts a line of speaker Name.
//! Markdown headings become `# SEG n | title` chapter comments (audio::chapters reads them).
use crate::Project;
use crate::script::{Item, id_ok};
use anyhow::{Result, bail};
use funkhaus_audio::script::Line;
use serde::Serialize;

pub const DEFAULT_SPEAKER: &str = "ERZ";

#[derive(Serialize, Debug, Default, PartialEq)]
pub struct Report {
    pub lines: usize,
    pub chapters: usize,
    pub speakers_added: Vec<String>,
    /// What was dropped (code fences, tables, rules), with source line numbers.
    pub skipped: Vec<String>,
}

/// `Name: text` / `NAME: text` -> (name, text). Name: starts uppercase, ≤ 3 words, ≤ 32 chars, letters/space/.-_ only (no digits:
/// `Um 12:30` is no speaker). Candidate only: `import` also needs the name ≥ 2× in the text or already known.
pub fn speaker_prefix(l: &str) -> Option<(&str, &str)> {
    let (n, t) = l.split_once(':')?;
    let n = n.trim();
    let ok = n.chars().next().is_some_and(char::is_uppercase)
        && n.chars().count() <= 32
        && n.split_whitespace().count() <= 3
        && n.chars().all(|c| c.is_alphabetic() || " .-_".contains(c))
        && !t.starts_with(|c: char| c.is_ascii_digit())
        && !t.trim().is_empty();
    ok.then_some((n, t.trim()))
}

/// Speaker ID from a name: spaces -> `_`, other non-ID chars dropped; IDs audio::script would rewrite and the reserved
/// funkhaus hosts O/F get a `_` suffix.
pub fn id_of(name: &str) -> String {
    let id: String = name.trim().chars().filter_map(|c| if c.is_whitespace() { Some('_') } else { (c.is_alphanumeric() || "_-".contains(c)).then_some(c) }).collect();
    if id_ok(&id) && !matches!(id.as_str(), "O" | "F") { id } else { format!("{id}_") }
}

/// Markdown `[text](url)` -> text, `![alt](url)` -> alt; other brackets (`[pause 1s]`, `[sigh]`) stay.
fn unlink(t: &str) -> String {
    let mut out = String::new();
    let mut rest = t;
    while let Some(a) = rest.find('[') {
        let link = rest[a..].find("](").map(|b| a + b).filter(|&b| !rest[a + 1..b].contains(['[', ']'])).and_then(|b| rest[b + 2..].find(')').map(|c| (b, b + 2 + c)));
        let Some((b, c)) = link else {
            out.push_str(&rest[..=a]);
            rest = &rest[a + 1..];
            continue;
        };
        out.push_str(rest[..a].strip_suffix('!').unwrap_or(&rest[..a]));
        out.push_str(&rest[a + 1..b]);
        rest = &rest[c + 1..];
    }
    out + rest
}

/// Strips inline markdown and list/quote prefixes.
fn plain(l: &str) -> String {
    let t = l.trim();
    let t = t.trim_start_matches('>').trim_start();
    let t = ["- ", "* ", "+ "].iter().find_map(|p| t.strip_prefix(p)).unwrap_or(t);
    let t = t.split_once(". ").filter(|(a, _)| !a.is_empty() && a.chars().all(|c| c.is_ascii_digit())).map_or(t, |(_, b)| b);
    unlink(t).replace("**", "").replace("__", "").replace('`', "").replace('\t', " ")
}

/// Lowercased `--detect` candidate names that occur at least twice.
fn repeated(src: &str) -> std::collections::HashSet<String> {
    let mut n = std::collections::HashMap::<String, usize>::new();
    for l in src.lines() {
        if let Some((w, _)) = speaker_prefix(&plain(l)) {
            *n.entry(w.to_lowercase()).or_default() += 1;
        }
    }
    n.into_iter().filter(|(_, c)| *c >= 2).map(|(w, _)| w).collect()
}

enum Piece {
    Heading(String),
    /// (speaker name if detected, text)
    Para(Option<String>, String),
}

/// `known`: lowercased speaker IDs/labels accepted as names even if they occur only once.
fn pieces(src: &str, detect: bool, known: &[String], skipped: &mut Vec<String>) -> Vec<Piece> {
    let rep = if detect { repeated(src) } else { Default::default() };
    let (mut out, mut cur): (Vec<Piece>, Option<(Option<String>, String)>) = (vec![], None);
    let mut fence = None;
    let flush = |out: &mut Vec<Piece>, cur: &mut Option<(Option<String>, String)>| {
        if let Some((w, t)) = cur.take().filter(|(_, t)| !t.trim().is_empty()) {
            out.push(Piece::Para(w, t.split_whitespace().collect::<Vec<_>>().join(" ")));
        }
    };
    for (i, raw) in src.lines().enumerate() {
        let l = raw.trim();
        if l.starts_with("```") || l.starts_with("~~~") {
            match fence.take() {
                Some(a) => skipped.push(format!("Codeblock Zeilen {a}–{}", i + 1)),
                None => {
                    flush(&mut out, &mut cur);
                    fence = Some(i + 1);
                }
            }
            continue;
        }
        if fence.is_some() {
            continue;
        }
        if l.is_empty() {
            flush(&mut out, &mut cur);
            continue;
        }
        if l.starts_with('|') || (l.len() >= 3 && l.chars().all(|c| "-*_ ".contains(c))) {
            flush(&mut out, &mut cur);
            skipped.push(format!("Zeile {}: {}", i + 1, if l.starts_with('|') { "Tabelle" } else { "Trennlinie" }));
            continue;
        }
        if let Some(h) = l.strip_prefix('#').map(|h| h.trim_start_matches('#')).filter(|h| h.is_empty() || h.starts_with(' ')) {
            flush(&mut out, &mut cur);
            let h = plain(h).replace('|', "/");
            if !h.trim().is_empty() {
                out.push(Piece::Heading(h.trim().to_string()));
            }
            continue;
        }
        let t = plain(l);
        if detect && let Some((n, rest)) = speaker_prefix(&t).filter(|(n, _)| rep.contains(&n.to_lowercase()) || known.contains(&n.to_lowercase())) {
            flush(&mut out, &mut cur);
            cur = Some((Some(n.to_string()), rest.to_string()));
            continue;
        }
        match &mut cur {
            Some((_, s)) => {
                s.push(' ');
                s.push_str(&t);
            }
            None => cur = Some((None, t)),
        }
    }
    flush(&mut out, &mut cur);
    if let Some(a) = fence {
        skipped.push(format!("Codeblock ab Zeile {a} (nicht geschlossen)"));
    }
    out
}

/// Appends `src` to the project script. `speaker`: ID for lines without detected name (default: first project speaker, else ERZ).
/// New speakers are added to project.toml with persona unset. Caller saves.
pub fn import(p: &mut Project, src: &str, speaker: Option<&str>, detect: bool) -> Result<Report> {
    let mut r = Report::default();
    let mut ids: Vec<(String, String)> = p.meta.speakers.keys().map(|id| (id.to_lowercase(), id.clone())).chain(p.meta.speakers.iter().filter_map(|(id, s)| s.label.as_ref().map(|l| (l.to_lowercase(), id.clone())))).collect();
    let ps = pieces(src, detect, &ids.iter().map(|x| x.0.clone()).collect::<Vec<_>>(), &mut r.skipped);
    let fallback = speaker.map(String::from).or_else(|| p.meta.speakers.keys().next().cloned()).unwrap_or_else(|| DEFAULT_SPEAKER.into());
    if !id_ok(&fallback) {
        bail!("Sprecher-ID {fallback:?} ungültig (Buchstaben, Ziffern, _ -; nicht B – reserviert für Einwürfe)");
    }
    let mut chap = p.script.items.iter().filter(|i| matches!(i, Item::Note(n) if n.starts_with("# SEG"))).count();
    let mut first = p.script.is_empty();
    for x in ps {
        match x {
            Piece::Heading(h) => {
                chap += 1;
                r.chapters += 1;
                p.script.items.push(Item::Note(format!("# SEG {chap} | {h}")));
            }
            Piece::Para(who, text) => {
                let id = match who {
                    None => fallback.clone(),
                    // `Anna:` and `ANNA:` are the same speaker
                    Some(n) => match ids.iter().find(|(k, _)| *k == n.to_lowercase()) {
                        Some((_, id)) => id.clone(),
                        None => {
                            let id = id_of(&n);
                            ids.push((n.to_lowercase(), id.clone()));
                            if p.add_speaker(&id, (id != n).then_some(n.as_str())) {
                                r.speakers_added.push(id.clone());
                            }
                            id
                        }
                    },
                };
                if p.add_speaker(&id, None) {
                    r.speakers_added.push(id.clone());
                }
                let gap = if first { 0.0 } else { p.meta.settings.gap as f32 };
                first = false;
                p.script.items.push(Item::Line(Line::new(id, gap, text)));
                r.lines += 1;
            }
        }
    }
    Ok(r)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::script::Script;

    const DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/stimmwerk/project/");

    fn fresh() -> (tempfile::TempDir, Project) {
        let t = tempfile::tempdir().unwrap();
        let p = Project::create(&t.path().join("i.studio"), "I", "de").unwrap();
        (t, p)
    }

    #[test]
    fn plain_paragraphs_one_speaker() {
        let (_t, mut p) = fresh();
        let src = std::fs::read_to_string(format!("{DIR}prosa.md")).unwrap();
        let r = import(&mut p, &src, None, false).unwrap();
        assert_eq!((r.lines, r.chapters, r.speakers_added.clone()), (4, 2, vec!["ERZ".to_string()]));
        assert_eq!(r.skipped.len(), 1, "{:?}", r.skipped);
        p.save().unwrap();
        let s = std::fs::read_to_string(p.dir.join("script.txt")).unwrap();
        assert!(s.starts_with("# SEG 1 | Der Leuchtturm\nERZ|0|Am Rand der Insel"), "{s}");
        assert!(s.contains("# SEG 2 | Die Nacht\n") && s.contains("ERZ|0.5|Keine Fett-Markierung bleibt übrig."), "{s}");
        assert!(!s.contains("**") && !s.contains("```"));
        // Mara: lines stay with ERZ without --detect
        assert_eq!(Script::parse(&s).unwrap().lines().filter(|l| l.who == "ERZ").count(), 4);
        assert_eq!(chapters_of(&s), ["Der Leuchtturm", "Die Nacht"]);
    }

    fn chapters_of(s: &str) -> Vec<String> {
        funkhaus_audio::chapters::marks(s).into_iter().map(|m| m.1).collect()
    }

    #[test]
    fn detect_speakers() {
        let (_t, mut p) = fresh();
        p.add_speaker("ERZ", Some("Erzählerin"));
        let src = std::fs::read_to_string(format!("{DIR}dialog.txt")).unwrap();
        let r = import(&mut p, &src, None, true).unwrap();
        assert_eq!(r.speakers_added, ["Mara", "Jonas", "Frau_Ilse"]);
        assert_eq!(r.lines, 8);
        let who: Vec<&str> = p.script.lines().map(|l| l.who.as_str()).collect();
        assert_eq!(who, ["ERZ", "Mara", "Jonas", "Mara", "Frau_Ilse", "Jonas", "Frau_Ilse", "ERZ"]);
        assert_eq!(p.meta.speakers["Frau_Ilse"].label.as_deref(), Some("Frau Ilse"));
        assert_eq!(p.meta.speakers["Mara"], crate::project::Speaker::default());
        assert!(p.script.lines().nth(3).unwrap().text.ends_with("Licht brennt noch."), "continuation line joined");
        p.save().unwrap();
        let q = Project::load(&p.dir).unwrap();
        assert_eq!(q.script, p.script);
        let r = crate::lint::lint(&p.dir, &[], None).unwrap();
        assert_eq!(r.errors, 0, "{:?}", r.issues);
    }

    #[test]
    fn appends_and_explicit_speaker() {
        let (_t, mut p) = fresh();
        import(&mut p, "Eins.\n\nZwei.", Some("Lea"), false).unwrap();
        let r = import(&mut p, "# Teil\nDrei.", None, false).unwrap();
        assert_eq!((r.lines, r.speakers_added.len()), (1, 0));
        assert_eq!(p.script.format(), "Lea|0|Eins.\nLea|0.5|Zwei.\n# SEG 1 | Teil\nLea|0.5|Drei.\n");
        assert!(import(&mut p, "x", Some("A B"), false).is_err());
    }

    #[test]
    fn detect_guard() {
        let (_t, mut p) = fresh();
        p.add_speaker("ERZ", Some("Erzählerin"));
        let src = "Hinweis: Das ist nur ein Satz.\n\nUm 12:30 kommt die Fähre.\n\nErzählerin: Es war spät.\n\nO: Ich.\n\nO: Du.\n\nMehr steht [hier](https://x.org).";
        let r = import(&mut p, src, None, true).unwrap();
        assert_eq!(r.speakers_added, ["O_"]);
        let got: Vec<(&str, &str)> = p.script.lines().map(|l| (l.who.as_str(), l.text.as_str())).collect();
        assert_eq!(got, [("ERZ", "Hinweis: Das ist nur ein Satz."), ("ERZ", "Um 12:30 kommt die Fähre."), ("ERZ", "Es war spät."), ("O_", "Ich."), ("O_", "Du."), ("ERZ", "Mehr steht hier.")]);
    }

    #[test]
    fn names() {
        assert_eq!(speaker_prefix("MARA: Hallo"), Some(("MARA", "Hallo")));
        assert_eq!(speaker_prefix("Frau Ilse:  Ja."), Some(("Frau Ilse", "Ja.")));
        assert_eq!(speaker_prefix("es ist so: gut"), None);
        assert_eq!(speaker_prefix("Ein sehr langer Satz mit Doppelpunkt: x"), None);
        assert_eq!(speaker_prefix("Uhr: "), None);
        assert_eq!(speaker_prefix("Um 12:30 geht es los"), None);
        assert_eq!(speaker_prefix("Teil 2: Anfang"), None);
        assert_eq!(speaker_prefix("Zeit:12 Uhr"), None);
        assert_eq!((id_of("O"), id_of("F"), id_of("Otto")), ("O_".into(), "F_".into(), "Otto".into()));
        assert_eq!(unlink("Siehe [die Karte](https://x.org/a) und ![Bild](a.png)! [pause 1s] [sigh] [x] (y)"), "Siehe die Karte und Bild! [pause 1s] [sigh] [x] (y)");
        assert_eq!(plain("- [**Link**](u) `c`"), "Link c");
        assert_eq!((id_of("Frau Ilse"), id_of("b"), id_of("o"), id_of("Dr. Weiß")), ("Frau_Ilse".into(), "b_".into(), "o_".into(), "Dr_Weiß".into()));
    }
}
