//! "Stimme erzeugen" tab: Entwerfen (preset/instruct -> 3 candidates, ▶, Behalten) and Klonen (WAV, cut, ref text,
//! consent dialog per ui-strings-de.md `consent.*`).
use super::card;
use crate::app::{App, CONSENT_BODY, CONSENT_STATEMENT};
use crate::jobs::JobKind;
use crate::theme;
use eframe::egui::{self, RichText};
use stimmwerk_core::voice;

pub fn show(app: &mut App, ctx: &egui::Context) {
    egui::CentralPanel::default().show(ctx, |ui| {
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.add_space(8.0);
            design(app, ui);
            ui.add_space(14.0);
            clone(app, ui);
        });
    });
    if app.generate.consent_open {
        consent(app, ctx);
    }
}

fn design(app: &mut App, ui: &mut egui::Ui) {
    ui.label(RichText::new("Entwerfen").size(18.0).strong());
    ui.label(RichText::new("Beschreibe eine Stimme – du bekommst drei Kandidaten (verschiedene Seeds) zum Vergleichen. Erst „Behalten“ legt eine Stimme an.").weak());
    ui.add_space(6.0);
    let running = app.running(&JobKind::Design);
    let mut go = false;
    card(ui, |ui| {
        egui::Grid::new("design").num_columns(2).spacing([16.0, 10.0]).show(ui, |ui| {
            let g = &mut app.generate;
            ui.label("Vorlage");
            let sel = g.preset.map_or("– eigene Beschreibung –", |k| voice::PRESETS[k].0);
            let mut pick = g.preset;
            egui::ComboBox::from_id_salt("preset").width(300.0).selected_text(sel).show_ui(ui, |ui| {
                for (k, (label, ins)) in voice::PRESETS.iter().enumerate() {
                    ui.selectable_value(&mut pick, Some(k), *label).on_hover_text(*ins);
                }
            });
            if pick != g.preset {
                g.preset = pick;
                if let Some(k) = pick {
                    g.instruct = voice::PRESETS[k].1.into();
                }
            }
            ui.end_row();
            ui.label("Beschreibung");
            let r = ui.add(egui::TextEdit::singleline(&mut g.instruct).desired_width(420.0).hint_text("z. B. female, middle-aged, low pitch"));
            if r.changed() {
                g.preset = voice::PRESETS.iter().position(|(_, i)| *i == g.instruct.trim());
            }
            ui.end_row();
            ui.label("");
            ui.label(RichText::new("Englische Begriffe: male/female · child, teenager, young adult, middle-aged, elderly · very low … very high pitch · whisper").weak());
            ui.end_row();
            ui.label("Probetext");
            ui.add(egui::TextEdit::singleline(&mut g.text).desired_width(420.0));
            ui.end_row();
        });
        let warn = voice::check_instruct(&app.generate.instruct);
        if let Some(w) = warn.first() {
            ui.label(RichText::new(format!("⚠ {w}")).color(theme::pal(ui).warn));
        }
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            let ok = !app.generate.instruct.trim().is_empty() && !app.generate.text.trim().is_empty() && !running;
            let tip = "Aus einer Beschreibung drei Stimm-Kandidaten erzeugen; gespeichert wird erst mit „Behalten“";
            go = ui.add_enabled(ok, egui::Button::new(RichText::new("Entwerfen").strong())).on_hover_text(tip).clicked();
            if let Some(j) = app.jobs.iter().find(|j| j.kind == JobKind::Design) {
                ui.spinner();
                let t = if j.gpu_wait { "wartet auf GPU … (ein anderes Programm rechnet gerade)".into() } else { format!("Kandidat {} von {} …", j.cur.unwrap_or(1), app.generate.n) };
                ui.label(RichText::new(t).weak());
                if ui.button("Abbrechen").clicked() {
                    j.cancel();
                }
            }
        });
    });
    if go {
        app.design(ui.ctx());
    }
    let Some(b) = app.generate.batch.clone() else { return };
    ui.add_space(8.0);
    ui.label(RichText::new(format!("Kandidaten · {}", b.instruct)).strong());
    let mut act: Option<(usize, String)> = None;
    let mut play = None;
    let playing = app.player.playing().map(std::path::Path::to_path_buf);
    ui.horizontal_top(|ui| {
        let w = ((ui.available_width() - 24.0) / b.candidates.len().max(1) as f32).min(330.0);
        for c in &b.candidates {
            ui.allocate_ui(egui::vec2(w, 150.0), |ui| {
                egui::Frame::new().fill(theme::pal(ui).card).stroke(ui.visuals().widgets.noninteractive.bg_stroke).corner_radius(8.0).inner_margin(12.0).show(ui, |ui| {
                    ui.set_width(w - 26.0);
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(format!("Kandidat {}", c.id)).strong());
                        ui.label(RichText::new(format!("Seed {} · {} s", c.seed, super::de1(c.dur))).weak());
                    });
                    let on = playing.as_deref() == Some(c.wav.as_path());
                    if ui.button(if on { "■ Stopp" } else { "▶ Anhören" }).clicked() {
                        play = Some((on, c.wav.clone()));
                    }
                    let k = c.id - 1;
                    match app.generate.kept.get(k).cloned().flatten() {
                        Some(n) => {
                            ui.label(RichText::new(format!("✔ behalten als {n}")).color(theme::pal(ui).ok));
                        }
                        None => {
                            ui.horizontal(|ui| {
                                if let Some(name) = app.generate.names.get_mut(k) {
                                    ui.add(egui::TextEdit::singleline(name).desired_width(120.0).hint_text("name"));
                                    let ok = funkhaus_voice::persona::valid_name(name.trim());
                                    let r = ui.add_enabled(ok, egui::Button::new("Behalten"));
                                    if r.on_hover_text("Diesen Kandidaten als eigene Stimme speichern").on_disabled_hover_text("Name: a bis z, 0 bis 9, Bindestrich").clicked() {
                                        act = Some((c.id, name.trim().to_string()));
                                    }
                                }
                            });
                        }
                    }
                });
            });
        }
    });
    if let Some((on, w)) = play {
        if on {
            app.player.stop();
        } else {
            app.play(None, &w);
        }
    }
    if let Some((id, n)) = act {
        app.keep(ui.ctx(), id, &n);
    }
}

fn clone(app: &mut App, ui: &mut egui::Ui) {
    ui.label(RichText::new("Klonen").size(18.0).strong());
    ui.label(RichText::new("Stimme aus einer kurzen Aufnahme (3 bis 10 s, sauber, eine Person) – nur mit Einwilligung der Person.").weak());
    ui.add_space(6.0);
    let mut open = false;
    let mut probe = None;
    card(ui, |ui| {
        let g = &mut app.generate;
        egui::Grid::new("clone").num_columns(2).spacing([16.0, 10.0]).show(ui, |ui| {
            ui.label("Aufnahme (WAV)");
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut g.wav).desired_width(360.0).hint_text("Pfad zur WAV-Datei"));
                if ui.button("WAV wählen …").clicked()
                    && let Some(p) = rfd::FileDialog::new().add_filter("WAV", &["wav", "WAV"]).pick_file()
                {
                    g.wav = p.display().to_string();
                }
            });
            ui.end_row();
            ui.label("Ausschnitt");
            ui.horizontal(|ui| {
                ui.label("von");
                ui.add(egui::TextEdit::singleline(&mut g.start).desired_width(56.0).hint_text("0"));
                ui.label("bis");
                ui.add(egui::TextEdit::singleline(&mut g.end).desired_width(56.0).hint_text("Ende"));
                ui.label(RichText::new("Sekunden (leer = ganze Datei)").weak());
            });
            ui.end_row();
            ui.label("Gesprochener Text");
            ui.add(egui::TextEdit::multiline(&mut g.ref_text).desired_rows(2).desired_width(420.0).hint_text("genau das, was im Ausschnitt gesagt wird"));
            ui.end_row();
            ui.label("Name der Stimme");
            ui.add(egui::TextEdit::singleline(&mut g.name).desired_width(160.0).hint_text("a-z, 0-9, -"));
            ui.end_row();
        });
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            let ok = !g.wav.trim().is_empty() && !g.ref_text.trim().is_empty() && funkhaus_voice::persona::valid_name(g.name.trim());
            let r = ui.add_enabled(ok, egui::Button::new(RichText::new("Klonen …").strong()));
            open = r.on_hover_text("Stimme aus einer kurzen Aufnahme klonen – nur mit Einwilligung der Person (siehe Dialog)").on_disabled_hover_text("Aufnahme, gesprochenen Text und Namen ausfüllen").clicked();
            if let Some(n) = g.cloned.clone() {
                ui.label(RichText::new(format!("✔ {n} gespeichert")).color(theme::pal(ui).ok));
                if ui.button(format!("▶ Probe {n}")).on_hover_text("Kurze Probe mit dem Beispieltext aus dem Stimmen-Reiter").clicked() {
                    probe = Some(n);
                }
            }
        });
    });
    if open {
        app.generate.consent_open = true;
    }
    if let Some(n) = probe {
        app.probe(ui.ctx(), &n);
    }
}

/// Modal consent dialog; confirm only with a name and the ticked statement.
fn consent(app: &mut App, ctx: &egui::Context) {
    let mut go = false;
    let r = egui::Modal::new(egui::Id::new("consent")).show(ctx, |ui| {
        ui.set_width(520.0);
        let g = &mut app.generate;
        ui.label(RichText::new("Stimme klonen").size(18.0).strong());
        ui.add_space(6.0);
        ui.label(CONSENT_BODY);
        ui.add_space(8.0);
        ui.label("Wer hat eingewilligt?");
        ui.add(egui::TextEdit::singleline(&mut g.who).desired_width(f32::INFINITY).hint_text("Name der Person"));
        ui.label("Wie? (optional)");
        ui.add(egui::TextEdit::singleline(&mut g.how).desired_width(f32::INFINITY).hint_text("z. B. schriftlich per Mail"));
        ui.label("Datum der Einwilligung");
        ui.add(egui::TextEdit::singleline(&mut g.consent_date).desired_width(120.0).hint_text("JJJJ-MM-TT"));
        ui.add_space(4.0);
        ui.checkbox(&mut g.agreed, CONSENT_STATEMENT);
        if let Some((true, e)) = &app.msg {
            ui.label(RichText::new(e).color(theme::pal(ui).err));
        }
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            let date_ok = voice::check_date(&g.consent_date).is_ok();
            if !g.consent_date.trim().is_empty() && !date_ok {
                ui.label(RichText::new("Datum als JJJJ-MM-TT, nicht in der Zukunft").color(theme::pal(ui).err));
            }
            let ok = g.agreed && !g.who.trim().is_empty() && date_ok;
            go = ui.add_enabled(ok, egui::Button::new(RichText::new("Einwilligung bestätigen und klonen").strong())).on_disabled_hover_text("Nur aktiv, wenn Name, Datum (JJJJ-MM-TT, nicht in der Zukunft) und Einwilligung ausgefüllt sind").clicked();
            if ui.button("Abbrechen").on_hover_text("Nichts wird geschrieben").clicked() {
                g.consent_open = false;
            }
        });
    });
    if r.should_close() && !go {
        app.generate.consent_open = false;
    }
    if go {
        app.clone_voice(ctx);
    }
}
