//! R14 Personendaten-Heuristik (docs/stimmwerk/r14-online-warning.md). Draft, nicht in lib.rs verdrahtet. Keine Garantie.
use std::ops::Range;

/// Prioritaet = Deklarationsreihenfolge (IBAN > Telefon > Adresse > E-Mail > Gesundheit > Name).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    Iban,
    Phone,
    Address,
    Email,
    Health,
    Name,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    pub kind: Kind,
    pub span: Range<usize>,
    pub snippet: String,
}

const HEALTH: &[&str] = &[
    "krankheit", "krebs", "tumor", "diagnos", "therapi", "depress", "burnout", "diabet", "insulin", "blutdruck", "medikament", "metformin", "psychiat", "schizophren", "epilep", "hiv", "aids", "hepatitis", "schwangerschaft", "allergie", "demenz", "arztbrief", "krankenhaus", "reha", "behinderung", "sucht",
];
const STREET: &[&str] = &["straße", "strasse", "str.", "weg", "gasse", "allee", "platz", "ring", "damm"];
// Satzanfaenge, Monate, Wochentage, Firmen-Suffixe. Anreden (Herr/Frau) absichtlich nicht drin.
const STOP: &[&str] = &[
    "Die", "Der", "Das", "Ein", "Eine", "Wir", "Ihr", "Sie", "Es", "Hallo", "Liebe", "Lieber", "Sehr", "Guten", "Danke", "Bitte", "Frage", "Neue", "Neuer", "Neues", "Erste", "Letzte", "Gute", "Deutsche", "Europäische", "Bundes", "GmbH", "AG", "UG", "KG", "Ende", "Anfang", "Mitte", "Januar", "Februar", "März", "April", "Mai", "Juni", "Juli", "August", "September", "Oktober", "November", "Dezember", "Montag", "Dienstag", "Mittwoch", "Donnerstag", "Freitag", "Samstag", "Sonntag",
];

pub fn scan(t: &str) -> Vec<Hit> {
    let mut c: Vec<(Kind, Range<usize>)> = Vec::new();
    c.extend(ibans(t).into_iter().map(|r| (Kind::Iban, r)));
    c.extend(phones(t).into_iter().map(|r| (Kind::Phone, r)));
    c.extend(addrs(t).into_iter().map(|r| (Kind::Address, r)));
    c.extend(emails(t).into_iter().map(|r| (Kind::Email, r)));
    c.extend(health(t).into_iter().map(|r| (Kind::Health, r)));
    c.extend(names(t).into_iter().map(|r| (Kind::Name, r)));
    c.sort_by_key(|(k, r)| (*k, r.start));
    let mut out: Vec<Hit> = Vec::new();
    for (kind, span) in c {
        if out.iter().any(|h| h.span.start < span.end && span.start < h.span.end) {
            continue;
        }
        let snippet = mask(kind, &t[span.clone()]);
        out.push(Hit { kind, span, snippet });
    }
    out.sort_by_key(|h| h.span.start);
    out
}

fn runs(t: &str, f: impl Fn(char) -> bool) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    let mut st: Option<usize> = None;
    for (i, c) in t.char_indices() {
        match (f(c), st) {
            (true, None) => st = Some(i),
            (false, Some(s)) => {
                out.push(s..i);
                st = None;
            }
            _ => {}
        }
    }
    if let Some(s) = st {
        out.push(s..t.len());
    }
    out
}

fn emails(t: &str) -> Vec<Range<usize>> {
    let b = t.as_bytes();
    let loc = |c: u8| c.is_ascii_alphanumeric() || b"._+-".contains(&c);
    let dom = |c: u8| c.is_ascii_alphanumeric() || b".-".contains(&c);
    t.match_indices('@')
        .filter_map(|(at, _)| {
            let mut s = at;
            while s > 0 && loc(b[s - 1]) {
                s -= 1;
            }
            let mut e = at + 1;
            while e < b.len() && dom(b[e]) {
                e += 1;
            }
            while e > at + 1 && b[e - 1] == b'.' {
                e -= 1;
            }
            let d = &t[at + 1..e];
            let ok = s < at && d.contains('.') && d.split('.').all(|l| !l.is_empty());
            ok.then_some(s..e)
        })
        .collect()
}

fn is_date(c: &str) -> bool {
    let p: Vec<&str> = c.split('.').collect();
    p.len() == 3
        && (1..=2).contains(&p[0].len())
        && (1..=2).contains(&p[1].len())
        && (2..=4).contains(&p[2].len())
        && p.iter().all(|x| x.bytes().all(|b| b.is_ascii_digit()))
}

fn phones(t: &str) -> Vec<Range<usize>> {
    let b = t.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let pre = i == 0 || !(b[i - 1].is_ascii_alphanumeric() || b".:/-,_".contains(&b[i - 1]));
        if !(pre && (b[i] == b'+' || b[i] == b'0')) {
            i += 1;
            continue;
        }
        let mut e = i + 1;
        while e < b.len() && (b[e].is_ascii_digit() || b" /().-".contains(&b[e])) {
            e += 1;
        }
        while e > i && !b[e - 1].is_ascii_digit() {
            e -= 1;
        }
        let c = &t[i..e];
        if c.bytes().filter(u8::is_ascii_digit).count() >= 7 && !is_date(c) {
            out.push(i..e);
        }
        i = e.max(i + 1);
    }
    out
}

fn iban_ok(s: &str) -> bool {
    let n: String = s.chars().filter(|c| !c.is_whitespace()).collect();
    n[4..]
        .chars()
        .chain(n[..4].chars())
        .fold(0u32, |r, c| match c.to_digit(36) {
            Some(v) if v < 10 => (r * 10 + v) % 97,
            Some(v) => (r * 100 + v) % 97,
            None => r,
        })
        == 1
}

fn ibans(t: &str) -> Vec<Range<usize>> {
    let b = t.as_bytes();
    let up = |c: u8| c.is_ascii_uppercase() || c.is_ascii_digit();
    let mut out = Vec::new();
    for i in 0..b.len() {
        if i + 4 > b.len() || (i > 0 && b[i - 1].is_ascii_alphanumeric()) {
            continue;
        }
        if !(b[i].is_ascii_uppercase() && b[i + 1].is_ascii_uppercase() && b[i + 2].is_ascii_digit() && b[i + 3].is_ascii_digit()) {
            continue;
        }
        let (mut ends, mut p, mut g) = (Vec::new(), i + 4, 0usize);
        while g < 7 {
            let q = p + usize::from(b.get(p) == Some(&b' '));
            if q + 4 > b.len() || !b[q..q + 4].iter().all(|&c| up(c)) {
                break;
            }
            p = q + 4;
            g += 1;
            if g >= 2 && !b.get(p).is_some_and(u8::is_ascii_alphanumeric) {
                ends.push(p);
            }
        }
        let q = p + usize::from(b.get(p) == Some(&b' '));
        let k = (0..4).take_while(|&k| q + k < b.len() && up(b[q + k])).count();
        if g >= 2 && k > 0 && !b.get(q + k).is_some_and(u8::is_ascii_alphanumeric) {
            ends.push(q + k);
        }
        if let Some(&e) = ends.iter().rev().find(|&&e| iban_ok(&t[i..e])) {
            out.push(i..e);
        }
    }
    out
}

fn streets(t: &str) -> Vec<Range<usize>> {
    let b = t.as_bytes();
    runs(t, |c| c.is_alphabetic() || c == '-' || c == '.')
        .into_iter()
        .filter_map(|r| {
            let w = &t[r.clone()];
            let (lw, n) = (w.to_lowercase(), w.chars().count());
            let suf = w.chars().next()?.is_uppercase() && STREET.iter().any(|s| lw.ends_with(*s) && n > s.chars().count());
            if !suf {
                return None;
            }
            let s0 = r.end + b[r.end..].iter().take_while(|&&c| c == b' ').count();
            let dg = b[s0..].iter().take_while(|c| c.is_ascii_digit()).count();
            if dg == 0 {
                return None;
            }
            let mut e = s0 + dg;
            if b.get(e).is_some_and(u8::is_ascii_lowercase) {
                e += 1;
            }
            Some(r.start..e)
        })
        .collect()
}

fn plzs(t: &str) -> Vec<Range<usize>> {
    let b = t.as_bytes();
    let mut out = Vec::new();
    for i in 0..b.len() {
        if i + 5 > b.len() || !b[i..i + 5].iter().all(u8::is_ascii_digit) {
            continue;
        }
        if (i > 0 && (b[i - 1].is_ascii_alphanumeric() || b[i - 1] == b'_')) || b.get(i + 5).is_some_and(u8::is_ascii_digit) {
            continue;
        }
        let p = i + 5 + b[i + 5..].iter().take_while(|&&c| c == b' ').count();
        if p == i + 5 {
            continue;
        }
        let rest = &t[p..];
        let ort = rest.chars().take_while(|c| c.is_alphanumeric() || *c == '-');
        let (n, len) = (ort.clone().take(2).count(), ort.map(char::len_utf8).sum::<usize>());
        if n == 2 && rest.chars().next().is_some_and(char::is_uppercase) {
            out.push(i..p + len);
        }
    }
    out
}

fn gap(t: &str, a: &Range<usize>, b: &Range<usize>) -> usize {
    let (x, y) = if a.end <= b.start {
        (a.end, b.start)
    } else if b.end <= a.start {
        (b.end, a.start)
    } else {
        return 0;
    };
    t[x..y].chars().count()
}

/// Strasse+Nr und PLZ+Ort im Abstand <= 40 Zeichen = 1 Treffer.
fn addrs(t: &str) -> Vec<Range<usize>> {
    let pl = plzs(t);
    let mut used = vec![false; pl.len()];
    let mut out = Vec::new();
    for s in streets(t) {
        match pl.iter().enumerate().find(|&(j, p)| !used[j] && gap(t, &s, p) <= 40) {
            Some((j, p)) => {
                used[j] = true;
                out.push(s.start.min(p.start)..s.end.max(p.end));
            }
            None => out.push(s),
        }
    }
    out.extend(pl.iter().enumerate().filter(|(j, _)| !used[*j]).map(|(_, p)| p.clone()));
    out
}

fn is_name(w: &str) -> bool {
    let mut c = w.chars();
    c.next().is_some_and(char::is_uppercase) && w.chars().count() >= 2 && c.all(char::is_lowercase)
}

/// Weiches Signal: zwei Wörter mit Großbuchstabe, nur Whitespace dazwischen, keine Stopwörter.
fn names(t: &str) -> Vec<Range<usize>> {
    runs(t, char::is_alphabetic)
        .windows(2)
        .filter_map(|w| {
            let (a, b) = (&w[0], &w[1]);
            let (wa, wb) = (&t[a.clone()], &t[b.clone()]);
            let ok = is_name(wa) && is_name(wb) && t[a.end..b.start].chars().all(char::is_whitespace) && !STOP.contains(&wa) && !STOP.contains(&wb);
            ok.then_some(a.start..b.end)
        })
        .collect()
}

fn health(t: &str) -> Vec<Range<usize>> {
    runs(t, char::is_alphabetic)
        .into_iter()
        .filter(|r| {
            let w = t[r.clone()].to_lowercase();
            HEALTH.iter().any(|s| w.starts_with(*s))
        })
        .collect()
}

fn mask(k: Kind, s: &str) -> String {
    match k {
        Kind::Email => {
            let (l, d) = s.split_once('@').unwrap_or(("", s));
            format!("{}***@{d}", l.chars().next().unwrap_or('*'))
        }
        Kind::Iban => {
            let n = s.chars().filter(|c| !c.is_whitespace()).count();
            let mut i = 0;
            s.chars()
                .map(|c| {
                    if c.is_whitespace() {
                        return c;
                    }
                    i += 1;
                    if i <= 4 || i > n - 2 { c } else { '*' }
                })
                .collect()
        }
        _ => {
            let mut prev = true;
            s.chars()
                .map(|c| {
                    let o = if c.is_whitespace() || prev { c } else { '*' };
                    prev = c.is_whitespace();
                    o
                })
                .collect()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [Kind; 6] = [Kind::Iban, Kind::Phone, Kind::Address, Kind::Email, Kind::Health, Kind::Name];

    fn cnt(h: &[Hit], k: Kind) -> usize {
        h.iter().filter(|x| x.kind == k).count()
    }

    // Reihenfolge wie ALL: Iban, Phone, Address, Email, Health, Name
    #[test]
    fn r14_strings() {
        let cases: [(&str, &str, [usize; 6]); 15] = [
            ("T01", "Schreib an anna.schmidt@example.com bitte.", [0, 0, 0, 1, 0, 0]),
            ("T02", "Die Sitzung beginnt um 14:30 Uhr am 12.03.2026.", [0; 6]),
            ("T03", "Ruf mich unter +49 30 1234567 an.", [0, 1, 0, 0, 0, 0]),
            ("T04", "Die Preise steigen um 3,5 % und kosten 2 Euro 50.", [0; 6]),
            ("T05", "IBAN: DE89 3704 0044 0532 0130 00", [1, 0, 0, 0, 0, 0]),
            ("T06", "Wohnt in der Musterstraße 12, 10115 Berlin.", [0, 0, 1, 0, 0, 0]),
            ("T07", "Max Mustermann hat Diabetes Typ 2.", [0, 0, 0, 0, 1, 1]),
            ("T08", "Die Regierung hat neue Regeln beschlossen.", [0; 6]),
            ("T09", "Der Vertrag mit Schmidt GmbH läuft bis Ende Juni.", [0; 6]),
            ("T10", "Sie nimmt Insulin, der Blutdruck ist stabil.", [0, 0, 0, 0, 2, 0]),
            ("T11", "Tel: 0151 23456789 oder mail: info@beispiel.de", [0, 1, 0, 1, 0, 0]),
            ("T12", "Dateiname: video_2026-10-12_final.mp4, Version 3.2.1", [0; 6]),
            ("T13", "Hallo Anna, wir treffen uns in der Schillerstraße 4a.", [0, 0, 1, 0, 0, 0]),
            ("T14", "Krankheit: Krebs im Stadium 2, Therapie ab Montag.", [0, 0, 0, 0, 3, 0]),
            ("T15", "Die Zahl 0123 ist kein Telefon, Code 98765.", [0; 6]),
        ];
        for (id, text, exp) in cases {
            let h = scan(text);
            for (k, n) in ALL.iter().zip(exp) {
                assert_eq!(cnt(&h, *k), n, "{id} {k:?}");
            }
        }
    }

    #[test]
    fn masks() {
        let h = scan("IBAN: DE89 3704 0044 0532 0130 00");
        assert_eq!(h[0].snippet, "DE89 **** **** **** **** 00");
        let h = scan("Schreib an anna.schmidt@example.com bitte.");
        assert_eq!(h[0].snippet, "a***@example.com");
    }

    #[test]
    fn iban_bad_checksum() {
        assert_eq!(cnt(&scan("DE90 3704 0044 0532 0130 00"), Kind::Iban), 0);
    }
}
