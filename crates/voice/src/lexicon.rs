//! Per-notebook pronunciation lexicon (`lexicon.tsv`, de.tsv format), applied to TTS text before hashing.
use anyhow::{Result, bail};
use std::path::Path;
use unicode_normalization::{UnicodeNormalization, is_nfc};

#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub key: String,
    pub speakable: String,
    pub note: String,
}

#[derive(Clone, Debug, Default)]
pub struct Lexicon {
    /// (key chars, replacement), longest key first
    ents: Vec<(Vec<char>, String)>,
    pub warnings: Vec<String>,
}

fn word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn nfc(s: &str) -> String {
    if is_nfc(s) { s.to_string() } else { s.nfc().collect() }
}

fn cap(s: &str) -> String {
    let mut c = s.chars();
    c.next().map(|f| f.to_uppercase().chain(c).collect()).unwrap_or_default()
}

/// Parses rows; malformed ones are skipped with a warning in the second value.
pub fn parse_rows(src: &str) -> (Vec<Row>, Vec<String>) {
    let (mut rows, mut warn): (Vec<Row>, Vec<String>) = (vec![], vec![]);
    for (i, l) in src.lines().enumerate() {
        let l = l.trim_end_matches('\r');
        if l.trim().is_empty() || l.starts_with('#') {
            continue;
        }
        let mut c = l.split('\t');
        let (k, s, n) = (c.next().unwrap_or(""), c.next(), c.next().unwrap_or(""));
        match s {
            Some(s) if !k.is_empty() && !s.is_empty() => {
                if rows.iter().any(|r| r.key == k) {
                    warn.push(format!("lexicon.tsv Zeile {}: doppelter Schlüssel '{k}' (letzter gewinnt)", i + 1));
                    rows.retain(|r| r.key != k);
                }
                rows.push(Row { key: k.into(), speakable: s.into(), note: n.into() });
            }
            _ => warn.push(format!("lexicon.tsv Zeile {}: übersprungen (Format key<TAB>speakable)", i + 1)),
        }
    }
    (rows, warn)
}

impl Lexicon {
    pub fn parse(src: &str) -> Self {
        let (rows, warnings) = parse_rows(src);
        let mut exact: Vec<(Vec<char>, String)> = vec![];
        let mut var: Vec<(Vec<char>, String)> = vec![];
        for r in &rows {
            let (k, s) = (nfc(&r.key), nfc(&r.speakable));
            let kc: Vec<char> = k.chars().collect();
            if kc.first().is_some_and(|c| c.is_lowercase()) {
                let mut v = kc.clone();
                let up: Vec<char> = v[0].to_uppercase().collect();
                if up.len() == 1 {
                    v[0] = up[0];
                    var.push((v, cap(&s)));
                }
            }
            exact.push((kc, s));
        }
        let mut ents = exact;
        ents.extend(var);
        ents.sort_by_key(|e| std::cmp::Reverse(e.0.len())); // stable: exact before variant
        Self { ents, warnings }
    }

    /// Missing file = empty lexicon. Warnings go to stderr.
    pub fn load(path: &Path) -> Self {
        let l = std::fs::read_to_string(path).map(|s| Self::parse(&s)).unwrap_or_default();
        for w in &l.warnings {
            eprintln!("WARNUNG: {w}");
        }
        l
    }

    pub fn is_empty(&self) -> bool {
        self.ents.is_empty()
    }

    pub fn apply(&self, text: &str) -> String {
        if self.ents.is_empty() {
            return nfc(text);
        }
        let t: Vec<char> = nfc(text).chars().collect();
        let mut out = String::with_capacity(text.len());
        let mut i = 0;
        'o: while i < t.len() {
            if t[i] == '['
                && let Some(j) = t[i..].iter().take_while(|&&c| c != '\n').position(|&c| c == ']')
            {
                out.extend(&t[i..=i + j]);
                i += j + 1;
                continue;
            }
            for (k, s) in &self.ents {
                let e = i + k.len();
                if e > t.len() || t[i..e] != k[..] {
                    continue;
                }
                if word(k[0]) && i > 0 && word(t[i - 1]) || word(k[k.len() - 1]) && t.get(e).is_some_and(|&c| word(c)) {
                    continue;
                }
                out.push_str(s);
                i = e;
                continue 'o;
            }
            out.push(t[i]);
            i += 1;
        }
        out
    }
}

pub fn rows(path: &Path) -> Vec<Row> {
    std::fs::read_to_string(path).map(|s| parse_rows(&s).0).unwrap_or_default()
}

fn check(s: &str, what: &str) -> Result<()> {
    if s.is_empty() || s.contains(['\t', '\n', '\r']) {
        bail!("{what}: leer oder enthält Tab/Zeilenumbruch");
    }
    Ok(())
}

fn write(path: &Path, lines: &[String]) -> Result<()> {
    let tmp = path.with_extension("tsv.tmp");
    std::fs::write(&tmp, lines.iter().map(|l| format!("{l}\n")).collect::<String>())?;
    Ok(std::fs::rename(&tmp, path)?)
}

fn lines_of(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path).map(|s| s.lines().map(String::from).collect()).unwrap_or_default()
}

fn is_key(l: &str, key: &str) -> bool {
    !l.starts_with('#') && l.split('\t').next() == Some(key)
}

/// Adds or replaces the row for `key` (comments are kept).
pub fn add(path: &Path, key: &str, speakable: &str, note: &str) -> Result<Row> {
    check(key, "Schlüssel")?;
    check(speakable, "Aussprache")?;
    if note.contains(['\t', '\n', '\r']) {
        bail!("Notiz enthält Tab/Zeilenumbruch");
    }
    let row = format!("{key}\t{speakable}\t{note}");
    let mut ls = lines_of(path);
    match ls.iter().position(|l| is_key(l, key)) {
        Some(i) => {
            ls.retain(|l| !is_key(l, key));
            ls.insert(i, row);
        }
        None => ls.push(row),
    }
    write(path, &ls)?;
    Ok(Row { key: key.into(), speakable: speakable.into(), note: note.into() })
}

pub fn rm(path: &Path, key: &str) -> Result<()> {
    let mut ls = lines_of(path);
    let n = ls.len();
    ls.retain(|l| !is_key(l, key));
    if ls.len() == n {
        bail!("Schlüssel '{key}' nicht im Lexikon");
    }
    write(path, &ls)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/lexicon/");

    #[test]
    fn cases_tsv() {
        let lx = Lexicon::parse(&std::fs::read_to_string(format!("{ROOT}lexicon.tsv")).unwrap());
        assert!(lx.warnings.is_empty(), "{:?}", lx.warnings);
        let cases = std::fs::read_to_string(format!("{ROOT}cases.tsv")).unwrap();
        let mut n = 0;
        for l in cases.lines().skip(1).filter(|l| !l.is_empty()) {
            let (i, e) = l.split_once('\t').unwrap();
            assert_eq!(lx.apply(i), e, "input {i:?}");
            n += 1;
        }
        assert!(n >= 25);
    }

    #[test]
    fn rules() {
        let lx = Lexicon::parse("Lewa\tLehwah\nlewa\tlehwah\nRuse\tRusse\nRusse\tRusa\nFähre\tFehre\nladen\tlaedn\n");
        assert_eq!(lx.apply("Lewa im Laden, lewa."), "Lehwah im Laedn, lehwah.");
        assert_eq!(lx.apply("Laden laden LADEN"), "Laedn laedn LADEN");
        assert_eq!(lx.apply("Ruse."), "Russe.");
        assert_eq!(lx.apply("Fa\u{308}hre"), "Fehre"); // NFD
        assert_eq!(lx.apply("[Lewa] Lewa [oops"), "[Lewa] Lehwah [oops");
        assert_eq!(Lexicon::default().apply("Lewa"), "Lewa");
    }

    #[test]
    fn malformed_rows_warn() {
        let lx = Lexicon::parse("# c\nFähre\nA\tB\nA\tC\n\t\tx\n");
        assert_eq!(lx.warnings.len(), 3);
        assert_eq!(lx.apply("A"), "C");
    }

    #[test]
    fn edit_file() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("lexicon.tsv");
        add(&p, "Lewa", "Lehwah", "unverified").unwrap();
        add(&p, "Ruse", "Russe", "").unwrap();
        add(&p, "Lewa", "Lewah", "x").unwrap();
        let before = std::fs::read_to_string(&p).unwrap();
        assert!(add(&p, "Lewa", "Le\tX", "").is_err());
        assert_eq!(std::fs::read_to_string(&p).unwrap(), before);
        assert_eq!(rows(&p).iter().map(|r| r.speakable.as_str()).collect::<Vec<_>>(), ["Lewah", "Russe"]);
        rm(&p, "Lewa").unwrap();
        assert!(rm(&p, "Lewa").is_err());
        assert_eq!(rows(&p).len(), 1);
    }
}
