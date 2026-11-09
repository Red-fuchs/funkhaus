//! Project checks without rendering: script syntax per line (all errors, not just the first), speakers vs project.toml, personas, tags.
use crate::Project;
use crate::script::is_note;
use funkhaus_audio::script::{self as fs, TAGS, parse_pause};
use funkhaus_voice::persona::{Kind, Persona, resolve, valid_name};
use serde::Serialize;
use std::path::{Path, PathBuf};

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Issue {
    /// "error" | "warn"
    pub level: &'static str,
    /// syntax | empty | overlay_first | unknown_speaker | tag | gap | persona | persona_unset | builtins_unavailable | unused_speaker | reserved_id | settings | lexicon | empty_script | seg | cue_* (`crate::cues`)
    pub code: &'static str,
    /// 1-based script.txt line, None = project-level
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<usize>,
    pub msg: String,
}

#[derive(Serialize, Clone, Debug)]
pub struct SpeakerInfo {
    pub id: String,
    pub persona: Option<String>,
    /// builtin | design | clone | implicit (no persona file: omnivoice builtin voice) | unset | error
    pub kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<PathBuf>,
}

#[derive(Serialize, Clone, Debug)]
pub struct Report {
    pub errors: usize,
    pub warnings: usize,
    pub speakers: Vec<SpeakerInfo>,
    pub issues: Vec<Issue>,
}

fn err(code: &'static str, line: Option<usize>, msg: String) -> Issue {
    Issue { level: "error", code, line, msg }
}
fn warn(code: &'static str, line: Option<usize>, msg: String) -> Issue {
    Issue { level: "warn", code, line, msg }
}

/// `[x]` tags that are neither omnivoice tags nor pauses (pause range errors come from `segments`).
pub fn bad_tags(text: &str) -> Vec<String> {
    let mut v = vec![];
    let mut rest = text;
    while let Some(a) = rest.find('[') {
        let Some(b) = rest[a..].find(']') else {
            v.push(rest[a..].to_string());
            break;
        };
        let tag = &rest[a..=a + b];
        if !TAGS.contains(&&tag[1..tag.len() - 1]) && parse_pause(tag).is_none() {
            v.push(tag.to_string());
        }
        rest = &rest[a + b + 1..];
    }
    v
}

/// omnivoice's builtin voice names (`<tts> voices list`, JSON), queried once per process (CLI); None if omnivoice is unavailable.
pub fn builtins() -> Option<&'static [String]> {
    static B: std::sync::OnceLock<Option<Vec<String>>> = std::sync::OnceLock::new();
    B.get_or_init(query_builtins).as_deref()
}

/// `builtins` without the process cache (GUI: "Prüfen" asks again, e.g. after omnivoice was installed/started).
pub fn query_builtins() -> Option<Vec<String>> {
    let cmd = crate::render::tts_cmd();
    let o = std::process::Command::new(cmd).args(["voices", "list"]).stdin(std::process::Stdio::null()).stderr(std::process::Stdio::null()).output().ok().filter(|o| o.status.success())?;
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).ok()?;
    v["voices"].as_array().map(|a| a.iter().filter_map(|x| x["name"].as_str().map(String::from)).collect())
}

/// Resolves a persona across dirs (first file wins, else implicit builtin as funkhaus does).
/// `bi`: omnivoice builtin names; an implicit persona must be one of them (None = unknown, not checked).
pub fn persona(dirs: &[PathBuf], name: &str, bi: Option<&[String]>) -> (&'static str, Option<PathBuf>, Result<(), String>) {
    let kind = |k: Kind| match k {
        Kind::Builtin => "builtin",
        Kind::Design => "design",
        Kind::Clone => "clone",
    };
    match dirs.iter().find(|d| Persona::path(d, name).exists()) {
        Some(d) => match resolve(d, name) {
            Ok(s) => (kind(s.kind), Some(Persona::path(d, name)), Ok(())),
            Err(e) => ("error", Some(Persona::path(d, name)), Err(format!("{e:#}"))),
        },
        None if valid_name(name) => match bi {
            Some(b) if !b.iter().any(|x| x == name) => ("error", None, Err(format!("Stimme „{name}“ gibt es nicht – weder eigene Stimme noch omnivoice-Stimme (vorhanden: {})", b.join(", ")))),
            _ => ("implicit", None, Ok(())),
        },
        None => ("error", None, Err(format!("Stimme „{name}“ unbekannt (Name nur a bis z, 0 bis 9 und Bindestrich; keine Datei in {})", dirs.iter().map(|d| d.display().to_string()).collect::<Vec<_>>().join(", ")))),
    }
}

/// `xx`, `xxx`, optionally `-Region` (de, en, de-AT).
pub fn lang_ok(l: &str) -> bool {
    let (a, b) = l.split_once('-').map_or((l, None), |(a, b)| (a, Some(b)));
    (2..=3).contains(&a.len()) && a.bytes().all(|c| c.is_ascii_lowercase()) && b.is_none_or(|b| (2..=8).contains(&b.len()) && b.bytes().all(|c| c.is_ascii_alphanumeric()))
}

/// Lints the script text against `p.meta` (script read raw from disk, so a broken script.txt still gets per-line reports).
/// `bi`: omnivoice builtin voices (`builtins()`); None -> implicit personas get a warning instead of a check.
pub fn lint(dir: &Path, persona_dirs: &[PathBuf], bi: Option<&[String]>) -> anyhow::Result<Report> {
    let p = Project::load(dir).or_else(|e| {
        // keep going on script errors: load meta alone
        let meta = crate::project::Meta::parse(&std::fs::read_to_string(dir.join(crate::project::FILE)).map_err(|_| e)?)?;
        anyhow::Ok(Project { dir: dir.into(), meta, script: Default::default() })
    })?;
    let src = std::fs::read_to_string(dir.join(crate::project::SCRIPT)).unwrap_or_default();
    let mut is = vec![];
    let mut used = std::collections::HashSet::new();
    let mut prev_main: Option<&str> = None;
    let mut prev_text = String::new();
    for (i, raw) in src.lines().enumerate() {
        let n = Some(i + 1);
        if is_note(raw) {
            continue;
        }
        let f: Vec<&str> = raw.trim().splitn(3, '|').collect();
        if f.len() == 3 && f[2].trim().is_empty() {
            is.push(err("empty", n, "Zeile hat keinen Text".into()));
            continue;
        }
        let w = f[0].trim();
        let over = w.starts_with('+') || w.eq_ignore_ascii_case("b");
        let Some(ctx) = (if over { prev_main.map(|m| format!("{m}\n{raw}")) } else { Some(raw.to_string()) }) else {
            is.push(err("overlay_first", n, format!("Einwurf {w} steht vor der ersten Sprecherzeile")));
            continue;
        };
        let l = match fs::parse(&ctx) {
            Ok(v) => v.into_iter().last().expect("non-empty"),
            Err(e) => {
                let m = e.to_string();
                let m = m.split_once(": ").filter(|(a, _)| a.starts_with("Zeile ")).map_or(m.clone(), |(_, b)| b.to_string());
                is.push(err("syntax", n, m.replace("GAP", "Abstand")));
                continue;
            }
        };
        if over && !(0.0..=1.0).contains(&l.gap) {
            is.push(err("gap", n, format!("Einwurf-Abstand {} liegt außerhalb 0 bis 1 (Anteil der Vorzeile)", l.gap)));
        }
        if !over && prev_main.is_some() && l.gap < 0.0 && -(l.gap as f64) > crate::project::est_secs(&prev_text) {
            is.push(warn("gap", n, format!("Abstand {} s ist länger als die Vorzeile (geschätzt {:.1} s): Zeile startet vor ihr", l.gap, crate::project::est_secs(&prev_text))));
        }
        if !over {
            prev_main = Some(raw);
            prev_text.clone_from(&l.text);
        }
        if !p.meta.speakers.contains_key(&l.who) {
            is.push(err("unknown_speaker", n, format!("Sprecher „{}“ hat noch keine Stimme im Projekt – unter „Stimmen“ zuweisen oder Tippfehler korrigieren", l.who)));
        }
        used.insert(l.who.clone());
        if let Err(e) = fs::segments(&l.text) {
            is.push(err("tag", n, e.to_string()));
        }
        for t in bad_tags(&l.text) {
            is.push(err("tag", n, format!("Unbekannter Tag {t} (erlaubt: {}, [pause …])", TAGS.join(", "))));
        }
    }
    if prev_main.is_none() && is.is_empty() {
        is.push(warn("empty_script", None, "Das Skript enthält noch keine Zeilen".into()));
    }
    // sync projects have no chapters (cue text only; export skips `# SEG`)
    if !dir.join(crate::project::SYNC).is_file() {
        is.extend(funkhaus_audio::chapters::seg_issues(&src).into_iter().map(|(n, s, r)| warn("seg", Some(n), funkhaus_audio::chapters::seg_warning(&s, r))));
    }
    if let Err(e) = p.meta.settings.loudness.parse::<funkhaus_audio::mix::Loudness>() {
        is.push(err("settings", None, format!("Lautheits-Einstellung ungültig: {e}")));
    }
    if !lang_ok(&p.meta.lang) {
        is.push(err("settings", None, format!("Sprache „{}“ ungültig (z. B. de, en, de-AT)", p.meta.lang)));
    }
    if lang_ok(&p.meta.lang) && p.meta.settings.disclosure && p.meta.disclosure_text().is_none() {
        is.push(err("settings", None, format!("Kein KI-Hinweistext für Sprache „{}“: eigenen Text setzen oder den gesprochenen KI-Hinweis ausschalten", p.meta.lang)));
    }
    let mut speakers = vec![];
    let mut implicit = false;
    for (id, s) in &p.meta.speakers {
        let (kind, file) = match &s.persona {
            None => {
                is.push(warn("persona_unset", None, format!("Sprecher {id} hat noch keine Stimme")));
                ("unset", None)
            }
            Some(name) => {
                let (k, f, r) = persona(persona_dirs, name, bi);
                implicit |= k == "implicit";
                if let Err(e) = r {
                    is.push(err("persona", None, format!("Sprecher {id}: {e}")));
                }
                (k, f)
            }
        };
        if matches!(id.as_str(), "O" | "F") {
            is.push(warn("reserved_id", None, format!("Sprecher-ID {id} ist reserviert (O und F sind die Funkhaus-Hosts; B, o und f im Skript werden auf O/F umgedeutet)")));
        }
        if !used.contains(id) {
            is.push(warn("unused_speaker", None, format!("Sprecher {id} kommt im Skript nicht vor")));
        }
        speakers.push(SpeakerInfo { id: id.clone(), persona: s.persona.clone(), kind, file });
    }
    if implicit && bi.is_none() {
        is.push(warn("builtins_unavailable", None, "omnivoice-Stimmliste nicht verfügbar: Standard-Stimmen werden nicht geprüft".into()));
    }
    if let Ok(s) = std::fs::read_to_string(p.lexicon_path()) {
        is.extend(funkhaus_voice::lexicon::parse_rows(&s).1.into_iter().map(|w| warn("lexicon", None, w)));
    }
    is.extend(crate::cues::lint(&p));
    let errors = is.iter().filter(|i| i.level == "error").count();
    Ok(Report { errors, warnings: is.len() - errors, speakers, issues: is })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proj(script: &str) -> (tempfile::TempDir, PathBuf) {
        let t = tempfile::tempdir().unwrap();
        let d = t.path().join("l.studio");
        let mut p = Project::create(&d, "L", "de").unwrap();
        p.add_speaker("ERZ", None);
        p.add_speaker("Mara", None);
        p.meta.speakers["ERZ"].persona = Some("gt".into());
        p.save().unwrap();
        std::fs::write(d.join("script.txt"), script).unwrap();
        (t, d)
    }

    fn bi() -> Option<&'static [String]> {
        static B: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
        Some(B.get_or_init(|| ["fable", "gerstaecker", "gt", "kn"].map(String::from).to_vec()))
    }

    fn codes(r: &Report) -> Vec<(&str, Option<usize>)> {
        r.issues.iter().map(|i| (i.code, i.line)).collect()
    }

    #[test]
    fn clean_fixture() {
        let d = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/stimmwerk/project/leuchtturm.studio"));
        let r = lint(d, &[], bi()).unwrap();
        assert_eq!((r.errors, r.warnings), (0, 0), "{:?}", r.issues);
        assert_eq!(r.speakers.iter().map(|s| s.kind).collect::<Vec<_>>(), ["implicit"; 3]);
    }

    #[test]
    fn finds_unknown_speaker_and_more() {
        let (_t, d) = proj("+Mara|0.5|Mhm.\nERZ|0|Hallo.\nOtto|0.4|Wer bin ich?\nMara|0.3|\nERZ|x|Zahl.\nERZ|0|Ha [bogus] [pause 20s]\n+Mara|0.5|Ja [sigh].\n");
        let r = lint(&d, &[], bi()).unwrap();
        assert_eq!(
            codes(&r),
            [
                ("overlay_first", Some(1)),
                ("unknown_speaker", Some(3)),
                ("empty", Some(4)),
                ("syntax", Some(5)),
                ("tag", Some(6)),
                ("tag", Some(6)),
                ("persona_unset", None)
            ]
        );
        assert_eq!(r.errors, 6);
        assert!(r.issues[1].msg.contains("Otto"));
        assert!(r.issues[3].msg.contains("Abstand"), "{}", r.issues[3].msg);
    }

    #[test]
    fn personas_from_dirs() {
        let (t, d) = proj("ERZ|0|Hallo.\nMara|0.3|Hi.\n");
        let vd = t.path().join("voices");
        std::fs::create_dir_all(&vd).unwrap();
        std::fs::write(vd.join("tief.toml"), "name = \"tief\"\nkind = \"design\"\ninstruct = \"male, low pitch\"\n").unwrap();
        std::fs::write(vd.join("kaputt.toml"), "name = \"kaputt\"\nkind = \"design\"\n").unwrap();
        let mut p = Project::load(&d).unwrap();
        p.meta.speakers["Mara"].persona = Some("tief".into());
        p.save().unwrap();
        let r = lint(&d, std::slice::from_ref(&vd), bi()).unwrap();
        assert_eq!(r.errors, 0, "{:?}", r.issues);
        assert_eq!(r.speakers[1].kind, "design");
        for (name, code) in [("kaputt", "persona"), ("Nicht Da", "persona")] {
            p.meta.speakers["Mara"].persona = Some(name.into());
            p.save().unwrap();
            let r = lint(&d, std::slice::from_ref(&vd), bi()).unwrap();
            assert_eq!(codes(&r), [(code, None)], "{name}");
        }
    }

    #[test]
    fn reserved_ids() {
        let (_t, d) = proj("ERZ|0|Hallo.\nMara|0.3|Hi.\n");
        let mut p = Project::load(&d).unwrap();
        p.add_speaker("O", None);
        p.meta.speakers["O"].persona = Some("gt".into());
        p.meta.speakers["Mara"].persona = Some("gt".into());
        p.save().unwrap();
        std::fs::write(d.join("script.txt"), "ERZ|0|Hallo.\nMara|0.3|Hi.\no|0.2|Ich.\n").unwrap();
        let r = lint(&d, &[], bi()).unwrap();
        assert_eq!(codes(&r), [("reserved_id", None)]);
    }

    #[test]
    fn builtins_gaps_settings() {
        let (_t, d) = proj("ERZ|0|Hallo du.\n+Mara|1.5|Mhm.\nMara|-5|Ja.\nERZ|-0.2|Gut.\n");
        let mut p = Project::load(&d).unwrap();
        p.meta.speakers["Mara"].persona = Some("anna".into());
        p.meta.settings.loudness = "laut".into();
        p.meta.lang = "Deutsch".into();
        p.save().unwrap();
        let r = lint(&d, &[], bi()).unwrap();
        assert_eq!(codes(&r), [("gap", Some(2)), ("gap", Some(3)), ("settings", None), ("settings", None), ("persona", None)]);
        assert_eq!((r.errors, r.warnings), (4, 1));
        assert!(r.issues[4].msg.contains("anna") && r.issues[4].msg.contains("kn"), "{}", r.issues[4].msg);
        p.meta.speakers["Mara"].persona = Some("kn".into());
        p.meta.settings.loudness = "video".into();
        p.meta.lang = "de-AT".into();
        p.save().unwrap();
        std::fs::write(d.join("script.txt"), "ERZ|0|Hallo du.\nMara|0.2|Ja.\n").unwrap();
        assert_eq!(lint(&d, &[], bi()).unwrap().issues, []);
        assert_eq!(codes(&lint(&d, &[], None).unwrap()), [("builtins_unavailable", None)]);
        assert!(lang_ok("de") && lang_ok("yue") && !lang_ok("") && !lang_ok("DE") && !lang_ok("de_AT"));
    }

    /// `# SEG` without speech after it or before the next SEG: warning with the script line, no error.
    #[test]
    fn seg_without_lines_warns() {
        let (_t, d) = proj("# SEG 1 | A\n# SEG 2 | B\nERZ|0|Hallo.\nMara|0.3|Hi.\n# SEG 3 | C\n");
        let mut p = Project::load(&d).unwrap();
        p.meta.speakers["Mara"].persona = Some("kn".into());
        p.save().unwrap();
        let r = lint(&d, &[], bi()).unwrap();
        assert_eq!(codes(&r), [("seg", Some(1)), ("seg", Some(5))]);
        assert_eq!((r.errors, r.warnings), (0, 2));
        assert!(r.issues[0].msg.contains("SEG 1") && r.issues[0].msg.contains("nächsten SEG"), "{}", r.issues[0].msg);
        assert!(r.issues[1].msg.contains("SEG 3") && r.issues[1].msg.contains("danach"), "{}", r.issues[1].msg);
        // sync project (sync.json present): no chapter warnings
        std::fs::write(d.join(crate::project::SYNC), "{}").unwrap();
        assert_eq!(lint(&d, &[], bi()).unwrap().issues, []);
    }

    #[test]
    fn tags() {
        assert!(bad_tags("a [sigh] [pause] [pause 2s] [laughter]").is_empty());
        assert_eq!(bad_tags("a [lachen] b [pause"), ["[lachen]", "[pause"]);
    }
}
