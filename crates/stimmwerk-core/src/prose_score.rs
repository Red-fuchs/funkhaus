//! Score of a prose -> role script result against a reference (fixtures/stimmwerk/prosa/README.md §0–§6,
//! docs/stimmwerk/r13-prose-score.md): format, line alignment (recall/precision), verbatim speech, speaker accuracy,
//! tag F1, numbers/date verbatim. `total` ranks, `pass` gates (a changed number never passes).
use crate::wdiff::numbers_changed;

pub const ERZ: &str = "ERZ";

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Report {
    /// §1 lines with a format error (fields, GAP, unknown ID, hallucinated digits)
    pub fmt_bad: usize,
    /// `# SEG` lines in the output
    pub seg: usize,
    /// 1-based output lines whose digits are not in the reference
    pub digit_lines: Vec<usize>,
    /// quote characters („ “ ") in the output
    pub quote_count: usize,
    pub fmt: f64,
    pub recall: f64,
    pub precision: f64,
    pub extra: usize,
    pub missing: usize,
    /// (verbatim, non-ERZ reference lines)
    pub speech_verbatim: (usize, usize),
    pub speech: f64,
    pub speaker_acc: f64,
    pub erz_as_fig: usize,
    pub fig_as_erz: usize,
    pub tag_f1: f64,
    /// dates verbatim in a same-ID line and no reference number lost
    pub date_ok: bool,
    /// reference digits missing/changed in the output: (old, replacement)
    pub numbers_lost: Vec<(String, Option<String>)>,
    /// digits only in the output
    pub numbers_added: Vec<(String, Option<String>)>,
    pub total: f64,
    pub pass: bool,
}

struct Line {
    id: String,
    text: String,
    w: Vec<String>,
    tags: Vec<String>,
}

struct Parsed {
    lines: Vec<Line>,
    seg: usize,
    quotes: usize,
    bad: usize,
    digit_lines: Vec<usize>,
}

/// Weights (r13-prose-score.md §2.5): speech and speaker are the product promise.
const W: [(f64, &str); 7] = [(0.10, "fmt"), (0.15, "recall"), (0.10, "precision"), (0.25, "speech"), (0.20, "speaker"), (0.10, "tags"), (0.10, "date")];

/// `out` scored against `expected` (both `ID|GAP|TEXT` scripts); `cast_ids` = allowed speaker IDs besides ERZ.
pub fn score(out: &str, expected: &str, cast_ids: &[String]) -> Report {
    let exp = parse(expected, cast_ids, None);
    let exp_digits: Vec<String> = exp.lines.iter().flat_map(|l| digit_tokens(&l.text)).collect();
    let g = parse(out, cast_ids, Some(&exp_digits));
    let (e, n) = (&exp.lines, &g.lines);

    // §2 alignment: same ID ∧ word F1 ≥ 0.8
    let al = lcs(e, n, |x, y| x.id == y.id && f1(&x.w, &y.w) >= 0.8);
    let m = al.len();
    let (recall, precision) = (r(m, e.len()), r(m, n.len()));

    // §3 verbatim speech over the text-only alignment (a wrong voice is a speaker error, not a speech error)
    let tal = lcs(e, n, |x, y| f1(&x.w, &y.w) >= 0.8);
    let mut tmap = vec![None; e.len()];
    for &(i, j) in &tal {
        tmap[i] = Some(j);
    }
    let fig: Vec<usize> = (0..e.len()).filter(|&i| e[i].id != ERZ).collect();
    let sp_ok = fig.iter().filter(|&&i| tmap[i].is_some_and(|j| contains_seq(&n[j].w, &e[i].w))).count();
    let speech = if fig.is_empty() { 1.0 } else { r(sp_ok, fig.len()) };

    // §4 speaker
    let same = tal.iter().filter(|&&(i, j)| e[i].id == n[j].id).count();
    let (mut erz_as_fig, mut fig_as_erz) = (0, 0);
    for &(i, j) in &tal {
        match (e[i].id == ERZ, n[j].id == ERZ) {
            (true, false) => erz_as_fig += 1,
            (false, true) => fig_as_erz += 1,
            _ => {}
        }
    }
    let speaker = if tal.is_empty() { 0.0 } else { r(same, tal.len()) };

    // §5 tags: TP over aligned pairs, F1 over all tags
    let (te, tg): (usize, usize) = (e.iter().map(|x| x.tags.len()).sum(), n.iter().map(|x| x.tags.len()).sum());
    let mut tp = 0;
    for &(i, j) in &al {
        let mut left = n[j].tags.clone();
        for t in &e[i].tags {
            if let Some(p) = left.iter().position(|y| y == t) {
                left.swap_remove(p);
                tp += 1;
            }
        }
    }
    let tags = if te + tg == 0 { 1.0 } else { 2.0 * tp as f64 / (te + tg) as f64 };

    // §6 numbers and dates verbatim
    let norm = |ls: &[Line]| ls.iter().map(|l| digit_tokens(&l.text).join(" ")).filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" ");
    let numbers_lost = numbers_changed(&norm(e), &norm(n));
    let numbers_added = numbers_changed(&norm(n), &norm(e));
    let dates = e.iter().filter_map(|x| date_of(&x.text).map(|d| (x.id.as_str(), d))).all(|(id, d)| n.iter().any(|y| y.id == id && y.text.contains(&d)));
    let date_ok = dates && numbers_lost.is_empty();

    // §1 format
    let errs = g.bad + g.quotes + exp.seg.abs_diff(g.seg);
    let fmt = (1.0 - errs as f64 / n.len().max(1) as f64).max(0.0);

    let vals = [fmt, recall, precision, speech, speaker, tags, if date_ok { 1.0 } else { 0.0 }];
    let total = W.iter().zip(vals).map(|((w, _), v)| w * v).sum::<f64>();
    let pass = fmt == 1.0 && recall >= 0.9 && precision >= 0.9 && speech == 1.0 && speaker == 1.0 && tags == 1.0 && date_ok && numbers_added.is_empty();
    Report {
        fmt_bad: g.bad,
        seg: g.seg,
        digit_lines: g.digit_lines,
        quote_count: g.quotes,
        fmt,
        recall,
        precision,
        extra: n.len() - m,
        missing: e.len() - m,
        speech_verbatim: (sp_ok, fig.len()),
        speech,
        speaker_acc: speaker,
        erz_as_fig,
        fig_as_erz,
        tag_f1: tags,
        date_ok,
        numbers_lost,
        numbers_added,
        total: (total * 1000.0).round() / 1000.0,
        pass,
    }
}

/// Table keys of a `cast.toml` (fixtures/stimmwerk/prosa).
pub fn load_cast(toml_src: &str) -> Vec<String> {
    toml_src.parse::<toml::Table>().map(|t| t.keys().cloned().collect()).unwrap_or_default()
}

/// Script -> `{cast, lines}`-style check helper: IDs used in a script (ERZ excluded).
pub fn cast_of(script: &str) -> Vec<String> {
    let mut v: Vec<String> = script.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')).filter_map(|l| l.split('|').next()).map(|s| s.trim().trim_start_matches('+').to_string()).filter(|s| s != ERZ).collect();
    v.sort();
    v.dedup();
    v
}

fn parse(src: &str, cast: &[String], ref_digits: Option<&[String]>) -> Parsed {
    let mut p = Parsed { lines: vec![], seg: 0, quotes: 0, bad: 0, digit_lines: vec![] };
    for (ln, raw) in src.lines().enumerate() {
        let l = raw.trim();
        if l.starts_with("# SEG") {
            p.seg += 1;
            continue;
        }
        if l.is_empty() || l.starts_with('#') {
            continue;
        }
        let f: Vec<&str> = l.splitn(3, '|').collect();
        let (id, gap, text) = match f.as_slice() {
            &[i, g, t] => (i.trim().trim_start_matches('+'), g.trim(), t.trim()),
            _ => ("", "", ""),
        };
        let digits_ok = match ref_digits {
            Some(d) => digit_tokens(text).iter().all(|t| d.contains(t)),
            None => true,
        };
        if !digits_ok {
            p.digit_lines.push(ln + 1);
        }
        let ok = f.len() == 3 && gap.parse::<f64>().is_ok_and(|g| (0.0..=1.0).contains(&g)) && (id == ERZ || cast.iter().any(|c| c == id)) && digits_ok;
        if !ok {
            p.bad += 1;
        }
        p.quotes += text.chars().filter(|c| matches!(c, '„' | '“' | '"')).count();
        let (tags, body) = split_tags(text);
        p.lines.push(Line { id: id.to_string(), text: text.to_string(), w: words(&body), tags });
    }
    p
}

// Removes [laughter]/[sigh]/[pause …] and the other omnivoice tags, returns their names. Other brackets stay.
fn split_tags(s: &str) -> (Vec<String>, String) {
    let (mut tags, mut out, mut rest) = (vec![], String::new(), s);
    while let Some(i) = rest.find('[') {
        out += &rest[..i];
        rest = &rest[i..];
        let Some(j) = rest.find(']') else { break };
        let name = rest[1..j].split(|c: char| !c.is_alphabetic() && c != '-').next().unwrap_or("");
        if name == "pause" || funkhaus_audio::script::TAGS.contains(&name) {
            tags.push(name.to_string());
        } else {
            out += &rest[..=j];
        }
        rest = &rest[j + 1..];
    }
    out += rest;
    (tags, out)
}

fn words(s: &str) -> Vec<String> {
    s.to_lowercase().split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(str::to_string).collect()
}

/// Tokens with a digit, punctuation trimmed (`3.` -> `3`, `18,90` stays), tags removed.
fn digit_tokens(text: &str) -> Vec<String> {
    split_tags(text).1.split_whitespace().map(|w| w.trim_matches(|c: char| !c.is_alphanumeric()).to_string()).filter(|w| w.chars().any(|c| c.is_ascii_digit())).collect()
}

fn f1(a: &[String], b: &[String]) -> f64 {
    if a.is_empty() && b.is_empty() {
        return 1.0;
    }
    let mut left = b.to_vec();
    let mut same = 0;
    for x in a {
        if let Some(p) = left.iter().position(|y| y == x) {
            left.swap_remove(p);
            same += 1;
        }
    }
    2.0 * same as f64 / (a.len() + b.len()) as f64
}

fn lcs<T>(a: &[T], b: &[T], eq: impl Fn(&T, &T) -> bool) -> Vec<(usize, usize)> {
    let (n, m) = (a.len(), b.len());
    let mut dp = vec![vec![0u32; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            dp[i][j] = if eq(&a[i], &b[j]) { dp[i + 1][j + 1] + 1 } else { dp[i + 1][j].max(dp[i][j + 1]) };
        }
    }
    let (mut i, mut j, mut out) = (0, 0, vec![]);
    while i < n && j < m {
        if eq(&a[i], &b[j]) {
            out.push((i, j));
            i += 1;
            j += 1;
        } else if dp[i + 1][j] >= dp[i][j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    out
}

fn contains_seq(h: &[String], nd: &[String]) -> bool {
    nd.is_empty() || h.windows(nd.len()).any(|w| w == nd)
}

// First "3. Oktober 1964" pattern (digit token, word, 4-digit year), verbatim.
fn date_of(s: &str) -> Option<String> {
    let t: Vec<&str> = s.split_whitespace().collect();
    t.windows(3).find(|w| w[0].chars().any(|c| c.is_ascii_digit()) && w[2].len() == 4 && w[2].chars().all(|c| c.is_ascii_digit())).map(|w| w.join(" "))
}

fn r(a: usize, b: usize) -> f64 {
    if b == 0 { 0.0 } else { a as f64 / b as f64 }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXP: &str = include_str!("../../../fixtures/stimmwerk/prosa/kapitel1.expected.txt");
    const CAST: &str = include_str!("../../../fixtures/stimmwerk/prosa/cast.toml");

    fn cast() -> Vec<String> {
        load_cast(CAST)
    }

    fn edit(f: impl FnOnce(&mut Vec<String>)) -> String {
        let mut v: Vec<String> = EXP.lines().map(String::from).collect();
        f(&mut v);
        v.join("\n")
    }

    fn at(v: &[String], pre: &str) -> usize {
        v.iter().position(|l| l.starts_with(pre)).unwrap()
    }

    #[test]
    fn self_is_perfect() {
        assert_eq!(cast(), ["ERZ", "Ilse", "Jonas", "Mara"]);
        let s = score(EXP, EXP, &cast());
        assert_eq!((s.total, s.pass, s.seg, s.fmt_bad, s.quote_count), (1.0, true, 2, 0, 0));
        assert_eq!((s.fmt, s.recall, s.precision, s.speech, s.speaker_acc, s.tag_f1, s.date_ok), (1.0, 1.0, 1.0, 1.0, 1.0, 1.0, true));
        assert_eq!((s.missing, s.extra, s.speech_verbatim), (0, 0, (9, 9)));
        assert!(s.numbers_lost.is_empty() && s.numbers_added.is_empty() && s.digit_lines.is_empty());
    }

    #[test]
    fn missing_line() {
        let got = edit(|v| {
            let i = at(v, "ERZ|0.6|Der Flur");
            v.remove(i);
        });
        let s = score(&got, EXP, &cast());
        assert_eq!((s.missing, s.extra, s.pass), (1, 0, true));
        assert!(s.recall < 1.0 && s.total < 1.0 && s.total > 0.99);
        assert_eq!((s.precision, s.speech), (1.0, 1.0));
    }

    #[test]
    fn wrong_speaker_keeps_speech() {
        let got = edit(|v| {
            let i = at(v, "Mara|0.6|Du hast");
            v[i] = v[i].replacen("Mara", "Ilse", 1);
        });
        let s = score(&got, EXP, &cast());
        assert_eq!((s.missing, s.extra, s.speech, s.pass), (1, 1, 1.0, false));
        assert!(s.speaker_acc < 1.0 && s.speaker_acc > 0.9);
    }

    #[test]
    fn number_changed_gates_date() {
        let got = edit(|v| {
            let i = at(v, "ERZ|0.8|Am 3.");
            v[i] = v[i].replace("1964", "1965");
        });
        let s = score(&got, EXP, &cast());
        assert!(!s.date_ok && !s.pass && s.fmt < 1.0);
        assert_eq!(s.numbers_lost, [("1964".to_string(), Some("1965".to_string()))]);
        assert_eq!(s.digit_lines.len(), 1);
        assert!((s.total - 0.9).abs() < 0.01, "{}", s.total);
    }

    #[test]
    fn date_spelled_fails() {
        let got = edit(|v| {
            let i = at(v, "ERZ|0.8|Am 3.");
            v[i] = v[i].replace("3. Oktober 1964", "dritten Oktober neunzehnhundertvierundsechzig");
        });
        let s = score(&got, EXP, &cast());
        assert!(!s.date_ok && !s.pass);
        assert_eq!(s.numbers_lost.len(), 2);
        assert_eq!(s.fmt, 1.0, "no digits -> no format error, the date gate catches it");
    }

    #[test]
    fn attribution_extra() {
        let got = edit(|v| {
            let i = at(v, "Mara|0.6|Du hast");
            v.insert(i, "ERZ|0.5|sagte Ilse.".into());
        });
        let s = score(&got, EXP, &cast());
        assert_eq!((s.missing, s.extra, s.pass), (0, 1, true));
        assert_eq!((s.recall, s.speech, s.fmt), (1.0, 1.0, 1.0));
        assert!(s.precision < 1.0 && s.total < 1.0);
    }

    #[test]
    fn tag_dropped_and_hallucinated() {
        let got = edit(|v| {
            let i = at(v, "Jonas|0.6|Ich habe");
            v[i] = v[i].replace(" [laughter]", "");
        });
        let s = score(&got, EXP, &cast());
        assert!(s.tag_f1 < 1.0 && !s.pass && s.speech == 1.0);
        let got = edit(|v| {
            let i = at(v, "Mara|0.5|Welche");
            v[i] = v[i].replace("Welche", "[surprise-oh] Welche");
        });
        assert!(score(&got, EXP, &cast()).tag_f1 < 1.0);
    }

    #[test]
    fn digit_outside_date_and_quotes_and_seg() {
        let got = edit(|v| {
            let i = at(v, "ERZ|0.4|Die Uhr");
            v[i] = v[i].replace("war stehen", "war seit 1870 stehen");
        });
        let s = score(&got, EXP, &cast());
        assert!(s.fmt < 1.0 && !s.pass && s.date_ok);
        assert_eq!((s.digit_lines.len(), s.numbers_added), (1, vec![("1870".to_string(), None)]));
        let got = edit(|v| {
            let i = at(v, "Mara|0.5|Welche");
            v[i] = "Mara|0.5|„Welche Kiste?“".into();
            v.retain(|l| !l.starts_with("# SEG 2"));
        });
        let s = score(&got, EXP, &cast());
        assert_eq!((s.quote_count, s.seg), (2, 1));
        assert!(s.fmt < 1.0 && !s.pass);
        // unknown ID and GAP out of range are format errors
        let s = score("Otto|0.5|Hallo.\nERZ|2|Ja.\n", EXP, &cast());
        assert_eq!(s.fmt_bad, 2);
    }

    #[test]
    fn reordered_lines() {
        let got = edit(|v| {
            let (a, b) = (at(v, "Mara|0.6|Du"), at(v, "Jonas|0.6|Ich"));
            v.swap(a, b);
        });
        let s = score(&got, EXP, &cast());
        assert!(s.missing >= 1 && s.recall < 1.0 && s.total < 1.0);
    }

    /// Manual scoring of a real run: `STIMMWERK_SCORE_OUT=<script.txt> STIMMWERK_SCORE_EXP=<pN.expected.json>
    /// cargo test -p stimmwerk-core score_files -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn score_files() {
        let out = std::fs::read_to_string(std::env::var("STIMMWERK_SCORE_OUT").expect("STIMMWERK_SCORE_OUT")).unwrap();
        let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(std::env::var("STIMMWERK_SCORE_EXP").expect("STIMMWERK_SCORE_EXP")).unwrap()).unwrap();
        let exp = crate::prose::to_script(&v, 0.5).unwrap();
        let mut ids = cast_of(&exp);
        ids.extend(cast_of(&out));
        println!("{}", serde_json::to_string_pretty(&score(&out, &exp, &ids)).unwrap());
    }

    /// Set 2 (fixtures/stimmwerk/prose): expected JSON through `prose::to_script` scores itself perfect; p4's many
    /// numbers are no format error; a changed amount or an invented figure fails.
    #[test]
    fn prose_set_scores() {
        let fix = concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/stimmwerk/prose/");
        for n in ["p1", "p2", "p3", "p4"] {
            let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(format!("{fix}{n}.expected.json")).unwrap()).unwrap();
            let sc = crate::prose::to_script(&v, 0.5).unwrap();
            let s = score(&sc, &sc, &cast_of(&sc));
            assert!(s.pass && s.total == 1.0 && s.digit_lines.is_empty(), "{n}: {s:?}");
            if n == "p4" {
                let bad = sc.replace("849,50", "894,50");
                let s = score(&bad, &sc, &cast_of(&sc));
                assert!(!s.pass && !s.date_ok && s.numbers_lost.iter().any(|(o, _)| o == "849,50"), "{s:?}");
                let bad = sc.replace("1.250 Euro", "tausendzweihundertfünfzig Euro");
                assert!(!score(&bad, &sc, &cast_of(&sc)).pass);
            }
            if n == "p2" {
                let bad = sc.replace("ERZ|0.5|Wer dort", "JUNGE|0.5|Wer dort");
                let s = score(&bad, &sc, &cast_of(&sc));
                assert!(s.fmt_bad == 1 && s.erz_as_fig == 1 && s.missing == 1 && !s.pass, "{s:?}");
                let s = score(&bad, &sc, &["JUNGE".to_string()]);
                assert!(s.fmt_bad == 0 && s.erz_as_fig == 1 && s.speaker_acc < 1.0, "{s:?}");
            }
        }
    }
}
