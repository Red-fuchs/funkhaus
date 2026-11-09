//! Headless screenshots of the real UI via egui_kittest + offscreen wgpu: no window, no focus change.
use crate::{App, tabs::Tab, theme};
use anyhow::{Context, Result, bail};
use eframe::egui;
use std::path::{Path, PathBuf};

pub const USAGE: &str = "stimmwerk-gui --shot OUT.png [--tab skript|stimmen|erzeugen|musik|export|einstellungen] [--project P] \
[--size 1280x820] [--theme system|hell|dunkel] [--scale 2] [--frames 6]";

/// Harness for `app` (also used by the kittest tests).
pub fn harness(app: App, size: egui::Vec2, scale: f32, pref: egui::ThemePreference) -> egui_kittest::Harness<'static, App> {
    let sys = match pref {
        egui::ThemePreference::Dark => egui::Theme::Dark,
        egui::ThemePreference::Light => egui::Theme::Light,
        egui::ThemePreference::System => if theme::system_is_dark() { egui::Theme::Dark } else { egui::Theme::Light },
    };
    // ~30 fps of simulated time (kittest's default 0.25 s/frame makes two consecutive clicks no double click)
    let hs = egui_kittest::Harness::builder().with_size(size).with_pixels_per_point(scale).with_theme(sys).with_step_dt(1.0 / 30.0).wgpu().build_eframe(move |cc| {
        theme::apply(&cc.egui_ctx, pref);
        app
    });
    hs.ctx.set_theme(pref); // the builder forces a concrete theme; restore the real preference
    hs
}

/// Steps until background jobs are done (max `secs`), then a few frames to settle.
pub fn settle(hs: &mut egui_kittest::Harness<'static, App>, secs: f64) {
    let t0 = std::time::Instant::now();
    hs.run_steps(2);
    while !(hs.state().jobs.is_empty() && hs.state().orphans.is_empty()) && t0.elapsed().as_secs_f64() < secs {
        std::thread::sleep(std::time::Duration::from_millis(30));
        hs.run_steps(1);
    }
    hs.run_steps(4);
}

pub fn save_png(hs: &mut egui_kittest::Harness<'static, App>, out: &Path) -> Result<(u32, u32)> {
    let img = hs.render().map_err(|e| anyhow::anyhow!("Rendern fehlgeschlagen: {e}"))?;
    if let Some(d) = out.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(d)?;
    }
    let f = std::io::BufWriter::new(std::fs::File::create(out)?);
    let mut enc = png::Encoder::new(f, img.width(), img.height());
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header()?.write_image_data(img.as_raw())?;
    Ok((img.width(), img.height()))
}

pub fn run(args: &[String]) -> Result<()> {
    let mut out = None;
    let (mut w, mut h, mut scale, mut frames) = (1280.0f32, 820.0f32, 1.0f32, 6usize);
    let (mut th, mut tab, mut proj) = (None::<String>, Tab::Skript, None::<String>);
    let mut it = args.iter().skip(1);
    while let Some(a) = it.next() {
        let mut val = || it.next().cloned().with_context(|| format!("{a}: Wert fehlt\n{USAGE}"));
        match a.as_str() {
            "--size" => {
                let v = val()?;
                let (a, b) = v.split_once(['x', 'X']).context("--size WxH")?;
                (w, h) = (a.parse()?, b.parse()?);
            }
            "--theme" => th = Some(val()?),
            "--tab" => {
                let v = val()?;
                tab = Tab::parse(&v).with_context(|| format!("--tab {v}?\n{USAGE}"))?;
            }
            "--project" => proj = Some(val()?),
            "--scale" => scale = val()?.parse()?,
            "--frames" => frames = val()?.parse()?,
            s if !s.starts_with("--") && out.is_none() => out = Some(PathBuf::from(s)),
            s => bail!("unbekannte Option {s}\n{USAGE}"),
        }
    }
    let out = out.context(USAGE)?;
    let app = App::new(None);
    let pref = theme::pref(th.as_deref().unwrap_or(&app.cfg.theme));
    let mut hs = harness(app, egui::vec2(w, h), scale, pref);
    if let Some(p) = proj {
        let d = stimmwerk_core::locate(&p);
        let ctx = hs.ctx.clone();
        hs.state_mut().open(&ctx, &d);
        if let Some((true, e)) = &hs.state().msg {
            bail!("{e}");
        }
    }
    hs.state_mut().tab = tab;
    hs.run_steps(frames);
    settle(&mut hs, 15.0);
    let (iw, ih) = save_png(&mut hs, &out)?;
    println!("{{\"ok\": true, \"png\": {:?}, \"width\": {iw}, \"height\": {ih}}}", out.display().to_string());
    Ok(())
}
