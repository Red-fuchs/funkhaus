//! Prose chapter -> role script via LLM (docs/stimmwerk/prompts.md §1): JSON `{cast, lines}` validated in code (the
//! schema stays simple for strict modes), one retry with the error list, then post-processed – cast entries whose name
//! never occurs in the text fold into the narrator (`guard_cast`), lines with several sentences split at sentence ends
//! (`sentences`) – and written as `ID|GAP|TEXT` for script.txt. The cast becomes project speakers (persona unset) with
//! the LLM's voice hints as suggestion – nothing is cast automatically.
use crate::script::{Script, id_ok};
use anyhow::{Context, Result, bail};
use funkhaus_llm::{Msg, parse_json, sys, user};
use serde::Serialize;
use serde_json::{Value, json};

pub const SYSTEM: &str = "Du machst aus einem deutschen Prosatext ein Hörspiel-Rollenskript. Du antwortest NUR mit JSON nach Schema, kein Text davor oder danach.

REGELN
- Erzähler hat id \"ERZ\", name \"Erzähler\". Jede Figur mit wörtlicher Rede bekommt eine kurze id aus Buchstaben (z. B. \"MARIA\"), dazu name, description (ein Satz) und suggested_voice_instruct (englisch, z. B. \"female, middle-aged, low pitch\").
- cast enthält nur Figuren, die im Text vorkommen: mit Namen genannt, oder als Person beschrieben, die mehrmals spricht (dann Rollenname wie \"der Mann\", kein erfundener Name). Spricht eine unbenannte Person nur einmal, bleibt ihr Satz beim Erzähler.
- Schilder, Zettel, Aufschriften, Briefe, Durchsagen und Lautsprecher sind keine Figuren: ihr Wortlaut bleibt beim Erzähler (who \"ERZ\"), zusammen mit der Einleitung aus dem Text.
- Direkte Rede (in „…“, »…« oder \"…\") wörtlich aus dem Text, Wortlaut unverändert, ohne Anführungszeichen; who = die Figur, die spricht.
- Erzählertext wörtlich übernehmen, who \"ERZ\". Nichts umformulieren, nichts zusammenfassen.
- Genau EIN Satz je Zeile: Jeder Satz (Ende mit . ! ? …) wird eine eigene Zeile, auch wenn dieselbe Figur mehrere Sätze hintereinander spricht. Abkürzungen (z. B., Dr., Nr., S.) und Zahlen (3,5 oder 3.5) sind kein Satzende.
- Reine Sprecherzuordnungen wie „sagte Jonas“, „fragte sie leise“ entfallen (die Stimme macht das hörbar). Erzählsätze mit eigenem Inhalt bleiben.
- Nichts erfinden: keine Figuren, Sätze, Zahlen, Orte, Gefühle. Kommt eine Figur nicht vor, gibt es sie nicht.
- Zahlen, Daten, Namen, Satzzeichen genau so lassen, wie sie im Text stehen (keine Ausschreibung, keine Umformung).
- who muss eine id aus cast sein. Jede id in cast kommt in lines vor.
- gap: Pause in Sekunden vor der Zeile, 0 bis 1; null, wenn keine besondere Pause.
- over: true nur für Zwischenrufe, die gleichzeitig zur vorigen Zeile laufen; sonst null.
- Tags wie [laughter] oder [sigh] nur, wenn der Text selbst Lachen oder Seufzen der Figur beschreibt; dann am Anfang oder Ende der Figurenzeile. Erlaubt: [laughter], [sigh], [question-ah], [surprise-oh], [dissatisfaction-hnn]. Sonst kein Tag.
- Reihenfolge wie im Text. Überschriften und Markdown weglassen.";

pub const TAGS: &[&str] = &["laughter", "sigh", "question-ah", "surprise-oh", "dissatisfaction-hnn"];

/// Strict-mode friendly: no `pattern`, `minItems`, `enum` – those checks live in `validate`. Every property is
/// required (OpenAI strict rejects optional ones); `gap`/`over` are nullable instead (Anthropic accepts `null` types).
pub fn schema() -> Value {
    json!({
        "type": "object", "additionalProperties": false, "required": ["cast", "lines"],
        "properties": {
            "cast": {"type": "array", "items": {"type": "object", "additionalProperties": false,
                "required": ["id", "name", "description", "suggested_voice_instruct"],
                "properties": {"id": {"type": "string"}, "name": {"type": "string"}, "description": {"type": "string"}, "suggested_voice_instruct": {"type": "string"}}}},
            "lines": {"type": "array", "items": {"type": "object", "additionalProperties": false, "required": ["who", "text", "gap", "over"],
                "properties": {"who": {"type": "string"}, "text": {"type": "string"}, "gap": {"type": ["number", "null"]}, "over": {"type": ["boolean", "null"]}}}}
        }
    })
}

/// Markdown headings and emphasis markers dropped; paragraphs kept.
pub fn plain(src: &str) -> String {
    let t: Vec<&str> = src.lines().map(str::trim_end).filter(|l| !l.trim_start().starts_with('#')).collect();
    t.join("\n").replace("**", "").replace("__", "").trim().to_string()
}

pub fn messages(text: &str) -> Vec<Msg> {
    vec![sys(SYSTEM), user(format!("Text:\n<<<\n{}\n>>>", text.trim()))]
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Cast {
    pub id: String,
    pub name: String,
    pub description: String,
    /// Voice hint for `stimmwerk voice design --instruct`; not applied.
    pub suggested_voice_instruct: String,
}

fn s(v: &Value, k: &str) -> String {
    v[k].as_str().unwrap_or_default().trim().to_string()
}

/// Cast entries (`description`/`desc`, hints optional).
pub fn cast(v: &Value) -> Vec<Cast> {
    v["cast"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|c| Cast { id: s(c, "id"), name: s(c, "name"), description: if c.get("description").is_some() { s(c, "description") } else { s(c, "desc") }, suggested_voice_instruct: s(c, "suggested_voice_instruct") })
        .collect()
}

/// Error list (empty = valid): shape, ids, who ∈ cast, gap 0..=1 (null/missing = default), over bool, tags, cast used.
pub fn validate(v: &Value) -> Vec<String> {
    let mut e = vec![];
    let Some(cs) = v["cast"].as_array() else { return vec!["cast fehlt oder ist keine Liste".into()] };
    let Some(ls) = v["lines"].as_array() else { return vec!["lines fehlt oder ist keine Liste".into()] };
    let ids: Vec<String> = cs.iter().map(|c| s(c, "id")).collect();
    for (i, c) in cs.iter().enumerate() {
        let id = s(c, "id");
        if !id_ok(&id) || id.contains(['+', '|']) {
            e.push(format!("cast[{i}]: id {id:?} ungültig (Buchstaben, Ziffern, _ -; nicht B)"));
        } else if ids.iter().filter(|x| **x == id).count() > 1 {
            e.push(format!("cast: id {id:?} doppelt"));
        }
        if s(c, "name").is_empty() {
            e.push(format!("cast[{i}]: name fehlt"));
        }
    }
    if ls.is_empty() {
        e.push("lines ist leer".into());
    }
    let mut used = std::collections::BTreeSet::new();
    for (i, l) in ls.iter().enumerate() {
        let (who, text) = (s(l, "who"), s(l, "text"));
        if !ids.contains(&who) {
            e.push(format!("lines[{i}]: who {who:?} ist keine cast-id"));
        }
        used.insert(who);
        if text.is_empty() {
            e.push(format!("lines[{i}]: text leer"));
        }
        if text.contains('|') || text.contains('\n') {
            e.push(format!("lines[{i}]: text darf kein | und keinen Zeilenumbruch enthalten"));
        }
        if let Some(g) = l.get("gap").filter(|g| !g.is_null())
            && !g.as_f64().is_some_and(|g| (0.0..=1.0).contains(&g))
        {
            e.push(format!("lines[{i}]: gap {g} nicht in 0..1"));
        }
        if let Some(o) = l.get("over").filter(|o| !o.is_null()) {
            if !o.is_boolean() {
                e.push(format!("lines[{i}]: over {o} ist kein Wahrheitswert"));
            } else if i == 0 && *o == true {
                e.push("lines[0]: over vor der ersten Zeile".into());
            }
        }
        for t in tags(&text) {
            if !TAGS.contains(&t) {
                e.push(format!("lines[{i}]: Tag [{t}] nicht erlaubt (nur {})", TAGS.join(", ")));
            }
        }
    }
    for id in &ids {
        if !used.contains(id) {
            e.push(format!("cast-id {id:?} kommt in lines nicht vor"));
        }
    }
    e
}

/// `[name]` tags of a line (pauses excluded).
fn tags(text: &str) -> Vec<&str> {
    text.split('[').skip(1).filter_map(|r| r.split(']').next()).filter(|t| !t.starts_with("pause") && !t.contains(' ')).collect()
}

/// Words a period does not end a sentence after (lower-case; single letters and numbers are handled in `abbrev`).
const ABBR: &[&str] = &["dr", "prof", "nr", "st", "ca", "bzw", "usw", "etc", "vgl", "ggf", "evtl", "inkl", "exkl", "zzgl", "hr", "fr", "mio", "mrd", "tel", "abs", "art", "jh", "sog", "geb", "gest", "str", "bd", "aufl", "kap", "abb", "tab", "jan", "feb", "sept", "okt", "nov", "dez"];

/// The word before a period that keeps the sentence going: a number (`3.`, `3.5`), a single letter (`z. B.`, `S. 12`,
/// initials) or an `ABBR`.
fn abbrev(before: &[char]) -> bool {
    let w: String = before.iter().rev().take_while(|c| c.is_alphanumeric()).collect::<String>().chars().rev().collect();
    !w.is_empty() && (w.chars().all(|c| c.is_ascii_digit()) || w.chars().count() == 1 || ABBR.contains(&w.to_lowercase().as_str()))
}

/// One sentence per element: split after `. ! ? …` (closing quotes/brackets may follow) when whitespace and a word
/// starting with an upper-case letter, a digit or an opening quote follow – not after `abbrev` words and not before a
/// tag (`Ja. [laughter]` stays one line).
pub fn sentences(text: &str) -> Vec<String> {
    let cs: Vec<char> = text.chars().collect();
    let (mut out, mut from, mut i) = (vec![], 0, 0);
    while i < cs.len() {
        if !matches!(cs[i], '.' | '!' | '?' | '…') {
            i += 1;
            continue;
        }
        let mut j = i + 1;
        while j < cs.len() && matches!(cs[j], '.' | '!' | '?' | '…' | '“' | '”' | '«' | '»' | '"' | '\'' | '’' | ')') {
            j += 1;
        }
        let mut k = j;
        while k < cs.len() && cs[k].is_whitespace() {
            k += 1;
        }
        let next_ok = k > j && k < cs.len() && (cs[k].is_uppercase() || cs[k].is_ascii_digit() || matches!(cs[k], '„' | '»' | '«' | '"' | '‚' | '('));
        if next_ok && (cs[i] != '.' || !abbrev(&cs[..i])) {
            out.push(cs[from..j].iter().collect::<String>().trim().to_string());
            from = k;
        }
        i = j;
    }
    let rest: String = cs[from..].iter().collect();
    if !rest.trim().is_empty() {
        out.push(rest.trim().to_string());
    }
    out
}

/// Stop words that never identify a figure.
const STOP: &[&str] = &["der", "die", "das", "den", "dem", "des", "ein", "eine", "einer", "eines", "einem", "einen", "und", "aus", "von", "vom", "mit", "bei", "the"];

/// Cast guard: a cast entry whose name and id never occur in the source text is no figure of it (an invented speaker,
/// e.g. a loudspeaker) – its lines go to the narrator, the entry is dropped. Returns the folded ids; ERZ always stays.
/// Name words of ≥ 3 letters count (articles excluded); ≥ 4 letters also match as a word prefix (`Lena` – `Lenas`).
pub fn guard_cast(v: &mut Value, src: &str) -> Vec<String> {
    let erz = crate::prose_score::ERZ;
    let words: Vec<String> = src.to_lowercase().split(|c: char| !c.is_alphanumeric()).filter(|w| w.chars().count() >= 3).map(str::to_string).collect();
    let known = |n: &str| n.to_lowercase().split(|c: char| !c.is_alphanumeric()).filter(|w| w.chars().count() >= 3 && !STOP.contains(w)).any(|w| words.iter().any(|x| *x == w || (w.chars().count() >= 4 && x.starts_with(w))));
    let gone: Vec<String> = cast(v).iter().filter(|c| c.id != erz && !known(&c.name) && !known(&c.id)).map(|c| c.id.clone()).collect();
    if gone.is_empty() {
        return gone;
    }
    if let Some(cs) = v["cast"].as_array_mut() {
        cs.retain(|c| !gone.contains(&s(c, "id")));
        if !cs.iter().any(|c| s(c, "id") == erz) {
            cs.insert(0, json!({"id": erz, "name": "Erzähler", "description": "", "suggested_voice_instruct": ""}));
        }
    }
    for l in v["lines"].as_array_mut().into_iter().flatten().filter(|l| gone.contains(&s(l, "who"))) {
        l["who"] = json!(erz);
        l["over"] = Value::Null;
    }
    gone
}

/// `ID|GAP|TEXT` lines (`+ID` for `over`); `gap` missing = `default_gap`. A line with several sentences becomes one line
/// per sentence (`sentences`; the first keeps gap/over, the rest get `default_gap`). Parsed back to make sure script.txt reads it.
pub fn to_script(v: &Value, default_gap: f64) -> Result<String> {
    let mut out = String::new();
    for l in v["lines"].as_array().into_iter().flatten() {
        let gap = l["gap"].as_f64().unwrap_or(default_gap);
        let over = if l["over"] == true { "+" } else { "" };
        let text = s(l, "text").split_whitespace().collect::<Vec<_>>().join(" ");
        for (k, sent) in sentences(&text).into_iter().enumerate() {
            let (ov, g) = if k == 0 { (over, gap) } else { ("", default_gap) };
            out += &format!("{ov}{}|{}|{sent}\n", s(l, "who"), g as f32);
        }
    }
    let sc = Script::parse(&out)?;
    if let Some(n) = sc.bad().first() {
        bail!("Rollenskript Zeile {n} nicht lesbar: {}", out.lines().nth(n - 1).unwrap_or_default());
    }
    Ok(out)
}

pub type Llm<'a> = &'a mut dyn FnMut(&[Msg], &Value) -> Result<String>;

#[derive(Serialize, Clone, Debug)]
pub struct Outcome {
    pub script: String,
    pub cast: Vec<Cast>,
    pub lines: usize,
    pub retried: bool,
    /// cast ids the guard folded into the narrator (name never in the text)
    pub merged: Vec<String>,
}

/// One LLM call (+1 retry with the error list), validated, formatted.
pub fn convert(llm: Llm, text: &str, default_gap: f64) -> Result<Outcome> {
    let text = plain(text);
    if text.is_empty() {
        bail!("Text ist leer");
    }
    let schema = schema();
    let mut msgs = messages(&text);
    let mut retried = false;
    loop {
        let out = llm(&msgs, &schema)?;
        let (v, errs) = match parse_json(&out) {
            Some(v) => {
                let e = validate(&v);
                (Some(v), e)
            }
            None => (None, vec!["Antwort ist kein gültiges JSON".to_string()]),
        };
        if errs.is_empty() {
            let mut v = v.context("kein JSON")?;
            let merged = guard_cast(&mut v, &text);
            let script = to_script(&v, default_gap)?;
            return Ok(Outcome { lines: script.lines().count(), script, cast: cast(&v), retried, merged });
        }
        if retried {
            bail!("Rollenskript nach 2 Versuchen ungültig:\n- {}", errs.join("\n- "));
        }
        retried = true;
        msgs.push(funkhaus_llm::asst(out));
        msgs.push(user(format!("Die Antwort hat Fehler:\n- {}\nAntworte erneut NUR mit korrigiertem JSON nach Schema.", errs.join("\n- "))));
    }
}

#[derive(Serialize, Clone, Debug)]
pub struct Report {
    pub backend: String,
    pub online: bool,
    pub lines: usize,
    pub retried: bool,
    pub elapsed_s: f64,
    pub speakers_added: Vec<String>,
    /// LLM's suggestions for the new speakers (persona stays unset): `stimmwerk voice design --instruct "…"`.
    pub cast: Vec<Cast>,
    /// speakers the LLM invented (name not in the text) – their lines went to the narrator
    pub cast_merged: Vec<String>,
}

/// Prose -> script.txt of `p` (refuses to overwrite a non-empty script without `replace`), speakers added for the cast.
/// The gate (`r.allowed()`) runs before the LLM is called; the caller handles the online warning/confirmation.
pub fn run(p: &mut crate::Project, r: &crate::llm_cfg::Resolved, text: &str, replace: bool) -> Result<Report> {
    if !p.script.is_empty() && !replace {
        bail!("script.txt ist nicht leer – mit --replace ersetzen (Besetzung bleibt)");
    }
    r.allowed()?;
    let t = std::time::Instant::now();
    let mut llm = |m: &[Msg], s: &Value| funkhaus_llm::complete(&r.backend, None, r.online_ok, m, Some(s));
    let o = convert(&mut llm, text, p.meta.settings.gap)?;
    p.script = Script::parse(&o.script)?;
    let added: Vec<String> = o.cast.iter().filter(|c| p.add_speaker(&c.id, Some(&c.name).filter(|n| **n != c.id).map(|n| n.as_str()))).map(|c| c.id.clone()).collect();
    p.save()?;
    Ok(Report { backend: r.name.clone(), online: !r.is_local(), lines: o.lines, retried: o.retried, elapsed_s: (t.elapsed().as_secs_f64() * 10.0).round() / 10.0, speakers_added: added, cast: o.cast, cast_merged: o.merged })
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIX: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/stimmwerk/prose/");

    fn expected(n: &str) -> Value {
        serde_json::from_str(&std::fs::read_to_string(format!("{FIX}{n}.expected.json")).unwrap()).unwrap()
    }

    #[test]
    fn schema_and_prompt() {
        let s = schema();
        assert_eq!(s["additionalProperties"], false);
        assert_eq!(s["properties"]["cast"]["items"]["additionalProperties"], false);
        assert!(serde_json::to_string(&s).unwrap().matches("additionalProperties").count() == 3);
        // OpenAI strict: every property required, optional ones nullable → the request really goes out strict
        assert!(funkhaus_llm::strict_ok(&s), "{s}");
        let l = &s["properties"]["lines"]["items"];
        assert_eq!(l["required"], json!(["who", "text", "gap", "over"]));
        assert_eq!((l["properties"]["gap"]["type"].clone(), l["properties"]["over"]["type"].clone()), (json!(["number", "null"]), json!(["boolean", "null"])));
        let b = funkhaus_llm::Backend::default();
        assert_eq!(funkhaus_llm::request(&b, "m", &messages("x"), Some(&s)).unwrap().body["response_format"]["json_schema"]["strict"], true);
        assert!(SYSTEM.contains("gap: Pause") && SYSTEM.contains("null, wenn keine besondere Pause") && !SYSTEM.contains("(optional)"));
        assert!(SYSTEM.contains("Genau EIN Satz je Zeile") && SYSTEM.contains("Durchsagen und Lautsprecher sind keine Figuren") && SYSTEM.contains("nur Figuren, die im Text vorkommen"));
        let m = messages("Ein Satz.");
        assert!(m[0].content.contains("NUR mit JSON") && m[1].content.contains("<<<\nEin Satz.\n>>>"));
        assert_eq!(plain("# Titel\n\n## Kap\nText **fett** hier.\n"), "Text fett hier.");
    }

    /// Sentence split: abbreviations, numbers, single letters and tags keep the line; several sentences of one speaker
    /// become several lines (first keeps gap/over). The expected fixtures (one sentence per line) stay unchanged.
    #[test]
    fn sentences_split_and_keep_abbreviations() {
        let one = |t: &str| assert_eq!(sentences(t), vec![t.to_string()], "{t}");
        for t in ["Dr. Kern stand vor der Tafel und zeigte auf die Zahlen.", "Die Wetterdaten stammen z. B. aus der Station am Hafen.", "Der Bericht steht auf S. 12 im zweiten Ordner.", "Bitte nehmen Sie sich ca. 20 Minuten Zeit.", "Um 14 Uhr, in Nr. 4 am Ende des Flurs, begann die Besprechung.", "Das Thermometer zeigt 3,5 Grad, das ist knapp.", "Am 3. Oktober 1964 war es 3.5 Grad kalt.", "Gut, dass du da bist. [laughter]", "[sigh] Ach so.", "Na so was … wer hätte das gedacht.", "Er kam um 8:00 Uhr an."] {
            one(t);
        }
        assert_eq!(sentences("Der Wartesaal ist geschlossen. Seit Mai."), ["Der Wartesaal ist geschlossen.", "Seit Mai."]);
        assert_eq!(sentences("Seit Mai? Woher weißt du das?"), ["Seit Mai?", "Woher weißt du das?"]);
        assert_eq!(sentences("Hm. Gut, dass du da bist"), ["Hm.", "Gut, dass du da bist"]);
        assert_eq!(sentences("Was?! Nein … Das glaube ich nicht."), ["Was?!", "Nein …", "Das glaube ich nicht."]);
        assert_eq!(sentences("Im Flur hing ein Schild: Bitte Tür leise schließen. Danke."), ["Im Flur hing ein Schild: Bitte Tür leise schließen.", "Danke."]);
        assert_eq!(sentences("Er sagte: „Komm.“ Dann ging er. Es war 14 Uhr. 15 Minuten später regnete es."), ["Er sagte: „Komm.“", "Dann ging er.", "Es war 14 Uhr.", "15 Minuten später regnete es."]);
        assert_eq!(sentences("Ärger! Öde."), ["Ärger!", "Öde."]);
        assert!(sentences("   ").is_empty());
        for n in ["p1", "p2", "p3", "p4", "p5", "p6", "p7", "p8"] {
            let v = expected(n);
            assert_eq!(to_script(&v, 0.5).unwrap().lines().count(), v["lines"].as_array().unwrap().len(), "{n}: expected lines are single sentences");
        }
        let v = json!({"cast": [{"id": "A", "name": "a"}], "lines": [{"who": "A", "text": "Los."}, {"who": "A", "text": "Ja. Nein.", "over": true, "gap": 0.2}, {"who": "A", "text": "Hm. [sigh] Gut.", "gap": 0.4}]});
        assert_eq!(to_script(&v, 0.1).unwrap(), "A|0.1|Los.\n+A|0.2|Ja.\nA|0.1|Nein.\nA|0.4|Hm. [sigh] Gut.\n");
    }

    /// Cast guard: an invented speaker (name/id not in the text) folds into ERZ, reported; named figures, role names
    /// (`der Mann`, `Stimme aus dem Nebenzimmer`) and genitives (`Lenas`) pass; ERZ is added when it was missing.
    #[test]
    fn guard_cast_folds_unknown_speakers() {
        let p1 = std::fs::read_to_string(format!("{FIX}p1.txt")).unwrap();
        let mut v = json!({"cast": [{"id": "ERZ", "name": "Erzähler"}, {"id": "RIKE", "name": "Rike"}, {"id": "LENA", "name": "Lena"}, {"id": "LSP", "name": "Ansager", "description": "x"}],
            "lines": [{"who": "ERZ", "text": "Der Bahnsteig war leer."}, {"who": "LSP", "text": "Der Regionalzug fährt ein.", "over": true}, {"who": "LENA", "text": "Sieben Minuten."}]});
        assert_eq!(guard_cast(&mut v, &p1), ["LSP"]);
        assert_eq!(cast(&v).iter().map(|c| c.id.as_str()).collect::<Vec<_>>(), ["ERZ", "RIKE", "LENA"]);
        assert_eq!((v["lines"][1]["who"].as_str(), v["lines"][1]["over"].is_null()), (Some("ERZ"), true));
        assert!(validate(&v).iter().any(|e| e.contains("\"RIKE\" kommt")), "RIKE has no line left – the usual validation still sees that");
        // "Lautsprecher" occurs in the text: such a speaker passes the guard (the prompt forbids it, the guard cannot)
        let mut v = json!({"cast": [{"id": "ERZ", "name": "E"}, {"id": "LSP", "name": "Lautsprecher"}], "lines": [{"who": "LSP", "text": "x"}]});
        assert!(guard_cast(&mut v, &p1).is_empty());
        let p7 = std::fs::read_to_string(format!("{FIX}p7.txt")).unwrap();
        let mut v = expected("p7");
        assert!(guard_cast(&mut v, &p7).is_empty(), "role names pass");
        let mut v = json!({"cast": [{"id": "LENAS", "name": "Lenas Freundin"}, {"id": "BOB", "name": "Bob"}], "lines": [{"who": "LENAS", "text": "a"}, {"who": "BOB", "text": "b"}]});
        assert_eq!(guard_cast(&mut v, "Lenas Zug kam. Die Frau ging."), ["BOB"]);
        assert_eq!(cast(&v)[0].id, "ERZ");
        assert_eq!(v["lines"][1]["who"], "ERZ");
    }

    #[test]
    fn validate_fixtures_and_errors() {
        for n in ["p1", "p2", "p3", "p4"] {
            let v = expected(n);
            assert!(validate(&v).is_empty(), "{n}: {:?}", validate(&v));
            let sc = to_script(&v, 0.5).unwrap();
            assert_eq!(sc.lines().count(), v["lines"].as_array().unwrap().len());
            assert!(sc.lines().all(|l| l.split('|').nth(1) == Some("0.5")));
        }
        let c = cast(&expected("p1"));
        assert_eq!((c[1].id.as_str(), c[1].name.as_str(), c[1].suggested_voice_instruct.as_str()), ("RIKE", "Rike", ""));
        assert!(c[1].description.starts_with("Frau"));
        let v = json!({"cast": [{"id": "ERZ", "name": "E"}, {"id": "B", "name": "b"}, {"id": "ERZ", "name": "x"}, {"id": "NIE", "name": "n"}],
            "lines": [{"who": "XYZ", "text": "a", "gap": 1.5, "over": true}, {"who": "ERZ", "text": "", "gap": 0}, {"who": "ERZ", "text": "x [tanz] y"}, {"who": "B", "text": "a|b"}]});
        let e = validate(&v).join("\n");
        for want in ["XYZ", "gap 1.5", "doppelt", "\"B\" ungültig", "text leer", "[tanz]", "kein |", "\"NIE\" kommt", "over vor"] {
            assert!(e.contains(want), "{want}:\n{e}");
        }
        assert!(validate(&json!({"cast": []})).iter().any(|e| e.contains("lines")));
        assert!(validate(&json!({"cast": [], "lines": []})).iter().any(|e| e.contains("leer")));
        // over -> +ID, gap formatting, whitespace collapsed
        let v = json!({"cast": [{"id": "A", "name": "a"}, {"id": "ERZ", "name": "e"}], "lines": [{"who": "ERZ", "text": "Es  war."}, {"who": "A", "text": "Ja.", "over": true, "gap": 0.2}]});
        assert_eq!(to_script(&v, 0.1).unwrap(), "ERZ|0.1|Es war.\n+A|0.2|Ja.\n");
        // strict replies send explicit nulls: null gap = default_gap, null over = no overlay, nothing flagged
        let v = json!({"cast": [{"id": "ERZ", "name": "e"}, {"id": "A", "name": "a"}], "lines": [{"who": "ERZ", "text": "Eins.", "gap": null, "over": null}, {"who": "A", "text": "Zwei.", "gap": 0.3, "over": null}, {"who": "ERZ", "text": "Drei.", "gap": null, "over": true}]});
        assert!(validate(&v).is_empty(), "{:?}", validate(&v));
        assert_eq!(to_script(&v, 0.5).unwrap(), "ERZ|0.5|Eins.\nA|0.3|Zwei.\n+ERZ|0.5|Drei.\n");
        let v = json!({"cast": [{"id": "ERZ", "name": "e"}], "lines": [{"who": "ERZ", "text": "x", "gap": "0.2", "over": "ja"}]});
        let e = validate(&v).join("\n");
        assert!(e.contains("gap \"0.2\" nicht in 0..1") && e.contains("over \"ja\" ist kein Wahrheitswert"), "{e}");
    }

    #[test]
    fn convert_retries_once_with_error_list() {
        let good = serde_json::to_string(&expected("p2")).unwrap();
        let bad = r#"{"cast": [{"id": "ERZ", "name": "E"}], "lines": [{"who": "WER", "text": "x"}]}"#;
        let mut calls: Vec<Vec<Msg>> = vec![];
        let mut replies = vec![good.clone(), bad.to_string()];
        let mut llm = |m: &[Msg], _: &Value| -> Result<String> {
            calls.push(m.to_vec());
            Ok(replies.pop().unwrap())
        };
        let o = convert(&mut llm, &std::fs::read_to_string(format!("{FIX}p2.txt")).unwrap(), 0.5).unwrap();
        assert!(o.retried && o.lines == 19 && o.cast.len() == 1 && o.merged.is_empty(), "{o:?}");
        assert_eq!(calls.len(), 2);
        assert!(calls[1].len() == 4 && calls[1][3].role == "user" && calls[1][3].content.contains("\"WER\" ist keine cast-id"), "{:?}", calls[1][3]);
        let mut llm = |_: &[Msg], _: &Value| -> Result<String> { Ok("kein json".into()) };
        let e = convert(&mut llm, "Text.", 0.5).unwrap_err().to_string();
        assert!(e.contains("2 Versuchen") && e.contains("kein gültiges JSON"), "{e}");
        let mut llm = |_: &[Msg], _: &Value| -> Result<String> { anyhow::bail!("offline") };
        assert!(convert(&mut llm, "  ", 0.5).unwrap_err().to_string().contains("leer"));
        assert!(convert(&mut llm, "Text.", 0.5).unwrap_err().to_string().contains("offline"));
    }

    #[test]
    fn run_writes_script_and_speakers() {
        let t = tempfile::tempdir().unwrap();
        let mut p = crate::Project::create(&t.path().join("x.studio"), "X", "de").unwrap();
        let m = funkhaus_llm::testsrv::Mock::start(vec![funkhaus_llm::testsrv::fixture("openai/200_success")]);
        let mut app = crate::llm_cfg::AppLlm::builtin();
        app.backends.get_mut("gemma").unwrap().url = format!("{}/v1", m.url());
        let r = crate::llm_cfg::resolve(&app, None).unwrap();
        let k1 = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/stimmwerk/prosa/kapitel1.md")).unwrap();
        let rep = run(&mut p, &r, &k1, false).unwrap();
        assert_eq!((rep.lines, rep.online, rep.retried, rep.speakers_added.len(), rep.cast_merged.len()), (37, false, false, 4, 0), "34 lines of the reply, 3 of them with two sentences");
        assert_eq!(p.meta.speakers.keys().collect::<Vec<_>>(), ["ERZ", "Mara", "Jonas", "Ilse"]);
        assert_eq!(p.meta.speakers["ERZ"].label.as_deref(), Some("Erzähler"));
        assert!(p.meta.speakers["Mara"].label.is_none() && p.meta.speakers["Mara"].persona.is_none());
        let q = crate::Project::load(&p.dir).unwrap();
        assert_eq!(q.script.lines().count(), 37);
        assert!(q.script.lines().any(|l| l.text.contains("3. Oktober 1964")));
        assert!(run(&mut p, &r, "x", false).unwrap_err().to_string().contains("--replace"));
        assert_eq!(m.hits(), 1);
        // a text without the cast's names: the guard folds Mara/Jonas/Ilse into ERZ (reported), no new speakers
        let m2 = funkhaus_llm::testsrv::Mock::start(vec![funkhaus_llm::testsrv::fixture("openai/200_success")]);
        app.backends.get_mut("gemma").unwrap().url = format!("{}/v1", m2.url());
        let r = crate::llm_cfg::resolve(&app, None).unwrap();
        let mut q = crate::Project::create(&t.path().join("y.studio"), "Y", "de").unwrap();
        let rep = run(&mut q, &r, "Egal, der Mock antwortet mit Kapitel 1.", false).unwrap();
        assert_eq!((rep.cast_merged.clone(), rep.speakers_added.clone(), rep.cast.len()), (vec!["Mara".to_string(), "Jonas".into(), "Ilse".into()], vec!["ERZ".to_string()], 1));
        assert!(q.script.lines().all(|l| l.who == "ERZ") && q.script.lines().count() >= 34);
        // online backend without consent: refused before any request
        let mut app2 = app.clone();
        app2.backends.insert("claude".into(), crate::llm_cfg::Entry { url: format!("http://127.0.0.1.nip.io:{}/v1", m.url().rsplit(':').next().unwrap()), ..crate::llm_cfg::Preset::get("anthropic").unwrap().entry() });
        let r2 = crate::llm_cfg::resolve(&app2, Some(&crate::project::ProjectLlm { backend: Some("claude".into()), online_ok: false })).unwrap();
        assert!(run(&mut p, &r2, "x", true).unwrap_err().to_string().contains("nicht freigegeben"));
        assert_eq!(m.hits(), 1);
    }
}
