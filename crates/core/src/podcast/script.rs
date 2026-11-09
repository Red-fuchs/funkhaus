
pub(crate) use funkhaus_audio::script::{Line, format, parse};
use funkhaus_audio::script::{other, plain, resolve, segments};

const ABBR_SPEAK: &[(&[&str], &str)] = &[
    (&["z.", "B."], "zum Beispiel"),
    (&["z.B."], "zum Beispiel"),
    (&["d.", "h."], "das heißt"),
    (&["d.h."], "das heißt"),
    (&["u.", "a."], "unter anderem"),
    (&["u.a."], "unter anderem"),
    (&["usw."], "und so weiter"),
    (&["bzw."], "beziehungsweise"),
    (&["ca."], "circa"),
    (&["etc."], "et cetera"),
    (&["Nr."], "Nummer"),
];

/// Rewrites symbols and common abbreviations (whole tokens only) into speakable German.
/// Digits are left to omnivoice's normaliser.
pub fn speakable(s: &str) -> String {
    let mut t = s.replace(['|', '\n', '\r', '*', '_', '#', '`'], " ");
    for (a, b) in [("%", " Prozent"), ("€", " Euro"), ("$", " Dollar"), ("&", " und "), ("+", " plus "), ("→", ", "), ("–", ", "), ("—", ", ")] {
        t = t.replace(a, b);
    }
    let toks: Vec<&str> = t.split_whitespace().collect();
    let mut out: Vec<String> = vec![];
    let mut i = 0;
    'tok: while i < toks.len() {
        for (pat, rep) in ABBR_SPEAK {
            let n = pat.len();
            if i + n > toks.len() {
                continue;
            }
            // trailing punctuation after the abbreviation ("usw.,") is kept
            let last = toks[i + n - 1];
            let core = last.trim_end_matches([',', ';', ':', '!', '?', ')']);
            if toks[i..i + n - 1] == pat[..n - 1] && core == pat[n - 1] {
                out.push(format!("{rep}{}", &last[core.len()..]));
                i += n;
                continue 'tok;
            }
        }
        out.push(toks[i].to_string());
        i += 1;
    }
    // a leading "-" would be read as a CLI flag and is never spoken
    out.join(" ").replace(" ,", ",").replace(" .", ".").trim_start_matches(['-', ' ']).to_string()
}

/// Enforces the format rules on LLM output: no leading/consecutive B, short B, clamped gaps, speakable text.
pub fn tidy(lines: Vec<Line>) -> Vec<Line> {
    let mut out: Vec<Line> = vec![];
    for mut l in lines {
        l.text = speakable(&l.text);
        if l.text.trim_matches(|c: char| !c.is_alphanumeric()).is_empty() && plain(&l.text) == l.text {
            continue;
        }
        if l.over {
            if matches!(l.who.as_str(), "O" | "F") {
                l.who.clear(); // O/F overlays follow the final previous line (resolve)
            }
            let words = l.text.split_whitespace().count();
            match out.last() {
                None => l = Line::new("O", l.gap, l.text),
                Some(p) if p.over => continue,
                Some(p) if words > 4 => l = Line::new(other(&p.who), 0.1, l.text),
                _ => {}
            }
        }
        l.gap = if l.over {
            if (0.2..=0.9).contains(&l.gap) { l.gap } else { 0.55 }
        } else if out.is_empty() {
            0.0
        } else {
            l.gap.clamp(-0.2, 0.3)
        };
        l.gap = (l.gap * 100.0).round() / 100.0;
        out.push(l);
    }
    while out.last().is_some_and(|l| l.over) {
        out.pop();
    }
    resolve(&mut out);
    out
}

const ABBR: &[&str] = &[
    "z.", "bzw.", "usw.", "etc.", "ca.", "Nr.", "Dr.", "Prof.", "vs.", "inkl.", "ggf.", "evtl.", "bzgl.", "d.h.", "u.a.",
];
const UNITS: &[&str] = &["km", "kg", "cm", "mm", "ml", "GB", "MB", "KB", "TB", "EUR", "USD", "CHF", "KI", "API", "PDF"];

/// Rule violations per docs/podcast-prompt.md (used for the LLM retry). `words` = target word count, 0 = skip.
pub fn check(lines: &[Line], words: usize) -> Vec<String> {
    let mut e = vec![];
    let mut run = ("", 0);
    for (i, l) in lines.iter().enumerate() {
        let n = i + 1;
        if let Err(x) = segments(&l.text) {
            e.push(format!("Zeile {n}: {x}"));
        }
        let t = &plain(&l.text);
        if t.chars().any(|c| c.is_ascii_digit()) {
            e.push(format!("Zeile {n}: Ziffern ausschreiben: {t}"));
        }
        if t.chars().any(|c| "%€$£&/+=#*_<>|~^\\".contains(c)) {
            e.push(format!("Zeile {n}: Sonderzeichen entfernen: {t}"));
        }
        let toks: Vec<&str> = t.split_whitespace().collect();
        if toks.iter().any(|w| ABBR.contains(w) || UNITS.contains(&w.trim_matches(|c: char| !c.is_alphanumeric()))) {
            e.push(format!("Zeile {n}: Abkürzung ausschreiben: {t}"));
        }
        if t.contains("http") || t.contains("www.") || t.contains('@') {
            e.push(format!("Zeile {n}: keine URLs oder Mailadressen"));
        }
        if t.contains(['[', ']', '(', ')']) {
            e.push(format!("Zeile {n}: keine Klammern außer Tags wie [laughter] und [pause 1s]"));
        }
        if toks.len() > 35 {
            e.push(format!("Zeile {n}: zu lang ({} Wörter, max. fünfunddreißig)", toks.len()));
        }
        if !l.over {
            run = if run.0 == l.who { (&l.who, run.1 + 1) } else { (&l.who, 1) };
            if run.1 == 3 {
                e.push(format!("Zeile {n}: dreimal hintereinander derselbe Sprecher"));
            }
        }
        if i > 0 && l.text.contains("[laughter]") && lines[i - 1].text.contains("[laughter]") {
            e.push(format!("Zeile {n}: [laughter] nicht in zwei Zeilen hintereinander"));
        }
    }
    if words > 0 {
        let w: usize = lines.iter().filter(|l| !l.over).map(|l| plain(&l.text).split_whitespace().count()).sum();
        if w * 10 < words * 7 {
            e.push(format!("Skript zu kurz: {w} Wörter, Ziel etwa {words}"));
        } else if w * 10 > words * 14 {
            e.push(format!("Skript zu lang: {w} Wörter, Ziel etwa {words}"));
        }
    }
    e
}

/// Remaining non-speakable text (warnings only).
pub fn problems(lines: &[Line]) -> Vec<String> {
    check(lines, 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ref_script_roundtrip() {
        let s = include_str!("../../assets/example_script.txt");
        let v = parse(s).unwrap();
        assert_eq!(v.len(), 32);
        assert_eq!(v[3], Line::new("F", -0.15, "Ja, das ist ja das Problem! [laughter]"));
        assert!(v[5].over);
        assert_eq!(parse(&format(&v)).unwrap(), v);
        assert_eq!(format(&v), s.lines().filter(|l| !l.trim().is_empty() && !l.starts_with('#')).map(|l| format!("{}\n", l.trim())).collect::<String>());
        assert_eq!(v[5].who, "F");
        assert_eq!(v[14].who, "F");
        assert!(problems(&v).is_empty(), "{:?}", problems(&v));
        for ex in [include_str!("../../assets/example_sauerteig.txt"), include_str!("../../assets/example_wale.txt")] {
            let v = parse(ex).unwrap();
            assert!(check(&v, 220).iter().all(|e| e.contains("Skript zu")), "{:?}", check(&v, 220));
        }
    }

    #[test]
    fn checks() {
        let l = |w, t: &str| Line::ofb(w, 0.1, t);
        let e = check(&[l('O', "Das kostet 5 km bzw. so (siehe www.x.de)."), l('O', "a [laughter]"), l('O', "b [laughter]")], 100);
        assert_eq!(e.len(), 7, "{e:?}");
    }

    #[test]
    fn all_tags_and_pauses_pass() {
        let l = |t: &str| Line::new("O", 0.1, t);
        for g in funkhaus_audio::script::TAGS {
            assert!(problems(&[l(&format!("Hallo [{g}] Welt."))]).is_empty(), "{g}");
        }
        for p in ["[pause 500ms]", "[pause 1.5s]", "[pause]", "Ja. [pause 10s] Nein."] {
            assert!(problems(&[l(&format!("Hallo. {p}"))]).is_empty(), "{p}");
        }
        assert!(problems(&[l("[sigh]"), l("[pause 1.5s]")]).is_empty());
        assert_eq!(tidy(vec![l("Hallo."), l("[pause 1s]"), l("[sigh]"), l("...")]).len(), 3);
        let e = problems(&[l("Hallo. [pause 11s]")]);
        assert!(e.len() == 1 && e[0].contains("10"), "{e:?}");
        assert_eq!(problems(&[l("Ha [bogus] ho")]).len(), 1);
    }

    #[test]
    fn speakable_tokens() {
        assert_eq!(speakable("Auf Mallorca. Die Nr. eins, ca. drei, z. B. hier, usw., Ecke."), "Auf Mallorca. Die Nummer eins, circa drei, zum Beispiel hier, und so weiter, Ecke.");
        assert_eq!(speakable("- Strich vorne"), "Strich vorne");
        assert_eq!(speakable("Kanada."), "Kanada.");
    }

    #[test]
    fn tidy_rules() {
        let l = |w, g, t: &str| Line::ofb(w, g, t);
        let v = tidy(vec![
            l('B', 0.5, "Mhm."),
            l('F', 2.0, "Das kostet 5 € bzw. 10%."),
            l('B', 1.5, "Genau."),
            l('B', 0.5, "Ja."),
            l('O', -1.0, "..."),
            l('F', 0.1, "Okay."),
            l('B', 0.5, "Das ist ein ganzer Satz mit vielen Wörtern."),
        ]);
        assert_eq!(v.len(), 5);
        assert_eq!(v[0], l('O', 0.0, "Mhm."));
        assert_eq!(v[1], l('F', 0.3, "Das kostet 5 Euro beziehungsweise 10 Prozent."));
        assert_eq!(v[2], Line { who: "O".into(), ..l('B', 0.55, "Genau.") });
        assert_eq!((v[4].who.as_str(), v[4].gap), ("O", 0.1));
        assert_eq!(problems(&v).len(), 1);
    }
}
