use eframe::egui;
use std::path::PathBuf;
use stimmwerk_gui::{App, cfg_path, icon, shot, theme};

fn main() -> eframe::Result {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("--write-icon") => {
            let p = PathBuf::from(args.get(2).map(String::as_str).unwrap_or("icon.png"));
            let n = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(1024);
            icon::write_png(&p, n).expect("icon");
            return Ok(());
        }
        Some("--shot") => {
            if let Err(e) = shot::run(&args[1..]) {
                eprintln!("Fehler: {e:#}");
                std::process::exit(1);
            }
            return Ok(());
        }
        Some(a) if !a.starts_with("-psn") => {
            // never open a window for unknown arguments
            println!("Stimmwerk GUI\n  stimmwerk-gui                       Fenster öffnen\n  {}\n  stimmwerk-gui --write-icon OUT.png [PX]", shot::USAGE);
            std::process::exit(if a == "-h" || a == "--help" { 0 } else { 2 });
        }
        _ => {}
    }
    funkhaus_sys::fix_path();
    let app = App::new(Some(cfg_path()));
    let n = 256;
    let opts = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Stimmwerk")
            .with_inner_size([1280.0, 820.0])
            .with_min_inner_size([900.0, 560.0])
            .with_icon(egui::IconData { rgba: icon::rgba(n), width: n, height: n }),
        ..Default::default()
    };
    eframe::run_native("Stimmwerk", opts, Box::new(move |cc| {
        theme::apply(&cc.egui_ctx, theme::pref(&app.cfg.theme));
        Ok(Box::new(app))
    }))
}
