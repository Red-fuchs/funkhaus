//! Stimmen tab: speaker -> persona (persona files + omnivoice builtins), kind chip, "Vorhören".
use super::{card, chip};
use crate::app::App;
use crate::jobs::JobKind;
use crate::theme;
use eframe::egui::{self, RichText};
use stimmwerk_core::{lint, speakers, voice};

fn kind_text(k: &str) -> &'static str {
    match k {
        "builtin" | "implicit" => "Standard-Stimme",
        "design" => "entworfen",
        "clone" => "geklont",
        "unset" => "keine Stimme",
        _ => "Fehler",
    }
}

/// Choices for the dropdown: persona files, then omnivoice builtins, plus the current value.
pub fn choices(app: &App, cur: Option<&str>) -> Vec<String> {
    let mut v = app.personas.clone();
    v.extend(app.bi.iter().flatten().filter(|b| !app.personas.contains(b)).cloned());
    if let Some(c) = cur.filter(|c| !v.iter().any(|x| x == c)) {
        v.insert(0, c.to_string());
    }
    v
}

pub fn show(app: &mut App, ctx: &egui::Context) {
    egui::CentralPanel::default().show(ctx, |ui| egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        let Some(p) = app.proj.clone() else { return };
        ui.add_space(8.0);
        ui.label(RichText::new("Wer spricht mit welcher Stimme?").size(18.0).strong());
        ui.label(RichText::new("Jeder Sprecher im Skript bekommt eine Stimme: eine eigene (entworfen oder geklont) oder eine Standard-Stimme von omnivoice.").weak());
        ui.add_space(8.0);
        let infos: Vec<lint::SpeakerInfo> = app.lint.as_ref().map(|l| l.speakers.clone()).unwrap_or_default();
        let busy = app.busy();
        let mut set = None;
        let mut probe = None;
        card(ui, |ui| {
            egui::Grid::new("cast").num_columns(5).spacing([16.0, 10.0]).show(ui, |ui| {
                for h in ["Sprecher", "Stimme", "Art", "Zeilen", ""] {
                    ui.label(RichText::new(h).weak());
                }
                ui.end_row();
                // cast first, then speakers the script uses but the project does not know yet (assigning a voice adds them)
                let mut rows: Vec<(String, Option<String>, Option<String>)> = p.meta.speakers.iter().map(|(id, s)| (id.clone(), s.label.clone(), s.persona.clone())).collect();
                for l in p.script.lines() {
                    if !rows.iter().any(|r| r.0 == l.who) {
                        rows.push((l.who.clone(), None, None));
                    }
                }
                for (ci, (id, label, persona)) in rows.iter().enumerate() {
                    let id = id.clone();
                    let known = p.meta.speakers.contains_key(&id);
                    let (f, fg) = theme::speaker(ui, if known { ci } else { 7 });
                    ui.horizontal(|ui| {
                        chip(ui, &id, f, fg, 60.0);
                        if let Some(l) = label {
                            ui.label(l);
                        }
                    });
                    let cur = persona.clone();
                    let mut pick = cur.clone();
                    let shown = cur.as_deref().map_or_else(|| "– Stimme wählen –".to_string(), |c| super::voice_label(app, c).0);
                    ui.add_enabled_ui(!busy, |ui| {
                        let r = egui::ComboBox::from_id_salt(("persona", &id)).width(220.0).selected_text(shown).show_ui(ui, |ui| {
                            for c in choices(app, cur.as_deref()) {
                                let (lbl, tip) = super::voice_label(app, &c);
                                let lbl = if app.personas.contains(&c) { format!("{lbl}  · eigene") } else { lbl };
                                ui.selectable_value(&mut pick, Some(c), lbl).on_hover_text(tip);
                            }
                        });
                        if let Some(c) = &cur {
                            r.response.on_hover_text(super::voice_label(app, c).1);
                        }
                    });
                    if pick != cur {
                        set = pick.map(|n| (id.clone(), n));
                    }
                    let info = infos.iter().find(|i| i.id == id);
                    let k = info.map_or(if cur.is_some() { "implicit" } else { "unset" }, |i| i.kind);
                    let pal = theme::pal(ui);
                    let c = match k {
                        "error" | "unset" => pal.err,
                        _ => ui.visuals().text_color(),
                    };
                    ui.label(RichText::new(kind_text(k)).color(c));
                    ui.label(p.script.lines().filter(|l| l.who == id).count().to_string());
                    let name = cur.clone().unwrap_or_default();
                    let running = app.running(&JobKind::Probe(name.clone()));
                    let r = ui.add_enabled(cur.is_some() && !running && k != "error", egui::Button::new(if running { "… lädt" } else { "▶ Vorhören" }));
                    if r.on_hover_text("Kurze Probe der Stimme mit dem Beispieltext (Entwurfsqualität, wird gecacht)").clicked() {
                        probe = Some(name);
                    }
                    ui.end_row();
                }
            });
        });
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            ui.label("Beispieltext:");
            ui.add(egui::TextEdit::singleline(&mut app.probe_text).desired_width(420.0));
            if app.player.playing().is_some() && app.playing.is_none() && ui.button("■ Stopp").clicked() {
                app.player.stop();
            }
        });
        ui.add_space(6.0);
        let st = match &app.bi {
            Some(b) => format!("omnivoice: {} Standard-Stimmen verfügbar", b.len()),
            None if app.lint.is_some() => "omnivoice nicht erreichbar: Standard-Stimmen werden nicht geprüft".into(),
            None => "omnivoice wird abgefragt …".into(),
        };
        ui.label(RichText::new(st).weak());
        if let Some((id, n)) = set
            && let Some(p) = app.proj.as_mut() {
                let r = speakers::set(p, &app.dirs, app.bi.as_deref(), &id, &n, None).and_then(|w| p.save_meta().map(|_| w));
                match r {
                    Ok(Some(w)) => app.msg = Some((false, w)),
                    Ok(None) => app.msg = Some((false, format!("{id} spricht jetzt mit {}.", super::voice_label(app, &n).0))),
                    Err(e) => app.err(format!("{e:#}")),
                }
                app.stamp_toml(); // our own project.toml write: no reload for it, an external script.txt edit still reloads
                app.relint(ctx);
            }
        ui.add_space(14.0);
        if let Some(n) = library(app, ui) {
            probe = Some(n);
        }
        if let Some(n) = probe {
            app.probe(ctx, &n);
        }
    }));
    if app.generate.revoke_ask.is_some() {
        revoke_dialog(app, ctx);
    }
    if app.generate.rm_ask.is_some() {
        rm_dialog(app, ctx);
    }
}

/// Herkunft column: instruct/seed for designs, consent for clones.
pub fn origin(e: &voice::Entry) -> String {
    match e.kind.as_str() {
        "design" => {
            let ins = e.provenance.as_ref().and_then(|p| p.instruct.clone()).or_else(|| e.instruct.clone()).unwrap_or_default();
            let seed = e.seed.map(|s| format!(" · Seed {s}")).unwrap_or_default();
            format!("entworfen: {ins}{seed}{}", if e.anchored { " · verankert" } else { "" })
        }
        "clone" => format!("Einwilligung: {} · {}", e.consent_who.as_deref().unwrap_or("?"), e.consent_date.as_deref().unwrap_or("?")),
        "builtin" => format!("omnivoice-Stimme {}", e.name),
        _ => e.error.clone().unwrap_or_default(),
    }
}

/// Persona files (own + funkhaus) with kind, origin, state, probe and revoke. Returns a persona to probe.
fn library(app: &mut App, ui: &mut egui::Ui) -> Option<String> {
    ui.label(RichText::new("Deine Stimmen").size(18.0).strong());
    ui.label(RichText::new("Entworfene und geklonte Stimmen (neue legst du unter „Stimme erzeugen“ an).").weak());
    ui.add_space(6.0);
    let mut probe = None;
    if app.lib.is_empty() {
        ui.label(RichText::new("Noch keine eigenen Stimmen.").weak());
        return None;
    }
    let lib = app.lib.clone();
    card(ui, |ui| {
        egui::Grid::new("lib").num_columns(5).spacing([16.0, 8.0]).show(ui, |ui| {
            for h in ["Stimme", "Art", "Herkunft", "Status", ""] {
                ui.label(RichText::new(h).weak());
            }
            ui.end_row();
            for e in &lib {
                let pal = theme::pal(ui);
                let (lbl, tip) = super::voice_label(app, &e.name);
                ui.label(RichText::new(lbl).strong()).on_hover_text(format!("{tip}\n{}", super::tilde(&e.file)));
                ui.label(kind_text(&e.kind));
                ui.label(super::clip_chars(&origin(e), 60)).on_hover_text(origin(e));
                let (t, c) = match (&e.revoked, e.valid) {
                    (Some(d), _) => (format!("widerrufen {d}"), pal.err),
                    (None, true) => ("bereit".into(), pal.ok),
                    (None, false) => ("Fehler".into(), pal.err),
                };
                let r = ui.label(RichText::new(t).color(c));
                if let Some(er) = &e.error {
                    r.on_hover_text(er);
                }
                ui.horizontal(|ui| {
                    let running = app.running(&JobKind::Probe(e.name.clone()));
                    if ui.add_enabled(e.valid && !running, egui::Button::new("▶ Vorhören")).clicked() {
                        probe = Some(e.name.clone());
                    }
                    // any running job (probe/design of this voice too) could still write its audio
                    let busy = !app.may_purge();
                    let wait = "Erst den laufenden Auftrag abwarten oder abbrechen (auch Vorhören/Entwerfen)";
                    if e.kind == "clone" && e.revoked.is_none() && ui.add_enabled(!busy, egui::Button::new("Widerrufen")).on_disabled_hover_text(wait).on_hover_text("Einwilligung zurückziehen: Die Stimme wird nicht mehr gerendert und alle Aufnahmen mit ihr werden gelöscht").clicked() {
                        app.generate.revoke_ask = Some(e.name.clone());
                    }
                    if e.kind == "design" && e.source == "stimmwerk" && ui.add_enabled(!busy, egui::Button::new("Löschen")).on_disabled_hover_text(wait).on_hover_text("Entworfene Stimme mit allen Aufnahmen löschen").clicked() {
                        app.generate.rm_ask = Some(e.name.clone());
                    }
                });
                ui.end_row();
            }
        });
    });
    probe
}

fn revoke_dialog(app: &mut App, ctx: &egui::Context) {
    let Some(name) = app.generate.revoke_ask.clone() else { return };
    let mut go = false;
    let r = egui::Modal::new(egui::Id::new("revoke")).show(ctx, |ui| {
        ui.set_width(480.0);
        ui.label(RichText::new(format!("Einwilligung für „{name}“ widerrufen?")).size(17.0).strong());
        ui.add_space(6.0);
        ui.label(crate::app::REVOKE_BODY);
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            go = ui.button(RichText::new("Widerrufen und Aufnahmen löschen").color(theme::pal(ui).err).strong()).clicked();
            if ui.button("Abbrechen").clicked() {
                app.generate.revoke_ask = None;
            }
        });
    });
    if r.should_close() && !go {
        app.generate.revoke_ask = None;
    }
    if go {
        app.generate.revoke_ask = None;
        app.revoke(ctx, &name);
    }
}

fn rm_dialog(app: &mut App, ctx: &egui::Context) {
    let Some(name) = app.generate.rm_ask.clone() else { return };
    let mut go = false;
    let r = egui::Modal::new(egui::Id::new("rm")).show(ctx, |ui| {
        ui.set_width(480.0);
        ui.label(RichText::new(format!("Stimme „{name}“ löschen?")).size(17.0).strong());
        ui.add_space(6.0);
        ui.label(crate::app::RM_BODY);
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            go = ui.button(RichText::new("Stimme und Aufnahmen löschen").color(theme::pal(ui).err).strong()).clicked();
            if ui.button("Abbrechen").clicked() {
                app.generate.rm_ask = None;
            }
        });
    });
    if r.should_close() && !go {
        app.generate.rm_ask = None;
    }
    if go {
        app.generate.rm_ask = None;
        app.rm_voice(ctx, &name);
    }
}
