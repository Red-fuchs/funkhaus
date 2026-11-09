//! No project open: new / open (file dialog, path field) / list of projects and recent ones. A row selects on click,
//! opens on double click or "Öffnen"; context menu and buttons rename (modal with live check) or move to the Trash
//! after two confirmations (the second types the name). The dialogs (`dialogs`) also serve the "Projekt …" menu of
//! an open project. Texts: ui-strings-de.md `start.*`.
use super::card;
use crate::app::{App, Rm};
use crate::theme;
use eframe::egui::{self, RichText};
use std::path::{Path, PathBuf};
use stimmwerk_core::project::{self, Info};
use stimmwerk_core::{EXT, projects_dir};

/// Projects in the projects dir plus recent ones elsewhere, recent first. Loads every project: call via `App::known` (cached).
pub fn known(app: &App) -> Vec<Info> {
    let mut dirs: Vec<PathBuf> = app.cfg.recent.clone();
    let mut local: Vec<PathBuf> = std::fs::read_dir(projects_dir()).into_iter().flatten().flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == EXT)).collect();
    local.sort();
    dirs.extend(local);
    let mut seen = std::collections::HashSet::new();
    dirs.into_iter().filter(|d| seen.insert(d.clone())).filter_map(|d| project::info(&d).ok()).collect()
}

/// What a row's menu / buttons can do.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Act {
    Open,
    Rename,
    Trash,
}

/// "2026-10-14 13:05" from the RFC 3339 stamp.
fn when(rfc: &str) -> String {
    if rfc.len() >= 16 { format!("{} {}", &rfc[..10], &rfc[11..16]) } else { rfc.into() }
}

/// Context menu items of a project (row or "Projekt …" menu); `can`: no job of it still running.
pub fn menu(ui: &mut egui::Ui, can: bool, open: bool) -> Option<Act> {
    let mut act = None;
    if !open && ui.button("Öffnen").clicked() {
        act = Some(Act::Open);
    }
    if ui.add_enabled(can, egui::Button::new("Umbenennen…")).on_disabled_hover_text(BUSY).clicked() {
        act = Some(Act::Rename);
    }
    if ui.add_enabled(can, egui::Button::new(RichText::new("Löschen…").color(theme::pal(ui).err))).on_disabled_hover_text(BUSY).clicked() {
        act = Some(Act::Trash);
    }
    if act.is_some() {
        ui.close();
    }
    act
}

pub const BUSY: &str = "Erst den laufenden Auftrag abwarten oder abbrechen.";

/// Starts `act` for project `dir` (`title` for the dialogs).
pub fn act(app: &mut App, ctx: &egui::Context, act: Act, dir: &Path, title: &str) {
    match act {
        Act::Open => app.open(ctx, dir),
        Act::Rename => app.start.rename = Some((dir.to_path_buf(), project::stem(dir))),
        Act::Trash => app.start.rm = Some(Rm { dir: dir.to_path_buf(), name: project::stem(dir), title: title.into(), step: 1 }),
    }
}

pub fn show(app: &mut App, ctx: &egui::Context) {
    egui::CentralPanel::default().show(ctx, |ui| {
        ui.add_space(18.0);
        ui.label(RichText::new("Deine Stimmen. Dein Skript.").size(22.0).strong());
        ui.label(RichText::new("Stimmwerk macht aus Skripten Hörspiele, Hörbücher und Sprechertexte – alles lokal auf deinem Mac.").weak());
        ui.add_space(14.0);
        card(ui, |ui| {
            ui.label(RichText::new("Neues Projekt").strong());
            ui.horizontal(|ui| {
                let r = ui.add(egui::TextEdit::singleline(&mut app.new_name).hint_text("Name, z. B. leuchtturm").desired_width(280.0));
                let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if ui.button("Anlegen").on_hover_text("Neues Stimmwerk-Projekt (Ordner <name>.studio) anlegen").clicked() || enter {
                    let n = app.new_name.clone();
                    app.create(ctx, &n);
                }
            });
            ui.add_space(10.0);
            ui.label(RichText::new("Projekt öffnen").strong());
            ui.horizontal(|ui| {
                if ui.button("🗁 Ordner wählen …").on_hover_text("Projektordner (.studio) auswählen").clicked()
                    && let Some(d) = rfd::FileDialog::new().set_directory(projects_dir()).pick_folder() {
                        app.open(ctx, &d);
                    }
                ui.label("oder");
                ui.add(egui::TextEdit::singleline(&mut app.open_path).hint_text("Pfad oder Projektname").desired_width(320.0));
                if ui.add_enabled(!app.open_path.trim().is_empty(), egui::Button::new("Öffnen")).clicked() {
                    let d = stimmwerk_core::locate(app.open_path.trim());
                    app.open(ctx, &d);
                }
            });
        });
        ui.add_space(14.0);
        let list = app.known().to_vec();
        ui.label(RichText::new(if list.is_empty() { "Noch keine Projekte – leg oben eins an." } else { "Deine Projekte" }).strong());
        if !list.is_empty() {
            ui.label(RichText::new("Klick wählt aus, Doppelklick öffnet. Rechtsklick für Umbenennen und Löschen.").weak());
        }
        let mut todo: Option<(Act, PathBuf, String)> = None;
        egui::ScrollArea::vertical().show(ui, |ui| {
            for i in list {
                let (sel, can) = (app.start.sel.as_deref() == Some(i.path.as_path()), !app.ending(&i.path));
                ui.horizontal(|ui| {
                    let r = ui.selectable_label(sel, RichText::new(&i.title).size(15.0)).on_hover_text("Doppelklick öffnet das Projekt");
                    if r.clicked() {
                        app.start.sel = Some(i.path.clone());
                    }
                    if r.double_clicked() {
                        todo = Some((Act::Open, i.path.clone(), i.title.clone()));
                    }
                    r.context_menu(|ui| {
                        if let Some(a) = menu(ui, can, false) {
                            todo = Some((a, i.path.clone(), i.title.clone()));
                        }
                    });
                    ui.label(RichText::new(format!("{}.{EXT}", i.name)).weak()).on_hover_text(super::tilde(&i.path));
                    ui.label(RichText::new(format!("{} Zeilen · {}{}", i.lines, when(&i.modified), if i.sync { " · Sync" } else { "" })).weak());
                    if sel {
                        if ui.small_button("Öffnen").clicked() {
                            todo = Some((Act::Open, i.path.clone(), i.title.clone()));
                        }
                        if ui.add_enabled(can, egui::Button::new("Umbenennen…").small()).on_hover_text("Ordner und Titel des Projekts ändern").on_disabled_hover_text(BUSY).clicked() {
                            todo = Some((Act::Rename, i.path.clone(), i.title.clone()));
                        }
                        if ui.add_enabled(can, egui::Button::new(RichText::new("Löschen…").color(theme::pal(ui).err)).small()).on_hover_text("Projekt in den Papierkorb verschieben (fragt zweimal nach)").on_disabled_hover_text(BUSY).clicked() {
                            todo = Some((Act::Trash, i.path.clone(), i.title.clone()));
                        }
                    }
                });
            }
        });
        if let Some((a, d, t)) = todo {
            act(app, ctx, a, &d, &t);
        }
    });
}

/// Rename modal and the two delete confirmations (start screen and open project).
pub fn dialogs(app: &mut App, ctx: &egui::Context) {
    rename_dialog(app, ctx);
    rm_dialogs(app, ctx);
}

fn rename_dialog(app: &mut App, ctx: &egui::Context) {
    let Some((dir, mut name)) = app.start.rename.clone() else { return };
    let (mut go, mut stop) = (false, false);
    let r = egui::Modal::new(egui::Id::new("proj-mv")).show(ctx, |ui| {
        ui.set_width(480.0);
        ui.label(RichText::new("Projekt umbenennen").size(17.0).strong());
        ui.add_space(6.0);
        ui.label(RichText::new(format!("Ordner und Titel werden zu „<Name>.{EXT}“ – Takes, Musik und Exporte bleiben.")).weak());
        ui.add_space(6.0);
        let t = ui.add(egui::TextEdit::singleline(&mut name).hint_text("Neuer Name").desired_width(f32::INFINITY));
        let why = project::rename_check(&dir, &name);
        let same = project::stem(&dir) == name.trim();
        let ok = why.is_none() && !same;
        ui.add_space(4.0);
        match &why {
            Some(w) => ui.label(RichText::new(w).color(theme::pal(ui).err)),
            None if same => ui.label(RichText::new("Das ist schon der Name.").weak()),
            None => ui.label(RichText::new(format!("Wird zu „{}.{EXT}“.", name.trim())).weak()),
        };
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            go = ui.add_enabled(ok, egui::Button::new("Speichern")).clicked() || (ok && t.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)));
            stop = ui.button("Abbrechen").clicked();
        });
    });
    if stop || (r.should_close() && !go) {
        app.start.rename = None;
    } else if go {
        app.rename_project(ctx, &dir, &name);
    } else {
        app.start.rename = Some((dir, name));
    }
}

/// Two confirmations before a project folder goes to the Trash: what goes, then a last click.
fn rm_dialogs(app: &mut App, ctx: &egui::Context) {
    let Some(mut rm) = app.start.rm.clone() else { return };
    let (mut go, mut stop) = (false, false);
    let r = egui::Modal::new(egui::Id::new("proj-rm")).show(ctx, |ui| {
        ui.set_width(520.0);
        let pal = theme::pal(ui);
        if rm.step == 1 {
            ui.label(RichText::new(format!("Projekt „{}“ löschen?", rm.title)).size(17.0).strong());
            ui.add_space(6.0);
            ui.add(egui::Label::new(format!("Der Ordner {} wandert mit Skript, Takes, Musik und Exporten in den Papierkorb – von dort kannst du ihn zurückholen. Deine Stimmen bleiben.", super::tilde(&rm.dir))).wrap());
            ui.add_space(4.0);
            ui.add(egui::Label::new(RichText::new(stimmwerk_core::project::TRASH_NOTE).color(pal.err)).wrap());
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                go = ui.button(RichText::new("Weiter …").color(pal.err).strong()).on_hover_text("Noch nichts passiert – es folgt eine letzte Bestätigung").clicked();
                stop = ui.button("Abbrechen").clicked();
            });
        } else {
            ui.label(RichText::new("Wirklich in den Papierkorb?").size(17.0).strong());
            ui.add_space(6.0);
            ui.add(egui::Label::new(RichText::new(format!("„{}“ kommt jetzt in den Papierkorb.", rm.title)).color(pal.err)).wrap());
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                go = ui.button(RichText::new("In den Papierkorb").color(pal.err).strong()).clicked();
                stop = ui.button("Abbrechen").clicked();
            });
        }
    });
    if stop || (r.should_close() && !go) {
        app.start.rm = None;
    } else if go && rm.step == 1 {
        rm.step = 2;
        app.start.rm = Some(rm);
    } else if go {
        app.delete_project(&rm.dir, &rm.title);
    } else {
        app.start.rm = Some(rm);
    }
}
