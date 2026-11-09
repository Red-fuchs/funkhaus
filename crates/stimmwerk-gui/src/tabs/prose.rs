//! "Aus Prosa erstellen …" (Skript tab): file or pasted text -> LLM job -> cast/script preview -> confirm replaces script.txt.
use super::{chip, llm};
use crate::app::App;
use crate::jobs::JobKind;
use crate::theme;
use eframe::egui::{self, RichText};

pub const INTRO: &str = "Ein Sprachmodell macht aus Erzähltext ein Rollenskript: Erzähler plus Figuren mit wörtlicher Rede. Der Wortlaut bleibt, nichts wird erfunden. Geschrieben wird erst, wenn du die Vorschau übernimmst.";

pub fn dialog(app: &mut App, ctx: &egui::Context) {
    let mut run = false;
    let mut apply = false;
    let mut close = false;
    let running = app.running(&JobKind::Prose);
    let n_old = app.n_lines();
    let r = egui::Modal::new(egui::Id::new("prose")).show(ctx, |ui| {
        ui.set_width(640.0);
        ui.label(RichText::new("Rollenskript aus Prosa").size(18.0).strong());
        ui.label(RichText::new(INTRO).weak());
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.label("Datei");
            ui.add(egui::TextEdit::singleline(&mut app.prose.path).desired_width(380.0).hint_text("Text- oder Markdown-Datei"));
            if ui.button("Datei wählen …").clicked()
                && let Some(p) = rfd::FileDialog::new().add_filter("Text", &["txt", "md", "markdown"]).pick_file()
            {
                app.prose.path = p.display().to_string();
            }
        });
        ui.label(RichText::new("oder Text einfügen:").weak());
        ui.add(egui::TextEdit::multiline(&mut app.prose.text).desired_rows(5).desired_width(f32::INFINITY).hint_text("Kapitel hier einfügen …"));
        ui.add_space(6.0);
        llm::project_row(app, ui);
        if let Some(e) = &app.prose.err {
            ui.add(egui::Label::new(RichText::new(e).color(theme::pal(ui).err)).wrap());
        }
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            let has = !app.prose.path.trim().is_empty() || !app.prose.text.trim().is_empty();
            run = ui.add_enabled(has && !running, egui::Button::new(RichText::new("Rollenskript erzeugen").strong())).on_hover_text("Text ans Sprachmodell schicken; das Ergebnis siehst du erst als Vorschau").on_disabled_hover_text("Erst eine Datei wählen oder Text einfügen").clicked();
            if running {
                ui.spinner();
                let who = app.resolved().map(|r| r.name).unwrap_or_default();
                ui.label(RichText::new(format!("fragt {who} …")).weak());
            }
            if ui.button(if app.prose.res.is_some() { "Verwerfen" } else { "Schließen" }).on_hover_text("Nichts wird geschrieben").clicked() {
                close = true;
            }
        });
        if let Some(o) = app.prose.res.as_deref() {
            ui.add_space(10.0);
            ui.separator();
            ui.label(RichText::new(format!("Vorschau: {} Zeilen · {} Rollen{}", o.lines, o.cast.len(), if o.retried { " · nach einer Korrektur" } else { "" })).strong());
            if !o.merged.is_empty() {
                ui.label(RichText::new(format!("Hinweis: {} kommen im Text nicht vor – ihre Zeilen spricht der Erzähler.", o.merged.join(", "))).weak()).on_hover_text("Das Sprachmodell hat Rollen erfunden, deren Name im Text fehlt (z. B. „Lautsprecher“); solche Zeilen gehen an den Erzähler.");
            }
            egui::Grid::new("cast-prev").num_columns(3).spacing([12.0, 4.0]).show(ui, |ui| {
                for (k, c) in o.cast.iter().enumerate() {
                    let (f, fg) = theme::speaker(ui, k);
                    chip(ui, &c.id, f, fg, 60.0);
                    ui.label(&c.name);
                    ui.label(RichText::new(super::clip_chars(&format!("{} · Stimme: {}", c.description, c.suggested_voice_instruct), 70)).weak()).on_hover_text(format!("{}\nStimm-Vorschlag: {}", c.description, c.suggested_voice_instruct));
                    ui.end_row();
                }
            });
            ui.add_space(4.0);
            egui::ScrollArea::vertical().max_height(160.0).show(ui, |ui| {
                ui.label(RichText::new(o.script.lines().take(12).collect::<Vec<_>>().join("\n")).monospace().weak());
                if o.lines > 12 {
                    ui.label(RichText::new(format!("… und {} weitere Zeilen", o.lines - 12)).weak());
                }
            });
            ui.add_space(8.0);
            let (label, tip) = if n_old > 0 { (format!("Vorhandenes Skript ersetzen ({n_old} Zeilen)"), "Ersetzt script.txt; die Besetzung bleibt, neue Rollen kommen dazu") } else { ("Skript übernehmen".to_string(), "Schreibt script.txt und legt die Rollen als Sprecher an") };
            let c = if n_old > 0 { theme::pal(ui).err } else { theme::pal(ui).ok };
            apply = ui.button(RichText::new(label).color(c).strong()).on_hover_text(tip).clicked();
        }
    });
    if run {
        app.prose_run(ctx);
    }
    if apply {
        app.prose_apply(ctx);
    }
    if close || (r.should_close() && !run && !apply) {
        (app.prose.open, app.prose.res, app.prose.err) = (false, None, None);
    }
}
