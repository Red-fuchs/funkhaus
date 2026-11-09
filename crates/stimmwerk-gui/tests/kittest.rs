//! Headless UI tests + screenshots (qa/loop/stimmwerk/shots/, gitignored) on a copy of fixtures/stimmwerk/project/leuchtturm.studio.
//! No window, no real TTS/acestep/keychain/LLM: STIMMWERK_TTS_CMD is a fake that only answers `voices list`,
//! STIMMWERK_ACESTEP_CMD copies a tone WAV, FUNKHAUS_SECURITY stores keys in a temp dir, LLMs are `testsrv::Mock`s on
//! 127.0.0.1; homes point into the target dir.
use eframe::egui::{self, ThemePreference};
use egui_kittest::kittest::{NodeT, Queryable, by};
use funkhaus_llm::testsrv::{Mock, err, fixture, ok};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use stimmwerk_core::render::{Quality, State};
use stimmwerk_core::{cues, export, llm_cfg, render, sync};
use stimmwerk_gui::jobs::{self, JobKind, Out};
use stimmwerk_gui::shot::{harness, save_png, settle};
use stimmwerk_gui::tabs::Tab;
use stimmwerk_gui::{App, jobs::Msg};

const FIX: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/stimmwerk/project/leuchtturm.studio");
const SHOTS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../qa/loop/stimmwerk/shots");

type H = egui_kittest::Harness<'static, App>;

fn env() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let t = Path::new(env!("CARGO_TARGET_TMPDIR")).join("stimmwerk-gui");
        std::fs::create_dir_all(&t).unwrap();
        let fake = t.join("fake-tts.sh");
        std::fs::write(&fake, "#!/bin/sh\nif [ \"$1\" = voices ]; then echo '{\"voices\":[{\"name\":\"gt\"},{\"name\":\"kn\"},{\"name\":\"fable\"}]}'; exit 0; fi\nexit 1\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        // fake acestep: logs its arguments, copies a 12 s tone to `-o`
        let src = t.join("acestep-tone.wav");
        funkhaus_audio::mix::write(&src, &vec![0.2; 24_000 * 12], 24_000).unwrap();
        let ace = t.join("fake-acestep.sh");
        std::fs::write(&ace, format!("#!/bin/sh\n[ \"$1\" = --version ] && {{ echo 'acestep 0.0-fake'; exit 0; }}\necho \"$@\" >> '{}'\nout=''\nwhile [ $# -gt 0 ]; do if [ \"$1\" = -o ]; then out=\"$2\"; fi; shift; done\ncp '{}' \"$out\"\n", ace_log().display(), src.display())).unwrap();
        std::fs::set_permissions(&ace, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::create_dir_all(kc_dir()).unwrap();
        let kc = funkhaus_llm::keychain::fake(&kc_dir());
        // SAFETY: set once, before any test reads these (all tests call env() first).
        unsafe {
            std::env::set_var("STIMMWERK_HOME", t.join("home"));
            std::env::set_var("FUNKHAUS_HOME", t.join("fhome"));
            std::env::set_var(render::TTS_ENV, &fake);
            std::env::set_var(stimmwerk_core::music::ACESTEP_ENV, &ace);
            std::env::set_var(funkhaus_llm::keychain::ENV, &kc);
            // project delete goes into this folder, never the real Trash
            std::env::set_var(stimmwerk_core::project::TRASH_DIR_ENV, t.join("trash"));
        }
    });
}

fn tmp() -> PathBuf {
    Path::new(env!("CARGO_TARGET_TMPDIR")).join("stimmwerk-gui")
}
fn ace_log() -> PathBuf {
    tmp().join("acestep-args.log")
}
/// Fake keychain store: one file `stimmwerk-llm--<account>` per key.
fn kc_dir() -> PathBuf {
    tmp().join("keychain")
}
fn disabled(n: &egui_kittest::Node<'_>) -> bool {
    n.accesskit_node().is_disabled()
}
/// OpenAI chat body with `content`.
fn chat(content: &str) -> String {
    serde_json::json!({"choices": [{"index": 0, "message": {"role": "assistant", "content": content}, "finish_reason": "stop"}]}).to_string()
}

/// Fixture copy (+ optional extra script line).
fn project(extra: &str) -> (tempfile::TempDir, PathBuf) {
    env();
    let t = tempfile::tempdir().unwrap();
    let d = t.path().join("leuchtturm.studio");
    std::fs::create_dir_all(&d).unwrap();
    for f in ["project.toml", "script.txt", "lexicon.tsv"] {
        std::fs::copy(Path::new(FIX).join(f), d.join(f)).unwrap();
    }
    if !extra.is_empty() {
        let s = std::fs::read_to_string(d.join("script.txt")).unwrap();
        std::fs::write(d.join("script.txt"), format!("{s}{extra}\n")).unwrap();
    }
    (t, d)
}

fn open(dir: &Path, tab: Tab, pref: ThemePreference) -> H {
    let mut hs = harness(App::new(None), egui::vec2(1280.0, 820.0), 1.0, pref);
    let ctx = hs.ctx.clone();
    hs.state_mut().open(&ctx, dir);
    hs.state_mut().tab = tab;
    settle(&mut hs, 10.0);
    hs
}

fn shoot(hs: &mut H, name: &str) {
    let p = Path::new(SHOTS).join(format!("{name}.png"));
    let (w, h) = save_png(hs, &p).unwrap();
    assert_eq!((w, h), (1280, 820));
}

#[test]
fn shot_start() {
    env();
    let mut hs = harness(App::new(None), egui::vec2(1280.0, 820.0), 1.0, ThemePreference::Dark);
    settle(&mut hs, 2.0);
    hs.get_by_role_and_label(egui::accesskit::Role::Button, "Anlegen");
    shoot(&mut hs, "start-dunkel");
}

#[test]
fn shot_tabs() {
    let (_t, d) = project("");
    for (tab, name) in [(Tab::Skript, "skript"), (Tab::Stimmen, "stimmen"), (Tab::Erzeugen, "stimme-erzeugen"), (Tab::Musik, "musik"), (Tab::Export, "export"), (Tab::Einstellungen, "einstellungen")] {
        let mut hs = open(&d, tab, ThemePreference::Light);
        assert_eq!(hs.state().lint.as_ref().map(|l| l.errors), Some(0), "{name}");
        match tab {
            Tab::Musik => drop(hs.get_by_label("Musik erzeugen (acestep)")),
            Tab::Stimmen => {
                // readable names for omnivoice's builtins, id in the tooltip
                for l in ["Oskar (tief, männlich)", "Standard (mittlere Lage)", "Fable (weiblich)"] {
                    hs.get(by().role(egui::accesskit::Role::ComboBox).value(l));
                }
            }
            Tab::Erzeugen => drop(hs.get_by_role_and_label(egui::accesskit::Role::Button, "Entwerfen")),
            _ => {}
        }
        shoot(&mut hs, &format!("{name}-hell"));
    }
    let mut hs = open(&d, Tab::Skript, ThemePreference::Dark);
    shoot(&mut hs, "skript-dunkel");
}

#[test]
fn shot_lint_errors_block_render() {
    let (_t, d) = project("Nina|0|Wer bin ich?");
    let mut hs = open(&d, Tab::Skript, ThemePreference::Light);
    let l = hs.state().lint.clone().expect("lint");
    assert!(l.errors >= 1 && l.issues.iter().any(|i| i.code == "unknown_speaker"));
    assert!(hs.get_by_label("Rendern").accesskit_node().is_disabled());
    hs.get_by_label_contains("hat noch keine Stimme im Projekt");
    assert!(hs.query_all_by_label_contains("project.toml").next().is_none(), "no file jargon");
    hs.get_by_label("✖ Fehler");
    hs.get_by_label("Rendern gesperrt: 1 Fehler im Skript (siehe Hinweise)");
    shoot(&mut hs, "skript-lint-fehler");
    hs.state_mut().tab = Tab::Export;
    hs.run_steps(3);
    assert!(hs.get_by_role_and_label(egui::accesskit::Role::Button, "Exportieren").accesskit_node().is_disabled());
    // Stimmen: the unknown speaker is listed; picking a voice adds it to the project and the error goes away
    hs.state_mut().tab = Tab::Stimmen;
    hs.run_steps(3);
    hs.get_by_label("Nina");
    hs.get(by().role(egui::accesskit::Role::ComboBox).value("– Stimme wählen –")).click();
    hs.run_steps(3);
    shoot(&mut hs, "stimmen-fehlender-sprecher");
    hs.get_all_by_label("Oskar (tief, männlich)").last().unwrap().click();
    hs.run_steps(2);
    settle(&mut hs, 10.0);
    let p = stimmwerk_core::Project::load(&d).unwrap();
    assert_eq!(p.meta.speakers.get("Nina").and_then(|s| s.persona.as_deref()), Some("gt"));
    assert_eq!(hs.state().lint.as_ref().map(|l| l.errors), Some(0));
    assert!(hs.state().msg.as_ref().is_some_and(|(e, m)| !e && m.contains("Nina spricht jetzt mit Oskar")), "{:?}", hs.state().msg);
}

/// Job that only ends when cancelled.
fn idle_job(ctx: &egui::Context, kind: JobKind) -> jobs::Job {
    jobs::spawn(ctx, kind, |c, _| {
        while !c.load(Ordering::Relaxed) {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        anyhow::bail!("abgebrochen")
    })
}

#[test]
fn shot_rendering_and_escape_cancels() {
    let (_t, d) = project("");
    let mut hs = open(&d, Tab::Skript, ThemePreference::Light);
    let ctx = hs.ctx.clone();
    {
        let a = hs.state_mut();
        let mut j = idle_job(&ctx, JobKind::Render);
        (j.total, j.done, j.cur, j.stage) = (7, 3, Some(4), Some("render".into()));
        a.jobs.push(j);
        a.states = vec![Some(State::Cached), Some(State::Done), Some(State::Done), Some(State::Rendering), Some(State::Queued), Some(State::Queued), Some(State::Queued)];
    }
    hs.run_steps(4);
    hs.get_by_label("Zeile 4 läuft …");
    assert!(hs.get_by_label("Rendern").accesskit_node().is_disabled());
    shoot(&mut hs, "skript-rendert");
    let t0 = std::time::Instant::now();
    hs.key_press(egui::Key::Escape);
    settle(&mut hs, 2.0);
    assert!(hs.state().jobs.is_empty(), "job still running");
    assert!(t0.elapsed().as_secs_f64() < 2.0);
    let a = hs.state();
    assert!(a.msg.as_ref().is_some_and(|(e, m)| !e && m.contains("abgebrochen")), "{:?}", a.msg);
    // cancelled: unfinished lines back to open, finished ones keep their state
    assert_eq!((a.states[1], a.states[3], a.states[5]), (Some(State::Done), None, None));
}

#[test]
fn tab_switch_play_undo_states() {
    let (_t, d) = project("");
    let mut hs = open(&d, Tab::Skript, ThemePreference::Light);
    hs.get_by_label("Stimmen").click();
    hs.run_steps(2);
    assert_eq!(hs.state().tab, Tab::Stimmen);
    hs.get_by_label("Skript").click();
    hs.run_steps(2);
    // undo only with an earlier take
    assert!(hs.get_all_by_label("⟲ Zurück").all(|n| n.accesskit_node().is_disabled()));
    hs.state_mut().takes = vec![0, 2, 0, 0, 0, 0, 0];
    hs.run_steps(2);
    let undo: Vec<bool> = hs.get_all_by_label("⟲ Zurück").map(|n| n.accesskit_node().is_disabled()).collect();
    assert_eq!(undo, [true, false, true, true, true, true, true]);
    // row buttons carry text (screen reader + no guessing at glyphs) and win over the clickable row
    assert_eq!(hs.get_all_by_role_and_label(egui::accesskit::Role::Button, "▶ Anhören").count(), 7);
    hs.get_all_by_role_and_label(egui::accesskit::Role::Button, "▶ Anhören").nth(1).unwrap().click();
    hs.run_steps(2);
    assert!(hs.state().jobs.iter().any(|j| j.kind == JobKind::PlayLine(2)), "click reached the button, not the row");
    hs.state_mut().cancel_all();
    settle(&mut hs, 5.0);
    assert_eq!(hs.get_all_by_role_and_label(egui::accesskit::Role::Button, "Neu würfeln").count(), 7);
    hs.get_by_label("↪ Einwurf");
    for (s, t) in [(None, "○ offen"), (Some(State::Rendering), "⟳ rendert"), (Some(State::Cached), "✔ aus Cache"), (Some(State::Failed), "✖ Fehler")] {
        assert_eq!(stimmwerk_gui::tabs::skript::state_text(s).0, t);
    }
}

#[test]
fn setup_error_is_banner_not_line_error() {
    let (_t, d) = project("");
    let mut hs = open(&d, Tab::Skript, ThemePreference::Light);
    let ctx = hs.ctx.clone();
    let j = jobs::spawn(&ctx, JobKind::Render, |_, prog| {
        prog(render::Event::Line { i: 1, n: 7, state: State::Failed, tts_secs: None });
        anyhow::bail!("omnivoice nicht gefunden")
    });
    hs.state_mut().jobs.push(j);
    settle(&mut hs, 2.0);
    let a = hs.state();
    assert_eq!(a.banner.as_deref(), Some("omnivoice nicht gefunden"));
    assert_eq!(a.states[0], None);
    hs.get_by_label_contains("Einrichtung: omnivoice");
}

fn report(in_spec: bool, dir: &Path) -> export::Report {
    export::Report {
        kind: export::Kind::Mp3,
        preset: export::Loudness::Hoerbuch,
        file: dir.join("out/leuchtturm.mp3"),
        manifest: None,
        parts: 0,
        duration: 16.4,
        target: export::Target { lufs: -19.0, tp: -3.0 },
        measured: funkhaus_audio::mix::Measure { lufs: if in_spec { -19.0 } else { -21.3 }, tp: -3.8 },
        in_spec,
        gain_db: 4.2,
        limiter_gr_db: 0.0,
        ai: export::Ai { disclosed: true, text: "Diese Aufnahme wurde mit KI-Stimmen erzeugt.".into(), id3: true },
        chapters: 2,
        tts_calls: 0,
        rendered: vec![],
        cached: 7,
        elapsed: 1.3,
        sync: None,
        cues: vec![],
        acestep_calls: 0,
        warnings: vec![],
    }
}

#[test]
fn shot_export_results() {
    let (_t, d) = project("");
    for ok in [true, false] {
        let mut hs = open(&d, Tab::Export, ThemePreference::Light);
        let ctx = hs.ctx.clone();
        let r = report(ok, &d);
        hs.state_mut().jobs.push(jobs::spawn(&ctx, JobKind::Export, move |_, _| Ok(Out::Export(Box::new(r)))));
        settle(&mut hs, 2.0);
        assert!(hs.state().ex.res.is_some());
        let (title, detail) = if ok { ("✔ Export fertig (MP3): leuchtturm.mp3", "Lautheit passt für Hörbuch (−19,0 LUFS · Spitze −3,8 dBTP)") } else { ("⚠ Export geschrieben, aber Lautheit verfehlt (MP3): leuchtturm.mp3", "Ziel −19,0 LUFS ±0,5, Spitze ≤ −3,0 dBTP") };
        hs.get_by_label(title);
        hs.get_by_label_contains(detail);
        assert!(hs.query_all_by_label_contains("Spec").next().is_none(), "no Spec jargon");
        assert_eq!(hs.state().ex.kind, export::Kind::Mp3, "format choice follows the result");
        hs.get_by_label("Im Finder zeigen");
        hs.get_by_label("KI-Kennzeichnung: gesprochen und getaggt");
        shoot(&mut hs, if ok { "export-fertig" } else { "export-nicht-in-spec" });
    }
    // cue warnings of the export: in the result card and in the message bar
    let mut hs = open(&d, Tab::Export, ThemePreference::Light);
    let ctx = hs.ctx.clone();
    let mut r = report(true, &d);
    r.warnings = vec!["Cue 2: reicht über das Ende der Sprache hinaus – am Mix-Ende (16.4 s) abgeschnitten".into()];
    hs.state_mut().jobs.push(jobs::spawn(&ctx, JobKind::Export, move |_, _| Ok(Out::Export(Box::new(r)))));
    settle(&mut hs, 2.0);
    assert_eq!(hs.get_all_by_label_contains("⚠ Musik: Cue 2").count(), 2, "card + message bar");
    shoot(&mut hs, "r15-export-cue-warnung");
}

/// LlmDlg: unedited fields of an existing entry (gpu, max_tokens, temperature, key ref) survive an edit; Debug never
/// shows the typed key.
#[test]
fn llm_dlg_keeps_fields_and_redacts_key() {
    let mut e = llm_cfg::Preset::get("gemma").unwrap().entry();
    (e.gpu, e.max_tokens, e.temperature, e.key) = (Some(true), 4096, 0.2, Some(funkhaus_llm::KeyRef::Env("X_KEY".into())));
    let mut d = stimmwerk_gui::app::LlmDlg::edit("gemma", &e);
    (d.model, d.key) = ("neu".into(), "sk-geheim".into());
    let n = d.entry(Some(&e));
    assert_eq!((n.gpu, n.max_tokens, n.temperature, n.key.clone(), n.model.as_str(), n.timeout_s), (Some(true), 4096, 0.2, e.key.clone(), "neu", 900));
    let s = format!("{d:?}");
    assert!(s.contains("***") && !s.contains("geheim"), "{s}");
    assert!(d.entry(None).gpu.is_none() && !format!("{:?}", stimmwerk_gui::app::LlmDlg::new()).contains("***"));
}

/// A settings.toml that does not parse: builtins shown read-only, `.broken` copy, no save path writes anything.
#[test]
fn settings_broken_is_read_only() {
    let (_t, d) = project("");
    let t = tempfile::tempdir().unwrap();
    let f = t.path().join("settings.toml");
    std::fs::write(&f, "[llm]\nfoo = 1\n").unwrap();
    let mut hs = harness(App::with_settings(None, llm_cfg::load_from(&f), &f), egui::vec2(1280.0, 820.0), 1.0, ThemePreference::Light);
    let ctx = hs.ctx.clone();
    hs.state_mut().open(&ctx, &d);
    hs.state_mut().tab = Tab::Einstellungen;
    settle(&mut hs, 10.0);
    assert!(hs.state().llm.broken.is_some(), "{:?}", hs.state().msg);
    assert_eq!(std::fs::read_to_string(t.path().join("settings.toml.broken")).unwrap(), "[llm]\nfoo = 1\n");
    assert_eq!(hs.state().settings.llm, llm_cfg::AppLlm::builtin());
    hs.get_by_label_contains("settings.toml ist fehlerhaft");
    hs.get_by_label_contains("settings.toml.broken");
    assert!(disabled(&hs.get_by_label("+ Anbieter hinzufügen")));
    assert!(hs.get_all_by_label("✏ Bearbeiten").all(|n| disabled(&n)));
    hs.get_by_label("⟳ Neu laden");
    shoot(&mut hs, "r15-einstellungen-kaputt");
    for (k, go) in [("default", true), ("remove", true), ("key", true)] {
        hs.state_mut().msg = None;
        match k {
            "default" => hs.state_mut().llm_set_default("vmlx"),
            "remove" => hs.state_mut().llm_remove("ollama", false),
            _ => hs.state_mut().key_clear("vmlx"),
        }
        assert!(go && hs.state().msg.as_ref().is_some_and(|(e, m)| *e && m.contains("nicht überschrieben")), "{k}: {:?}", hs.state().msg);
    }
    assert!(!hs.state_mut().save_settings());
    assert_eq!(std::fs::read_to_string(&f).unwrap(), "[llm]\nfoo = 1\n", "original untouched");
}

/// Prose job: "✖ Abbrechen" ends the job at once although the mock answers only after 3 s; nothing is applied.
#[test]
fn prose_cancel_abandons_request() {
    let (_t, d) = project("");
    let m = Mock::start(vec![ok(&chat("{}")).delay(std::time::Duration::from_secs(3))]);
    let mut hs = open(&d, Tab::Skript, ThemePreference::Light);
    hs.state_mut().settings.llm.backends.get_mut("gemma").unwrap().url = format!("{}/v1", m.url());
    hs.state_mut().prose.text = "Ein Satz.".into();
    let ctx = hs.ctx.clone();
    hs.state_mut().prose_run(&ctx);
    hs.run_steps(3);
    assert!(hs.state().running(&JobKind::Prose) && hs.state().jobs.iter().any(|j| j.kind.cancellable()));
    std::thread::sleep(std::time::Duration::from_millis(300));
    hs.run_steps(2);
    let t0 = std::time::Instant::now();
    hs.get_by_label("✖ Abbrechen").click();
    settle(&mut hs, 2.0);
    assert!(hs.state().jobs.is_empty(), "job still running after cancel");
    assert!(t0.elapsed() < std::time::Duration::from_millis(1500), "{:?}", t0.elapsed());
    assert_eq!(hs.state().prose.err.as_deref(), Some("Abgebrochen – nichts geändert."));
    assert!(hs.state().prose.res.is_none());
    assert_eq!(m.hits(), 1, "the request had been sent; its reply is dropped");
    assert!(JobKind::Shorten(1).cancellable() && JobKind::LlmTest("x".into()).cancellable() && !JobKind::KeySave("x".into()).cancellable());
}

/// Start screen: click selects, double click opens; "Umbenennen…" (row button / context menu) renames folder + title
/// via a modal with live check; "Löschen…" needs two confirmations, the second a last click, then the folder is
/// in the (fake) Trash. Abbrechen in the second deletes nothing.
#[test]
fn start_select_open_rename_and_trash() {
    let (t, d) = project("");
    let mut hs = harness(App::new(None), egui::vec2(1280.0, 820.0), 1.0, ThemePreference::Light);
    hs.state_mut().cfg.recent = vec![d.clone()];
    hs.state_mut().known = None;
    settle(&mut hs, 2.0);
    // single click: selected, not open; row buttons appear
    assert_eq!(hs.get_all_by_label("Öffnen").count(), 1, "only the path field's Öffnen");
    hs.get_by_label("Der Leuchtturm").click();
    hs.run_steps(2);
    assert_eq!(hs.state().start.sel, Some(d.clone()));
    assert!(hs.state().proj.is_none());
    assert_eq!(hs.get_all_by_label("Öffnen").count(), 2);
    shoot(&mut hs, "r15-start-auswahl");
    // row button opens; back to the start screen
    hs.get_all_by_role_and_label(egui::accesskit::Role::Button, "Öffnen").last().unwrap().click();
    settle(&mut hs, 10.0);
    assert_eq!(hs.state().proj.as_ref().map(|p| p.dir.clone()), Some(d.clone()));
    hs.state_mut().close();
    settle(&mut hs, 5.0);
    // double click opens too (two clicks within egui's double-click delay: consecutive frames)
    hs.get_by_label("Der Leuchtturm").click();
    hs.run_steps(1);
    hs.get_by_label("Der Leuchtturm").click();
    settle(&mut hs, 10.0);
    assert!(hs.state().proj.is_some(), "double click opens: {:?}", hs.state().msg);
    hs.state_mut().close();
    settle(&mut hs, 5.0);
    // context menu → rename modal: bad name shows why and keeps Speichern disabled
    hs.get_by_label("Der Leuchtturm").click_secondary();
    hs.run_steps(2);
    assert_eq!(hs.get_all_by_label("Umbenennen…").count(), 2, "row button + context menu item");
    hs.get_all_by_label("Umbenennen…").last().unwrap().click();
    hs.run_steps(2);
    assert_eq!(hs.state().start.rename.as_ref().map(|(p, n)| (p.clone(), n.clone())), Some((d.clone(), "leuchtturm".to_string())));
    hs.get_by_label("Das ist schon der Name.");
    assert!(disabled(&hs.get_by_label("Speichern")));
    hs.state_mut().start.rename.as_mut().unwrap().1 = "a/b".into();
    hs.run_steps(2);
    hs.get_by_label_contains("Schrägstrich");
    assert!(disabled(&hs.get_by_label("Speichern")));
    hs.state_mut().start.rename.as_mut().unwrap().1 = "Leuchtfeuer".into();
    hs.run_steps(2);
    hs.get_by_label("Wird zu „Leuchtfeuer.studio“.");
    shoot(&mut hs, "r15-start-umbenennen");
    hs.get_by_label("Speichern").click();
    settle(&mut hs, 5.0);
    let nd = t.path().join("Leuchtfeuer.studio");
    assert!(nd.is_dir() && !d.exists(), "{:?}", hs.state().msg);
    assert_eq!(stimmwerk_core::Project::load(&nd).unwrap().meta.title, "Leuchtfeuer");
    assert_eq!(hs.state().cfg.recent, vec![nd.clone()]);
    assert!(hs.state().start.rename.is_none() && hs.state().start.sel == Some(nd.clone()));
    hs.get_by_label("Leuchtfeuer");
    // delete: two dialogs; Abbrechen in the second keeps the project
    hs.get_by_label("Löschen…").click();
    hs.run_steps(3);
    hs.get_by_label("Projekt „Leuchtfeuer“ löschen?");
    hs.get_by_label_contains("Takes, Musik und Exporten in den Papierkorb");
    hs.get_by_label_contains("Papierkorb danach leeren");
    shoot(&mut hs, "r15-projekt-loeschen-1");
    hs.get_by_label("Weiter …").click();
    hs.run_steps(3);
    hs.get_by_label("Wirklich in den Papierkorb?");
    hs.get_by_label_contains("„Leuchtfeuer“ kommt jetzt in den Papierkorb");
    hs.get_by_label("Abbrechen").click();
    hs.run_steps(2);
    assert!(hs.state().start.rm.is_none() && nd.is_dir());
    hs.get_by_label("Löschen…").click();
    hs.run_steps(3);
    hs.get_by_label("Weiter …").click();
    hs.run_steps(3);
    shoot(&mut hs, "r15-projekt-loeschen-2");
    hs.get_by_label("In den Papierkorb").click();
    settle(&mut hs, 5.0);
    assert!(!nd.exists(), "{:?}", hs.state().msg);
    assert!(tmp().join("trash/Leuchtfeuer.studio/project.toml").is_file(), "in the fake Trash, not deleted");
    assert!(hs.state().cfg.recent.is_empty() && hs.state().start.rm.is_none() && hs.state().start.sel.is_none());
    assert!(hs.state().msg.as_ref().is_some_and(|(e, m)| !e && m.contains("In den Papierkorb verschoben")), "{:?}", hs.state().msg);
    assert!(hs.query_by_label("Leuchtfeuer").is_none());
}

/// Inside a project: "Projekt …" renames (project closed, reopened at the new place) and trashes the open project
/// (back on the start screen); both disabled while a job runs.
#[test]
fn project_menu_rename_and_trash_open_project() {
    let (t, d) = project("");
    let mut hs = open(&d, Tab::Skript, ThemePreference::Dark);
    hs.get_by_label("Projekt …").click();
    hs.run_steps(2);
    hs.get_by_label("Umbenennen…");
    shoot(&mut hs, "r15-projekt-menu");
    hs.get_by_label("Umbenennen…").click();
    hs.run_steps(2);
    hs.state_mut().start.rename.as_mut().unwrap().1 = "Fackel".into();
    hs.run_steps(2);
    hs.get_by_label("Speichern").click();
    settle(&mut hs, 10.0);
    let nd = t.path().join("Fackel.studio");
    assert_eq!(hs.state().proj.as_ref().map(|p| p.dir.clone()), Some(nd.clone()), "{:?}", hs.state().msg);
    assert!(!d.exists() && hs.state().msg.as_ref().is_some_and(|(_, m)| m.contains("Fackel")));
    hs.get_by_label("Fackel");
    // busy: items disabled
    let ctx = hs.ctx.clone();
    hs.state_mut().jobs.push(idle_job(&ctx, JobKind::Render));
    hs.run_steps(2);
    hs.get_by_label("Projekt …").click();
    hs.run_steps(2);
    assert!(disabled(&hs.get_by_label("Umbenennen…")) && disabled(&hs.get_by_label("Löschen…")));
    hs.get_by_label("Projekt …").click(); // closes the menu again
    hs.state_mut().jobs.iter().for_each(|j| j.cancel());
    settle(&mut hs, 5.0);
    assert!(hs.query_by_label("Löschen…").is_none(), "menu closed");
    hs.get_by_label("Projekt …").click();
    hs.run_steps(2);
    hs.get_by_label("Löschen…").click();
    hs.run_steps(3);
    hs.get_by_label("Weiter …").click();
    hs.run_steps(3);
    hs.get_by_label("In den Papierkorb").click();
    settle(&mut hs, 5.0);
    assert!(hs.state().proj.is_none() && !nd.exists(), "{:?}", hs.state().msg);
    assert!(tmp().join("trash").join("Fackel.studio").is_dir());
    hs.get_by_label("Anlegen");
}

#[test]
fn quality_toggle_and_msg_channel() {
    env();
    let mut a = App::new(None);
    assert_eq!(a.quality(), Quality::Final);
    a.draft = true;
    assert_eq!(a.quality(), Quality::Draft);
    let ctx = egui::Context::default();
    let j = jobs::spawn(&ctx, JobKind::Lint, |_, _| anyhow::bail!("x"));
    assert!(matches!(j.rx.recv().unwrap(), Msg::Done(Err(_))));
}

/// App on the fixture with its own (temp) persona folder.
fn open_with_voices(dir: &Path, vdir: &Path, tab: Tab) -> H {
    let mut app = App::new(None);
    app.dirs = vec![vdir.to_path_buf()];
    let mut hs = harness(app, egui::vec2(1280.0, 820.0), 1.0, ThemePreference::Light);
    let ctx = hs.ctx.clone();
    hs.state_mut().open(&ctx, dir);
    hs.state_mut().tab = tab;
    settle(&mut hs, 10.0);
    hs
}

fn tone(p: &Path, secs: f64) {
    funkhaus_audio::mix::write(p, &vec![0.2; (secs * 24_000.0) as usize], 24_000).unwrap();
}

#[test]
fn erzeugen_design_keep() {
    let (t, d) = project("");
    let vdir = t.path().join("voices");
    let mut hs = open_with_voices(&d, &vdir, Tab::Erzeugen);
    assert!(hs.get_by_role_and_label(egui::accesskit::Role::Button, "Entwerfen").accesskit_node().is_disabled());
    shoot(&mut hs, "stimme-erzeugen-hell");
    // preset fills the instruct
    hs.state_mut().generate.preset = Some(0);
    hs.state_mut().generate.instruct = stimmwerk_core::voice::PRESETS[0].1.into();
    hs.run_steps(2);
    assert!(!hs.get_by_role_and_label(egui::accesskit::Role::Button, "Entwerfen").accesskit_node().is_disabled());
    // a real batch on disk (fake TTS), delivered through the job channel like a GPU run
    let mut say = |w: &Path, _: &str, _: &funkhaus_voice::persona::VoiceSpec, s: u64| funkhaus_audio::mix::write(w, &vec![0.1 + s as f32 * 0.05; 24_000 * 3], 24_000);
    let dreq = stimmwerk_core::voice::Design { instruct: "male, middle-aged, low pitch".into(), n: 3, seeds: None, text: "Hallo, so klinge ich.".into(), lang: "de".into(), quality: Quality::Final };
    let b = stimmwerk_core::voice::design_with(&vdir, &dreq, &std::sync::atomic::AtomicBool::new(false), &|_| {}, &mut say).unwrap();
    let ctx = hs.ctx.clone();
    hs.state_mut().jobs.push(jobs::spawn(&ctx, JobKind::Design, move |_, _| Ok(Out::Design(Box::new(b)))));
    settle(&mut hs, 2.0);
    assert_eq!(hs.get_all_by_label("▶ Anhören").count(), 3);
    assert!(hs.get_all_by_label("Behalten").all(|n| n.accesskit_node().is_disabled()), "no name yet");
    // a name that would shadow an omnivoice builtin is refused
    hs.state_mut().generate.names[0] = "gt".into();
    hs.run_steps(2);
    hs.get_all_by_label("Behalten").next().unwrap().click();
    hs.run_steps(2);
    assert!(hs.state().msg.as_ref().is_some_and(|(e, m)| *e && m.contains("omnivoice-Standard-Stimme")), "{:?}", hs.state().msg);
    assert!(!vdir.join("gt.toml").exists());
    hs.state_mut().generate.names[0].clear();
    hs.state_mut().generate.names[1] = "erzaehler".into();
    hs.run_steps(2);
    shoot(&mut hs, "stimme-erzeugen-kandidaten");
    let keep: Vec<bool> = hs.get_all_by_label("Behalten").map(|n| n.accesskit_node().is_disabled()).collect();
    assert_eq!(keep, [true, false, true]);
    hs.get_all_by_label("Behalten").nth(1).unwrap().click();
    settle(&mut hs, 5.0);
    let p = funkhaus_voice::persona::Persona::load(&vdir, "erzaehler").unwrap();
    assert_eq!((p.seed, p.instruct.as_deref()), (Some(2), Some("male, middle-aged, low pitch")));
    hs.get_by_label("✔ behalten als erzaehler");
    // new persona in the speaker dropdown choices and the library
    let a = hs.state();
    assert!(a.personas.contains(&"erzaehler".to_string()), "{:?}", a.personas);
    assert!(stimmwerk_gui::tabs::stimmen::choices(a, None).contains(&"erzaehler".to_string()));
    assert!(a.lib.iter().any(|e| e.name == "erzaehler" && e.kind == "design" && e.anchored));
    // Stimmen tab: a design is deleted (with its anchor clip), not revoked
    hs.state_mut().tab = Tab::Stimmen;
    hs.run_steps(3);
    assert!(hs.query_by_label("Widerrufen").is_none());
    hs.get_by_label("Löschen").click();
    hs.run_steps(3);
    hs.get_by_label_contains("„erzaehler“ löschen?");
    hs.get_by_label_contains("Anker-Clip, Takes und Mischungen");
    shoot(&mut hs, "stimmen-loeschen-dialog");
    hs.get_by_label("Stimme und Aufnahmen löschen").click();
    settle(&mut hs, 3.0);
    assert!(!vdir.join("erzaehler.toml").exists() && !vdir.join("clips/erzaehler.wav").exists());
    assert!(!hs.state().lib.iter().any(|e| e.name == "erzaehler"));
}

#[test]
fn erzeugen_clone_consent_and_revoke() {
    let (t, d) = project("");
    let vdir = t.path().join("voices");
    let wav = t.path().join("aufnahme.wav");
    tone(&wav, 8.0);
    let mut hs = open_with_voices(&d, &vdir, Tab::Erzeugen);
    {
        let g = &mut hs.state_mut().generate;
        (g.wav, g.start, g.end, g.ref_text, g.name) = (wav.display().to_string(), "1".into(), "5,5".into(), "Hallo, das ist meine Stimme.".into(), "eigen".into());
    }
    hs.run_steps(2);
    hs.get_by_label("Klonen …").click();
    hs.run_steps(3);
    assert!(hs.state().generate.consent_open);
    hs.get_by_label_contains("ausdrücklich eingewilligt");
    hs.get_by_label_contains("dafür bist du verantwortlich");
    hs.get_by_label_contains("bleiben lokal auf diesem Mac");
    hs.get_by_label_contains("nicht mehr gerendert und alle Aufnahmen werden gelöscht");
    assert_eq!(hs.state().generate.consent_date, stimmwerk_core::voice::today());
    let confirm = "Einwilligung bestätigen und klonen";
    // consent date: JJJJ-MM-TT, not in the future
    for bad in ["1.10.2026", "2999-01-01"] {
        hs.state_mut().generate.consent_date = bad.into();
        hs.run_steps(2);
        hs.get_by_label("Datum als JJJJ-MM-TT, nicht in der Zukunft");
    }
    hs.state_mut().generate.consent_date = "2026-10-01".into();
    hs.run_steps(2);
    assert!(hs.query_by_label("Datum als JJJJ-MM-TT, nicht in der Zukunft").is_none());
    assert!(hs.get_by_label(confirm).accesskit_node().is_disabled());
    hs.state_mut().generate.who = "Max (eigene synthetische Stimme)".into();
    hs.run_steps(2);
    assert!(hs.get_by_label(confirm).accesskit_node().is_disabled(), "checkbox still open");
    hs.get_by_label(stimmwerk_gui::app::CONSENT_STATEMENT).click();
    hs.run_steps(2);
    assert!(!hs.get_by_label(confirm).accesskit_node().is_disabled());
    shoot(&mut hs, "stimme-erzeugen-einwilligung");
    hs.get_by_label(confirm).click();
    settle(&mut hs, 5.0);
    let a = hs.state();
    assert!(!a.generate.consent_open && a.generate.cloned.as_deref() == Some("eigen"), "{:?}", a.msg);
    let p = funkhaus_voice::persona::Persona::load(&vdir, "eigen").unwrap();
    let c = p.consent.unwrap();
    assert_eq!((c.who.as_str(), c.date.as_str()), ("Max (eigene synthetische Stimme)", "2026-10-01"));
    assert!(c.statement.starts_with(stimmwerk_gui::app::CONSENT_STATEMENT));
    let (x, sr) = funkhaus_audio::mix::read(&vdir.join("clips/eigen.wav")).unwrap();
    assert_eq!(x.len(), (4.5 * sr as f64) as usize);
    hs.get_by_label("▶ Probe eigen");
    shoot(&mut hs, "stimme-erzeugen-geklont");
    // Stimmen tab: library with origin + Widerrufen
    hs.state_mut().tab = Tab::Stimmen;
    hs.run_steps(3);
    hs.get_by_label_contains("Einwilligung: Max");
    // any running job (here a probe of this very voice) blocks Widerrufen, not only project jobs
    let ctx = hs.ctx.clone();
    hs.state_mut().jobs.push(idle_job(&ctx, JobKind::Probe("eigen".into())));
    hs.run_steps(2);
    assert!(hs.get_by_label("Widerrufen").accesskit_node().is_disabled());
    hs.state_mut().cancel_all();
    settle(&mut hs, 3.0);
    hs.get_by_label("Widerrufen").click();
    hs.run_steps(3);
    hs.get_by_label_contains("widerrufen?");
    hs.get_by_label_contains("Alle Aufnahmen");
    assert!(hs.query_by_label("Löschen").is_none(), "clones are revoked, not deleted");
    shoot(&mut hs, "stimmen-widerrufen-dialog");
    hs.get_by_label("Widerrufen und Aufnahmen löschen").click();
    hs.run_steps(1);
    assert!(hs.state().jobs.iter().any(|j| j.kind == JobKind::Revoke("eigen".into())) || hs.state().lib.iter().any(|e| e.name == "eigen" && e.revoked.is_some()), "revoke runs as a job");
    settle(&mut hs, 5.0);
    assert!(hs.state().msg.as_ref().is_some_and(|(e, m)| !e && m.contains("Transkript bleibt für spätere Bereinigung gespeichert")), "{:?}", hs.state().msg);
    let e = hs.state().lib.iter().find(|e| e.name == "eigen").cloned().unwrap();
    assert!(e.revoked.is_some() && !e.valid);
    assert!(stimmwerk_core::speakers::spec(std::slice::from_ref(&vdir), "eigen").unwrap_err().to_string().contains("widerrufen"));
    assert!(!vdir.join("clips/eigen.wav").exists(), "clip deleted on revoke");
    hs.get_by_label_contains("widerrufen 20");
    shoot(&mut hs, "stimmen-bibliothek");
}

#[test]
fn clone_without_consent_writes_nothing() {
    let (t, d) = project("");
    let vdir = t.path().join("voices");
    let wav = t.path().join("a.wav");
    tone(&wav, 4.0);
    let mut hs = open_with_voices(&d, &vdir, Tab::Erzeugen);
    {
        let g = &mut hs.state_mut().generate;
        (g.wav, g.ref_text, g.name, g.who) = (wav.display().to_string(), "Hallo.".into(), "x".into(), "A".into());
    }
    let ctx = hs.ctx.clone();
    hs.state_mut().clone_voice(&ctx); // agreed == false -> empty consent
    assert!(hs.state().msg.as_ref().is_some_and(|(e, m)| *e && m.contains("Einwilligung fehlt")));
    assert!(!vdir.join("x.toml").exists() && !vdir.join("clips/x.wav").exists());
    hs.get_by_label("Klonen …").click();
    hs.run_steps(2);
    hs.get_by_label("Abbrechen").click();
    hs.run_steps(2);
    assert!(!hs.state().generate.consent_open);
}

/// Line states and undo come from the cache on open (no render in this session).
#[test]
fn states_and_undo_from_cache_on_open() {
    let (_t, d) = project("");
    let p = stimmwerk_core::Project::load(&d).unwrap();
    let no = std::sync::atomic::AtomicBool::new(false);
    let mut say = |w: &Path, _: &str, _: &funkhaus_voice::persona::VoiceSpec, s: u64| funkhaus_audio::mix::write(w, &vec![0.1 + (s % 100) as f32 * 0.001; 2400], 24_000);
    render::render_with(&p, &[], &render::Opts::default(), &no, &|_| {}, &mut say).unwrap();
    render::reroll_with(&p, &[], 3, false, Quality::Final, &no, &|_| {}, &mut say).unwrap();
    // line 5 edited after the render -> open
    let s = std::fs::read_to_string(d.join("script.txt")).unwrap().replace("Sie steigen die Wendeltreppe hinauf.", "Sie steigen langsam hinauf.");
    std::fs::write(d.join("script.txt"), s).unwrap();
    let mut app = App::new(None);
    app.dirs = vec![];
    let mut hs = harness(app, egui::vec2(1280.0, 820.0), 1.0, ThemePreference::Light);
    let ctx = hs.ctx.clone();
    hs.state_mut().open(&ctx, &d);
    settle(&mut hs, 10.0);
    let a = hs.state();
    let c = Some(State::Cached);
    assert_eq!(a.states, [c, c, c, c, None, c, c]);
    assert_eq!(a.takes, [0, 0, 1, 0, 0, 0, 0]);
    let undo: Vec<bool> = hs.get_all_by_label("⟲ Zurück").map(|n| n.accesskit_node().is_disabled()).collect();
    assert_eq!(undo, [true, true, false, true, true, true, true]);
    assert_eq!(hs.get_all_by_label("Entwurf – · Endfassung ✔").count(), 6);
    hs.get_by_label("Entwurf – · Endfassung –");
    shoot(&mut hs, "skript-status-aus-cache");
    // draft has no takes yet: all open, undo stays (takes are project-wide)
    let ctx = hs.ctx.clone();
    hs.state_mut().set_draft(&ctx, true);
    settle(&mut hs, 10.0);
    assert_eq!(hs.state().states, vec![None; 7]);
    assert_eq!(hs.state().takes[2], 1);
}

/// Job that runs until cancelled, then still reports a finished line 1 (a late event) and fails.
fn late_job(ctx: &egui::Context, kind: JobKind) -> jobs::Job {
    jobs::spawn(ctx, kind, |c, prog| {
        while !c.load(Ordering::Relaxed) {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        prog(render::Event::Line { i: 1, n: 7, state: State::Done, tts_secs: Some(1.0) });
        anyhow::bail!("spät")
    })
}

/// Closing/switching a project: its jobs are cancelled and detached (late events/results ignored, nothing stays busy),
/// and the new project gets its own lint although the old one's lint was still running.
#[test]
fn switch_project_detaches_jobs_and_relints() {
    let (_ta, a) = project("");
    let (_tb, b) = project("Nina|0|Wer bin ich?");
    let mut hs = open(&a, Tab::Skript, ThemePreference::Light);
    let ctx = hs.ctx.clone();
    {
        let s = hs.state_mut();
        let j = late_job(&ctx, JobKind::Render);
        s.jobs.push(jobs::Job { dir: Some(a.clone()), ..j });
        s.jobs.push(late_job(&ctx, JobKind::Lint));
        assert!(s.busy());
    }
    hs.get_by_label("⬅ Projekte").click();
    hs.run_steps(2);
    assert!(hs.state().proj.is_none() && !hs.state().busy() && hs.state().jobs.is_empty());
    hs.state_mut().open(&ctx, &b);
    settle(&mut hs, 10.0);
    let s = hs.state();
    assert!(s.orphans.is_empty(), "orphans reaped");
    assert_eq!(s.states, vec![None; 8], "late event of the old project ignored");
    assert!(s.msg.is_none(), "late result of the old project ignored: {:?}", s.msg);
    let l = s.lint.as_ref().expect("new project linted");
    assert!(l.issues.iter().any(|i| i.code == "unknown_speaker"), "lint is the new project's");
    assert!(!s.busy());
}

/// on_exit path: jobs cancelled and joined within 2 s, GPU priority files of this process removed.
#[test]
fn shutdown_joins_jobs() {
    let (_t, d) = project("");
    let mut hs = open(&d, Tab::Skript, ThemePreference::Light);
    let ctx = hs.ctx.clone();
    hs.state_mut().jobs.push(idle_job(&ctx, JobKind::Render));
    hs.state_mut().orphans.push(idle_job(&ctx, JobKind::Probe("gt".into())));
    let t0 = std::time::Instant::now();
    hs.state_mut().shutdown();
    assert!(t0.elapsed().as_secs_f64() < 2.0);
    assert!(hs.state().jobs.is_empty() && hs.state().orphans.is_empty());
}

/// script.txt saved in an editor: reloaded and relinted (not while a job runs); "Neu laden" + "Skript bearbeiten" exist.
#[test]
fn external_edit_reloads() {
    let (_t, d) = project("");
    let mut hs = open(&d, Tab::Skript, ThemePreference::Light);
    hs.get_by_role_and_label(egui::accesskit::Role::Button, "✏ Skript bearbeiten");
    hs.get_by_role_and_label(egui::accesskit::Role::Button, "⟳ Neu laden");
    let f = d.join("script.txt");
    let bump = |extra: &str| {
        let s = std::fs::read_to_string(&f).unwrap();
        std::fs::write(&f, format!("{s}{extra}\n")).unwrap();
        let t = std::fs::metadata(&f).unwrap().modified().unwrap() + std::time::Duration::from_secs(2);
        std::fs::File::options().write(true).open(&f).unwrap().set_modified(t).unwrap();
    };
    // busy: no reload yet (even after the debounce)
    let ctx = hs.ctx.clone();
    hs.state_mut().jobs.push(idle_job(&ctx, JobKind::Render));
    bump("ERZ|0|Neue Zeile am Ende.");
    let step = |hs: &mut H| {
        hs.state_mut().watch.at = None;
        hs.run_steps(2);
    };
    step(&mut hs);
    age_pending(&mut hs);
    step(&mut hs);
    assert_eq!(hs.state().summary.lines, 7);
    hs.state_mut().cancel_all();
    settle(&mut hs, 3.0);
    age_pending(&mut hs);
    step(&mut hs);
    settle(&mut hs, 10.0);
    let s = hs.state();
    assert_eq!((s.summary.lines, s.states.len()), (8, 8));
    assert!(s.msg.as_ref().is_some_and(|(e, m)| !e && m.contains("außerhalb geändert")), "{:?}", s.msg);
    // debounce: a change is seen, but reloaded only after it stayed unchanged for DEBOUNCE
    bump("ERZ|0|Noch eine.");
    step(&mut hs);
    assert!(hs.state().watch.pending.is_some());
    assert_eq!(hs.state().summary.lines, 8);
    // the editor writes again (second step of a save): the wait starts over
    bump("ERZ|0|Und die letzte.");
    age_pending(&mut hs);
    step(&mut hs);
    assert_eq!(hs.state().summary.lines, 8, "changed again -> wait again");
    age_pending(&mut hs);
    step(&mut hs);
    settle(&mut hs, 10.0);
    assert_eq!(hs.state().summary.lines, 10);
    hs.get_by_label("Neue Zeile am Ende.");
    hs.get_by_label("Noch eine.");
}

/// Pretends the pending change was seen longer than the debounce ago.
fn age_pending(hs: &mut H) {
    if let Some(p) = hs.state_mut().watch.pending.as_mut() {
        p.2 -= stimmwerk_gui::app::DEBOUNCE + std::time::Duration::from_millis(100);
    }
}

/// omnivoice's voice list: cached between lints, asked again on "Prüfen".
#[test]
fn builtins_refresh_on_pruefen() {
    let (_t, d) = project("");
    let mut hs = open(&d, Tab::Skript, ThemePreference::Light);
    assert_eq!(hs.state().bi.as_deref().map(<[String]>::len), Some(3));
    hs.state_mut().bi = Some(vec!["gt".into(), "kn".into(), "fable".into(), "alt".into()]);
    let ctx = hs.ctx.clone();
    hs.state_mut().relint(&ctx);
    settle(&mut hs, 10.0);
    assert_eq!(hs.state().bi.as_ref().map(Vec::len), Some(4), "cached");
    hs.get_by_label("⟳ Prüfen").click();
    hs.run_steps(1);
    settle(&mut hs, 10.0);
    assert_eq!(hs.state().bi.as_ref().map(Vec::len), Some(3), "asked again");
}

/// GPU busy elsewhere: visible as "wartet auf GPU" in the bar and on the line.
#[test]
fn shot_waiting_for_gpu() {
    let (_t, d) = project("");
    let mut hs = open(&d, Tab::Skript, ThemePreference::Light);
    let ctx = hs.ctx.clone();
    let j = jobs::spawn(&ctx, JobKind::Render, |c, prog| {
        prog(render::Event::Line { i: 1, n: 7, state: State::Cached, tts_secs: None });
        prog(render::Event::Line { i: 2, n: 7, state: State::Rendering, tts_secs: None });
        prog(render::Event::Msg { text: "wartet auf GPU …".into() });
        while !c.load(Ordering::Relaxed) {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        anyhow::bail!("abgebrochen")
    });
    hs.state_mut().jobs.push(jobs::Job { total: 7, done: 1, cur: Some(2), ..j });
    // the events come from another thread: step until they arrived
    let t0 = std::time::Instant::now();
    while !hs.state().jobs.first().is_some_and(|j| j.gpu_wait) && t0.elapsed().as_secs() < 5 {
        hs.run_steps(1);
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    hs.run_steps(2);
    hs.get_by_label_contains("wartet auf GPU … (ein anderes Programm");
    hs.get_by_label("○ wartet auf GPU");
    shoot(&mut hs, "skript-wartet-gpu");
    hs.state_mut().cancel_all();
    settle(&mut hs, 2.0);
}

/// Assigning a voice writes only project.toml: an edit of script.txt made in an external editor after opening stays.
#[test]
fn assign_voice_keeps_external_script_edit() {
    let (_t, d) = project("");
    let toml = d.join("project.toml");
    std::fs::write(&toml, std::fs::read_to_string(&toml).unwrap().replace("[speakers.Mara]", "# Mara: warm\n[speakers.Mara]")).unwrap();
    let mut hs = open(&d, Tab::Stimmen, ThemePreference::Light);
    let f = d.join("script.txt");
    let edited = format!("{}ERZ|0|Im Editor ergänzt, noch nicht neu geladen.\n", std::fs::read_to_string(&f).unwrap());
    std::fs::write(&f, &edited).unwrap();
    hs.get(by().role(egui::accesskit::Role::ComboBox).value("Standard (mittlere Lage)")).click();
    hs.run_steps(3);
    hs.get_all_by_label("Fable (weiblich)").last().unwrap().click();
    hs.run_steps(2);
    assert_eq!(std::fs::read(&f).unwrap(), edited.as_bytes());
    let t = std::fs::read_to_string(&toml).unwrap();
    assert!(t.contains("# Mara: warm\n[speakers.Mara]\npersona = \"fable\""), "{t}");
    assert_eq!(stimmwerk_core::Project::load(&d).unwrap().meta.speakers["Mara"].persona.as_deref(), Some("fable"));
}

/// An external script.txt save pending in the debounce window when a voice is assigned (own project.toml write) is
/// still reloaded: the own write stamps only project.toml.
#[test]
fn assign_voice_keeps_pending_script_reload() {
    let (_t, d) = project("");
    let mut hs = open(&d, Tab::Stimmen, ThemePreference::Light);
    let n0 = hs.state().summary.lines;
    let f = d.join("script.txt");
    std::fs::write(&f, format!("{}ERZ|0|Im Editor gespeichert.\n", std::fs::read_to_string(&f).unwrap())).unwrap();
    let t = std::fs::metadata(&f).unwrap().modified().unwrap() + std::time::Duration::from_secs(2);
    std::fs::File::options().write(true).open(&f).unwrap().set_modified(t).unwrap();
    hs.state_mut().watch.at = None;
    hs.run_steps(2);
    assert!(hs.state().watch.pending.is_some(), "change seen, waiting for the debounce");
    hs.get(by().role(egui::accesskit::Role::ComboBox).value("Standard (mittlere Lage)")).click();
    hs.run_steps(3);
    hs.get_all_by_label("Fable (weiblich)").last().unwrap().click();
    hs.run_steps(2);
    age_pending(&mut hs);
    hs.state_mut().watch.at = None;
    hs.run_steps(2);
    settle(&mut hs, 10.0);
    let s = hs.state();
    assert_eq!(s.summary.lines, n0 + 1, "{:?}", s.msg);
    assert_eq!(s.proj.as_ref().unwrap().meta.speakers["Mara"].persona.as_deref(), Some("fable"));
}

/// A sync project (sync.json): GUI render disabled with the hint (also ⌘R); export stays.
#[test]
fn sync_project_render_disabled() {
    let (_t, d) = project("");
    std::fs::write(d.join(stimmwerk_core::project::SYNC), "{}").unwrap();
    let mut hs = open_nodirs(&d, Tab::Skript);
    let b = hs.get_by_role_and_label(egui::accesskit::Role::Button, "Rendern");
    assert!(b.accesskit_node().is_disabled());
    let ctx = hs.ctx.clone();
    hs.state_mut().start(&ctx, JobKind::Render);
    let a = hs.state();
    assert!(a.jobs.iter().all(|j| j.kind != JobKind::Render));
    assert_eq!(a.msg.as_ref().map(|m| m.1.as_str()), Some(stimmwerk_gui::app::SYNC_RENDER));
}

/// Revoke/rm messages: a partial result does not claim everything was deleted; what was not is its own sentence.
#[test]
fn purge_result_texts() {
    let r = stimmwerk_core::voice::Revoked { persona: "x".into(), revoked: "2026-10-13".into(), file: "x.toml".into(), already: false, deleted: vec!["a".into(), "b".into()], kept_external: None, skipped: vec!["c nicht gelöscht: hat weitere harte Links".into()], ref_text_kept: true };
    let (err, m) = App::revoked_text(&r);
    assert!(err && m.contains("2 Dateien gelöscht, nicht alles.") && m.contains(" Nicht gelöscht (1): c nicht gelöscht: hat weitere harte Links."), "{m}");
    assert!(m.contains("Transkript bleibt für spätere Bereinigung gespeichert"));
    let (err, m) = App::revoked_text(&stimmwerk_core::voice::Revoked { skipped: vec![], ref_text_kept: false, ..r });
    assert!(!err && !m.contains("Nicht gelöscht") && !m.contains("Transkript"), "{m}");
}

fn fake_say() -> impl FnMut(&Path, &str, &funkhaus_voice::persona::VoiceSpec, u64) -> anyhow::Result<()> {
    |w: &Path, _: &str, _: &funkhaus_voice::persona::VoiceSpec, s: u64| funkhaus_audio::mix::write(w, &vec![0.1 + (s % 100) as f32 * 0.001; 2400], 24_000)
}

fn render_q(d: &Path, q: Quality, line: Option<usize>) {
    let p = stimmwerk_core::Project::load(d).unwrap();
    let no = std::sync::atomic::AtomicBool::new(false);
    render::render_with(&p, &[], &render::Opts { quality: q, line, fresh: false }, &no, &|_| {}, &mut fake_say()).unwrap();
}

fn open_nodirs(d: &Path, tab: Tab) -> H {
    let mut app = App::new(None);
    app.dirs = vec![];
    let mut hs = harness(app, egui::vec2(1280.0, 820.0), 1.0, ThemePreference::Light);
    let ctx = hs.ctx.clone();
    hs.state_mut().open(&ctx, d);
    hs.state_mut().tab = tab;
    settle(&mut hs, 10.0);
    hs
}

/// Entwurf|Final: no remembered quality -> the one with more cached lines; per line both states; summary; the
/// "Endfassung rendern" offer; the switch is remembered in project.toml without a reload loop.
#[test]
fn quality_toggle_states_and_final_offer() {
    let (_t, d) = project("");
    render_q(&d, Quality::Draft, None); // e.g. from the CLI
    render_q(&d, Quality::Final, Some(2));
    let mut hs = open_nodirs(&d, Tab::Skript);
    let a = hs.state();
    assert!(a.draft && !a.auto_quality, "more draft lines cached -> opens in draft");
    assert_eq!((a.draft_only(), a.final_missing(), a.offer_final()), (6, 6, true));
    assert_eq!(hs.get_all_by_label("Entwurf ✔ · Endfassung –").count(), 6);
    hs.get_by_label("Entwurf ✔ · Endfassung ✔");
    hs.get_by_label_contains("6 Zeilen nur als Entwurf");
    hs.get(by().role(egui::accesskit::Role::Button).label("Entwurf"));
    assert!(!hs.get_by_role_and_label(egui::accesskit::Role::Button, "Endfassung rendern").accesskit_node().is_disabled());
    shoot(&mut hs, "skript-entwurf-final");
    // busy: offered but disabled
    let ctx = hs.ctx.clone();
    hs.state_mut().jobs.push(idle_job(&ctx, JobKind::Render));
    hs.run_steps(2);
    assert!(hs.get_by_role_and_label(egui::accesskit::Role::Button, "Endfassung rendern").accesskit_node().is_disabled());
    hs.state_mut().cancel_all();
    settle(&mut hs, 3.0);
    // switch to Final: remembered in project.toml (only that), script untouched, no "changed outside" reload
    let script = std::fs::read(d.join("script.txt")).unwrap();
    hs.get_by_role_and_label(egui::accesskit::Role::Button, "Endfassung").click();
    hs.run_steps(2);
    hs.state_mut().watch.at = None;
    settle(&mut hs, 5.0);
    let a = hs.state();
    assert!(!a.draft);
    assert!(!a.msg.as_ref().is_some_and(|(_, m)| m.contains("außerhalb")), "{:?}", a.msg);
    assert_eq!(stimmwerk_core::Project::load(&d).unwrap().meta.settings.last_quality, Some(Quality::Final));
    assert_eq!(std::fs::read(d.join("script.txt")).unwrap(), script);
    let c = Some(State::Cached);
    assert_eq!(a.states, [None, c, None, None, None, None, None]);
    // reopen: remembered quality wins over the heuristic
    let hs2 = open_nodirs(&d, Tab::Skript);
    assert!(!hs2.state().draft && !hs2.state().auto_quality);
    // all final -> no offer, no summary note
    render_q(&d, Quality::Final, None);
    let mut hs3 = open_nodirs(&d, Tab::Skript);
    assert_eq!((hs3.state().draft_only(), hs3.state().offer_final()), (0, false));
    assert!(hs3.query_by_label("Endfassung rendern").is_none());
    hs3.state_mut().tab = Tab::Export;
    hs3.run_steps(3);
    hs3.get_by_label("Exportiert wird immer die Endfassung (volle Qualität).");
}

/// Export never uses the draft: the job runs in final, missing final lines get a warning + "Endfassung rendern".
#[test]
fn export_is_always_final() {
    use stimmwerk_gui::tabs::{
        export::final_missing_text,
        skript::{draft_only_text, pair_text},
    };
    assert_eq!(App::job_quality(Quality::Draft, &JobKind::Export), Quality::Final);
    assert_eq!(App::job_quality(Quality::Draft, &JobKind::Render), Quality::Draft);
    assert_eq!(App::job_quality(Quality::Draft, &JobKind::PlayLine(1)), Quality::Draft);
    assert_eq!(pair_text(true, false).0, "Entwurf ✔ · Endfassung –");
    assert_eq!(pair_text(false, true).0, "Entwurf – · Endfassung ✔");
    assert_eq!((draft_only_text(0), draft_only_text(1)), (String::new(), " · 1 Zeile nur als Entwurf".to_string()));
    assert!(final_missing_text(1).starts_with("⚠ Für 1 Zeile fehlt die Endfassung") && final_missing_text(3).starts_with("⚠ Für 3 Zeilen fehlt"));
    let (_t, d) = project("");
    render_q(&d, Quality::Draft, None);
    let mut hs = open_nodirs(&d, Tab::Export);
    assert!(hs.state().draft);
    hs.get_by_label_contains("Für 7 Zeilen fehlt die Endfassung");
    shoot(&mut hs, "export-final-fehlt");
    hs.get_by_role_and_label(egui::accesskit::Role::Button, "Endfassung rendern").click();
    hs.run_steps(2);
    let a = hs.state();
    assert_eq!((a.draft, a.tab), (false, Tab::Skript));
    assert!(a.jobs.iter().any(|j| j.kind == JobKind::Render));
    hs.state_mut().cancel_all();
    settle(&mut hs, 5.0);
}

/// Lines with a lint error: Anhören / Neu würfeln disabled.
#[test]
fn error_line_buttons_disabled() {
    let (_t, d) = project("Nina|0|Wer bin ich?");
    let hs = open(&d, Tab::Skript, ThemePreference::Light);
    let dis = |l: &str| hs.get_all_by_role_and_label(egui::accesskit::Role::Button, l).map(|n| n.accesskit_node().is_disabled()).collect::<Vec<_>>();
    assert_eq!(dis("▶ Anhören"), [false, false, false, false, false, false, false, true]);
    assert_eq!(dis("Neu würfeln"), [false, false, false, false, false, false, false, true]);
}

/// First TTS line taking long: the bar says the model is loading (instead of a silent 30–60 s).
#[test]
fn model_load_hint() {
    let (_t, d) = project("");
    let mut hs = open(&d, Tab::Skript, ThemePreference::Light);
    let ctx = hs.ctx.clone();
    let mut j = idle_job(&ctx, JobKind::Render);
    (j.total, j.cur, j.stage, j.line_t0) = (7, Some(1), Some("render".into()), Some(std::time::Instant::now() - stimmwerk_gui::app::MODEL_LOAD_HINT));
    assert!(j.model_loading());
    hs.state_mut().jobs.push(j);
    hs.run_steps(3);
    hs.get_by_label("Zeile 1: lädt Sprachmodell (erstes Mal 30–60 s) …");
    shoot(&mut hs, "skript-modell-laedt");
    // after one spoken line no hint any more
    hs.state_mut().jobs[0].tts_done = 1;
    hs.run_steps(2);
    assert!(hs.query_by_label_contains("Sprachmodell").is_none());
    hs.get_by_label("Zeile 1 läuft …");
    hs.state_mut().cancel_all();
    settle(&mut hs, 3.0);
}

/// A cancelled render already finished some lines: their states come back from disk (both qualities), not only after
/// a successful render.
#[test]
fn cancelled_job_refreshes_states_from_disk() {
    let (_t, d) = project("");
    let mut hs = open_nodirs(&d, Tab::Skript);
    assert_eq!(hs.state().final_missing(), 7);
    render_q(&d, Quality::Final, Some(1)); // what the job wrote before it was cancelled
    let ctx = hs.ctx.clone();
    hs.state_mut().jobs.push(idle_job(&ctx, JobKind::Render));
    hs.run_steps(2);
    hs.state_mut().cancel_all();
    settle(&mut hs, 10.0);
    assert_eq!((hs.state().final_missing(), hs.state().both[0]), (6, [false, true]));
}

/// Einstellungen → KI-Anbieter: local presets listed, add dialog (preset Anthropic → online warning, first-activation
/// modal, masked key into the fake keychain, never into files or the tree), Testen blocked without the online switch,
/// remove dialog deletes the key.
#[test]
fn einst_ki_anbieter_dialog_key_and_remove() {
    let (_t, d) = project("");
    let _ = std::fs::remove_file(llm_cfg::path());
    let _ = std::fs::remove_file(kc_dir().join("stimmwerk-llm--anthropic"));
    let mut hs = open(&d, Tab::Einstellungen, ThemePreference::Light);
    hs.get_by_label("KI-Anbieter");
    hs.get_by_label("gemma");
    assert_eq!(hs.get_all_by_label("lokal").count(), 4, "four local presets");
    assert!(hs.get_all_by_label("🗑 Entfernen").any(|n| disabled(&n)), "default cannot be removed");
    hs.get_by_label("+ Anbieter hinzufügen").click();
    hs.run_steps(3);
    hs.get_by_label("Anbieter hinzufügen");
    assert!(disabled(&hs.get_by_label("Speichern")));
    shoot(&mut hs, "r14-llm-dialog-leer");
    hs.state_mut().llm.dlg.as_mut().unwrap().preset(llm_cfg::Preset::get("anthropic").unwrap());
    hs.run_steps(2);
    assert_eq!(hs.state().llm.dlg.as_ref().unwrap().url, "https://api.anthropic.com/v1");
    hs.get_by_label_contains("Gib dort keine Personendaten ein");
    hs.get_by_label("API-Schlüssel");
    // first tick asks; Abbrechen leaves it off, Erlauben turns it on
    hs.get_by_label("Online-Anbieter erlauben").click();
    hs.run_steps(3);
    assert!(hs.state().llm.online_ask && !hs.state().llm.dlg.as_ref().unwrap().allow_online);
    hs.get_by_label("Online-Anbieter erlauben?");
    hs.get_by_label_contains("Server außerhalb deines Macs");
    shoot(&mut hs, "r14-llm-online-frage");
    hs.get_all_by_label("Abbrechen").last().unwrap().click();
    hs.run_steps(2);
    assert!(!hs.state().llm.online_ask && !hs.state().llm.dlg.as_ref().unwrap().allow_online);
    hs.get_by_label("Online-Anbieter erlauben").click();
    hs.run_steps(3);
    hs.get_by_label("Erlauben").click();
    hs.run_steps(2);
    assert!(hs.state().llm.dlg.as_ref().unwrap().allow_online);
    hs.state_mut().llm.dlg.as_mut().unwrap().key = "sk-test-leer".into();
    hs.run_steps(2);
    assert!(hs.query_all_by_label_contains("sk-test").next().is_none(), "key never readable in the tree");
    assert!(!format!("{:?}", hs.state().llm.dlg).contains("sk-test"), "Debug redacts the key");
    shoot(&mut hs, "r14-llm-dialog-online");
    hs.get_by_label("Speichern").click();
    settle(&mut hs, 5.0);
    assert!(hs.state().llm.dlg.is_none());
    let st = llm_cfg::load().unwrap();
    let e = &st.llm.backends["anthropic"];
    assert!(e.allow_online && !e.is_local() && e.key_info().as_deref() == Some("keychain"), "{e:?}");
    let txt = std::fs::read_to_string(llm_cfg::path()).unwrap();
    assert!(!txt.contains("sk-test"), "key in settings.toml");
    assert_eq!(std::fs::read_to_string(kc_dir().join("stimmwerk-llm--anthropic")).unwrap(), "sk-test-leer");
    hs.get_by_label("✔ im Schlüsselbund");
    hs.get_by_label("online");
    assert!(hs.query_all_by_label_contains("sk-test").next().is_none());
    // while a key is being written the chip spins
    let ctx = hs.ctx.clone();
    let j = idle_job(&ctx, JobKind::KeySave("anthropic".into()));
    hs.state_mut().jobs.push(j);
    hs.run_steps(2);
    hs.get_by_label("⟳ speichere …");
    assert!(hs.query_by_label("✔ im Schlüsselbund").is_none());
    hs.state_mut().jobs.last().unwrap().cancel();
    settle(&mut hs, 3.0);
    hs.get_by_label("✔ im Schlüsselbund");
    // editing never prefills the key
    hs.get_all_by_label("✏ Bearbeiten").next().unwrap().click();
    hs.run_steps(3);
    assert!(hs.state().llm.dlg.as_ref().unwrap().key.is_empty());
    hs.get_by_label("Leer lassen = bisheriger Schlüssel bleibt");
    hs.get_all_by_label("Abbrechen").last().unwrap().click();
    hs.run_steps(2);
    // online switch off again: Testen is blocked for that row only
    hs.state_mut().settings.llm.backends.get_mut("anthropic").unwrap().allow_online = false;
    hs.run_steps(2);
    let dis: Vec<bool> = hs.get_all_by_label("⟳ Testen").map(|n| disabled(&n)).collect();
    assert_eq!(dis.iter().filter(|d| **d).count(), 1, "{dis:?}");
    shoot(&mut hs, "r14-einstellungen-ki-hell");
    let mut dark = open(&d, Tab::Einstellungen, ThemePreference::Dark);
    shoot(&mut dark, "r14-einstellungen-ki-dunkel");
    // remove: dialog, key deleted with it
    hs.get_all_by_label("🗑 Entfernen").find(|n| !disabled(n)).unwrap().click();
    hs.run_steps(3);
    hs.get_by_label("Anbieter „anthropic“ entfernen?");
    hs.get_by_label_contains("auch aus dem Schlüsselbund löschen");
    shoot(&mut hs, "r14-llm-entfernen");
    hs.get_by_label("Anbieter entfernen").click();
    hs.run_steps(2);
    assert!(!hs.state().settings.llm.backends.contains_key("anthropic"));
    assert!(!llm_cfg::load().unwrap().llm.backends.contains_key("anthropic"));
    assert!(!kc_dir().join("stimmwerk-llm--anthropic").exists(), "keychain item deleted");
}

/// "Testen" asks only `GET /models` of a mock; results as chips, errors in du-form without the key.
#[test]
fn llm_test_against_mock() {
    let (_t, d) = project("");
    let m = Mock::start(vec![ok(r#"{"data":[{"id":"a"},{"id":"b"}]}"#)]);
    let mut hs = open(&d, Tab::Einstellungen, ThemePreference::Light);
    hs.state_mut().settings.llm.backends.insert("mock".into(), llm_cfg::Entry { url: format!("{}/v1", m.url()), model: "a".into(), ..llm_cfg::Preset::get("gemma").unwrap().entry() });
    hs.run_steps(2);
    // rows are sorted: gemma, lmstudio, mock, ollama, vmlx
    hs.get_all_by_label("⟳ Testen").nth(2).unwrap().click();
    settle(&mut hs, 10.0);
    assert_eq!(hs.state().llm.status.get("mock"), Some(&Ok(2)));
    hs.get_by_label("✔ erreichbar · 2 Modelle");
    let r = m.reqs();
    assert!(r.len() == 1 && r[0].method == "GET" && r[0].path == "/v1/models", "{r:?}");
    shoot(&mut hs, "r14-llm-test-ok");
    let m2 = Mock::start(vec![err(401, r#"{"error":{"type":"authentication_error","message":"invalid x-api-key"}}"#)]);
    hs.state_mut().settings.llm.backends.get_mut("mock").unwrap().url = format!("{}/v1", m2.url());
    let ctx = hs.ctx.clone();
    hs.state_mut().llm_test(&ctx, "mock");
    settle(&mut hs, 10.0);
    let Some(Err((chip, detail))) = hs.state().llm.status.get("mock").cloned() else { panic!("{:?}", hs.state().llm.status) };
    assert_eq!(chip, "✖ abgelehnt");
    assert!(detail.contains("lehnt deinen Schlüssel ab (401)"), "{detail}");
    hs.get_by_label("✖ abgelehnt");
    // closed port
    let port = { std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port() };
    hs.state_mut().settings.llm.backends.get_mut("mock").unwrap().url = format!("http://127.0.0.1:{port}/v1");
    hs.state_mut().llm_test(&ctx, "mock");
    settle(&mut hs, 15.0);
    assert!(matches!(hs.state().llm.status.get("mock"), Some(Err((c, _))) if c == "✖ nicht erreichbar"), "{:?}", hs.state().llm.status);
    let (c, t) = stimmwerk_gui::tabs::llm::err_text("x", "m", "LLM-Fehler 404: no model");
    assert!(c == "✖ Modell fehlt" && t.contains("„m“"));
    assert_eq!(stimmwerk_gui::tabs::llm::err_text("x", "m", "LLM-Fehler 529: overloaded").0, "⚠ überlastet");
}

/// Skript → "Aus Prosa erstellen …": LLM job (mock) → preview → Verwerfen writes nothing → confirm replaces the script
/// and adds the cast; an online backend without the project switch is refused before any request.
#[test]
fn prose_dialog_preview_and_apply() {
    let (_t, d) = project("");
    let m = Mock::start(vec![fixture("openai/200_success")]);
    let mut hs = open(&d, Tab::Skript, ThemePreference::Light);
    hs.state_mut().settings.llm.backends.get_mut("gemma").unwrap().url = format!("{}/v1", m.url());
    let before = std::fs::read(d.join("script.txt")).unwrap();
    hs.get_by_label("Aus Prosa erstellen …").click();
    hs.run_steps(3);
    assert!(hs.state().prose.open);
    hs.get_by_label("Rollenskript aus Prosa");
    assert!(disabled(&hs.get_by_label("Rollenskript erzeugen")));
    // names of the mock's cast in the text, else the cast guard folds them into ERZ (a long text would grow the dialog past the window)
    hs.state_mut().prose.text = "Mara, Jonas und Ilse: der Mock antwortet mit Kapitel 1.".into();
    hs.run_steps(2);
    hs.get_by_label("Rollenskript erzeugen").click();
    settle(&mut hs, 15.0);
    let o = hs.state().prose.res.clone().unwrap_or_else(|| panic!("{:?}", hs.state().prose.err));
    assert_eq!((o.lines, o.cast.len(), o.merged.len()), (37, 4, 0));
    hs.get_by_label_contains("Vorschau: 37 Zeilen · 4 Rollen");
    hs.get_by_label("Vorhandenes Skript ersetzen (7 Zeilen)");
    shoot(&mut hs, "r14-prosa-vorschau");
    assert_eq!(std::fs::read(d.join("script.txt")).unwrap(), before, "preview writes nothing");
    hs.get_by_label("Verwerfen").click();
    hs.run_steps(2);
    assert!(!hs.state().prose.open && hs.state().prose.res.is_none());
    assert_eq!(std::fs::read(d.join("script.txt")).unwrap(), before);
    assert_eq!(m.hits(), 1);
    // again, confirmed
    hs.state_mut().prose.open = true;
    hs.run_steps(2);
    hs.get_by_label("Rollenskript erzeugen").click();
    settle(&mut hs, 15.0);
    hs.get_by_label("Vorhandenes Skript ersetzen (7 Zeilen)").click();
    settle(&mut hs, 10.0);
    let p = stimmwerk_core::Project::load(&d).unwrap();
    assert_eq!(p.script.lines().count(), 37);
    assert!(p.meta.speakers.contains_key("Ilse") && p.meta.speakers["Mara"].persona.is_some() && p.meta.speakers["Ilse"].persona.is_none(), "cast kept, new roles added unset");
    let a = hs.state();
    assert!(!a.prose.open && a.summary.lines == 37);
    assert!(a.msg.as_ref().is_some_and(|(e, t)| !e && t.contains("Rollenskript übernommen: 37 Zeilen")), "{:?}", a.msg);
    // online backend chosen for the project, consent missing: refused, no request
    hs.state_mut().settings.llm.backends.insert("claude".into(), llm_cfg::Preset::get("anthropic").unwrap().entry());
    hs.state_mut().set_project_llm(Some(Some("claude".into())), None);
    hs.state_mut().prose.open = true;
    hs.run_steps(3);
    hs.get_by_label_contains("Gib dort keine Personendaten ein");
    hs.get_by_label("Rollenskript erzeugen").click();
    hs.run_steps(3);
    assert!(hs.state().prose.err.as_deref().is_some_and(|e| e.contains("nicht freigegeben")), "{:?}", hs.state().prose.err);
    assert_eq!(m.hits(), 2);
    assert_eq!(stimmwerk_core::Project::load(&d).unwrap().meta.llm.as_ref().and_then(|l| l.backend.clone()).as_deref(), Some("claude"));
}

/// Sync project with a too-long cue: "Kürzen vorschlagen" → proposal shown (numbers kept) → applied only on click.
#[test]
fn shorten_sync_cue() {
    env();
    let t = tempfile::tempdir().unwrap();
    let d = t.path().join("sync.studio");
    let mut p = stimmwerk_core::Project::create(&d, "Sync", "de").unwrap();
    p.add_speaker("ERZ", None);
    p.meta.speakers["ERZ"].persona = Some("gt".into());
    p.save_meta().unwrap();
    let srt = "1\n00:00:00,000 --> 00:00:02,000\nDie Firma zahlte dreihundert Euro im März, und der Vertrag wurde pünktlich unterschrieben, sagte er leise.\n\n2\n00:00:03,000 --> 00:00:06,000\nGut, dann ist das geklärt.\n";
    sync::import(&mut p, srt, "x.srt", Some("ERZ"), false).unwrap();
    let m = Mock::start(vec![ok(&chat(r#"{"vorschlag": "Dreihundert Euro im März.", "zeichen": 25, "begruendung": "Nebensatz weg."}"#))]);
    let mut hs = open(&d, Tab::Skript, ThemePreference::Light);
    hs.state_mut().settings.llm.backends.get_mut("gemma").unwrap().url = format!("{}/v1", m.url());
    let rep = hs.state().sync_rep.clone().expect("sync report from lint");
    assert_eq!((rep.summary.too_long + rep.summary.overlap, rep.cues[0].slot), (1, 2.0), "{:?}", rep.summary);
    assert_eq!(hs.get_all_by_label("Kürzen vorschlagen").count(), 1, "only the too-long cue");
    hs.get_by_label("Kürzen vorschlagen").click();
    hs.run_steps(2);
    assert!(hs.state().jobs.iter().any(|j| j.kind == JobKind::Shorten(1)) || hs.state().shorten.contains_key(&1), "row buttons must win over the row click: {:?}", hs.state().msg);
    settle(&mut hs, 15.0);
    let s = hs.state().shorten.get(&1).cloned().unwrap_or_else(|| panic!("{:?}", hs.state().msg));
    assert!(s.accepted && s.proposal.as_deref() == Some("Dreihundert Euro im März."), "{s:?}");
    hs.get_by_label_contains("Kürzungsvorschlag: 25 statt");
    hs.get_by_label("Dreihundert Euro im März.");
    shoot(&mut hs, "r14-kuerzen-vorschlag");
    assert!(stimmwerk_core::Project::load(&d).unwrap().script.lines().next().unwrap().text.starts_with("Die Firma"), "nothing applied yet");
    hs.get_by_label("Übernehmen").click();
    settle(&mut hs, 10.0);
    let p = stimmwerk_core::Project::load(&d).unwrap();
    assert_eq!(p.script.lines().next().unwrap().text, "Dreihundert Euro im März.");
    assert_eq!(sync::load(&p).unwrap().unwrap().cues[0].text, "Dreihundert Euro im März.", "sync.json follows the script");
    assert!(hs.state().shorten.is_empty());
    assert_eq!(hs.state().sync_rep.as_ref().map(|r| r.summary.too_long + r.summary.overlap), Some(0));
    assert_eq!(m.hits(), 1);
}

/// Musik tab: generate via fake acestep (job, args contract, cache hit), place as cue, import a WAV, inline edit, remove.
#[test]
fn musik_generate_import_place() {
    let (t, d) = project("");
    let mut hs = open(&d, Tab::Musik, ThemePreference::Light);
    hs.get_by_label("Musik erzeugen (acestep)");
    hs.get_by_label_contains("Noch keine Cues");
    assert!(disabled(&hs.get_by_label("Erzeugen")));
    shoot(&mut hs, "r14-musik-hell");
    {
        let m = &mut hs.state_mut().music;
        (m.prompt, m.dur, m.seed, m.bpm) = ("calm maritime intro, acoustic guitar".into(), 12, 7, 70);
    }
    hs.run_steps(2);
    hs.get_by_label("○ noch nicht erzeugt");
    let log0 = std::fs::read_to_string(ace_log()).unwrap_or_default().lines().count();
    hs.get_by_label("Erzeugen").click();
    settle(&mut hs, 20.0);
    let b = hs.state().music.last.clone().unwrap_or_else(|| panic!("{:?} {:?}", hs.state().msg, hs.state().banner));
    assert!(!b.cached && b.path.is_file() && b.secs == 12.0 && b.sr == 24_000, "{b:?}");
    assert!(b.path.starts_with(d.join("takes/music")));
    let log = std::fs::read_to_string(ace_log()).unwrap();
    let last = log.lines().last().unwrap();
    for want in ["gen calm maritime intro, acoustic guitar -i -d 12", "--seed 7", "--bpm 70", "-f wav24", "-o "] {
        assert!(last.contains(want), "{want}: {last}");
    }
    assert_eq!(log.lines().count(), log0 + 1);
    hs.get_by_label("✔ im Cache");
    hs.get_by_label("Auf Zeile legen").click();
    settle(&mut hs, 10.0);
    let p = stimmwerk_core::Project::load(&d).unwrap();
    assert_eq!(p.meta.cues.len(), 1);
    let c = &p.meta.cues[0];
    assert!(c.gen_.as_ref().is_some_and(|g| g.seed == 7 && g.dur == 12) && c.at_line == Some(1) && c.duck && c.kind == cues::Kind::Music, "{c:?}");
    assert_eq!(hs.state().lint.as_ref().map(|l| l.errors), Some(0));
    shoot(&mut hs, "r14-musik-cue");
    // same spec again: cache hit, no acestep call
    hs.get_by_label("Erzeugen").click();
    settle(&mut hs, 10.0);
    assert!(hs.state().music.last.as_ref().is_some_and(|b| b.cached));
    assert_eq!(std::fs::read_to_string(ace_log()).unwrap().lines().count(), log0 + 1, "cache hit: no acestep run");
    // import a WAV, place it as ambience on line 3 without ducking
    let w = t.path().join("Meer (CC0).wav");
    tone(&w, 6.0);
    {
        let m = &mut hs.state_mut().music;
        (m.import_path, m.at_line, m.duck, m.loop_) = (w.display().to_string(), 3, false, true);
    }
    hs.run_steps(2);
    hs.get_by_label("Importieren").click();
    hs.run_steps(3);
    assert!(d.join("assets/ambience/Meer-CC0.wav").is_file(), "{:?}", hs.state().msg);
    assert!(std::fs::read_to_string(d.join("assets/SOURCES.md")).unwrap().contains("Meer-CC0.wav"));
    hs.get_all_by_label("Auf Zeile legen").last().unwrap().click();
    settle(&mut hs, 10.0);
    let p = stimmwerk_core::Project::load(&d).unwrap();
    assert_eq!(p.meta.cues.len(), 2);
    let c = &p.meta.cues[1];
    assert!(c.file.as_deref() == Some("assets/ambience/Meer-CC0.wav") && c.at_line == Some(3) && c.loop_ && !c.duck && c.kind == cues::Kind::Ambience, "{c:?}");
    assert_eq!(hs.get_all_by_label("🗑 Entfernen").count(), 2);
    hs.get_by_label("Atmo");
    for h in ["Pegel (dB)", "Einblenden (s)", "Ausblenden (s)"] {
        hs.get_by_label(h);
    }
    shoot(&mut hs, "r14-musik-cues");
    let mut dark = open(&d, Tab::Musik, ThemePreference::Dark);
    shoot(&mut dark, "r14-musik-dunkel");
    // inline edit path writes project.toml only
    let script = std::fs::read(d.join("script.txt")).unwrap();
    hs.state_mut().proj.as_mut().unwrap().meta.cues[1].gain_db = -12.0;
    let ctx = hs.ctx.clone();
    hs.state_mut().cues_changed(&ctx, "Cues gespeichert.");
    settle(&mut hs, 10.0);
    assert_eq!(stimmwerk_core::Project::load(&d).unwrap().meta.cues[1].gain_db, -12.0);
    assert_eq!(std::fs::read(d.join("script.txt")).unwrap(), script);
    // remove the second cue; its file stays
    hs.get_all_by_label("🗑 Entfernen").last().unwrap().click();
    settle(&mut hs, 10.0);
    assert_eq!(stimmwerk_core::Project::load(&d).unwrap().meta.cues.len(), 1);
    assert!(d.join("assets/ambience/Meer-CC0.wav").is_file());
    // a cue that the lint refuses is not written
    let mut bad = cues::Cue::new(cues::Kind::Music);
    (bad.at_line, bad.file) = (Some(99), Some("assets/nope.wav".into()));
    hs.state_mut().cue_add(&ctx, bad);
    assert!(hs.state().msg.as_ref().is_some_and(|(e, t)| *e && t.contains("nichts geändert")), "{:?}", hs.state().msg);
    assert_eq!(stimmwerk_core::Project::load(&d).unwrap().meta.cues.len(), 1);
}

/// acestep waiting for the GPU: visible in the Musik tab, Erzeugen blocked, Abbrechen works.
#[test]
fn shot_musik_waiting_for_gpu() {
    let (_t, d) = project("");
    let mut hs = open(&d, Tab::Musik, ThemePreference::Light);
    hs.state_mut().music.prompt = "tense strings".into();
    let ctx = hs.ctx.clone();
    let j = jobs::spawn(&ctx, JobKind::MusicGen("k".into()), |c, prog| {
        prog(render::Event::Msg { text: "wartet auf GPU …".into() });
        while !c.load(Ordering::Relaxed) {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        anyhow::bail!("abgebrochen")
    });
    hs.state_mut().jobs.push(jobs::Job { dir: Some(d.clone()), ..j });
    let t0 = std::time::Instant::now();
    while !hs.state().jobs.first().is_some_and(|j| j.gpu_wait) && t0.elapsed().as_secs() < 5 {
        hs.run_steps(1);
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    hs.run_steps(2);
    hs.get_by_label_contains("wartet auf GPU … (ein anderes Programm");
    assert!(disabled(&hs.get_by_label("Erzeugen")));
    shoot(&mut hs, "r14-musik-wartet-gpu");
    hs.get_by_label("Abbrechen").click();
    settle(&mut hs, 3.0);
    assert!(hs.state().jobs.is_empty());
    assert!(hs.state().msg.as_ref().is_some_and(|(e, t)| !e && t.contains("abgebrochen")), "{:?}", hs.state().msg);
}
