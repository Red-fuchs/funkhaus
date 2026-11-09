//! Skript tab: line list (speaker chip, text, state, Anhören / Neu würfeln / Zurück), render bar with progress/cancel,
//! edit/reload of script.txt, lint panel.
use super::chip;
use crate::app::App;
use crate::jobs::JobKind;
use crate::theme;
use eframe::egui::{self, Color32, RichText};
use stimmwerk_core::render::State;
use stimmwerk_core::script::Item;

/// (symbol + text, tooltip) of a line state (ui-strings-de.md `state.*`).
pub fn state_text(s: Option<State>) -> (&'static str, &'static str) {
    match s {
        None => ("○ offen", "Noch nicht gerendert: kein Take im Cache – „Rendern“ spricht die Zeile"),
        Some(State::Queued) => ("○ wartet", "Zeile ist eingereiht, wartet auf die GPU"),
        Some(State::Rendering) => ("⟳ rendert", "Zeile wird gerade gesprochen"),
        Some(State::Cached) => ("✔ aus Cache", "Zeile unverändert, Take kommt aus dem Cache – kein neues Rendern nötig"),
        Some(State::Done) => ("✔ fertig", "Zeile gerendert und im Mix"),
        Some(State::Failed) => ("✖ Fehler", "Zeile konnte nicht gerendert werden; Details unten in der Meldung"),
    }
}

/// (text, tooltip) of a line's cache state in both qualities: „Entwurf ✔ · Endfassung –“.
pub fn pair_text(draft: bool, fin: bool) -> (String, &'static str) {
    let m = |b: bool| if b { "✔" } else { "–" };
    let tip = match (draft, fin) {
        (true, true) => "Entwurf und Endfassung sind gerendert (im Cache)",
        (true, false) => "Nur als Entwurf gerendert – „Endfassung rendern“ spricht die Zeile in voller Qualität (gleiche Stimme, gleicher Take)",
        (false, true) => "Endfassung ist gerendert; ein Entwurf fehlt (nicht nötig für den Export)",
        (false, false) => "Noch nicht gerendert: weder Entwurf noch Endfassung im Cache",
    };
    (format!("Entwurf {} · Endfassung {}", m(draft), m(fin)), tip)
}

/// Summary line addition: „· 3 Zeilen nur als Entwurf“ (empty if none).
pub fn draft_only_text(n: usize) -> String {
    match n {
        0 => String::new(),
        1 => " · 1 Zeile nur als Entwurf".into(),
        n => format!(" · {n} Zeilen nur als Entwurf"),
    }
}

fn state_color(ui: &egui::Ui, s: Option<State>) -> Color32 {
    let p = theme::pal(ui);
    match s {
        Some(State::Done | State::Cached) => p.ok,
        Some(State::Failed) => p.err,
        Some(State::Rendering) => p.accent,
        _ => ui.visuals().weak_text_color(),
    }
}

pub fn show(app: &mut App, ctx: &egui::Context) {
    render_bar(app, ctx);
    if app.show_lint {
        lint_panel(app, ctx);
    }
    egui::CentralPanel::default().show(ctx, |ui| lines(app, ui));
    if app.prose.open {
        super::prose::dialog(app, ctx);
    }
}

/// Cue of a sync project whose speech does not fit (too_long / overlap): offered "Kürzen vorschlagen".
fn too_long(app: &App, k: usize) -> bool {
    app.sync_rep.as_ref().is_some_and(|r| r.cues.iter().any(|c| c.index == k && matches!(c.status, stimmwerk_core::sync::Kind::TooLong | stimmwerk_core::sync::Kind::Overlap) && c.cut_s > 0.0))
}

/// Proposal box under a line: text, numbers kept, violations; apply only on click.
fn shorten_box(app: &mut App, ui: &mut egui::Ui, k: usize) {
    let Some(s) = app.shorten.get(&k).cloned() else {
        return;
    };
    let mut act = None;
    egui::Frame::new().fill(theme::pal(ui).card).corner_radius(6.0).inner_margin(egui::Margin::symmetric(10, 6)).show(ui, |ui| {
        let pal = theme::pal(ui);
        match &s.proposal {
            Some(t) => {
                ui.label(RichText::new(format!("Kürzungsvorschlag: {} statt {} Zeichen (Ziel {}):", s.proposal_chars, s.chars, s.target_chars)).weak());
                ui.add(egui::Label::new(RichText::new(t).strong()).wrap());
            }
            None => drop(ui.add(egui::Label::new(RichText::new("Kein brauchbarer Vorschlag (Zahlen wären verändert worden).").color(pal.err)).wrap())),
        }
        if !s.reason.is_empty() {
            ui.label(RichText::new(format!("Begründung: {}", s.reason)).weak());
        }
        for v in &s.violations {
            ui.label(RichText::new(format!("⚠ {v}")).color(pal.warn));
        }
        ui.horizontal(|ui| {
            if ui.add_enabled(s.proposal.is_some(), egui::Button::new(RichText::new("Übernehmen").strong())).on_hover_text("Zeilentext durch den Vorschlag ersetzen (script.txt)").clicked() {
                act = Some(true);
            }
            if ui.button("Verwerfen").on_hover_text("Vorschlag verwerfen, Zeile bleibt").clicked() {
                act = Some(false);
            }
        });
    });
    match act {
        Some(true) => app.shorten_apply(ui.ctx(), k),
        Some(false) => drop(app.shorten.remove(&k)),
        None => {}
    }
}

fn render_bar(app: &mut App, ctx: &egui::Context) {
    egui::TopBottomPanel::bottom("renderbar").exact_height(46.0).show(ctx, |ui| {
        ui.horizontal_centered(|ui| {
            let busy = app.busy();
            let errs = app.lint.as_ref().map_or(0, |l| l.errors);
            let sync = app.is_sync();
            let tip = if sync {
                crate::app::SYNC_RENDER
            } else if errs > 0 {
                "Erst die Fehler im Skript beheben (siehe Hinweise)"
            } else {
                "Alle offenen Zeilen rendern; fertige Zeilen kommen aus dem Cache (⌘R)"
            };
            if ui.add_enabled(!busy && !sync && errs == 0 && app.n_lines() > 0, egui::Button::new(RichText::new("Rendern").strong())).on_hover_text(tip).on_disabled_hover_text(tip).clicked() {
                app.start(ctx, JobKind::Render);
            }
            if app.offer_final() && !sync {
                let tip = "Alle Zeilen in voller Qualität sprechen, die bisher nur als Entwurf da sind – gleiche Stimmen und Takes, fertige Zeilen kommen aus dem Cache";
                if ui.add_enabled(!busy && errs == 0, egui::Button::new("Endfassung rendern")).on_hover_text(tip).on_disabled_hover_text(tip).clicked() {
                    app.render_final(ctx);
                }
            }
            let job = app.jobs.iter().position(|j| j.kind.exclusive());
            if ui.add_enabled(job.is_some_and(|k| app.jobs[k].kind.cancellable()), egui::Button::new("Abbrechen")).on_hover_text("Stoppt sofort (Esc); fertige Zeilen bleiben erhalten").clicked() {
                app.cancel_all();
            }
            let mut draft = app.draft;
            ui.add_enabled_ui(!busy, |ui| {
                ui.label(RichText::new("Qualität").weak());
                ui.selectable_value(&mut draft, true, "Entwurf").on_hover_text("Schnell, etwas einfacher – zum Probehören. Gleiche Stimmen und Takes wie die Endfassung");
                ui.selectable_value(&mut draft, false, "Endfassung").on_hover_text("Volle Qualität für den Export (dauert länger)");
            });
            app.set_draft(ui.ctx(), draft);
            ui.separator();
            match job.map(|k| &app.jobs[k]) {
                Some(j) => {
                    let f = if j.total > 0 { j.done as f32 / j.total as f32 } else { 0.0 };
                    let t = funkhaus_sys::mmss(j.t0.elapsed().as_secs_f64());
                    ui.add(egui::ProgressBar::new(f).desired_width(260.0).text(format!("{}/{} · {t}", j.done, j.total)));
                    let what = match (j.stage.as_deref(), j.cur) {
                        _ if j.gpu_wait => "wartet auf GPU … (ein anderes Programm rechnet gerade)".to_string(),
                        (_, Some(i)) if j.model_loading() => format!("Zeile {i}: lädt Sprachmodell (erstes Mal 30–60 s) …"),
                        (Some("mix"), _) => "mischt …".to_string(),
                        (Some("master" | "export" | "parts"), _) => "exportiert …".to_string(),
                        (Some("disclosure"), _) => "KI-Hinweis …".to_string(),
                        (_, Some(i)) => format!("Zeile {i} läuft …"),
                        _ => "startet …".to_string(),
                    };
                    ui.label(RichText::new(if j.cancelled() { "bricht ab …".into() } else { what }).weak());
                }
                None if errs > 0 => {
                    ui.label(RichText::new(format!("Rendern gesperrt: {errs} Fehler im Skript (siehe Hinweise)")).color(theme::pal(ui).err));
                }
                None => {
                    let s = &app.summary;
                    ui.label(RichText::new(format!("{} Zeilen · ca. {} · {} Sprecher{}", s.lines, s.est, s.speakers, draft_only_text(app.draft_only()))).weak());
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let label = if app.show_lint { "Hinweise ausblenden" } else { "Hinweise zeigen" };
                if ui.button(label).clicked() {
                    app.show_lint = !app.show_lint;
                }
                if ui.add_enabled(!busy, egui::Button::new("⟳ Neu laden")).on_hover_text("Skript neu von der Festplatte lesen (passiert auch automatisch, wenn du speicherst)").clicked() {
                    app.reload(ui.ctx(), "Skript neu geladen.");
                }
                if ui.button("✏ Skript bearbeiten").on_hover_text("Öffnet script.txt im Texteditor; nach dem Speichern lädt Stimmwerk es neu").clicked() {
                    app.edit_script();
                }
                if ui.add_enabled(!busy, egui::Button::new("Aus Prosa erstellen …")).on_hover_text("Erzähltext per Sprachmodell in ein Rollenskript wandeln – mit Vorschau, bevor etwas geschrieben wird").clicked() {
                    app.prose.open = true;
                }
            });
        });
    });
}

fn lint_panel(app: &mut App, ctx: &egui::Context) {
    let w = (ctx.content_rect().width() * 0.26).clamp(240.0, 340.0);
    egui::SidePanel::right("lint").exact_width(w).resizable(false).show(ctx, |ui| {
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new("Hinweise").strong());
            if ui.small_button("⟳ Prüfen").on_hover_text("Skript und Stimmen erneut prüfen (fragt auch omnivoice neu)").clicked() {
                app.relint_with(ctx, true);
            }
        });
        ui.separator();
        let Some(l) = &app.lint else {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("prüft …");
            });
            return;
        };
        if l.issues.is_empty() {
            ui.label(RichText::new("✔ Keine Fehler, keine Hinweise.").color(theme::pal(ui).ok));
        }
        let mut pick = None;
        let line_no = app.proj.as_ref().map(line_numbers).unwrap_or_default();
        egui::ScrollArea::vertical().show(ui, |ui| {
            for lev in ["error", "warn"] {
                for i in l.issues.iter().filter(|i| i.level == lev) {
                    let p = theme::pal(ui);
                    let (sym, c) = if lev == "error" { ("✖", p.err) } else { ("⚠", p.warn) };
                    let head = i.line.map_or(String::new(), |n| match line_no.get(n - 1).copied().flatten() {
                        Some(k) => format!("Zeile {k}: "),
                        None => format!("Skriptdatei, Zeile {n}: "),
                    });
                    let r = ui.add(egui::Label::new(RichText::new(format!("{sym} {head}{}", i.msg)).color(c)).wrap().sense(egui::Sense::click()));
                    if let Some(n) = i.line.filter(|_| r.clicked()) {
                        pick = Some(n - 1);
                    }
                    ui.add_space(4.0);
                }
            }
        });
        if pick.is_some() {
            app.sel = pick;
        }
    });
}

/// Script line number (1-based, as shown and as the core counts) per script.txt item; None for notes/bad lines.
pub fn line_numbers(p: &stimmwerk_core::Project) -> Vec<Option<usize>> {
    let mut k = 0;
    p.script
        .items
        .iter()
        .map(|it| {
            matches!(it, Item::Line(_)).then(|| {
                k += 1;
                k
            })
        })
        .collect()
}

fn lines(app: &mut App, ui: &mut egui::Ui) {
    let Some(p) = app.proj.clone() else { return };
    if p.script.items.is_empty() {
        ui.add_space(20.0);
        ui.label(RichText::new("Das Skript enthält noch keine Zeilen.").size(16.0).strong());
        ui.add_space(4.0);
        ui.label("Eine Zeile pro Satz: WER|ABSTAND|TEXT, z. B.");
        ui.label(RichText::new("ERZ|0|Es war einmal ein Leuchtturm.\nMara|0.5|Und ich war dabei!").monospace());
        ui.label(RichText::new(format!("Datei: {}", super::tilde(&p.dir.join("script.txt")))).weak());
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            if ui.button("✏ Skript bearbeiten").on_hover_text("Öffnet script.txt im Texteditor; nach dem Speichern lädt Stimmwerk es neu").clicked() {
                app.edit_script();
            }
            if ui.button("Aus Prosa erstellen …").on_hover_text("Erzähltext per Sprachmodell in ein Rollenskript wandeln – mit Vorschau, bevor etwas geschrieben wird").clicked() {
                app.prose.open = true;
            }
        });
        return;
    }
    let wait = app.jobs.iter().find(|j| j.kind.exclusive() && j.gpu_wait).and_then(|j| j.cur);
    let cast: Vec<&String> = p.meta.speakers.keys().collect();
    let busy = app.busy();
    let mut act = None;
    let mut shorten = None;
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        let mut k = 0; // script line counter (1-based after increment) = core's line numbering
        for (idx, it) in p.script.items.iter().enumerate() {
            match it {
                Item::Note(n) if n.trim().is_empty() => ui.add_space(4.0),
                Item::Note(n) => {
                    let t = n.trim_start_matches('#').trim();
                    let t = t.strip_prefix("SEG").map_or(t.to_string(), |r| format!("Kapitel {}", r.trim().replacen(" | ", " · ", 1)));
                    ui.add_space(6.0);
                    ui.label(RichText::new(t).strong().color(theme::pal(ui).accent));
                }
                Item::Bad(raw) => {
                    ui.label(RichText::new(format!("✖ Zeile {} nicht lesbar: {raw}", idx + 1)).color(theme::pal(ui).err));
                }
                Item::Line(l) => {
                    k += 1;
                    let st = app.states.get(k - 1).copied().flatten();
                    let take = app.takes.get(k - 1).copied();
                    let both = app.both.get(k - 1).copied();
                    let sel = app.sel == Some(idx);
                    let fill = if sel { theme::row_sel(ui) } else { Color32::TRANSPARENT };
                    // the row senses clicks via its Ui (registered before its children, so the buttons stay on top)
                    let r = ui.scope_builder(egui::UiBuilder::new().sense(egui::Sense::click()), |ui| {
                        egui::Frame::new().fill(fill).corner_radius(6.0).inner_margin(egui::Margin::symmetric(6, 3)).show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.add_sized([26.0, 18.0], egui::Label::new(RichText::new(k.to_string()).weak().monospace()));
                                let ci = cast.iter().position(|c| *c == &l.who).unwrap_or(7);
                                let (f, fg) = theme::speaker(ui, ci);
                                let name = p.meta.speakers.get(&l.who).and_then(|s| s.label.clone()).unwrap_or_else(|| l.who.clone());
                                if l.over {
                                    ui.label(RichText::new("↪ Einwurf").small()).on_hover_text("Einwurf: spricht in die vorige Zeile hinein (Abstand = Anteil der Vorzeile, 0 bis 1)");
                                }
                                chip(ui, &super::clip_chars(&name, 14), f, fg, if l.over { 56.0 } else { 104.0 }).on_hover_text(format!("Sprecher {} · Abstand {} s{}", l.who, l.gap, if l.over { " · Einwurf" } else { "" }));
                                let right = if too_long(app, k) { 584.0 } else { 434.0 };
                                let tw = (ui.available_width() - right).max(120.0);
                                ui.allocate_ui_with_layout(egui::vec2(tw, 18.0), egui::Layout::left_to_right(egui::Align::Center).with_main_wrap(true), |ui| {
                                    ui.set_width(tw);
                                    ui.add(egui::Label::new(styled(ui, &l.text)).wrap());
                                });
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    let issue = app.lint.as_ref().and_then(|l| l.issues.iter().filter(|i| i.line == Some(idx + 1)).min_by_key(|i| i.level != "error"));
                                    let bad = issue.is_some_and(|i| i.level == "error");
                                    const W: f32 = 150.0;
                                    match (st, issue, both) {
                                        (Some(State::Rendering), _, _) if wait == Some(k) => {
                                            ui.add_sized([W, 18.0], egui::Label::new(RichText::new("○ wartet auf GPU").color(theme::pal(ui).accent))).on_hover_text("Ein anderes Programm nutzt gerade die GPU; die Zeile startet danach. „Abbrechen“ wirkt sofort.");
                                        }
                                        (None, Some(i), _) => {
                                            let (t, c) = if i.level == "error" { ("✖ Fehler", theme::pal(ui).err) } else { ("⚠ Hinweis", theme::pal(ui).warn) };
                                            ui.add_sized([W, 18.0], egui::Label::new(RichText::new(t).color(c))).on_hover_text(&i.msg);
                                        }
                                        (None | Some(State::Cached | State::Done), _, Some([d, f])) => {
                                            let (t, tip) = pair_text(d, f);
                                            let on = if app.draft { d } else { f };
                                            let c = if on { theme::pal(ui).ok } else { ui.visuals().weak_text_color() };
                                            ui.add_sized([W, 18.0], egui::Label::new(RichText::new(t).color(c))).on_hover_text(tip);
                                        }
                                        _ => {
                                            let (t, tip) = state_text(st);
                                            ui.add_sized([W, 18.0], egui::Label::new(RichText::new(t).color(state_color(ui, st)))).on_hover_text(tip);
                                        }
                                    }
                                    if too_long(app, k) {
                                        let running = app.running(&JobKind::Shorten(k));
                                        if ui
                                            .add_enabled(!running && !busy, egui::Button::new(if running { "⟳ fragt …" } else { "Kürzen vorschlagen" }))
                                            .on_hover_text("Zu lang für den Untertitel-Slot: das Sprachmodell schlägt eine kürzere Fassung vor (Zahlen bleiben); übernommen wird erst per Klick")
                                            .clicked()
                                        {
                                            shorten = Some(k);
                                        }
                                    }
                                    let undo_ok = take.is_some_and(|t| t > 0) && !busy;
                                    if ui.add_enabled(undo_ok, egui::Button::new("⟲ Zurück")).on_hover_text("Vorherigen Take dieser Zeile wiederherstellen (kein neues Sprechen)").on_disabled_hover_text("Noch kein früherer Take – erst „Neu würfeln“").clicked() {
                                        act = Some(JobKind::Reroll { line: k, undo: true });
                                    }
                                    let fix = "Erst den Fehler in dieser Zeile beheben (siehe Hinweise)";
                                    if ui.add_enabled(!busy && !bad, egui::Button::new("Neu würfeln")).on_hover_text("Diese Zeile neu sprechen lassen (anderer Zufallswert); der alte Take bleibt für „Zurück“").on_disabled_hover_text(if bad { fix } else { "Es läuft gerade ein Auftrag" }).clicked()
                                    {
                                        act = Some(JobKind::Reroll { line: k, undo: false });
                                    }
                                    let playing = app.playing == Some(k);
                                    let (pl, ptip) = if playing { ("■ Stopp", "Wiedergabe anhalten") } else { ("▶ Anhören", "Zeile abspielen (kommt aus dem Cache; fehlt der Take, wird er gesprochen)") };
                                    if ui.add_enabled(playing || (!busy && !bad), egui::Button::new(pl)).on_hover_text(ptip).on_disabled_hover_text(if bad { fix } else { "Es läuft gerade ein Auftrag" }).clicked() {
                                        if playing {
                                            app.player.stop();
                                            app.playing = None;
                                        } else {
                                            act = Some(JobKind::PlayLine(k));
                                        }
                                    }
                                });
                            });
                        });
                        ui.response()
                    });
                    if r.inner.clicked() {
                        app.sel = Some(idx);
                    }
                    if app.shorten.contains_key(&k) {
                        shorten_box(app, ui, k);
                    }
                }
            }
        }
    });
    if let Some(a) = act {
        app.start(ui.ctx(), a);
    }
    if let Some(k) = shorten {
        app.shorten_run(ui.ctx(), k);
    }
}

/// Tags (`[laughter]`, `[pause 500ms]`) dimmed.
fn styled(ui: &egui::Ui, text: &str) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob::default();
    let (fg, weak) = (ui.visuals().text_color(), ui.visuals().weak_text_color());
    let font = egui::TextStyle::Body.resolve(ui.style());
    let mut rest = text;
    while let Some(a) = rest.find('[') {
        let b = rest[a..].find(']').map_or(rest.len(), |b| a + b + 1);
        job.append(&rest[..a], 0.0, egui::TextFormat::simple(font.clone(), fg));
        job.append(&rest[a..b], 0.0, egui::TextFormat { italics: true, ..egui::TextFormat::simple(font.clone(), weak) });
        rest = &rest[b..];
    }
    job.append(rest, 0.0, egui::TextFormat::simple(font, fg));
    job
}
