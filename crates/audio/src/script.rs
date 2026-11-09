use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

/// One script line `WHO|GAP|TEXT`. WHO = speaker id; GAP = pause in seconds after the previous main line (negative = overlap).
/// `over` lines (backchannels) are laid over the previous main line, starting at fraction GAP of it.
/// Script forms: `ID|..` main line, `+ID|..` overlay by ID; funkhaus shorthand `B|..` = overlay by the other of O/F.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(from = "Raw")]
pub struct Line {
    pub who: String,
    pub gap: f32,
    pub text: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub over: bool,
}

/// Pre-n-speaker JSON (`"who": "B"`) still loads as an unresolved O/F overlay.
#[derive(Deserialize)]
struct Raw {
    who: String,
    gap: f32,
    text: String,
    #[serde(default)]
    over: bool,
}

impl From<Raw> for Line {
    fn from(r: Raw) -> Self {
        if r.who == "B" { Line::ofb('B', r.gap, r.text) } else { Line { who: r.who, gap: r.gap, text: r.text, over: r.over } }
    }
}

impl Line {
    pub fn new(who: impl Into<String>, gap: f32, text: impl Into<String>) -> Self {
        Line { who: who.into(), gap, text: text.into(), over: false }
    }
    /// funkhaus letter O/F/B; B = overlay with speaker left empty until `resolve`.
    pub fn ofb(c: char, gap: f32, text: impl Into<String>) -> Self {
        match c {
            'B' => Line { who: String::new(), gap, text: text.into(), over: true },
            c => Line::new(c.to_string(), gap, text),
        }
    }
    /// WHO field for display (LLM prompts): `B` for any O/F overlay, `+ID` for others.
    /// Lossy for `+O` after an O line; files go through [`format`], which uses the context.
    pub fn mark(&self) -> String {
        match (self.over, self.who.as_str()) {
            (true, "" | "O" | "F") => "B".into(),
            (true, w) => format!("+{w}"),
            (false, w) => w.into(),
        }
    }
}

/// The other funkhaus host.
pub fn other(who: &str) -> &'static str {
    if who == "O" { "F" } else { "O" }
}

/// Fills the speaker of unresolved O/F overlays: the other host of the previous main line (O before any).
pub fn resolve(lines: &mut [Line]) {
    let mut prev = "O".to_string();
    for l in lines {
        if !l.over {
            prev.clone_from(&l.who);
        } else if l.who.is_empty() && matches!(prev.as_str(), "O" | "F") {
            l.who = other(&prev).into();
        }
    }
}

fn id_ok(s: &str) -> bool {
    !s.is_empty() && s != "B" && s.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '-')
}

pub fn parse(s: &str) -> Result<Vec<Line>> {
    let mut v = vec![];
    for (i, l) in s.lines().enumerate() {
        let l = l.trim();
        if l.is_empty() || l.starts_with('#') {
            continue;
        }
        let mut p = l.splitn(3, '|');
        let (Some(w), Some(g), Some(t)) = (p.next(), p.next(), p.next()) else {
            bail!("Zeile {}: erwartet WER|GAP|TEXT", i + 1);
        };
        let w = w.trim();
        let (over, id) = match w.strip_prefix('+') {
            Some(x) => (true, x.trim()),
            None => (false, w),
        };
        let id = if id.len() == 1 && "ofbOFB".contains(id) { id.to_uppercase() } else { id.to_string() };
        if !(id == "B" && !over || id_ok(&id)) {
            bail!("Zeile {}: unbekannter Sprecher {w:?}", i + 1);
        }
        let Ok(gap) = g.trim().parse::<f32>() else { bail!("Zeile {}: GAP keine Zahl: {g:?}", i + 1) };
        if t.trim().is_empty() {
            bail!("Zeile {}: leerer Text", i + 1);
        }
        let mut line = if id == "B" { Line::ofb('B', gap, t.trim()) } else { Line { over, ..Line::new(id, gap, t.trim()) } };
        if v.is_empty() && line.over {
            bail!("Skript darf nicht mit einem Einwurf (B) beginnen");
        }
        if line.who.is_empty() {
            let prev = v.iter().rev().find(|x: &&Line| !x.over).map_or("", |x| x.who.as_str());
            if !matches!(prev, "O" | "F") {
                bail!("Zeile {}: B nur nach O/F; Einwurf eines anderen Sprechers: +ID", i + 1);
            }
            line.who = other(prev).into();
        }
        v.push(line);
    }
    if v.is_empty() {
        bail!("leeres Skript");
    }
    Ok(v)
}

/// `B` only where parse reads it back as the same speaker (unresolved, or the other host of the previous main O/F line); else `+ID`.
pub fn format(lines: &[Line]) -> String {
    let mut prev = "";
    let mut out = String::new();
    for l in lines {
        let w = match (l.over, l.who.as_str()) {
            (true, "") => "B".into(),
            (true, w) if matches!(prev, "O" | "F") && w == other(prev) => "B".into(),
            (true, w) => format!("+{w}"),
            (false, w) => {
                prev = w;
                w.into()
            }
        };
        out.push_str(&format!("{w}|{}|{}\n", l.gap, l.text));
    }
    out
}

/// The 13 omnivoice non-verbal tags (docs/m3/omnivoice.md).
pub const TAGS: &[&str] = &[
    "laughter", "sigh", "confirmation-en", "question-en", "question-ah", "question-oh", "question-ei", "question-yi",
    "surprise-ah", "surprise-oh", "surprise-wa", "surprise-yo", "dissatisfaction-hnn",
];
pub const PAUSE_DEFAULT: f64 = 0.35;
pub const PAUSE_MAX: f64 = 10.0;

/// Text part of a line: spoken text or silence (seconds).
#[derive(Clone, Debug, PartialEq)]
pub enum Seg {
    Text(String),
    Pause(f64),
}

/// Exact `[pause]`, `[pause 500ms]`, `[pause 1.5s]` -> seconds. None = not a pause tag; Err above 10 s / malformed.
pub fn parse_pause(tag: &str) -> Option<Result<f64>> {
    let b = tag.trim().strip_prefix("[pause")?.strip_suffix(']')?;
    if !b.is_empty() && !b.starts_with(' ') {
        return None;
    }
    let b = b.trim();
    let secs = if b.is_empty() {
        Some(PAUSE_DEFAULT)
    } else if let Some(ms) = b.strip_suffix("ms") {
        ms.trim().parse::<f64>().ok().map(|v| v / 1000.0)
    } else {
        b.strip_suffix('s').and_then(|v| v.trim().parse::<f64>().ok())
    };
    Some(match secs {
        Some(v) if v.is_finite() && (0.0..=PAUSE_MAX).contains(&v) => Ok(v),
        Some(v) => Err(anyhow::anyhow!("Pause {v} s: höchstens {PAUSE_MAX} s erlaubt")),
        None => Err(anyhow::anyhow!("Pause {tag:?}: erwartet [pause], [pause 500ms] oder [pause 1.5s]")),
    })
}

pub fn pause_samples(tag: &str, sr: u32) -> Option<Result<usize>> {
    parse_pause(tag).map(|r| r.map(|s| (s * sr as f64).round() as usize))
}

/// Splits a line into text and pause segments (text trimmed, empty ones dropped).
pub fn segments(text: &str) -> Result<Vec<Seg>> {
    match scan(text) {
        (v, None) => Ok(v),
        (_, Some(e)) => Err(e),
    }
}

/// Lenient `segments`: invalid pauses become zero-length and the first error is returned alongside.
fn scan(text: &str) -> (Vec<Seg>, Option<anyhow::Error>) {
    let (mut out, mut rest, mut err) = (vec![], text, None);
    let push = |o: &mut Vec<Seg>, t: &str| {
        if !t.trim().is_empty() {
            o.push(Seg::Text(t.trim().to_string()));
        }
    };
    while let Some(a) = rest.find("[pause") {
        let Some(len) = rest[a..].find(']') else { break };
        let tag = &rest[a..=a + len];
        match parse_pause(tag) {
            Some(r) => {
                push(&mut out, &rest[..a]);
                out.push(Seg::Pause(r.unwrap_or_else(|e| {
                    err.get_or_insert(e);
                    0.0
                })));
                rest = &rest[a + len + 1..];
            }
            None => {
                push(&mut out, &rest[..a + 6]);
                rest = &rest[a + 6..];
            }
        }
    }
    push(&mut out, rest);
    (out, err)
}

/// Text with pause tags removed (invalid ones too; `segments` reports them).
pub fn without_pauses(text: &str) -> String {
    scan(text).0.into_iter().filter_map(|s| if let Seg::Text(t) = s { Some(t) } else { None }).collect::<Vec<_>>().join(" ")
}

/// Number of non-verbal tags (not pauses).
pub fn count_tags(text: &str) -> usize {
    TAGS.iter().map(|t| text.matches(&format!("[{t}]")).count()).sum()
}

/// Spoken words only: pauses and non-verbal tags removed.
pub fn plain(text: &str) -> String {
    let mut t = without_pauses(text);
    for g in TAGS {
        t = t.replace(&format!("[{g}]"), "");
    }
    t
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pause_parse() {
        assert_eq!(pause_samples("[pause 1.5s]", 24_000).unwrap().unwrap(), 36_000);
        assert_eq!(pause_samples("[pause 1500ms]", 24_000).unwrap().unwrap(), 36_000);
        assert_eq!(parse_pause("[pause]").unwrap().unwrap(), 0.35);
        assert!(parse_pause("[pause 20s]").unwrap().is_err());
        assert!(parse_pause("[pause 10s]").unwrap().is_ok());
        assert!(parse_pause("[pause x]").unwrap().is_err());
        assert!(parse_pause("[pausen]").is_none());
        assert!(parse_pause("Hallo.").is_none());
    }

    #[test]
    fn pause_segments() {
        assert_eq!(segments("Hallo. [pause 500ms] Ja [sigh]").unwrap(), vec![Seg::Text("Hallo.".into()), Seg::Pause(0.5), Seg::Text("Ja [sigh]".into())]);
        assert_eq!(segments("[pause]").unwrap(), vec![Seg::Pause(0.35)]);
        assert!(segments("a [pause 11s] b").is_err());
        assert_eq!(plain("Ja [sigh] gut [pause 2s]"), "Ja  gut");
        assert_eq!(count_tags("[sigh] a [laughter] [pause]"), 2);
    }

    #[test]
    fn errors() {
        assert!(parse("X Y|0|hi").is_err());
        assert!(parse("+B|0|hi").is_err());
        assert!(parse("Anna|0|hi\nB|0.5|Mhm.").is_err());
        assert!(parse("O|a|hi").is_err());
        assert!(parse("B|0.5|Mhm.").is_err());
        assert!(parse("O|0").is_err());
        assert!(parse("\n# Kommentar\n").is_err());
    }

    #[test]
    fn ofb_maps_to_ids_and_roundtrips() {
        let src = "O|0|Hallo.\nB|0.5|Mhm.\nF|0.1|Ja.\nb|0.4|Genau.\no|-0.2|Gut.\n";
        let v = parse(src).unwrap();
        let w: Vec<(&str, bool)> = v.iter().map(|l| (l.who.as_str(), l.over)).collect();
        assert_eq!(w, [("O", false), ("F", true), ("F", false), ("O", true), ("O", false)]);
        assert_eq!(format(&v), src.replace("b|", "B|").replace("o|", "O|"));
        assert_eq!(parse(&format(&v)).unwrap(), v);
    }

    #[test]
    fn n_speakers_and_overlays() {
        let v = parse("Anna|0|Hallo.\nBen|0.1|Hi.\n+Cleo|0.5|Mhm.\nCleo|0.1|Ich auch.").unwrap();
        let w: Vec<(&str, bool)> = v.iter().map(|l| (l.who.as_str(), l.over)).collect();
        assert_eq!(w, [("Anna", false), ("Ben", false), ("Cleo", true), ("Cleo", false)]);
        assert_eq!(parse(&format(&v)).unwrap(), v);
        assert_eq!(v[2].mark(), "+Cleo");
    }

    #[test]
    fn format_parse_roundtrip_mixed() {
        // every overlay speaker after every main speaker, O/F mixed with n speakers
        let ids = ["O", "F", "Anna", "Ben"];
        for a in ids {
            for b in ids {
                for c in ids {
                    let v = vec![Line::new(a, 0.0, "x"), Line { over: true, ..Line::new(b, 0.5, "y") }, Line::new(c, 0.2, "z"), Line { over: true, ..Line::new(a, 0.3, "w") }];
                    let f = format(&v);
                    assert_eq!(parse(&f).unwrap(), v, "{f}");
                    assert_eq!(format(&parse(&f).unwrap()), f);
                }
            }
        }
        assert_eq!(format(&parse("O|0|a\n+O|0.5|b\nF|0|c\n+O|0.5|d\n").unwrap()), "O|0|a\n+O|0.5|b\nF|0|c\nB|0.5|d\n");
    }

    #[test]
    fn resolve_and_legacy_json() {
        let mut v = vec![Line::ofb('F', 0.0, "a"), Line::ofb('B', 0.5, "b"), Line::ofb('B', 0.5, "c"), Line::ofb('O', 0.1, "d"), Line::ofb('B', 0.5, "e")];
        resolve(&mut v);
        assert_eq!(v.iter().map(|l| l.who.as_str()).collect::<Vec<_>>(), ["F", "O", "O", "O", "F"]);
        let l: Line = serde_json::from_str(r#"{"who":"B","gap":0.5,"text":"Mhm."}"#).unwrap();
        assert_eq!((l.who.as_str(), l.over, l.mark()), ("", true, "B".to_string()));
        assert_eq!(serde_json::to_string(&Line::new("O", 0.0, "x")).unwrap(), r#"{"who":"O","gap":0.0,"text":"x"}"#);
    }
}
