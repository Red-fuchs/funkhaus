//! KI-Anbieter (Einstellungen): backend list with local/online badge, add from presets, edit, remove, key into the
//! keychain (masked, never shown again), "Verbindung testen" as a job; the project's backend + "online erlaubt" row.
//! Texts: docs/stimmwerk/ui-strings-de.md `llm.*`.
use super::{card, chip};
use crate::app::{App, LlmDlg};
use crate::jobs::JobKind;
use crate::theme;
use eframe::egui::{self, RichText};
use funkhaus_llm::Kind;
use stimmwerk_core::llm_cfg::{self, Entry, PRESETS};

pub const ONLINE_WARN: &str = "⚠ Texte dieses Projekts werden bei einem Online-Anbieter an dessen Server geschickt. Gib dort keine Personendaten ein, die du nicht weitergeben willst.";
pub const ONLINE_ASK_TITLE: &str = "Online-Anbieter erlauben?";
pub const TEST_BLOCKED: &str = "Erst Online-Anbieter erlauben";

fn online_ask_body(name: &str) -> String {
    format!("Skript-Text geht dann an „{name}“, einen Server außerhalb deines Macs. Personendaten gehören nicht ins Skript. Stimmwerk schickt nur, wenn du einen Vorgang startest, der das Sprachmodell braucht.")
}

/// (chip, detail) of a failed "Testen" – never the key (ui-strings-de.md `err.llm_*`).
pub fn err_text(name: &str, model: &str, e: &str) -> (String, String) {
    let lo = e.to_lowercase();
    if e.contains("LLM-Fehler 401") || e.contains("LLM-Fehler 403") {
        ("✖ abgelehnt".into(), format!("{name} lehnt deinen Schlüssel ab (401). Prüf ihn im Konto des Anbieters."))
    } else if e.contains("LLM-Fehler 404") {
        ("✖ Modell fehlt".into(), format!("{name} kennt das Modell „{model}“ nicht."))
    } else if e.contains("LLM-Fehler 429") || e.contains("LLM-Fehler 529") || e.contains("LLM-Fehler 503") {
        ("⚠ überlastet".into(), format!("{name} ist gerade überlastet. Versuch es in ein paar Minuten noch einmal."))
    } else if lo.contains("timed out") || lo.contains("timeout") || lo.contains("zeitüberschreitung") {
        ("✖ Zeitüberschreitung".into(), format!("{name} hat zu lange nicht geantwortet."))
    } else if e.contains("Schlüsselbund") || e.contains("API-Schlüssel") {
        ("⚠ kein Schlüssel".into(), format!("Trag einen Schlüssel ein, um {name} zu nutzen. ({e})"))
    } else if e.contains("nicht freigegeben") {
        ("⚠ gesperrt".into(), "Online-Anbieter sind nicht freigegeben. Erlaube sie in den Einstellungen.".into())
    } else if e.contains("nicht erreichbar") {
        ("✖ nicht erreichbar".into(), format!("{name} ist nicht erreichbar. Läuft der Server? Prüf die URL."))
    } else {
        ("✖ Fehler".into(), format!("{name}: {e}"))
    }
}

fn kind_chip(ui: &mut egui::Ui, local: bool) {
    let pal = theme::pal(ui);
    let (t, c, tip) = if local { ("lokal", pal.ok, "Läuft auf diesem Mac (localhost)") } else { ("online", pal.warn, "Sendet Text an einen fremden Server") };
    chip(ui, t, c.gamma_multiply(0.25), c, 48.0).on_hover_text(tip);
}

fn key_chip(ui: &mut egui::Ui, e: &Entry, saving: bool) {
    let pal = theme::pal(ui);
    if saving {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label(RichText::new("⟳ speichere …").weak()).on_hover_text("Der Schlüssel wird gerade in den Schlüsselbund geschrieben");
        });
        return;
    }
    match (e.key_info(), e.is_local()) {
        (Some(k), _) if k == "keychain" => drop(ui.label(RichText::new("✔ im Schlüsselbund").color(pal.ok)).on_hover_text("Ein Schlüssel ist gespeichert. Er wird nie angezeigt.")),
        (Some(k), _) => drop(ui.label(RichText::new(k).weak()).on_hover_text("Schlüssel aus einer Umgebungsvariable")),
        (None, false) => drop(ui.label(RichText::new("⚠ kein Schlüssel").color(pal.warn))),
        (None, true) => drop(ui.label(RichText::new("–").weak())),
    }
}

/// Settings card: provider table + "Anbieter hinzufügen". A broken settings.toml makes the card read-only.
pub fn card_ui(app: &mut App, ui: &mut egui::Ui) {
    card(ui, |ui| {
        ui.label(RichText::new("KI-Anbieter").strong());
        ui.label(RichText::new("Lokale und Online-Anbieter für Rollenskripte und Kürzungsvorschläge – standardmäßig lokal.").weak());
        ui.add_space(6.0);
        let broken = app.llm.broken.clone();
        if let Some((e, b)) = &broken {
            let pal = theme::pal(ui);
            let bk = b.as_ref().map(|b| format!(" Sicherungskopie: {}.", super::tilde(b))).unwrap_or_default();
            ui.add(egui::Label::new(RichText::new(format!("⚠ settings.toml ist fehlerhaft und wird nicht überschrieben: {e}{bk} Repariere die Datei und lade neu – bis dahin gelten die lokalen Vorlagen, nur lesen.")).color(pal.err)).wrap());
            if ui.button("⟳ Neu laden").on_hover_text("settings.toml erneut lesen").clicked() {
                app.reload_settings();
            }
            ui.add_space(6.0);
        }
        ui.add_enabled_ui(broken.is_none(), |ui| card_rows(app, ui));
    });
}

fn card_rows(app: &mut App, ui: &mut egui::Ui) {
    {
        let names: Vec<String> = app.settings.llm.backends.keys().cloned().collect();
        if names.is_empty() {
            ui.label("Noch kein Anbieter eingerichtet. Standard ist das lokale Modell.");
        }
        let default = app.settings.llm.default.clone();
        let mut act: Option<(&str, String)> = None;
        egui::Grid::new("llm").num_columns(7).spacing([14.0, 8.0]).show(ui, |ui| {
            if !names.is_empty() {
                for h in ["Standard", "Name", "Art", "Modell", "Schlüssel", "Status", ""] {
                    ui.label(RichText::new(h).weak());
                }
                ui.end_row();
            }
            for n in &names {
                let e = app.settings.llm.backends[n].clone();
                let is_def = default.as_deref() == Some(n);
                if ui.radio(is_def, "").on_hover_text("Dieser Anbieter wird genutzt, wenn ein Projekt keinen nennt").clicked() && !is_def {
                    act = Some(("default", n.clone()));
                }
                ui.label(RichText::new(n).strong()).on_hover_text(&e.url);
                kind_chip(ui, e.is_local());
                ui.label(if e.model.is_empty() { "(erstes Modell)".to_string() } else { super::clip_chars(&e.model, 28) }).on_hover_text(if e.model.is_empty() { "Kein Modell gesetzt: das erste aus der Liste des Anbieters" } else { e.model.as_str() });
                key_chip(ui, &e, app.running(&JobKind::KeySave(n.clone())));
                let pal = theme::pal(ui);
                if app.running(&JobKind::LlmTest(n.clone())) {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("⟳ prüfe …");
                    });
                } else {
                    match app.llm.status.get(n) {
                        Some(Ok(k)) => drop(ui.label(RichText::new(format!("✔ erreichbar · {k} Modelle")).color(pal.ok))),
                        Some(Err((c, d))) => drop(ui.label(RichText::new(c).color(pal.err)).on_hover_text(d)),
                        None => drop(ui.label(RichText::new("–").weak())),
                    }
                }
                ui.horizontal(|ui| {
                    let blocked = !e.is_local() && !e.allow_online;
                    let r = ui.add_enabled(!blocked && !app.running(&JobKind::LlmTest(n.clone())), egui::Button::new("⟳ Testen"));
                    if r.on_hover_text("Verbindung prüfen (ruft nur die Modellliste ab, es wird kein Text gesendet)").on_disabled_hover_text(TEST_BLOCKED).clicked() {
                        act = Some(("test", n.clone()));
                    }
                    if ui.button("✏ Bearbeiten").clicked() {
                        act = Some(("edit", n.clone()));
                    }
                    let r = ui.add_enabled(!is_def, egui::Button::new("🗑 Entfernen"));
                    if r.on_hover_text("Anbieter aus den Einstellungen entfernen").on_disabled_hover_text("Der Standard-Anbieter kann nicht entfernt werden. Wähle erst einen anderen Standard.").clicked() {
                        act = Some(("rm", n.clone()));
                    }
                });
                ui.end_row();
            }
        });
        ui.add_space(6.0);
        if ui.button("+ Anbieter hinzufügen").on_hover_text("Aus einer Vorlage (lokal: gemma-serve, vMLX, LM Studio, Ollama · online: Anthropic, OpenAI, OpenRouter) oder mit eigenen Angaben").clicked() {
            app.llm.dlg = Some(LlmDlg::new());
        }
        match act {
            Some(("default", n)) => app.llm_set_default(&n),
            Some(("test", n)) => app.llm_test(ui.ctx(), &n),
            Some(("edit", n)) => app.llm.dlg = app.settings.llm.backends.get(&n).map(|e| LlmDlg::edit(&n, e)),
            Some(("rm", n)) => (app.llm.rm_ask, app.llm.rm_key) = (Some(n), true),
            _ => {}
        }
    }
}

/// Backend + "online erlaubt" of the open project (Einstellungen card, Prosa dialog). Writes project.toml on change.
pub fn project_row(app: &mut App, ui: &mut egui::Ui) {
    let Some(p) = app.proj.as_ref() else { return };
    let cur = p.meta.llm.as_ref().and_then(|l| l.backend.clone());
    let ok = p.meta.llm.as_ref().is_some_and(|l| l.online_ok);
    let default = app.settings.llm.default.clone().unwrap_or_else(|| "–".into());
    let mut pick = cur.clone();
    let mut online_ok = ok;
    ui.horizontal(|ui| {
        ui.label("Anbieter für dieses Projekt");
        let shown = cur.clone().unwrap_or_else(|| format!("Standard ({default})"));
        egui::ComboBox::from_id_salt("proj-llm").width(220.0).selected_text(shown).show_ui(ui, |ui| {
            ui.selectable_value(&mut pick, None, format!("Standard ({default})"));
            for (n, e) in &app.settings.llm.backends {
                ui.selectable_value(&mut pick, Some(n.clone()), format!("{n}{}", if e.is_local() { "" } else { " · online" }));
            }
        });
        ui.checkbox(&mut online_ok, "Online erlaubt").on_hover_text("Texte dieses Projekts dürfen an einen Online-Anbieter gehen (zusätzlich muss der Anbieter in den Einstellungen erlaubt sein)");
    });
    match app.resolved() {
        Ok(r) if !r.is_local() => {
            let pal = theme::pal(ui);
            ui.add(egui::Label::new(RichText::new(ONLINE_WARN).color(pal.warn)).wrap());
            if let Err(e) = r.allowed() {
                ui.add(egui::Label::new(RichText::new(format!("⚠ {e}")).color(pal.warn)).wrap());
            }
        }
        Err(e) => drop(ui.add(egui::Label::new(RichText::new(format!("⚠ {e:#}")).color(theme::pal(ui).err)).wrap())),
        _ => {}
    }
    if pick != cur || online_ok != ok {
        app.set_project_llm((pick != cur).then_some(pick), (online_ok != ok).then_some(online_ok));
    }
}

/// Modals: add/edit, first online activation, remove.
pub fn dialogs(app: &mut App, ctx: &egui::Context) {
    if app.llm.dlg.is_some() {
        edit_dialog(app, ctx);
    }
    if app.llm.online_ask {
        online_dialog(app, ctx);
    }
    if app.llm.rm_ask.is_some() {
        rm_dialog(app, ctx);
    }
}

fn edit_dialog(app: &mut App, ctx: &egui::Context) {
    let mut save = false;
    let mut key_now = false;
    let mut clear_key = None;
    let mut close = false;
    let saving = app.llm.dlg.as_ref().and_then(|d| d.orig.clone()).is_some_and(|n| app.running(&JobKind::KeySave(n)));
    let existing = app.llm.dlg.as_ref().and_then(|d| d.orig.as_ref()).and_then(|n| app.settings.llm.backends.get(n)).cloned();
    let mut ask_online = false;
    let r = egui::Modal::new(egui::Id::new("llm-dlg")).show(ctx, |ui| {
        ui.set_width(560.0);
        let Some(d) = app.llm.dlg.as_mut() else { return };
        let title = match &d.orig {
            Some(n) => format!("Anbieter „{n}“ bearbeiten"),
            None => "Anbieter hinzufügen".into(),
        };
        ui.label(RichText::new(title).size(18.0).strong());
        ui.add_space(6.0);
        egui::Grid::new("llm-form").num_columns(2).spacing([16.0, 8.0]).show(ui, |ui| {
            if d.orig.is_none() {
                ui.label("Vorlage");
                let mut pick: Option<usize> = None;
                egui::ComboBox::from_id_salt("llm-preset").width(360.0).selected_text("Vorlage wählen …").show_ui(ui, |ui| {
                    for (k, p) in PRESETS.iter().enumerate() {
                        ui.selectable_value(&mut pick, Some(k), p.label);
                    }
                });
                if let Some(k) = pick {
                    d.preset(&PRESETS[k]);
                }
                ui.end_row();
            }
            ui.label("Name");
            match &d.orig {
                Some(n) => drop(ui.label(RichText::new(n).strong()).on_hover_text("Der Name bleibt (der Schlüsselbund-Eintrag hängt daran); umbenennen = neu anlegen")),
                None => drop(ui.add(egui::TextEdit::singleline(&mut d.name).desired_width(200.0).hint_text("z. B. claude")).on_hover_text("nur Buchstaben, Ziffern, _ - .")),
            }
            ui.end_row();
            ui.label("Art");
            egui::ComboBox::from_id_salt("llm-kind").width(220.0).selected_text(if d.kind == Kind::Anthropic { "Anthropic" } else { "OpenAI-kompatibel" }).show_ui(ui, |ui| {
                ui.selectable_value(&mut d.kind, Kind::OpenAi, "OpenAI-kompatibel").on_hover_text("gemma-serve, vMLX, LM Studio, Ollama, OpenAI, OpenRouter");
                ui.selectable_value(&mut d.kind, Kind::Anthropic, "Anthropic");
            });
            ui.end_row();
            ui.label("Adresse (URL)");
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut d.url).desired_width(300.0).hint_text("http://127.0.0.1:8095/v1")).on_hover_text("Serveradresse des Anbieters, meist mit /v1");
                kind_chip(ui, !d.online());
            });
            ui.end_row();
            ui.label("Modell");
            ui.add(egui::TextEdit::singleline(&mut d.model).desired_width(300.0).hint_text(if d.kind == Kind::Anthropic { funkhaus_llm::ANTHROPIC_MODEL } else { "leer = erstes Modell des Anbieters" })).on_hover_text("Modellname, den der Anbieter kennt");
            ui.end_row();
        });
        let online = d.online();
        if online {
            ui.add_space(6.0);
            egui::Grid::new("llm-key").num_columns(2).spacing([16.0, 8.0]).show(ui, |ui| {
                ui.label("API-Schlüssel");
                ui.horizontal(|ui| {
                    ui.add(egui::TextEdit::singleline(&mut d.key).password(true).desired_width(300.0).hint_text("wird nur im Schlüsselbund gespeichert")).on_hover_text("Wird im Schlüsselbund deines Macs gespeichert, nicht in der Konfigurationsdatei");
                    if let Some(e) = &existing {
                        key_chip(ui, e, saving);
                    }
                });
                ui.end_row();
                ui.label("");
                ui.horizontal(|ui| {
                    let r = ui.add_enabled(!d.key.trim().is_empty() && !saving, egui::Button::new(if saving { "⟳ speichere …" } else { "Schlüssel im Schlüsselbund speichern" }));
                    key_now = r.on_hover_text("Nur aktiv, wenn ein Schlüssel eingegeben ist").on_disabled_hover_text("Gib zuerst einen Schlüssel ein.").clicked();
                    if existing.as_ref().is_some_and(|e| e.key.is_some()) {
                        ui.label(RichText::new("Leer lassen = bisheriger Schlüssel bleibt").weak());
                        if ui.button(RichText::new("Schlüssel entfernen").color(theme::pal(ui).err)).on_hover_text("Löscht den Schlüssel aus dem Schlüsselbund").clicked() {
                            clear_key = d.orig.clone();
                        }
                    }
                });
                ui.end_row();
            });
            ui.add_space(4.0);
            let was = d.allow_online;
            let mut on = was;
            ui.checkbox(&mut on, "Online-Anbieter erlauben").on_hover_text("Erst dann dürfen Texte an diesen Anbieter gehen");
            if on && !was {
                ask_online = true; // confirmed in its own dialog
            } else if !on && was {
                d.allow_online = false;
            }
            ui.add(egui::Label::new(RichText::new(ONLINE_WARN).color(theme::pal(ui).warn)).wrap());
        }
        if let Some(e) = &d.err {
            ui.add_space(4.0);
            ui.add(egui::Label::new(RichText::new(e).color(theme::pal(ui).err)).wrap());
        }
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            let ok = !d.name.trim().is_empty() && !d.url.trim().is_empty() && (d.kind != Kind::Anthropic || !d.model.trim().is_empty());
            save = ui.add_enabled(ok, egui::Button::new(RichText::new("Speichern").strong())).on_hover_text("Anbieter speichern (ein eingegebener Schlüssel geht in den Schlüsselbund)").on_disabled_hover_text("Name, Adresse und – bei Anthropic – Modell ausfüllen").clicked();
            if ui.button("Abbrechen").on_hover_text("Nichts wird geschrieben").clicked() {
                close = true;
            }
        });
    });
    if ask_online {
        app.llm.online_ask = true;
    }
    if let Some(n) = clear_key {
        app.key_clear(&n);
    }
    if key_now && let Some(d) = app.llm.dlg.clone() {
        match &d.orig {
            Some(n) => {
                app.key_save(ctx, n, d.key);
                if let Some(dl) = app.llm.dlg.as_mut() {
                    dl.key.clear();
                }
            }
            None => app.llm_save_dlg(ctx),
        }
    }
    if save {
        app.llm_save_dlg(ctx);
    }
    if close || (r.should_close() && !save && !app.llm.online_ask) {
        app.llm.dlg = None;
    }
}

fn online_dialog(app: &mut App, ctx: &egui::Context) {
    let name = app.llm.dlg.as_ref().map(|d| d.name.clone()).unwrap_or_default();
    let mut go = false;
    let r = egui::Modal::new(egui::Id::new("llm-online")).show(ctx, |ui| {
        ui.set_width(480.0);
        ui.label(RichText::new(ONLINE_ASK_TITLE).size(17.0).strong());
        ui.add_space(6.0);
        ui.add(egui::Label::new(online_ask_body(&name)).wrap());
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            go = ui.button(RichText::new("Erlauben").color(theme::pal(ui).ok).strong()).clicked();
            if ui.button("Abbrechen").clicked() {
                app.llm.online_ask = false;
            }
        });
    });
    if r.should_close() && !go {
        app.llm.online_ask = false;
    }
    if go {
        app.llm.online_ask = false;
        if let Some(d) = app.llm.dlg.as_mut() {
            d.allow_online = true;
        }
    }
}

fn rm_dialog(app: &mut App, ctx: &egui::Context) {
    let Some(name) = app.llm.rm_ask.clone() else {
        return;
    };
    let has_key = app.settings.llm.backends.get(&name).is_some_and(|e| e.key.is_some());
    let mut go = false;
    let r = egui::Modal::new(egui::Id::new("llm-rm")).show(ctx, |ui| {
        ui.set_width(480.0);
        ui.label(RichText::new(format!("Anbieter „{name}“ entfernen?")).size(17.0).strong());
        ui.add_space(6.0);
        ui.label("Der Eintrag wird aus den Einstellungen gelöscht.");
        if has_key {
            ui.checkbox(&mut app.llm.rm_key, "Den gespeicherten Schlüssel auch aus dem Schlüsselbund löschen");
        }
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            go = ui.button(RichText::new("Anbieter entfernen").color(theme::pal(ui).err).strong()).clicked();
            if ui.button("Abbrechen").clicked() {
                app.llm.rm_ask = None;
            }
        });
    });
    if r.should_close() && !go {
        app.llm.rm_ask = None;
    }
    if go {
        app.llm.rm_ask = None;
        app.llm_remove(&name, has_key && app.llm.rm_key);
    }
}

/// Settings file hint for the card footer.
pub fn settings_hint() -> String {
    format!("Datei: {}", super::tilde(&llm_cfg::path()))
}
