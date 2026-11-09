//! Word-level diff (LCS over whitespace tokens) for the Sprechfassung check panel.
use std::collections::HashSet;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Op {
    Same(String),
    Del(String),
    Ins(String),
}

/// LCS over whitespace tokens, O(n·m). Del before Ins on ties.
pub fn diff(a: &str, b: &str) -> Vec<Op> {
    let (x, y): (Vec<&str>, Vec<&str>) = (a.split_whitespace().collect(), b.split_whitespace().collect());
    let (n, m) = (x.len(), y.len());
    let w = m + 1;
    let mut t = vec![0u32; (n + 1) * w]; // t[i*w+j] = LCS(x[i..], y[j..])
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            t[i * w + j] = if x[i] == y[j] { t[(i + 1) * w + j + 1] + 1 } else { t[(i + 1) * w + j].max(t[i * w + j + 1]) };
        }
    }
    let (mut i, mut j, mut out) = (0, 0, Vec::with_capacity(n.max(m)));
    while i < n && j < m {
        if x[i] == y[j] {
            out.push(Op::Same(x[i].into()));
            i += 1;
            j += 1;
        } else if t[(i + 1) * w + j] >= t[i * w + j + 1] {
            out.push(Op::Del(x[i].into()));
            i += 1;
        } else {
            out.push(Op::Ins(y[j].into()));
            j += 1;
        }
    }
    out.extend(x[i..].iter().map(|s| Op::Del((*s).into())));
    out.extend(y[j..].iter().map(|s| Op::Ins((*s).into())));
    out
}

fn has_digit(s: &str) -> bool {
    s.chars().any(|c| c.is_ascii_digit())
}

/// Digit tokens in `a` absent from `b`, paired with the k-th Ins of the same changed hunk (if any).
/// Expected use: guard when normalized text spells numbers out (`3. Oktober 1964` -> `dritten Oktober neunzehnhundert...`), so each entry is a number to verify manually.
pub fn numbers_changed(a: &str, b: &str) -> Vec<(String, Option<String>)> {
    let bs: HashSet<&str> = b.split_whitespace().filter(|s| has_digit(s)).collect();
    let ops = diff(a, b);
    let mut out = vec![];
    for hunk in ops.split(|o| matches!(o, Op::Same(_))) {
        let ins: Vec<&str> = hunk.iter().filter_map(|o| if let Op::Ins(s) = o { Some(s.as_str()) } else { None }).collect();
        for (k, o) in hunk.iter().filter(|o| matches!(o, Op::Del(_))).enumerate() {
            let Op::Del(s) = o else { unreachable!() };
            if has_digit(s) && !bs.contains(s.as_str()) {
                out.push((s.clone(), ins.get(k).map(|v| v.to_string())));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(x: &str) -> String {
        x.into()
    }

    #[test]
    fn identical() {
        assert_eq!(diff("a b c", "a b c"), vec![Op::Same(s("a")), Op::Same(s("b")), Op::Same(s("c"))]);
        assert!(numbers_changed("x 1 y", "x 1 y").is_empty());
    }

    #[test]
    fn insert() {
        assert_eq!(diff("a c", "a b c"), vec![Op::Same(s("a")), Op::Ins(s("b")), Op::Same(s("c"))]);
    }

    #[test]
    fn delete() {
        assert_eq!(diff("a b c", "a c"), vec![Op::Same(s("a")), Op::Del(s("b")), Op::Same(s("c"))]);
    }

    #[test]
    fn replace() {
        assert_eq!(diff("a b c", "a x c"), vec![Op::Same(s("a")), Op::Del(s("b")), Op::Ins(s("x")), Op::Same(s("c"))]);
    }

    #[test]
    fn umlauts() {
        assert_eq!(diff("Größe schön", "Größe schöner"), vec![Op::Same(s("Größe")), Op::Del(s("schön")), Op::Ins(s("schöner"))]);
        assert_eq!(numbers_changed("Übung 10 für Mädchen", "Übung zehn für Mädchen"), vec![(s("10"), Some(s("zehn")))]);
    }

    #[test]
    fn date_spelled_out() {
        let (a, b) = ("3. Oktober 1964", "dritten Oktober neunzehnhundertvierundsechzig");
        assert_eq!(
            diff(a, b),
            vec![Op::Del(s("3.")), Op::Ins(s("dritten")), Op::Same(s("Oktober")), Op::Del(s("1964")), Op::Ins(s("neunzehnhundertvierundsechzig"))]
        );
        assert_eq!(
            numbers_changed(a, b),
            vec![(s("3."), Some(s("dritten"))), (s("1964"), Some(s("neunzehnhundertvierundsechzig")))]
        );
    }
}
