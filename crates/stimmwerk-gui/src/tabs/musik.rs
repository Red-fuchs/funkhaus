//! Musik tab (docs/stimmwerk/music-tab.md): cue list with inline gain/fade/loop/duck, acestep generation as a job
//! (progress, cancel, cache indicator), WAV import, placing on a line, mix preview via export.
use super::{card, chip};
use crate::app::App;
use crate::jobs::JobKind;
use crate::theme;
use eframe::egui::{self, RichText};
use stimmwerk_core::cues::{self, Cue, Kind};
use stimmwerk_core::music;

pub fn kind_name(k: Kind) -> &'static str {
    match k {
        Kind::Music => "Musik",
        Kind::Ambience => "Atmo",
        Kind::Sfx => "Geräusch",
    }
}

pub fn show(app: &mut App, ctx: &egui::Context) {
    egui::CentralPanel::default().show(ctx, |ui| {
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.add_space(8.0);
            ui.label(RichText::new("Musik und Geräusche").size(18.0).strong());
            ui.label(RichText::new("Cues liegen auf einer Skriptzeile und laufen in Vorschau und Export unter der Sprache (nur erzeugte Musik); „Absenken“ dimmt sie, während gesprochen wird.").weak());
            ui.add_space(8.0);
            cue_list(app, ui);
            ui.add_space(12.0);
            generate(app, ui);
            ui.add_space(12.0);
            import(app, ui);
            ui.add_space(12.0);
            preview(app, ui);
        });
    });
}

fn cue_list(app: &mut App, ui: &mut egui::Ui) {
    let Some(p) = app.proj.clone() else { return };
    let n = p.script.lines().count();
    let issues: Vec<(usize, &'static str, String)> =
        app.lint.as_ref().map(|l| l.issues.iter().filter(|i| i.code.starts_with("cue_")).filter_map(|i| i.msg.strip_prefix("Cue ").and_then(|r| r.split(':').next()).and_then(|k| k.parse().ok()).map(|k| (k, i.level, i.msg.clone()))).collect()).unwrap_or_default();
    let mut rm = None;
    let mut play = None;
    let mut changed = false;
    let busy = app.busy();
    card(ui, |ui| {
        ui.label(RichText::new("Cues").strong());
        if p.meta.cues.is_empty() {
            ui.label(RichText::new("Noch keine Cues – erzeuge unten Musik oder importiere eine WAV und leg sie auf eine Zeile.").weak());
            return;
        }
        let Some(pm) = app.proj.as_mut() else { return };
        egui::Grid::new("cues").num_columns(10).spacing([12.0, 6.0]).show(ui, |ui| {
            for h in ["#", "Art", "Quelle", "Start", "Pegel (dB)", "Einblenden (s)", "Ausblenden (s)", "Loop", "Absenken", ""] {
                ui.label(RichText::new(h).weak());
            }
            ui.end_row();
            for (k, c) in pm.meta.cues.iter_mut().enumerate() {
                let i = k + 1;
                let pal = theme::pal(ui);
                ui.horizontal(|ui| {
                    ui.label(RichText::new(i.to_string()).monospace());
                    if let Some((_, lev, msg)) = issues.iter().find(|x| x.0 == i) {
                        let (s, c) = if *lev == "error" { ("✖", pal.err) } else { ("⚠", pal.warn) };
                        ui.label(RichText::new(s).color(c)).on_hover_text(msg);
                    }
                });
                chip(ui, kind_name(c.kind), pal.accent.gamma_multiply(0.25), pal.accent, 56.0);
                let src = c.label();
                ui.label(super::clip_chars(src.strip_prefix("assets/").unwrap_or(&src), 26)).on_hover_text(if let Some(g) = &c.gen_ { format!("acestep: {} · {} s · Seed {}", g.prompt, g.dur, g.seed) } else { src.clone() });
                // one grid cell per field (headers line up); all fields locked while a job runs
                ui.horizontal(|ui| {
                    if let Some(l) = c.at_line.as_mut() {
                        ui.label("Zeile");
                        changed |= ui.add_enabled(!busy, egui::DragValue::new(l).range(1..=n.max(1))).changed();
                    } else if let Some(s) = c.at_sec.as_mut() {
                        changed |= ui.add_enabled(!busy, egui::DragValue::new(s).range(0.0..=36_000.0).speed(0.1).suffix(" s")).changed();
                    }
                });
                changed |= ui.add_enabled(!busy, egui::DragValue::new(&mut c.gain_db).range(cues::GAIN_MIN..=cues::GAIN_MAX).speed(0.5)).on_hover_text("Lautstärke der Quelle in dB, −60 bis +6 (0 = unverändert)").changed();
                changed |= ui.add_enabled(!busy, egui::DragValue::new(&mut c.fade_in).range(0.0..=60.0).speed(0.1)).on_hover_text("Einblenden in Sekunden").changed();
                changed |= ui.add_enabled(!busy, egui::DragValue::new(&mut c.fade_out).range(0.0..=60.0).speed(0.1)).on_hover_text("Ausblenden in Sekunden").changed();
                let mut lp = c.looping();
                if ui.add_enabled(!busy && c.kind != Kind::Sfx, egui::Checkbox::without_text(&mut lp)).on_hover_text("Nahtlos wiederholen bis zum Ende – auch unter dem KI-Hinweis, blendet in den letzten 1,5 s aus").on_disabled_hover_text("Geräusche werden nicht geloopt").changed() {
                    c.loop_ = lp;
                    if !lp {
                        c.loop_until_line = None;
                    }
                    changed = true;
                }
                changed |= ui.add_enabled(!busy, egui::Checkbox::without_text(&mut c.duck)).on_hover_text(format!("Unter Sprache absenken ({} dB)", c.duck_db.unwrap_or(pm.meta.music.duck_db))).changed();
                ui.horizontal(|ui| {
                    let f = cues::source_path(&p, c).filter(|f| f.is_file());
                    let on = app.player.playing().is_some() && f.as_deref() == app.player.playing();
                    if ui.add_enabled(f.is_some() || on, egui::Button::new(if on { "■ Stopp" } else { "▶" })).on_hover_text("Quelle anhören").on_disabled_hover_text("Quelle fehlt – noch nicht erzeugt oder Datei nicht da").clicked() {
                        play = Some((on, f));
                    }
                    if ui.add_enabled(!busy, egui::Button::new("🗑 Entfernen")).on_hover_text("Cue aus dem Projekt nehmen; Dateien bleiben").clicked() {
                        rm = Some(i);
                    }
                });
                ui.end_row();
            }
        });
    });
    if changed {
        app.cues_changed(ui.ctx(), "Cues gespeichert.");
    }
    if let Some((on, f)) = play {
        if on {
            app.player.stop();
        } else if let Some(f) = f {
            app.play(None, &f);
        }
    }
    if let Some(i) = rm {
        app.cue_rm(ui.ctx(), i);
    }
}

fn place_row(app: &mut App, ui: &mut egui::Ui, kind: Kind, src: (Option<String>, Option<cues::Gen>)) -> Option<Cue> {
    let n = app.n_lines().max(1);
    let m = &mut app.music;
    let mut c = None;
    ui.horizontal(|ui| {
        ui.label("Auf Zeile");
        ui.add(egui::DragValue::new(&mut m.at_line).range(1..=n)).on_hover_text("Skriptzeile, mit der der Cue beginnt (Einwürfe zählen mit)");
        ui.add_enabled(kind != Kind::Sfx, egui::Checkbox::new(&mut m.loop_, "Loop bis Ende")).on_hover_text("Nahtlos wiederholen bis zum Ende – auch unter dem KI-Hinweis, blendet in den letzten 1,5 s aus");
        ui.checkbox(&mut m.duck, "Absenken unter Sprache");
        if ui.button(RichText::new("Auf Zeile legen").strong()).on_hover_text("Cue ins Projekt schreiben (project.toml)").clicked() {
            let mut cue = Cue::new(kind);
            (cue.at_line, cue.file, cue.gen_, cue.loop_, cue.duck) = (Some(m.at_line), src.0, src.1, m.loop_ && kind != Kind::Sfx, m.duck);
            if cue.loop_ {
                cue.fade_out = 2.0;
            }
            c = Some(cue);
        }
    });
    c
}

fn generate(app: &mut App, ui: &mut egui::Ui) {
    let Some(p) = app.proj.clone() else { return };
    let mut go = false;
    let mut place = None;
    let mut play = None;
    card(ui, |ui| {
        ui.label(RichText::new("Musik erzeugen (acestep)").strong());
        ui.label(RichText::new("Instrumental nach Beschreibung – englisch, ohne Zahlen (Tempo und Tonart haben eigene Felder). Gleiche Angaben kommen aus dem Cache.").weak());
        ui.add_space(4.0);
        {
            let m = &mut app.music;
            egui::Grid::new("gen").num_columns(2).spacing([16.0, 8.0]).show(ui, |ui| {
                ui.label("Beschreibung");
                ui.add(egui::TextEdit::singleline(&mut m.prompt).desired_width(460.0).hint_text("z. B. calm maritime intro, acoustic guitar, warm"));
                ui.end_row();
                ui.label("Dauer");
                ui.horizontal(|ui| {
                    ui.add(egui::DragValue::new(&mut m.dur).range(cues::DUR_MIN..=cues::DUR_MAX).suffix(" s"));
                    ui.label(RichText::new(format!("{}–{} s", cues::DUR_MIN, cues::DUR_MAX)).weak());
                    ui.label("Seed");
                    ui.add(egui::DragValue::new(&mut m.seed)).on_hover_text("Gleicher Seed = gleiches Ergebnis; ändern für eine andere Variante");
                    ui.label("Tempo");
                    ui.add(egui::DragValue::new(&mut m.bpm).range(0..=240).suffix(" bpm")).on_hover_text("0 = frei");
                    ui.label("Tonart");
                    ui.add(egui::TextEdit::singleline(&mut m.key).desired_width(90.0).hint_text("D major"));
                });
                ui.end_row();
            });
        }
        ui.add_space(4.0);
        let g = app.music.spec();
        let cached = !g.prompt.is_empty() && music::lookup(&p, &g).is_some();
        let running = app.jobs.iter().find(|j| matches!(j.kind, JobKind::MusicGen(_)));
        ui.horizontal(|ui| {
            let pal = theme::pal(ui);
            let ok = !g.prompt.is_empty() && !app.busy();
            go = ui
                .add_enabled(ok, egui::Button::new(RichText::new("Erzeugen").strong()))
                .on_hover_text("acestep rechnet auf der GPU (10 s ≈ 15–20 s, 60 s ≈ 40 s); im Cache vorhandene Musik kommt sofort")
                .on_disabled_hover_text(if g.prompt.is_empty() { "Erst eine Beschreibung eingeben" } else { "Es läuft gerade ein Auftrag" })
                .clicked();
            if cached {
                ui.label(RichText::new("✔ im Cache").color(pal.ok)).on_hover_text("Diese Angaben wurden schon erzeugt – kein acestep-Lauf nötig");
            } else if !g.prompt.is_empty() {
                ui.label(RichText::new("○ noch nicht erzeugt").weak());
            }
            if let Some(j) = running {
                ui.spinner();
                let t = if j.gpu_wait {
                    "wartet auf GPU … (ein anderes Programm rechnet gerade)".to_string()
                } else if j.cancelled() {
                    "bricht ab …".into()
                } else {
                    format!("acestep rechnet … {}", funkhaus_sys::mmss(j.t0.elapsed().as_secs_f64()))
                };
                ui.label(RichText::new(t).weak());
                if ui.button("Abbrechen").on_hover_text("acestep stoppen; nichts wird gespeichert").clicked() {
                    j.cancel();
                }
            }
        });
        if let Some(b) = app.music.last.clone() {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                let pal = theme::pal(ui);
                ui.label(RichText::new(format!("✔ {} s · {} Hz · {}", b.secs, b.sr, if b.cached { "aus Cache" } else { "neu erzeugt" })).color(pal.ok));
                ui.label(RichText::new(super::clip_chars(&b.gen_.prompt, 48)).weak()).on_hover_text(format!("Key {} · Seed {}", b.key, b.gen_.seed));
                let on = app.player.playing() == Some(b.path.as_path());
                if ui.button(if on { "■ Stopp" } else { "▶ Anhören" }).clicked() {
                    play = Some((on, b.path.clone()));
                }
            });
            place = place_row(app, ui, Kind::Music, (None, Some(b.gen_.clone())));
        }
    });
    if go {
        app.music_gen(ui.ctx());
    }
    if let Some((on, f)) = play {
        if on { app.player.stop() } else { app.play(None, &f) }
    }
    if let Some(c) = place {
        app.cue_add(ui.ctx(), c);
    }
}

fn import(app: &mut App, ui: &mut egui::Ui) {
    if app.proj.is_none() {
        return;
    }
    let mut go = false;
    let mut place = None;
    card(ui, |ui| {
        ui.label(RichText::new("WAV importieren").strong());
        ui.label(RichText::new("Eigene Musik, Atmo (z. B. CC0 von freesound) oder Geräusche nach assets/ kopieren; Herkunft landet in assets/SOURCES.md.").weak());
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            let m = &mut app.music;
            ui.add(egui::TextEdit::singleline(&mut m.import_path).desired_width(360.0).hint_text("Pfad zur WAV-Datei"));
            if ui.button("WAV wählen …").clicked()
                && let Some(f) = rfd::FileDialog::new().add_filter("Audio", &["wav", "WAV", "mp3", "flac", "aiff"]).pick_file()
            {
                m.import_path = f.display().to_string();
            }
            egui::ComboBox::from_id_salt("imp-kind").width(110.0).selected_text(kind_name(m.import_kind)).show_ui(ui, |ui| {
                for k in [Kind::Music, Kind::Ambience, Kind::Sfx] {
                    ui.selectable_value(&mut m.import_kind, k, kind_name(k));
                }
            });
            go = ui.add_enabled(!m.import_path.trim().is_empty(), egui::Button::new("Importieren")).on_hover_text("Kopiert die Datei nach assets/<art>/ (andere Formate wandelt ffmpeg um)").clicked();
        });
        if let Some(r) = app.music.imported.clone() {
            ui.add_space(4.0);
            ui.label(RichText::new(format!("✔ {} · {} s · {} Hz{}", r.file, r.secs, r.sr, if r.converted { " · umgewandelt" } else { "" })).color(theme::pal(ui).ok));
            let kind = app.music.import_kind;
            place = place_row(app, ui, kind, (Some(r.file.clone()), None));
        }
    });
    if go {
        app.music_import();
    }
    if let Some(c) = place {
        app.cue_add(ui.ctx(), c);
    }
}

fn preview(app: &mut App, ui: &mut egui::Ui) {
    let Some(p) = app.proj.clone() else { return };
    card(ui, |ui| {
        ui.label(RichText::new("Mix vorhören").strong());
        ui.label(RichText::new("Rendert fehlende Zeilen in der Endfassung, mischt die Cues dazu und spielt das Ergebnis (gleich wie der WAV-Export).").weak());
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            let errs = app.lint.as_ref().map_or(0, |l| l.errors);
            let tip = if errs > 0 { "Erst die Fehler beheben (Reiter Skript, Hinweise)" } else { "WAV-Export nach out/ und abspielen" };
            if ui.add_enabled(errs == 0 && !app.busy() && !p.meta.cues.is_empty(), egui::Button::new("▶ Mix vorhören")).on_hover_text(tip).on_disabled_hover_text(if p.meta.cues.is_empty() { "Erst einen Cue anlegen" } else { tip }).clicked() {
                app.mix_preview(ui.ctx());
            }
            if app.music.preview && app.running(&JobKind::Export) {
                ui.spinner();
                ui.label(RichText::new("mischt …").weak());
            }
            if app.player.playing().is_some() && app.playing.is_none() && ui.button("■ Stopp").clicked() {
                app.player.stop();
            }
        });
    });
}
