//! Guard against invented numbers: German number words in the script must occur in the sources.
use super::script::Line;
use funkhaus_audio::script::plain;

const SMALL: &[(&str, u64)] = &[
    ("null", 0), ("eins", 1), ("ein", 1), ("eine", 1), ("zwei", 2), ("drei", 3), ("vier", 4), ("fünf", 5), ("sechs", 6),
    ("sieben", 7), ("acht", 8), ("neun", 9), ("zehn", 10), ("elf", 11), ("zwölf", 12), ("dreizehn", 13), ("vierzehn", 14),
    ("fünfzehn", 15), ("sechzehn", 16), ("siebzehn", 17), ("achtzehn", 18), ("neunzehn", 19),
];
const TENS: &[(&str, u64)] = &[
    ("zwanzig", 20), ("dreißig", 30), ("vierzig", 40), ("fünfzig", 50), ("sechzig", 60), ("siebzig", 70), ("achtzig", 80), ("neunzig", 90),
];

fn lookup(t: &[(&str, u64)], s: &str) -> Option<u64> {
    t.iter().find(|(w, _)| *w == s).map(|&(_, v)| v)
}

fn below100(s: &str) -> Option<u64> {
    if let Some(v) = lookup(SMALL, s).or_else(|| lookup(TENS, s)) {
        return Some(v);
    }
    let (u, t) = s.split_once("und")?;
    let u = lookup(SMALL, u).filter(|&v| (1..10).contains(&v))?;
    Some(u + lookup(TENS, t)?)
}

/// Parses one German number word ("vierhundertzwölf", "neunzehnhundertzweiundfünfzig").
pub fn word(s: &str) -> Option<u64> {
    let s = s.to_lowercase();
    let s = s.trim_end_matches("ten").trim_end_matches("sten"); // ordinals are not handled precisely; avoid false hits
    if s.is_empty() {
        return None;
    }
    parse(s)
}

fn parse(s: &str) -> Option<u64> {
    if s.is_empty() {
        return Some(0);
    }
    if let Some(i) = s.find("tausend") {
        let a = if i == 0 { 1 } else { parse(&s[..i]).filter(|&v| v > 0)? };
        return Some(a * 1000 + parse(&s[i + 7..])?);
    }
    if let Some(i) = s.find("hundert") {
        let a = if i == 0 { 1 } else { below100(&s[..i])? };
        let r = &s[i + 7..];
        let b = if r.is_empty() { 0 } else { r.strip_prefix("und").map_or_else(|| below100(r), below100)? };
        return Some(a * 100 + b);
    }
    below100(s)
}

/// Numbers spoken in a script line; "X Komma Y" becomes a decimal. Values ≤ 12 are ignored (too common in speech).
pub fn spoken(text: &str) -> Vec<f64> {
    let ws: Vec<String> = text
        .split(|c: char| !c.is_alphabetic())
        .filter(|w| !w.is_empty())
        .map(|w| w.to_lowercase())
        .collect();
    let mut out = vec![];
    let mut i = 0;
    while i < ws.len() {
        if let Some(v) = word(&ws[i]) {
            let mut x = v as f64;
            // "sechzigtausend vierhundert" → one number
            if ws[i].ends_with("tausend")
                && let Some(r) = ws.get(i + 1).and_then(|w| word(w)).filter(|&r| r < 1000 && r > 0) {
                    x += r as f64;
                    i += 1;
                }
            if ws.get(i + 1).is_some_and(|w| w == "komma")
                && let Some(d) = ws.get(i + 2).and_then(|w| word(w)) {
                    x += d as f64 / 10f64.powi(d.to_string().len() as i32);
                    i += 2;
                }
            if x > 12.0 || x.fract() != 0.0 {
                out.push(x);
            }
        }
        i += 1;
    }
    out
}

/// Numbers written in source text: digits ("1.250", "12,5", "2025") plus number words.
pub fn written(text: &str) -> Vec<f64> {
    let mut out = spoken(text);
    let cs: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < cs.len() {
        if cs[i].is_ascii_digit() {
            let mut j = i;
            while j < cs.len() && (cs[j].is_ascii_digit() || ((cs[j] == '.' || cs[j] == ',') && cs.get(j + 1).is_some_and(|c| c.is_ascii_digit()))) {
                j += 1;
            }
            let raw: String = cs[i..j].iter().collect();
            // German: '.' groups thousands, ',' is the decimal mark; also accept plain English decimals
            let de = raw.replace('.', "").replace(',', ".");
            out.extend(de.parse::<f64>().ok());
            out.extend(raw.replace(',', "").parse::<f64>().ok());
            for part in raw.split(['.', ',']) {
                out.extend(part.parse::<f64>().ok());
            }
            i = j;
        } else {
            i += 1;
        }
    }
    out
}

const NOT_NUM: &[&str] = &["jahrhundert", "jahrtausend", "hunderte", "tausende", "hunderten", "tausenden", "hundertprozentig", "zwanziger", "dreißiger"];

/// Misspelled number words like "zwetausendfünf" (a quantised-model glitch).
pub fn malformed(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphabetic())
        .map(str::to_lowercase)
        .filter(|w| (w.contains("tausend") || w.contains("hundert")) && word(w).is_none())
        .filter(|w| !NOT_NUM.iter().any(|n| w.starts_with(n) || w.ends_with(n)))
        .collect()
}

/// Script lines with numbers that don't occur in the sources.
pub fn unsupported(lines: &[Line], sources: &str) -> Vec<String> {
    let known = written(sources);
    let ok = |x: f64| {
        // rounded mentions ("über einundsechzigtausend" for 61.400) pass if within 5 %
        let round = x >= 100.0 && x.fract() == 0.0 && (x as u64).to_string().trim_end_matches('0').len() <= 2;
        known.iter().any(|k| (k - x).abs() < 1e-6 || (round && (k - x).abs() <= k * 0.05))
    };
    lines
        .iter()
        .enumerate()
        .flat_map(|(i, l)| {
            let t = plain(&l.text);
            spoken(&t)
                .into_iter()
                .filter(|&x| !ok(x))
                .map(move |x| format!("Zeile {}: Zahl {x} steht so nicht in den Quellen – korrigieren oder weglassen: {}", i + 1, l.text))
                .chain(malformed(&t).into_iter().map(move |w| format!("Zeile {}: Zahlwort falsch geschrieben: {w}", i + 1)))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words() {
        for (w, v) in [
            ("vierhundertzwölf", 412),
            ("achtundvierzig", 48),
            ("zweitausendfünfundzwanzig", 2025),
            ("neunzehnhundertzweiundfünfzig", 1952),
            ("dreitausendvierhundert", 3400),
            ("eintausendzweihundertfünfzig", 1250),
            ("tausendzweihundertfünfzig", 1250),
            ("hundertundeins", 101),
            ("zwölf", 12),
        ] {
            assert_eq!(word(w), Some(v), "{w}");
        }
        for w in ["und", "hundertschaft", "Kleinholm", "einer"] {
            assert_eq!(word(w), None, "{w}");
        }
    }

    #[test]
    fn typos() {
        assert_eq!(malformed("Im Jahr zwetausendfünfundzwanzig, im Jahrhundert, Tausende"), vec!["zwetausendfünfundzwanzig"]);
    }

    #[test]
    fn guard() {
        assert_eq!(spoken("Zwölf Komma fünf Prozent, dreiundachtzig Leute und drei Säulen."), vec![12.5, 83.0]);
        let src = "61.400 Passagiere, 1.250 Einwohner, 12,5 % Zweitwohnungen, 412 Tonnen, 1,8 Mio. €, seit 1952.";
        let l = |t: &str| Line::new("O", 0.1, t);
        let lines = [
            l("Tausendzweihundertfünfzig Leute, zwölf Komma fünf Prozent."),
            l("Vierhundertzwölf Tonnen, eins Komma acht Millionen, seit neunzehnhundertzweiundfünfzig."),
            l("Achtundvierzig Tonnen Muscheln."),
            l("Über zwölfhundert Leute, sechzigtausend vierhundert Passagiere, tausendzweihundertfünfzig."),
        ];
        let e = unsupported(&lines, src);
        assert_eq!(e.len(), 2, "{e:?}");
        assert!(e[1].starts_with("Zeile 4: Zahl 60400"), "{e:?}");
        assert!(e[0].starts_with("Zeile 3: Zahl 48"));
    }
}
