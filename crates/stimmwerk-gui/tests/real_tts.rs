//! GUI paths with REAL omnivoice, headless (kittest, no window, no focus change). Ignored by default; run with
//! `STIMMWERK_REAL_TTS=1 STIMMWERK_HOME=… OMNIVOICE_PORT=… cargo test --release -p stimmwerk-gui --test real_tts -- --ignored --nocapture`.
//! Works on a fresh copy of the fixture under `$STIMMWERK_HOME/projects/r10-gui-real.studio`.
use eframe::egui::{self, ThemePreference};
use egui_kittest::kittest::Queryable;
use std::path::Path;
use std::time::{Duration, Instant};
use stimmwerk_core::render::{Quality, State};
use stimmwerk_gui::shot::{harness, save_png};
use stimmwerk_gui::tabs::Tab;
use stimmwerk_gui::App;

const FIX: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/stimmwerk/project/leuchtturm.studio");
const SHOTS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../qa/loop/stimmwerk/shots");

type H = egui_kittest::Harness<'static, App>;

fn wait(hs: &mut H, max: Duration, mut done: impl FnMut(&App) -> bool) -> f64 {
    let t0 = Instant::now();
    while !done(hs.state()) {
        assert!(t0.elapsed() < max, "timeout after {max:?}");
        hs.run_steps(1);
        std::thread::sleep(Duration::from_millis(50));
    }
    hs.run_steps(3);
    t0.elapsed().as_secs_f64()
}

fn idle(a: &App) -> bool {
    a.jobs.is_empty() && a.orphans.is_empty()
}

/// Our own omnivoice children (direct children of this test process).
fn children() -> String {
    let o = std::process::Command::new("pgrep").args(["-fl", "-P", &std::process::id().to_string()]).output().unwrap();
    String::from_utf8_lossy(&o.stdout).trim().to_string()
}

fn prio_files() -> Vec<String> {
    let d = funkhaus_sys::home().join(funkhaus_sys::gpu::APP_DIR);
    std::fs::read_dir(d).into_iter().flatten().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).filter(|n| n.starts_with(".gpu.prio")).collect()
}

#[test]
#[ignore]
fn real_render_cancel_final_export() {
    if std::env::var("STIMMWERK_REAL_TTS").as_deref() != Ok("1") {
        eprintln!("STIMMWERK_REAL_TTS=1 nicht gesetzt – übersprungen");
        return;
    }
    let d = stimmwerk_core::projects_dir().join("r10-gui-real.studio");
    let _ = std::fs::remove_dir_all(&d);
    for s in ["takes", "assets", "out"] {
        std::fs::create_dir_all(d.join(s)).unwrap();
    }
    for f in ["project.toml", "script.txt", "lexicon.tsv"] {
        std::fs::copy(Path::new(FIX).join(f), d.join(f)).unwrap();
    }
    let mut hs = harness(App::new(None), egui::vec2(1280.0, 820.0), 1.0, ThemePreference::Light);
    let ctx = hs.ctx.clone();
    hs.state_mut().open(&ctx, &d);
    wait(&mut hs, Duration::from_secs(30), idle);
    assert_eq!(hs.state().lint.as_ref().map(|l| l.errors), Some(0));
    // 1. draft via the toggle + "Rendern"
    hs.get_by_role_and_label(egui::accesskit::Role::Button, "Entwurf").click();
    hs.run_steps(2);
    hs.get_by_role_and_label(egui::accesskit::Role::Button, "Rendern").click();
    hs.run_steps(2);
    let t = wait(&mut hs, Duration::from_secs(300), idle);
    let a = hs.state();
    eprintln!("draft: {t:.1} s, msg {:?}", a.msg);
    assert!(a.draft && a.both.iter().all(|b| b[0] && !b[1]), "{:?}", a.both);
    assert!(a.offer_final());
    save_png(&mut hs, &Path::new(SHOTS).join("real-r10-entwurf.png")).unwrap();
    // 2. final, cancel while a line is speaking: child gone, no priority file of ours
    hs.get_by_role_and_label(egui::accesskit::Role::Button, "Endfassung rendern").click();
    hs.run_steps(2);
    wait(&mut hs, Duration::from_secs(120), |a| a.states.iter().filter(|s| **s == Some(State::Done)).count() >= 2 && a.states.contains(&Some(State::Rendering)));
    std::thread::sleep(Duration::from_millis(800));
    let before = children();
    assert!(before.contains("omnivoice"), "omnivoice child expected while rendering: {before:?}");
    let t0 = Instant::now();
    hs.key_press(egui::Key::Escape);
    let t = wait(&mut hs, Duration::from_secs(10), idle);
    let after = children();
    eprintln!("cancel: {t:.2} s ({:.2} s incl. key), children before {before:?}, after {after:?}, prio {:?}", t0.elapsed().as_secs_f64(), prio_files());
    assert!(after.is_empty(), "omnivoice child left: {after}");
    assert!(!prio_files().iter().any(|n| n.contains(&format!("{}", std::process::id()))));
    let a = hs.state();
    let done_final = a.both.iter().filter(|b| b[1]).count();
    eprintln!("after cancel: {done_final}/7 final cached, msg {:?}", a.msg);
    assert!((2..7).contains(&done_final));
    // 3. "Endfassung rendern" again: only the missing final clips
    hs.get_by_role_and_label(egui::accesskit::Role::Button, "Endfassung rendern").click();
    hs.run_steps(2);
    let t = wait(&mut hs, Duration::from_secs(300), idle);
    let a = hs.state();
    eprintln!("final rest: {t:.1} s, msg {:?}", a.msg);
    assert!(a.both.iter().all(|b| b[0] && b[1]) && !a.draft);
    // 4. rerun = cache only
    hs.get_by_role_and_label(egui::accesskit::Role::Button, "Rendern").click();
    hs.run_steps(2);
    let t = wait(&mut hs, Duration::from_secs(60), idle);
    eprintln!("rerun: {t:.2} s, msg {:?}", hs.state().msg);
    assert!(hs.state().msg.as_ref().is_some_and(|(_, m)| m.contains("0 neu")), "{:?}", hs.state().msg);
    // 5. export from the Export tab (always final)
    hs.state_mut().tab = Tab::Export;
    hs.run_steps(3);
    hs.get_by_label("Exportiert wird immer die Endfassung (volle Qualität).");
    hs.get_by_role_and_label(egui::accesskit::Role::Button, "Exportieren").click();
    hs.run_steps(2);
    let t = wait(&mut hs, Duration::from_secs(120), idle);
    let r = hs.state().ex.res.as_ref().expect("export result");
    eprintln!("export: {t:.1} s, {:?} {} LUFS / {} dBTP, in spec {}", r.file, r.measured.lufs, r.measured.tp, r.in_spec);
    assert!(r.in_spec);
    save_png(&mut hs, &Path::new(SHOTS).join("real-r10-export.png")).unwrap();
    assert_eq!(stimmwerk_core::Project::load(&d).unwrap().meta.settings.last_quality, Some(Quality::Final));
}
