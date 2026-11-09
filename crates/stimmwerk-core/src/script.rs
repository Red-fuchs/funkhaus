//! `script.txt`: audio::script lines (`ID|GAP|TEXT`, `+ID|..`) plus verbatim comment/blank lines, so load/save is lossless.
use anyhow::Result;
use funkhaus_audio::script::{self as fs, Line};

#[derive(Clone, Debug, PartialEq)]
pub enum Item {
    Line(Line),
    /// `#` comment (e.g. `# SEG n | title`) or blank line, kept as written.
    Note(String),
    /// Line that does not parse, kept verbatim so a broken script still opens; lint reports it, render refuses.
    Bad(String),
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Script {
    pub items: Vec<Item>,
}

pub fn is_note(l: &str) -> bool {
    let t = l.trim();
    t.is_empty() || t.starts_with('#')
}

/// Valid speaker ID as audio::script reads it back unchanged (letters, digits, `_`, `-`; not `B`; single o/f lowercase normalise).
pub fn id_ok(id: &str) -> bool {
    fs::parse(&format!("{id}|0|x")).is_ok_and(|v| v[0].who == id && !v[0].over)
}

/// One script line, parsed after `prev_main` (the last main line) so `B`/`+ID` overlays resolve as in the whole file.
pub fn parse_line(prev_main: Option<&str>, raw: &str) -> Result<Line> {
    let w = raw.trim().split('|').next().unwrap_or("").trim();
    let over = w.starts_with('+') || w.eq_ignore_ascii_case("b");
    let ctx = match (over, prev_main) {
        (true, Some(m)) => format!("{m}\n{raw}"),
        (true, None) => anyhow::bail!("Einwurf {w:?} vor der ersten Sprecherzeile"),
        _ => raw.to_string(),
    };
    Ok(fs::parse(&ctx)?.pop().expect("non-empty"))
}

impl Script {
    /// Never fails on content: unparsable lines become `Item::Bad` (overlays parse in the context of the previous main line).
    pub fn parse(s: &str) -> Result<Self> {
        let mut prev: Option<&str> = None;
        let items = s
            .lines()
            .map(|l| {
                if is_note(l) {
                    return Item::Note(l.trim_end().to_string());
                }
                match parse_line(prev, l) {
                    Ok(x) => {
                        if !x.over {
                            prev = Some(l);
                        }
                        Item::Line(x)
                    }
                    Err(_) => Item::Bad(l.trim_end().to_string()),
                }
            })
            .collect();
        Ok(Script { items })
    }

    /// 1-based script.txt line numbers of `Item::Bad`.
    pub fn bad(&self) -> Vec<usize> {
        self.items.iter().enumerate().filter_map(|(i, x)| matches!(x, Item::Bad(_)).then_some(i + 1)).collect()
    }

    pub fn format(&self) -> String {
        self.items
            .iter()
            .map(|i| match i {
                Item::Line(l) => fs::format(std::slice::from_ref(l)),
                Item::Note(n) | Item::Bad(n) => format!("{n}\n"),
            })
            .collect()
    }

    pub fn lines(&self) -> impl Iterator<Item = &Line> {
        self.items.iter().filter_map(|i| if let Item::Line(l) = i { Some(l) } else { None })
    }

    pub fn is_empty(&self) -> bool {
        self.lines().next().is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_notes_and_roundtrips() {
        let src = "# SEG 1 | Anfang\nERZ|0|Es war einmal.\n\n+Mara|0.5|Mhm.\nMara|0.4|Ja. [pause 1s] Gut.\n";
        let s = Script::parse(src).unwrap();
        assert_eq!(s.items.len(), 5);
        assert_eq!(s.lines().count(), 3);
        assert_eq!(s.format(), src);
        assert_eq!(Script::parse(&s.format()).unwrap(), s);
        assert!(Script::parse("").unwrap().is_empty());
        assert_eq!(Script::parse("# nur Kommentar\n").unwrap().format(), "# nur Kommentar\n");
        let b = Script::parse("+Mara|0|x\nERZ|x|Zahl.\nERZ|0|Gut.\n+Mara|0.5|Ja.\nMara|0.2|\n").unwrap();
        assert_eq!((b.bad(), b.lines().count()), (vec![1, 2, 5], 2));
        assert_eq!(b.format(), "+Mara|0|x\nERZ|x|Zahl.\nERZ|0|Gut.\n+Mara|0.5|Ja.\nMara|0.2|\n");
        let o = Script::parse("O|0|a\nB|0|b\n").unwrap();
        assert_eq!(o.lines().nth(1).unwrap().who, "F");
    }

    #[test]
    fn ids() {
        assert!(id_ok("ERZ") && id_ok("Mara") && id_ok("O") && id_ok("sprecher_2"));
        assert!(!id_ok("B") && !id_ok("o") && !id_ok("A B") && !id_ok("") && !id_ok("+X"));
    }
}
