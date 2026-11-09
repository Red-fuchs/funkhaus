//! Export tab: preset, format, AI disclosure, export button, result (LUFS/TP/in spec), "Im Finder zeigen".
use super::card;
use crate::app::App;
use crate::jobs::JobKind;
use crate::theme;
use eframe::egui::{self, RichText};
use stimmwerk_core::export::{self, Kind, Loudness};

fn preset_name(l: Loudness) -> &'static str {
    match l {
        Loudness::Podcast => "Podcast (−16 LUFS)",
        Loudness::Hoerbuch => "Hörbuch (−19 LUFS)",
        Loudness::Video => "Video (−14 LUFS)",
    }
}

fn kind_name(k: Kind) -> &'static str {
    match k {
        Kind::Wav => "WAV",
        Kind::Mp3 => "MP3",
        Kind::Parts => "Einzeldateien",
    }
}

fn preset_word(l: Loudness) -> &'static str {
    match l {
        Loudness::Podcast => "Podcast",
        Loudness::Hoerbuch => "Hörbuch",
        Loudness::Video => "Video",
    }
}

/// (title, detail line, tooltip) of an export result (ui-strings-de.md `export.result.*`, without "Spec" jargon):
/// out of spec is no "fertig" – the file is written, but the loudness misses the target.
pub fn result_text(r: &export::Report) -> (String, String, String) {
    let f = r.file.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let what = preset_word(r.preset);
    let meas = format!("{} LUFS · Spitze {} dBTP", super::de1(r.measured.lufs), super::de1(r.measured.tp));
    let tip = format!(
        "LUFS = Lautheit (EBU R128), dBTP = höchste Spitze (True Peak).\nZiel {what}: {} LUFS ±0,5 und Spitze höchstens {} dBTP.",
        super::de1(r.target.lufs as f64),
        super::de1(r.target.tp as f64)
    );
    if r.in_spec {
        (format!("✔ Export fertig ({}): {f}", kind_name(r.kind)), format!("{} · Lautheit passt für {what} ({meas})", funkhaus_sys::mmss(r.duration)), tip)
    } else {
        (
            format!("⚠ Export geschrieben, aber Lautheit verfehlt ({}): {f}", kind_name(r.kind)),
            format!("{} · gemessen {meas} – Ziel {} LUFS ±0,5, Spitze ≤ {} dBTP. Die Datei ist trotzdem gespeichert; prüf die Lautheit oder exportiere erneut.", funkhaus_sys::mmss(r.duration), super::de1(r.target.lufs as f64), super::de1(r.target.tp as f64)),
            tip,
        )
    }
}

/// Warning when final takes are missing: export renders them in final, never uses the draft.
pub fn final_missing_text(n: usize) -> String {
    let z = if n == 1 { "1 Zeile".to_string() } else { format!("{n} Zeilen") };
    format!("⚠ Für {z} fehlt die Endfassung – „Exportieren“ rendert sie dabei mit (nie aus dem Entwurf).")
}

pub fn show(app: &mut App, ctx: &egui::Context) {
    egui::CentralPanel::default().show(ctx, |ui| {
        let Some(p) = app.proj.clone() else { return };
        let proj_ld: Option<Loudness> = p.meta.settings.loudness.parse().ok();
        ui.add_space(8.0);
        ui.label(RichText::new("Exportieren").size(18.0).strong());
        ui.add_space(8.0);
        card(ui, |ui| {
            egui::Grid::new("ex").num_columns(2).spacing([18.0, 12.0]).show(ui, |ui| {
                ui.label("Lautheit");
                let sel = app.ex.preset.map_or_else(|| format!("wie im Projekt: {}", proj_ld.map_or("?", preset_name)), |l| preset_name(l).into());
                egui::ComboBox::from_id_salt("preset").width(260.0).selected_text(sel).show_ui(ui, |ui| {
                    ui.selectable_value(&mut app.ex.preset, None, format!("wie im Projekt: {}", proj_ld.map_or("?", preset_name)));
                    for l in [Loudness::Podcast, Loudness::Hoerbuch, Loudness::Video] {
                        ui.selectable_value(&mut app.ex.preset, Some(l), preset_name(l));
                    }
                });
                ui.end_row();
                ui.label("Format");
                ui.horizontal(|ui| {
                    ui.radio_value(&mut app.ex.kind, Kind::Wav, "WAV");
                    ui.radio_value(&mut app.ex.kind, Kind::Mp3, "MP3");
                    ui.radio_value(&mut app.ex.kind, Kind::Parts, "Einzeldateien + Manifest").on_hover_text("Eine WAV je Zeile (parts/NN-<wer>.wav) plus manifest.json mit Zeiten (z. B. für Videoschnitt) und die Gesamt-WAV");
                });
                ui.end_row();
                ui.label("KI-Hinweis");
                let on = app.ex.disclosure.unwrap_or(p.meta.settings.disclosure);
                let sel = match app.ex.disclosure {
                    None => format!("wie im Projekt: {}", if p.meta.settings.disclosure { "gesprochen" } else { "nicht gesprochen" }),
                    Some(true) => "gesprochen".into(),
                    Some(false) => "nicht gesprochen".into(),
                };
                egui::ComboBox::from_id_salt("disc").width(260.0).selected_text(sel).show_ui(ui, |ui| {
                    ui.selectable_value(&mut app.ex.disclosure, None, "wie im Projekt");
                    ui.selectable_value(&mut app.ex.disclosure, Some(true), "gesprochen");
                    ui.selectable_value(&mut app.ex.disclosure, Some(false), "nicht gesprochen");
                });
                ui.end_row();
                ui.label("");
                ui.add_space(2.0);
                let txt = p.meta.disclosure_text().unwrap_or_default();
                let note = if on { format!("Am Ende gesprochen: „{txt}“") } else { "Nicht gesprochen; die Datei wird trotzdem als KI-erzeugt markiert.".into() };
                ui.add(egui::Label::new(RichText::new(note).weak()).wrap());
                ui.end_row();
            });
            ui.add_space(10.0);
            let errs = app.lint.as_ref().map_or(0, |l| l.errors);
            let busy = app.busy();
            ui.horizontal(|ui| {
                let tip = if errs > 0 { "Erst die Fehler im Skript beheben" } else { "Mix mit diesen Einstellungen erzeugen (rendert fehlende Zeilen)" };
                if ui.add_enabled(errs == 0 && !busy, egui::Button::new(RichText::new("Exportieren").strong())).on_hover_text(tip).on_disabled_hover_text(tip).clicked() {
                    app.start(ctx, JobKind::Export);
                }
                if app.running(&JobKind::Export) {
                    ui.spinner();
                    ui.label("exportiert …");
                }
                ui.label(RichText::new(format!("speichert nach {}", super::tilde(&p.dir.join("out")))).weak());
            });
            // export is always the final quality; say so when final takes are missing (never a silent draft export)
            let miss = app.final_missing();
            if miss > 0 {
                ui.add_space(6.0);
                ui.add(egui::Label::new(RichText::new(final_missing_text(miss)).color(theme::pal(ui).warn)).wrap());
                ui.horizontal(|ui| {
                    let tip = "Fehlende Zeilen jetzt in voller Qualität sprechen (Reiter Skript zeigt den Fortschritt)";
                    if ui.add_enabled(!busy && errs == 0, egui::Button::new("Endfassung rendern")).on_hover_text(tip).clicked() {
                        app.render_final(ctx);
                        app.tab = crate::tabs::Tab::Skript;
                    }
                });
            } else {
                ui.label(RichText::new("Exportiert wird immer die Endfassung (volle Qualität).").weak());
            }
        });
        ui.add_space(12.0);
        if let Some(r) = app.ex.res.take() {
            card(ui, |ui| {
                let pal = theme::pal(ui);
                let (title, detail, tip) = result_text(&r);
                ui.add(egui::Label::new(RichText::new(title).color(if r.in_spec { pal.ok } else { pal.warn }).strong()).wrap()).on_hover_text(&tip);
                ui.add(egui::Label::new(detail).wrap()).on_hover_text(&tip);
                ui.add_space(4.0);
                let ki = match (r.ai.disclosed, r.ai.id3) {
                    (true, true) => "gesprochen und getaggt",
                    (true, false) => "gesprochen (WAV-Kommentar)",
                    (false, true) => "nur getaggt",
                    (false, false) => "nur WAV-Kommentar",
                };
                ui.label(format!("KI-Kennzeichnung: {ki}"));
                if let Some(m) = &r.manifest {
                    ui.label(format!("{} Einzeldateien · Manifest {}", r.parts, super::tilde(m)));
                }
                for w in &r.warnings {
                    ui.add(egui::Label::new(RichText::new(format!("⚠ Musik: {w}")).color(pal.warn)).wrap()).on_hover_text("Cue im Reiter Musik anpassen (Zeile, Loop oder Datei kürzen) und erneut exportieren");
                }
                ui.label(RichText::new(format!("Gespeichert: {}", super::tilde(&r.file))).weak());
                ui.horizontal(|ui| {
                    if ui.button("Im Finder zeigen").clicked() {
                        let _ = std::process::Command::new("open").arg("-R").arg(&r.file).spawn();
                    }
                    let playing = app.player.playing().is_some_and(|f| f == r.file);
                    if ui.button(if playing { "■ Stopp" } else { "▶ Anhören" }).clicked() {
                        if playing { app.player.stop() } else { app.play(None, &r.file) }
                    }
                });
            });
            app.ex.res = Some(r);
        }
    });
}
