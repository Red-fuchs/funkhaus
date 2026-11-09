//! Shortening proposal for a too-long sync cue (docs/stimmwerk/r13-kuerzen.md): one LLM proposal, checked in code
//! (numbers and number words kept, length band ±10 %, TTS-safe, one line), one retry with the violations. Never applied:
//! the caller shows it, the user decides. A proposal that changes a number is rejected outright.
use crate::wdiff::numbers_changed;
use anyhow::{Result, bail};
use funkhaus_llm::{Msg, parse_json, sys, user};
use serde::Serialize;
use serde_json::{Value, json};

pub const SYSTEM: &str = "Du kürzt einen deutschen Sprechtext für eine Untertitel-Zielzeit. Du antwortest NUR mit JSON nach Schema, kein Text davor oder danach.

REGELN
- Ein Vorschlag: ein Satz oder eine Zeile, kein Zeilenumbruch.
- Keine neuen Fakten, keine Ergänzungen, keine Umdeutung, keine Wertung.
- Alle Zahlen, Zahlwörter, Daten, Namen, Geld- und Prozentangaben aus dem Original bleiben erhalten, in gleicher Form (Wort bleibt Wort, Ziffer bleibt Ziffer).
- Keine neuen Ziffern, Symbole, Abkürzungen oder Klammern.
- Kürzen nur durch Weglassen: Füllwörter (also, eben, ehrlich gesagt, halt), Nebensätze, Wiederholungen, Ausschmückungen.
- Abschwächungen (etwa, ungefähr, vermutlich, könnte, soll) und Verneinungen bleiben erhalten.
- Personen bleiben namentlich. Ein Pronomen nur ersetzen, wenn der Bezug eindeutig bleibt.
- Ziel: zeichen im Bereich target_chars ±10 %. Zeichen = alle Zeichen inkl. Leerzeichen und Satzzeichen.
- Ist das Ziel nur mit Faktenverlust erreichbar: die kürzeste faktentreue Fassung liefern und in begruendung die Abweichung nennen.

AUSGABE
{\"vorschlag\": string, \"zeichen\": integer, \"begruendung\": string}";

pub fn schema() -> Value {
    json!({"type": "object", "additionalProperties": false, "required": ["vorschlag", "zeichen", "begruendung"],
        "properties": {"vorschlag": {"type": "string"}, "zeichen": {"type": "integer"}, "begruendung": {"type": "string"}}})
}

/// Characters that fit the window at `rate` chars/s.
pub fn target_chars(slot_s: f64, rate: f64) -> usize {
    (slot_s * rate).round().max(1.0) as usize
}

pub fn messages(text: &str, slot_s: f64, rate: f64) -> Vec<Msg> {
    vec![sys(SYSTEM), user(format!("Original: {}\nZielzeit: {slot_s:.2} Sekunden\nSprechtempo: {rate:.1} Zeichen pro Sekunde (gemessen)\ntarget_chars: {}", text.trim(), target_chars(slot_s, rate)))]
}

const NUM_STEMS: &[&str] = &[
    "null", "zwei", "drei", "vier", "fünf", "sechs", "sieb", "acht", "neun", "zehn", "elf", "zwölf", "zwanzig", "dreißig", "vierzig", "fünfzig", "sechzig", "siebzig", "achtzig", "neunzig",
    "hundert", "tausend", "million", "milliarde", "erst", "dritt", "januar", "februar", "märz", "april", "mai", "juni", "juli", "august", "september", "oktober", "november", "dezember",
];

fn norm_tokens(s: &str) -> Vec<String> {
    s.split_whitespace().map(|w| w.trim_matches(|c: char| !c.is_alphanumeric()).to_lowercase()).filter(|w| !w.is_empty()).collect()
}

/// Tokens that spell a number, ordinal or month (`eins` exact, else stem prefix: dreihundert, fünfzehnten, zweiundvierzig).
pub fn number_words(s: &str) -> Vec<String> {
    norm_tokens(s).into_iter().filter(|w| w == "eins" || NUM_STEMS.iter().any(|st| w.starts_with(st))).collect()
}

fn digits_norm(s: &str) -> String {
    s.split_whitespace().map(|w| w.trim_matches(|c: char| !c.is_alphanumeric())).filter(|w| !w.is_empty()).collect::<Vec<_>>().join(" ")
}

/// Violations of a proposal against the original (empty = acceptable). Number violations are listed first.
pub fn check(orig: &str, proposal: &str, target: usize) -> Vec<String> {
    let mut e = number_check(orig, proposal);
    let p = proposal.trim();
    if p.is_empty() {
        e.push("Vorschlag ist leer".into());
        return e;
    }
    if p.contains('\n') {
        e.push("Zeilenumbruch im Vorschlag".into());
    }
    let new: String = p.chars().filter(|c| !c.is_alphanumeric() && !c.is_whitespace() && !matches!(c, '.' | ',' | ';' | ':' | '!' | '?' | '-' | '–' | '\'' | '’' | '„' | '“' | '"') && !orig.contains(*c)).collect();
    if !new.is_empty() {
        e.push(format!("neue Zeichen, die nicht im Original stehen: {new}"));
    }
    let n = p.chars().count();
    let (lo, hi) = ((target as f64 * 0.9).floor() as usize, (target as f64 * 1.1).ceil() as usize);
    if n < lo || n > hi {
        e.push(format!("{n} Zeichen, Ziel {target} (erlaubt {lo}–{hi})"));
    }
    e
}

/// Digits changed/dropped/added (`wdiff::numbers_changed`) and number words missing.
pub fn number_check(orig: &str, proposal: &str) -> Vec<String> {
    let mut e = vec![];
    for (old, new) in numbers_changed(&digits_norm(orig), &digits_norm(proposal)) {
        e.push(match new {
            Some(n) => format!("Zahl geändert: {old} → {n}"),
            None => format!("Zahl fehlt: {old}"),
        });
    }
    let replaced: Vec<String> = e.iter().filter_map(|x| x.split(" → ").nth(1).map(String::from)).collect();
    for (new, _) in numbers_changed(&digits_norm(proposal), &digits_norm(orig)) {
        if !replaced.contains(&new) {
            e.push(format!("neue Zahl: {new}"));
        }
    }
    let have = number_words(proposal);
    for w in number_words(orig) {
        if !have.contains(&w) {
            e.push(format!("Zahlwort fehlt: {w}"));
        }
    }
    e.dedup();
    e
}

#[derive(Serialize, Clone, Debug)]
pub struct Suggestion {
    /// 1-based cue index
    pub cue: usize,
    pub who: String,
    pub text: String,
    pub slot_s: f64,
    pub rate: f64,
    pub target_chars: usize,
    pub chars: usize,
    /// None: rejected (numbers changed) – see `violations`
    pub proposal: Option<String>,
    pub proposal_chars: usize,
    pub reason: String,
    pub violations: Vec<String>,
    /// no violations at all; false = shown as a hint only
    pub accepted: bool,
    pub retried: bool,
}

pub type Llm<'a> = &'a mut dyn FnMut(&[Msg], &Value) -> Result<String>;

/// One proposal (+1 retry on violations). Numbers changed after the retry = no proposal.
pub fn suggest(llm: Llm, cue: usize, who: &str, text: &str, slot_s: f64, rate: f64) -> Result<Suggestion> {
    let target = target_chars(slot_s, rate);
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.is_empty() {
        bail!("Cue {cue}: kein Text");
    }
    let schema = schema();
    let mut msgs = messages(&text, slot_s, rate);
    let mut best: Option<(String, String, Vec<String>)> = None;
    let mut retried = false;
    loop {
        let out = llm(&msgs, &schema)?;
        let (prop, why, errs) = match parse_json(&out) {
            Some(v) => {
                let p = v["vorschlag"].as_str().unwrap_or_default().trim().to_string();
                let why = v["begruendung"].as_str().unwrap_or_default().trim().to_string();
                let e = check(&text, &p, target);
                (p, why, e)
            }
            None => (String::new(), String::new(), vec!["Antwort ist kein gültiges JSON".to_string()]),
        };
        if best.as_ref().is_none_or(|b| errs.len() < b.2.len()) {
            best = Some((prop, why, errs.clone()));
        }
        if errs.is_empty() || retried {
            break;
        }
        retried = true;
        msgs.push(funkhaus_llm::asst(out));
        msgs.push(user(format!("Der Vorschlag verstößt gegen die Regeln:\n- {}\nLiefere einen neuen Vorschlag, NUR JSON nach Schema.", errs.join("\n- "))));
    }
    let (prop, reason, violations) = best.unwrap_or_default();
    let numbers_bad = !number_check(&text, &prop).is_empty() || prop.is_empty();
    Ok(Suggestion {
        cue,
        who: who.into(),
        chars: text.chars().count(),
        text,
        slot_s,
        rate,
        target_chars: target,
        proposal_chars: prop.chars().count(),
        proposal: (!numbers_bad).then_some(prop),
        reason,
        accepted: violations.is_empty(),
        violations,
        retried,
    })
}

/// Proposals for the cues whose speech does not fit (too_long, or overlap into the next cue) of a sync project, or only
/// `cue` (any status). Reads only; nothing is written.
pub fn run(p: &crate::Project, s: &crate::sync::Sync, r: &crate::llm_cfg::Resolved, cue: Option<usize>, dirs: &[std::path::PathBuf]) -> Result<Vec<Suggestion>> {
    let mut llm = |m: &[Msg], sc: &Value| funkhaus_llm::complete(&r.backend, None, r.online_ok, m, Some(sc));
    run_with(p, s, r, cue, dirs, &mut llm)
}

/// `run` with the LLM call injected (GUI: cancellable `complete_cancel`).
pub fn run_with(p: &crate::Project, s: &crate::sync::Sync, r: &crate::llm_cfg::Resolved, cue: Option<usize>, dirs: &[std::path::PathBuf], llm: Llm) -> Result<Vec<Suggestion>> {
    r.allowed()?;
    let rep = crate::sync::check_project(p, s, dirs);
    let picked: Vec<&crate::sync::CueRep> = match cue {
        Some(n) => vec![rep.cues.iter().find(|c| c.index == n).ok_or_else(|| anyhow::anyhow!("Cue {n} gibt es nicht (1..={})", rep.cues.len()))?],
        None => rep.cues.iter().filter(|c| matches!(c.status, crate::sync::Kind::TooLong | crate::sync::Kind::Overlap) && c.cut_s > 0.0).collect(),
    };
    picked.into_iter().map(|c| suggest(llm, c.index, &c.who, &c.text, c.slot, c.rate)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn number_guard() {
        assert!(number_check("Der Preis liegt bei neunzehn Euro neunzig pro Monat.", "Neunzehn Euro neunzig pro Monat.").is_empty());
        assert_eq!(number_check("Die Firma zahlte dreihundert Euro im März.", "Die Firma zahlte Euro."), ["Zahlwort fehlt: dreihundert", "Zahlwort fehlt: märz"]);
        assert_eq!(number_check("Am 3. Oktober 1964 brannte er.", "Am 3. Oktober 1965 brannte er."), ["Zahl geändert: 1964 → 1965"]);
        assert_eq!(number_check("Am 3. Oktober 1964.", "Am dritten Oktober 1964."), ["Zahl geändert: 3 → dritten"]);
        assert_eq!(number_check("Es war stehen geblieben.", "Es war seit 1870 stehen geblieben."), ["neue Zahl: 1870"]);
        assert!(number_check("In der Nacht kam Achim.", "Achim kam.").is_empty(), "Nacht is no number word");
        assert_eq!(number_words("Zwölf Rollen, 12. Mai, eins, keine Einsicht, fünfzehnten"), ["zwölf", "mai", "eins", "fünfzehnten"]);
    }

    #[test]
    fn check_rules() {
        let o = "Die Lieferung kommt morgen früh am Vormittag.";
        assert!(check(o, "Die Lieferung kommt morgen früh.", 32).is_empty());
        assert_eq!(check(o, "Die Lieferung kommt.", 32), ["20 Zeichen, Ziel 32 (erlaubt 28–36)"]);
        assert!(check(o, "Die Lieferung kommt morgen früh.", 33).is_empty());
        assert!(check(o, "Die Lieferung kommt morgen früh (sicher).", 40).iter().any(|e| e.contains("neue Zeichen")));
        assert!(check(o, "Die Lieferung\nkommt morgen früh.", 32).iter().any(|e| e.contains("Zeilenumbruch")));
        assert_eq!(check(o, "  ", 32), ["Vorschlag ist leer"]);
        assert_eq!(target_chars(2.5, 13.0), 33);
        assert_eq!(target_chars(4.0, 14.0), 56);
        // digits present in the original may stay
        assert!(check("Es kostet 18,90 Euro pro Stück, sagte er.", "Es kostet 18,90 Euro pro Stück.", 32).is_empty());
        let m = messages("x", 2.5, 13.0);
        assert!(m[1].content.contains("target_chars: 33") && m[0].content.contains("NUR mit JSON"));
    }

    fn llm_seq(replies: Vec<&'static str>) -> impl FnMut(&[Msg], &Value) -> Result<String> {
        let mut r: Vec<String> = replies.into_iter().rev().map(String::from).collect();
        move |_, _| Ok(r.pop().unwrap_or_else(|| "{}".into()))
    }

    #[test]
    fn suggest_accept_retry_reject() {
        let o = "Der Vertrag wurde im März unterschrieben, und die Firma zahlte dreihundert Euro Vorschuss.";
        let mut ok = llm_seq(vec![r#"{"vorschlag": "Im März unterschrieben, die Firma zahlte dreihundert Euro.", "zeichen": 58, "begruendung": "Nebensatz weg."}"#]);
        let s = suggest(&mut ok, 2, "ERZ", o, 4.0, 14.0).unwrap();
        assert!(s.accepted && !s.retried && s.violations.is_empty());
        assert_eq!((s.target_chars, s.proposal_chars, s.proposal.as_deref()), (56, 58, Some("Im März unterschrieben, die Firma zahlte dreihundert Euro.")));
        // first too short -> retry with the violation, second fine
        let mut calls = 0;
        let mut seq = llm_seq(vec![r#"{"vorschlag": "Im März: dreihundert Euro.", "zeichen": 26, "begruendung": ""}"#, r#"{"vorschlag": "Im März unterschrieben, die Firma zahlte dreihundert Euro.", "zeichen": 58, "begruendung": "ok"}"#]);
        let mut llm = |m: &[Msg], v: &Value| {
            calls += 1;
            if calls == 2 {
                assert!(m[3].content.contains("Zeichen, Ziel 56"), "{}", m[3].content);
            }
            seq(m, v)
        };
        let s = suggest(&mut llm, 2, "ERZ", o, 4.0, 14.0).unwrap();
        assert!(s.accepted && s.retried && calls == 2);
        // numbers changed twice: rejected, no proposal, but no error
        let mut bad = llm_seq(vec![r#"{"vorschlag": "Im März zahlte die Firma zweihundert Euro Vorschuss, so der Vertrag.", "zeichen": 58, "begruendung": ""}"#]);
        let s = suggest(&mut bad, 2, "ERZ", o, 4.0, 14.0).unwrap();
        assert!(s.proposal.is_none() && !s.accepted && s.retried);
        assert!(s.violations.iter().any(|v| v == "Zahlwort fehlt: dreihundert"), "{:?}", s.violations);
        // only the band violated: proposal kept as a hint, not accepted
        let mut short = llm_seq(vec![r#"{"vorschlag": "Im März: die Firma zahlte dreihundert Euro.", "zeichen": 1, "begruendung": ""}"#]);
        let s = suggest(&mut short, 2, "ERZ", o, 4.0, 14.0).unwrap();
        assert!(s.proposal.is_some() && !s.accepted && s.violations.len() == 1);
        let mut junk = llm_seq(vec!["???"]);
        let s = suggest(&mut junk, 1, "ERZ", o, 4.0, 14.0).unwrap();
        assert!(s.proposal.is_none() && s.violations[0].contains("JSON"));
        assert!(suggest(&mut junk, 1, "ERZ", "  ", 4.0, 14.0).is_err());
    }
}
