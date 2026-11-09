//! Einstellungen: theme, data dir, omnivoice status (+ ask again), version; KI-Anbieter (tabs/llm.rs) + project backend.
use super::{card, llm};
use crate::app::App;
use crate::theme;
use eframe::egui::{self, RichText, ThemePreference};

pub fn show(app: &mut App, ctx: &egui::Context) {
    egui::CentralPanel::default().show(ctx, |ui| {
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.add_space(8.0);
            ui.label(RichText::new("Einstellungen").size(18.0).strong());
            ui.add_space(8.0);
            card(ui, |ui| {
                egui::Grid::new("einst").num_columns(2).spacing([18.0, 12.0]).show(ui, |ui| {
                    ui.label("Darstellung");
                    let mut p = theme::pref(&app.cfg.theme);
                    ui.horizontal(|ui| {
                        ui.selectable_value(&mut p, ThemePreference::System, "System");
                        ui.selectable_value(&mut p, ThemePreference::Light, "Hell");
                        ui.selectable_value(&mut p, ThemePreference::Dark, "Dunkel");
                    });
                    if p != theme::pref(&app.cfg.theme) {
                        app.cfg.theme = theme::name(p).into();
                        app.save_cfg();
                        ctx.set_theme(p);
                    }
                    ui.end_row();
                    ui.label("Datenordner");
                    ui.label(super::tilde(&stimmwerk_core::home()));
                    ui.end_row();
                    ui.label("omnivoice");
                    let pal = theme::pal(ui);
                    match &app.bi {
                        Some(b) => ui.label(RichText::new(format!("erreichbar · {} Standard-Stimmen", b.len())).color(pal.ok)),
                        None if app.lint.is_some() => ui.label(RichText::new("nicht erreichbar").color(pal.err)),
                        None => ui.label("wird abgefragt …"),
                    };
                    ui.end_row();
                    ui.label("");
                    if ui.add_enabled(!app.running(&crate::jobs::JobKind::Lint), egui::Button::new("⟳ Erneut prüfen")).on_hover_text("omnivoice neu nach seinen Stimmen fragen (z. B. nach Installation oder Start)").clicked() {
                        app.relint_with(ctx, true);
                    }
                    ui.end_row();
                    ui.label("Version");
                    ui.label(env!("CARGO_PKG_VERSION"));
                    ui.end_row();
                });
            });
            ui.add_space(12.0);
            llm::card_ui(app, ui);
            ui.label(RichText::new(llm::settings_hint()).weak());
            if app.proj.is_some() {
                ui.add_space(12.0);
                card(ui, |ui| {
                    ui.label(RichText::new("Dieses Projekt").strong());
                    ui.label(RichText::new("Welcher Anbieter Rollenskript und Kürzungen für dieses Projekt macht – und ob sein Text online gehen darf.").weak());
                    ui.add_space(4.0);
                    llm::project_row(app, ui);
                });
            }
        })
    });
    llm::dialogs(app, ctx);
}
