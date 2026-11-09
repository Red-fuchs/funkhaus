//! Headless screenshots of the real UI via egui_kittest + offscreen wgpu: no window, no focus change.
use super::{App, View, theme};
use anyhow::{Context, Result, bail};
use eframe::egui;
use funkhaus_core::{Store, podcast};
use std::path::PathBuf;

pub const USAGE: &str = "funkhaus-gui --shot OUT.png [--size 1280x820] [--theme system|hell|dunkel] [--nb ID] \
[--view start|notebook|script|source|guide] [--scale 2] [--frames 6] [--close-at N]";

pub fn run(args: &[String]) -> Result<()> {
    let mut out = None;
    let (mut w, mut h, mut scale, mut frames) = (1280.0f32, 820.0f32, 1.0f32, 6usize);
    let (mut th, mut nb, mut view) = (None::<String>, None::<String>, "start".to_string());
    let mut close_at = None;
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
            "--nb" => nb = Some(val()?),
            "--view" => view = val()?,
            "--scale" => scale = val()?.parse()?,
            "--frames" => frames = val()?.parse()?,
            "--close-at" => close_at = Some(val()?.parse::<usize>()?),
            s if !s.starts_with("--") && out.is_none() => out = Some(PathBuf::from(s)),
            s => bail!("unbekannte Option {s}\n{USAGE}"),
        }
    }
    let out = out.context(USAGE)?;
    let st = Store::open()?;
    let pref = theme::pref(th.as_deref().unwrap_or(&st.cfg.theme));
    let sys = if theme::system_is_dark() { egui::Theme::Dark } else { egui::Theme::Light };

    let mut app = App::new(st);
    if view != "start" {
        let n = match &nb {
            Some(k) => app.st.get(k)?,
            None => app.nbs.first().cloned().context("kein Notebook vorhanden")?,
        };
        app.open(n);
    }
    if let Some(c) = &app.cur {
        match view.as_str() {
            "script" => {
                let p = podcast::list(&app.st, &c.nb).into_iter().next().context("kein Podcast im Notebook")?;
                let host = (app.st.cfg.hosts.o.name.clone(), app.st.cfg.hosts.f.name.clone());
                let text = super::script_text(&std::fs::read_to_string(&p.script).unwrap_or_default(), &host);
                app.view = Some(View::new(format!("Skript · {}", p.title), text, Some(p.script)));
            }
            "source" => {
                let s = c.nb.sources.first().context("keine Quelle im Notebook")?;
                let text = app.st.text(&c.nb, &s.id).unwrap_or_default();
                app.view = Some(View::new(s.title.clone(), text, Some(s.origin.clone().into())));
            }
            "guide" => {
                let s = c.nb.sources.first().context("keine Quelle im Notebook")?;
                let g = c.kart.guides.get(&s.id).context("keine Quellenübersicht – erst `funkhaus guide`")?;
                app.view = Some(View { title: format!("Quellenübersicht · {}", s.title), text: super::guide_text(g), path: None, questions: g.suggested_questions.clone() });
            }
            "notebook" => {}
            v => bail!("--view {v}? start|notebook|script|source|guide"),
        }
    }

    let mut hs = egui_kittest::Harness::builder()
        .with_size(egui::vec2(w, h))
        .with_pixels_per_point(scale)
        .with_theme(sys)
        .wgpu()
        .build_eframe(move |cc| {
            theme::apply(&cc.egui_ctx, pref);
            app
        });
    hs.ctx.set_theme(pref); // the builder forces a concrete theme; restore the real preference
    match close_at {
        // user flow: viewer open for N frames, then closed, then the rest
        Some(n) if n < frames => {
            hs.run_steps(n);
            hs.state_mut().view = None;
            hs.run_steps(frames - n);
        }
        _ => hs.run_steps(frames),
    }
    let wd = hs.state().widths;
    let img = hs.render().map_err(|e| anyhow::anyhow!("Rendern fehlgeschlagen: {e}"))?;
    let f = std::io::BufWriter::new(std::fs::File::create(&out)?);
    let mut enc = png::Encoder::new(f, img.width(), img.height());
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header()?.write_image_data(img.as_raw())?;
    println!(
        "{{\"ok\": true, \"png\": {:?}, \"width\": {}, \"height\": {}, \"panels\": [{:.0}, {:.0}, {:.0}]}}",
        out.display().to_string(), img.width(), img.height(), wd[0], wd[1], wd[2]
    );
    Ok(())
}
