mod icon;
mod shot;
mod theme;

use eframe::egui::{self, Color32, RichText};
use funkhaus_core::{Backend, Notebook, Store, ask::Answer, guide, make, map, podcast, store::{Ctx, Note}, util};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::process::{Child, Command};
use std::sync::mpsc::{Receiver, channel};

enum Msg {
    Prog(String),
    Done(Result<Out, String>),
}

enum Out {
    Added(usize),
    Answer(Answer),
    Doc(make::Doc),
    Pod(podcast::Pod),
    Sugg(Vec<String>),
    Mapped(String),
}

struct Job {
    nb: String,
    label: String,
    last: String,
    rx: Receiver<Msg>,
    cancel: Option<Arc<AtomicBool>>,
}

/// Text shown in the viewer window (source excerpt, document, script).
struct View {
    title: String,
    text: String,
    path: Option<PathBuf>,
    /// clickable suggested questions (source guide) → ask
    questions: Vec<String>,
}

impl View {
    fn new(title: impl Into<String>, text: impl Into<String>, path: Option<PathBuf>) -> Self {
        Self { title: title.into(), text: text.into(), path, questions: vec![] }
    }
}

/// Per source: (mapped, total) chunks and the current source guide (cache only, no LLM).
#[derive(Default)]
struct Kart {
    done: HashMap<String, (usize, usize)>,
    guides: HashMap<String, guide::Guide>,
}

impl Kart {
    fn of(st: &Store, nb: &Notebook) -> Self {
        Self {
            done: map::status(st, nb).into_iter().map(|(id, d, t)| (id, (d, t))).collect(),
            guides: nb.sources.iter().filter_map(|s| guide::load(st, nb, s).map(|g| (s.id.clone(), g))).collect(),
        }
    }
}

fn guide_text(g: &guide::Guide) -> String {
    let mut t = format!("## Überblick\n{}\n\n## Themen\n", g.overview);
    for x in &g.topics {
        let secs: Vec<String> = x.section_ids.iter().filter_map(|id| g.sections.iter().find(|s| &s.id == id)).map(|s| format!("{}{}", s.title, s.span())).collect();
        t.push_str(&format!("- {}: {}\n", x.title, secs.join("; ")));
    }
    t.push_str("\n## Abschnitte\n");
    for s in &g.sections {
        t.push_str(&format!("**{}{}**\n{}\n\n", s.title, s.span(), s.summary));
    }
    t
}

struct Nb {
    nb: Notebook,
    chat: Vec<Answer>,
    notes: Vec<Note>,
    pods: Vec<podcast::Pod>,
    q: String,
    pending: Option<String>,
    url: String,
    focus: String,
    len: podcast::Len,
    sugg: Vec<String>,
    kart: Kart,
}

struct App {
    st: Store,
    nbs: Vec<Notebook>,
    cur: Option<Nb>,
    jobs: Vec<Job>,
    new_name: String,
    err: Option<String>,
    info: Option<String>,
    view: Option<View>,
    player: Option<(PathBuf, Child)>,
    confirm_rm: Option<String>,
    /// (notebook id, input) waiting for the running source import of that notebook
    queued: Vec<(String, String)>,
    /// last rendered widths of Quellen / Chat / Studio (for --shot diagnostics)
    widths: [f32; 3],
}

fn store_clone(st: &Store) -> Store {
    Store { root: st.root.clone(), cfg: st.cfg.clone() }
}

impl App {
    fn new(st: Store) -> Self {
        let nbs = st.list().unwrap_or_default();
        Self { st, nbs, cur: None, jobs: vec![], new_name: String::new(), err: None, info: None, view: None, player: None, confirm_rm: None, queued: vec![], widths: [0.0; 3] }
    }

    fn open(&mut self, nb: Notebook) {
        let chat = self.st.chat(&nb).unwrap_or_default();
        let notes = self.st.notes(&nb).unwrap_or_default();
        let pods = podcast::list(&self.st, &nb);
        let kart = Kart::of(&self.st, &nb);
        self.cur = Some(Nb { nb, chat, notes, pods, q: String::new(), pending: None, url: String::new(), focus: String::new(), len: podcast::Len::Kurz, sugg: vec![], kart });
    }

    fn reload(&mut self) {
        self.nbs = self.st.list().unwrap_or_default();
        if let Some(c) = &mut self.cur
            && let Ok(nb) = self.st.get(&c.nb.id) {
                c.notes = self.st.notes(&nb).unwrap_or_default();
                c.pods = podcast::list(&self.st, &nb);
                c.kart = Kart::of(&self.st, &nb);
                c.nb = nb;
            }
    }

    fn spawn(&mut self, ctx: &egui::Context, label: String, f: impl FnOnce(&Store, &Notebook, &dyn Fn(&str)) -> anyhow::Result<Out> + Send + 'static) {
        let Some(c) = &self.cur else { return };
        let nb = c.nb.clone();
        self.spawn_nb(ctx, nb, label, f);
    }

    fn spawn_nb(&mut self, ctx: &egui::Context, nb: Notebook, label: String, f: impl FnOnce(&Store, &Notebook, &dyn Fn(&str)) -> anyhow::Result<Out> + Send + 'static) {
        let (tx, rx) = channel();
        let st = store_clone(&self.st);
        let ctx = ctx.clone();
        self.jobs.push(Job { nb: nb.id.clone(), label, last: String::new(), rx, cancel: None });
        std::thread::spawn(move || {
            let p = |s: &str| {
                let _ = tx.send(Msg::Prog(s.to_string()));
                ctx.request_repaint();
            };
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(&st, &nb, &p)))
                .unwrap_or_else(|_| Err(anyhow::anyhow!("interner Fehler")));
            let _ = tx.send(Msg::Done(r.map_err(|e| format!("{e:#}"))));
            ctx.request_repaint();
        });
    }

    fn busy(&self, kind: &str) -> bool {
        let id = self.cur.as_ref().map(|c| c.nb.id.as_str()).unwrap_or("");
        self.busy_nb(id, kind)
    }

    fn busy_nb(&self, id: &str, kind: &str) -> bool {
        self.jobs.iter().any(|j| j.nb == id && j.label.starts_with(kind))
    }

    fn poll(&mut self, ctx: &egui::Context) {
        let mut done = vec![];
        for (i, j) in self.jobs.iter_mut().enumerate() {
            while let Ok(m) = j.rx.try_recv() {
                match m {
                    Msg::Prog(s) => j.last = s,
                    Msg::Done(r) => done.push((i, j.nb.clone(), j.label.clone(), r)),
                }
            }
        }
        let mut drain = vec![];
        for (i, nb, label, r) in done.into_iter().rev() {
            self.jobs.remove(i);
            if label.starts_with("Quellen") {
                drain.push(nb.clone());
            }
            match r {
                Err(e) => {
                    self.err = Some(e);
                    if label == "Frage"
                        && let Some(c) = self.cur.as_mut().filter(|c| c.nb.id == nb)
                    {
                        c.pending = None;
                    }
                    self.reload();
                }
                Ok(out) => {
                    let mine = self.cur.as_ref().is_some_and(|c| c.nb.id == nb);
                    match out {
                        Out::Added(n) => self.info = Some(format!("{n} Quelle(n) hinzugefügt")),
                        Out::Answer(a) => {
                            if let Some(c) = self.cur.as_mut().filter(|_| mine) {
                                c.pending = None;
                                c.chat.push(a);
                            }
                        }
                        Out::Doc(d) => {
                            if mine {
                                self.view = Some(View::new(make::title(&d.kind), d.markdown, Some(d.path)));
                            }
                        }
                        Out::Pod(p) => {
                            self.info = Some(match p.warnings.len() {
                                0 => format!("Podcast fertig: {}", p.title),
                                n => format!("Podcast fertig: {} – {n} Hinweise im Skript", p.title),
                            })
                        }
                        Out::Mapped(m) => self.info = Some(m),
                        Out::Sugg(v) => {
                            if let Some(c) = self.cur.as_mut().filter(|_| mine) {
                                c.sugg = v;
                            }
                        }
                    }
                    self.reload();
                }
            }
        }
        for id in drain {
            let inputs: Vec<String> = self.queued.iter().filter(|q| q.0 == id).map(|q| q.1.clone()).collect();
            self.queued.retain(|q| q.0 != id);
            if let (false, Ok(nb)) = (inputs.is_empty(), self.st.get(&id)) {
                self.spawn_import(ctx, nb, inputs);
            }
        }
    }

    fn add_sources(&mut self, ctx: &egui::Context, inputs: Vec<String>) {
        if inputs.is_empty() {
            return;
        }
        let Some(nb) = self.cur.as_ref().map(|c| c.nb.clone()) else { return };
        // one import job per notebook at a time; the rest waits
        if self.busy_nb(&nb.id, "Quellen") {
            self.queued.extend(inputs.into_iter().map(|i| (nb.id.clone(), i)));
            self.info = Some("Import läuft – weitere Quellen sind eingereiht".into());
            return;
        }
        self.spawn_import(ctx, nb, inputs);
    }

    fn spawn_import(&mut self, ctx: &egui::Context, nb: Notebook, inputs: Vec<String>) {
        let label = format!("Quellen: {}", inputs.len());
        self.spawn_nb(ctx, nb, label, move |st, nb, p| {
            let mut nb = st.get(&nb.id)?;
            let mut n = 0;
            let mut errs = vec![];
            for i in &inputs {
                match st.add_source(&mut nb, i, p) {
                    Ok(v) => n += v.len(),
                    Err(e) => errs.push(format!("{i}: {e:#}")),
                }
            }
            if n == 0 && !errs.is_empty() {
                anyhow::bail!(errs.join("\n"));
            }
            Ok(Out::Added(n))
        });
    }

    /// Background map + source guide; cancel between chunks/sections, resumes from the cache.
    fn spawn_map(&mut self, ctx: &egui::Context) {
        let cancel = Arc::new(AtomicBool::new(false));
        let cc = cancel.clone();
        self.spawn(ctx, "Kartieren".into(), move |st, nb, p| {
            let m = map::run_with(st, nb, None, false, p, &cc)?;
            if m["cancelled"] == true {
                return Ok(Out::Mapped(format!("Kartieren abgebrochen – {} Abschnitte gespeichert", m["done"])));
            }
            p("Quellenübersicht …");
            guide::run_with(st, nb, None, p, &cc)?;
            Ok(Out::Mapped(format!("Kartiert: {} Abschnitte, Quellenübersicht fertig", m["chunks"])))
        });
        if let Some(j) = self.jobs.last_mut() {
            j.cancel = Some(cancel);
        }
    }

    fn ask(&mut self, ctx: &egui::Context, q: String) {
        if let Some(c) = self.cur.as_mut() {
            c.pending = Some(q.clone());
        }
        self.spawn(ctx, "Frage".into(), move |st, nb, p| Ok(Out::Answer(funkhaus_core::ask::ask(st, nb, &q, p)?)));
    }

    fn play(&mut self, p: &PathBuf) {
        self.stop();
        if let Ok(ch) = Command::new("afplay").arg(p).spawn() {
            self.player = Some((p.clone(), ch));
        }
    }

    fn stop(&mut self) {
        if let Some((_, mut ch)) = self.player.take() {
            let _ = ch.kill();
            let _ = ch.wait();
        }
    }

    fn playing(&mut self) -> Option<PathBuf> {
        if let Some((p, ch)) = &mut self.player {
            if matches!(ch.try_wait(), Ok(None)) {
                return Some(p.clone());
            }
            self.player = None;
        }
        None
    }

    fn home(&mut self, ctx: &egui::Context) {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.add_space(16.0);
            ui.heading(RichText::new("Funkhaus").size(30.0));
            ui.label("Deine Quellen. Deine Fragen. Dein Podcast. Alles lokal auf diesem Mac.");
            ui.add_space(16.0);
            ui.horizontal(|ui| {
                let r = ui.add(egui::TextEdit::singleline(&mut self.new_name).hint_text("Name des neuen Notebooks").desired_width(320.0));
                let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if (ui.button("+ Neues Notebook").clicked() || enter) && !self.new_name.trim().is_empty() {
                    match self.st.create(self.new_name.trim()) {
                        Ok(nb) => {
                            self.new_name.clear();
                            self.reload();
                            self.open(nb);
                        }
                        Err(e) => self.err = Some(e.to_string()),
                    }
                }
            });
            ui.add_space(16.0);
            let mut open = None;
            egui::ScrollArea::vertical().show(ui, |ui| {
                if self.nbs.is_empty() {
                    ui.label(RichText::new("Noch keine Notebooks. Lege oben eines an.").italics());
                }
                // Frame::show never wraps inside horizontal_wrapped (size unknown before layout) -> explicit rows
                let gap = ui.spacing().item_spacing.x;
                let cols = ((ui.available_width() + gap) / (240.0 + 2.0 * 12.0 + 2.0 + gap)).floor().max(1.0) as usize;
                for row in self.nbs.chunks(cols) {
                    ui.horizontal(|ui| {
                        for nb in row {
                            egui::Frame::group(ui.style()).inner_margin(12.0).show(ui, |ui| {
                                ui.set_width(240.0);
                                ui.vertical(|ui| {
                                    ui.label(RichText::new(&nb.name).strong().size(17.0));
                                    ui.label(format!("{} Quellen · {}", nb.sources.len(), nb.created.get(..10).unwrap_or("")));
                                    ui.horizontal(|ui| {
                                        if ui.button("Öffnen").clicked() {
                                            open = Some(nb.clone());
                                        }
                                        if self.confirm_rm.as_deref() == Some(&nb.id) {
                                            if ui.button(RichText::new("Wirklich löschen").color(theme::pal(ui).err)).clicked() {
                                                let _ = self.st.remove(nb);
                                                self.confirm_rm = None;
                                            }
                                        } else if ui.small_button("Löschen").clicked() {
                                            self.confirm_rm = Some(nb.id.clone());
                                        }
                                    });
                                });
                            });
                        }
                    });
                }
            });
            if let Some(nb) = open {
                self.open(nb);
            }
        });
        if self.confirm_rm.is_some() && !self.nbs.iter().any(|n| Some(&n.id) == self.confirm_rm.as_ref()) {
            self.confirm_rm = None;
        }
        if self.st.list().map(|v| v.len()).unwrap_or(0) != self.nbs.len() {
            self.reload();
        }
    }

    fn notebook(&mut self, ctx: &egui::Context) {
        let dropped: Vec<String> = ctx.input(|i| i.raw.dropped_files.iter().filter_map(|f| f.path.as_ref().map(|p| p.to_string_lossy().into_owned())).collect());
        if !dropped.is_empty() {
            self.add_sources(ctx, dropped);
        }
        let hovering = ctx.input(|i| !i.raw.hovered_files.is_empty());

        // exact widths derived from the window: egui's resizable panels persist their content width and ratchet wider in the real app
        let sw = ctx.content_rect().width();
        let l = egui::SidePanel::left("quellen")
            .exact_width((sw * 0.22).clamp(220.0, 340.0))
            .resizable(false)
            .show(ctx, |ui| self.sources(ui, ctx, hovering));
        let r = egui::SidePanel::right("studio")
            .exact_width((sw * 0.25).clamp(260.0, 380.0))
            .resizable(false)
            .show(ctx, |ui| self.studio(ui, ctx));
        let m = egui::CentralPanel::default().show(ctx, |ui| self.chat(ui, ctx));
        self.widths = [l.response.rect.width(), m.response.rect.width(), r.response.rect.width()];
    }

    fn sources(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, hovering: bool) {
        let mapping = self.jobs.iter().find(|j| self.cur.as_ref().is_some_and(|c| c.nb.id == j.nb) && j.label == "Kartieren").map(|j| (j.cancel.clone(), j.last.clone()));
        let mut start_map = false;
        rtl_row(ui, |ui| {
            match &mapping {
                Some((cancel, _)) => {
                    if ui.button("Abbrechen").on_hover_text("Kartieren anhalten – Fertiges bleibt gespeichert").clicked()
                        && let Some(c) = cancel {
                            c.store(true, Ordering::Relaxed);
                        }
                    ui.spinner();
                }
                None => {
                    let any = self.cur.as_ref().is_some_and(|c| !c.nb.sources.is_empty());
                    if ui.add_enabled(any, egui::Button::new("Kartieren")).on_hover_text("Abschnitte im Hintergrund kartieren und Quellenübersicht erstellen (lokales LLM, pausiert für Fragen)").clicked() {
                        start_map = true;
                    }
                }
            }
            ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| ui.heading("Quellen"));
        });
        if let Some((_, last)) = &mapping {
            ui.add(egui::Label::new(RichText::new(if last.is_empty() { "startet …" } else { last.as_str() }).weak().small()).truncate());
        }
        if start_map {
            self.spawn_map(ctx);
        }
        let mut add = vec![];
        ui.horizontal(|ui| {
            if ui.button("+ Dateien…").clicked()
                && let Some(v) = rfd::FileDialog::new().add_filter("Quellen", &["md", "txt", "pdf", "srt", "vtt", "html", "htm", "docx"]).pick_files() {
                    add.extend(v.into_iter().map(|p| p.to_string_lossy().into_owned()));
                }
            if ui.button("+ Ordner…").clicked()
                && let Some(p) = rfd::FileDialog::new().pick_folder() {
                    add.push(p.to_string_lossy().into_owned());
                }
        });
        let c = self.cur.as_mut().unwrap();
        rtl_row(ui, |ui| {
            let b = ui.button("Laden");
            let r = ui.add(egui::TextEdit::singleline(&mut c.url).hint_text("https://…").desired_width(ui.available_width()));
            let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if (b.clicked() || enter) && c.url.trim().starts_with("http") {
                add.push(c.url.trim().to_string());
                c.url.clear();
            }
        });
        let fill = if hovering { theme::pal(ui).drop } else { Color32::TRANSPARENT };
        card(ui, egui::Frame::group(ui.style()).fill(fill), |ui| {
            ui.add(egui::Label::new(RichText::new(if hovering { "Loslassen zum Hinzufügen" } else { "Dateien oder Ordner hierher ziehen" }).weak()).wrap());
        });
        ui.separator();
        let mut toggle = None;
        let mut rm = None;
        let mut show = None;
        let mut info = None;
        let mut lvl = None;
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            if c.nb.sources.is_empty() {
                ui.label(RichText::new("Noch keine Quellen.").italics());
            }
            for s in &c.nb.sources {
                ui.horizontal(|ui| {
                    let mut on = s.enabled;
                    if ui.checkbox(&mut on, "").changed() {
                        toggle = Some((s.id.clone(), on));
                    }
                    if let Some(g) = c.kart.guides.get(&s.id)
                        && ui.small_button("ℹ").on_hover_text("Quellenübersicht").clicked() {
                            info = Some((s.title.clone(), g.clone()));
                        }
                    let r = ui.add(egui::Label::new(format!("{} · {}", s.title, s.kind)).truncate().sense(egui::Sense::click()));
                    if r.clicked() {
                        show = Some(s.clone());
                    }
                    r.on_hover_text(format!("{}\n{} Zeichen · {}", s.origin, s.chars, s.id)).context_menu(|ui| {
                        if ui.button("Entfernen").clicked() {
                            rm = Some(s.id.clone());
                            ui.close();
                        }
                    });
                });
                ui.horizontal(|ui| {
                    ui.add_space(ui.spacing().interact_size.y);
                    let mut l = s.ctx;
                    egui::ComboBox::from_id_salt(("ctx", &s.id)).selected_text(l.label()).width(90.0).show_ui(ui, |ui| {
                        for x in [Ctx::Aus, Ctx::Kurz, Ctx::Voll] {
                            ui.selectable_value(&mut l, x, x.label());
                        }
                    }).response.on_hover_text("Kontextstufe: Aus = nur Rohtext, Kurzfassung = Zusammenfassungen, Voll = alles");
                    if l != s.ctx {
                        lvl = Some((s.id.clone(), l));
                    }
                    let mut st = match c.kart.done.get(&s.id) {
                        Some((d, t)) if d == t && *t > 0 => "kartiert".to_string(),
                        Some((d, t)) => format!("{d}/{t} kartiert"),
                        None => String::new(),
                    };
                    if s.outcut > 0 {
                        st.push_str(&format!(" · {} ausgeblendet", s.outcut));
                    }
                    ui.add(egui::Label::new(RichText::new(st).weak().small()).truncate());
                });
                ui.add_space(2.0);
            }
        });
        if let Some((id, l)) = lvl
            && let Err(e) = self.st.set_ctx(&mut c.nb, &id, l) {
                self.err = Some(e.to_string());
            }
        if let Some((t, g)) = info {
            self.view = Some(View { title: format!("Quellenübersicht · {t}"), text: guide_text(&g), path: None, questions: g.suggested_questions.clone() });
        }
        if let Some((id, on)) = toggle
            && let Err(e) = self.st.toggle(&mut c.nb, &id, on) {
                self.err = Some(e.to_string());
            }
        if let Some(id) = rm
            && let Err(e) = self.st.rm_source(&mut c.nb, &id) {
                self.err = Some(e.to_string());
            }
        if let Some(s) = show {
            let text = self.st.text(&c.nb, &s.id).unwrap_or_default();
            self.view = Some(View::new(s.title, text, (!s.origin.starts_with("http")).then(|| PathBuf::from(&s.origin))));
        }
        self.add_sources(ctx, add);
    }

    fn chat(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let asking = self.busy("Frage");
        let gpu_wait = self.jobs.iter().any(|j| j.label == "Frage" && j.last.contains("wartet auf GPU"));
        let sugg_busy = self.busy("Vorschläge");
        let mut sugg: Option<String> = None;
        let mut want_sugg = false;
        let mut cite = None;
        let mut note = None;
        let c = self.cur.as_mut().unwrap();
        let active = c.nb.sources.iter().filter(|s| s.enabled).count();
        ui.horizontal(|ui| {
            ui.heading("Chat");
            ui.label(RichText::new(format!("{active} von {} Quellen aktiv", c.nb.sources.len())).weak());
        });
        ui.separator();
        let mut submit = None;
        egui::TopBottomPanel::bottom("frage").frame(egui::Frame::new()).show_inside(ui, |ui| {
            ui.add_space(6.0);
            rtl_row(ui, |ui| {
                let b = ui.add_enabled(!asking && active > 0, egui::Button::new("Fragen"));
                let r = ui.add_enabled(!asking, egui::TextEdit::singleline(&mut c.q).hint_text("Frage zu den Quellen …").desired_width(ui.available_width()));
                let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if (b.clicked() || enter) && !c.q.trim().is_empty() && !asking && active > 0 {
                    submit = Some(std::mem::take(&mut c.q).trim().to_string());
                }
            });
            ui.label(RichText::new("KI kann ungenau sein – Zitate prüfen.").weak().small());
        });
        egui::ScrollArea::vertical().auto_shrink([false, false]).stick_to_bottom(true).show(ui, |ui| {
            if c.chat.is_empty() && c.pending.is_none() {
                ui.add(egui::Label::new(RichText::new("Stell eine Frage zu deinen Quellen. Antworten enthalten klickbare Zitate [1].").italics()).wrap());
            }
            for (k, a) in c.chat.iter().enumerate() {
                bubble(ui, &a.q, true);
                ui.add_space(4.0);
                card(ui, egui::Frame::group(ui.style()).inner_margin(10.0), |ui| {
                    if let Some(n) = rich(ui, &a.text, &a.cites.iter().map(|c| c.n).collect::<Vec<_>>()) {
                        cite = a.cites.iter().find(|c| c.n == n).cloned();
                    }
                    ui.horizontal_wrapped(|ui| {
                        for ci in &a.cites {
                            if ui.small_button(match &ci.at {
                                Some(t) => format!("[{}] {} · {t}", ci.n, funkhaus_sys::clip(&ci.title, 20)),
                                None => format!("[{}] {}", ci.n, funkhaus_sys::clip(&ci.title, 24)),
                            }).on_hover_text(&ci.excerpt).clicked() {
                                cite = Some(ci.clone());
                            }
                        }
                    });
                    if ui.small_button("📌 Als Notiz speichern").clicked() {
                        note = Some(k);
                    }
                });
                ui.add_space(10.0);
            }
            if let Some(q) = &c.pending {
                bubble(ui, q, true);
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(if gpu_wait { "wartet auf GPU …" } else { "denkt nach …" });
                });
            } else if !asking {
                ui.horizontal_wrapped(|ui| {
                    for q in &c.sugg {
                        if ui.button(q).clicked() {
                            sugg = Some(q.clone());
                        }
                    }
                    if active > 0 && !sugg_busy && ui.small_button("💡 Fragen vorschlagen").clicked() {
                        want_sugg = true;
                    }
                });
            }
        });
        if let Some(q) = &sugg {
            c.sugg.retain(|x| x != q);
        }
        if let Some(q) = submit.or(sugg) {
            self.ask(ctx, q);
        }
        let c = self.cur.as_mut().unwrap();
        if let Some(k) = note {
            let a = &c.chat[k];
            if self.st.add_note(&c.nb, &a.q, &a.text).is_ok() {
                c.notes = self.st.notes(&c.nb).unwrap_or_default();
                self.info = Some("Als Notiz gespeichert".into());
            }
        }
        if want_sugg {
            self.spawn(ctx, "Vorschläge".into(), |st, nb, _| Ok(Out::Sugg(funkhaus_core::ask::suggest(st, nb)?)));
        }
        let c = self.cur.as_mut().unwrap();
        if let Some(ci) = cite {
            let text = self.st.chunk(&c.nb, &ci.chunk).map(|c| c.text).unwrap_or_else(|_| ci.excerpt.clone());
            self.view = Some(View::new(format!("[{}] {} · {}", ci.n, ci.title, ci.loc()), text, None));
        }
    }

    fn studio(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.heading("Studio");
        ui.separator();
        let pod_busy = self.busy("Podcast");
        let doc_busy = self.busy("Dokument");
        let playing = self.playing();
        let mut job: Option<(String, podcast::Opts)> = None;
        let mut doc = None;
        let mut play = None;
        let mut stop = false;
        let mut view = None;
        let importing = self.busy("Quellen");
        let host = (self.st.cfg.hosts.o.name.clone(), self.st.cfg.hosts.f.name.clone());
        let c = self.cur.as_mut().unwrap();
        card(ui, egui::Frame::group(ui.style()).inner_margin(10.0), |ui| {
            ui.label(RichText::new("🎤 Audio-Zusammenfassung").strong());
            ui.add(egui::TextEdit::multiline(&mut c.focus).hint_text("Fokus (optional), z. B. „nur die Kosten“").desired_rows(2).desired_width(ui.available_width()));
            ui.horizontal_wrapped(|ui| {
                ui.label("Länge:");
                for (l, s) in [(podcast::Len::Kurz, "kurz"), (podcast::Len::Mittel, "mittel"), (podcast::Len::Lang, "lang")] {
                    ui.selectable_value(&mut c.len, l, s);
                }
            });
            let why = if c.nb.sources.is_empty() {
                Some("erst Quellen hinzufügen")
            } else if importing {
                Some("Import läuft …")
            } else if pod_busy {
                Some("läuft im Hintergrund – du kannst weiterarbeiten")
            } else {
                None
            };
            let r = ui.add_enabled(why.is_none(), egui::Button::new("Podcast erzeugen"));
            if r.clicked() {
                let focus = Some(c.focus.trim().to_string()).filter(|s| !s.is_empty());
                job = Some(("Podcast".into(), podcast::Opts { focus, len: c.len, ..Default::default() }));
            }
            if let Some(w) = why {
                r.on_disabled_hover_text(w);
                ui.add(egui::Label::new(RichText::new(w).weak()).wrap());
            }
        });
        ui.add_space(6.0);
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            card(ui, egui::Frame::group(ui.style()).inner_margin(10.0), |ui| {
                ui.label(RichText::new(format!("Podcasts ({})", c.pods.len())).strong());
                if c.pods.is_empty() {
                    ui.label(RichText::new("Noch keine.").weak());
                }
                for p in c.pods.iter().take(8) {
                    ui.separator();
                    ui.add(egui::Label::new(RichText::new(&p.title).strong()).truncate()).on_hover_text(&p.title);
                    let dn = p.dir.file_name().unwrap_or_default().to_string_lossy();
                    ui.add(egui::Label::new(RichText::new(dn.as_ref()).weak().small()).truncate()).on_hover_text(p.dir.to_string_lossy());
                    ui.horizontal_wrapped(|ui| {
                        if let Some(m) = &p.mp3 {
                            if playing.as_ref() == Some(m) {
                                if ui.button("⏹ Stopp").clicked() {
                                    stop = true;
                                }
                            } else if ui.button("▶ Abspielen").clicked() {
                                play = Some(m.clone());
                            }
                        } else {
                            ui.label(RichText::new("kein Audio").weak());
                        }
                        if ui.button("Im Finder").clicked() {
                            let target = p.mp3.clone().unwrap_or_else(|| p.script.clone());
                            let _ = Command::new("open").arg("-R").arg(target).spawn();
                        }
                        if ui.button("Skript").clicked() {
                            let mut text = script_text(&std::fs::read_to_string(&p.script).unwrap_or_default(), &host);
                            if !p.warnings.is_empty() {
                                text = format!("Hinweise ({}):\n{}\n\n{text}", p.warnings.len(), p.warnings.join("\n"));
                            }
                            view = Some(View::new(format!("Skript · {}", p.title), text, Some(p.script.clone())));
                        }
                        // resumes after a TTS/mix failure; already rendered lines are reused
                        if p.mp3.is_none() && !pod_busy && ui.button("Erneut rendern").clicked() {
                            job = Some(("Podcast".into(), podcast::Opts { from_script: Some(p.script.clone()), ..Default::default() }));
                        }
                    });
                    if !p.warnings.is_empty() {
                        ui.label(RichText::new(format!("⚠ {} Hinweise im Skript", p.warnings.len())).small().color(theme::pal(ui).warn));
                    }
                }
            });
            ui.add_space(8.0);
            card(ui, egui::Frame::group(ui.style()).inner_margin(10.0), |ui| {
                ui.label(RichText::new("📄 Dokumente").strong());
                ui.horizontal_wrapped(|ui| {
                    for k in make::KINDS {
                        if ui.add_enabled(!doc_busy && !c.nb.sources.is_empty(), egui::Button::new(make::title(k))).clicked() {
                            doc = Some(k.to_string());
                        }
                    }
                });
            });
            ui.add_space(8.0);
            card(ui, egui::Frame::group(ui.style()).inner_margin(10.0), |ui| {
                ui.label(RichText::new(format!("📝 Notizen ({})", c.notes.len())).strong());
                let mut rm = None;
                for n in &c.notes {
                    ui.horizontal(|ui| {
                        if ui.small_button("✖").on_hover_text("Notiz löschen").clicked() {
                            rm = Some(n.id.clone());
                        }
                        if ui.add(egui::Label::new(n.title.as_str()).truncate().sense(egui::Sense::click())).on_hover_text(&n.title).clicked() {
                            view = Some(View::new(n.title.clone(), n.text.clone(), None));
                        }
                    });
                }
                if let Some(id) = rm {
                    let _ = self.st.rm_note(&c.nb, &id);
                    c.notes = self.st.notes(&c.nb).unwrap_or_default();
                }
            });
            ui.add_space(8.0);
            egui::CollapsingHeader::new("⚙ Einstellungen").show(ui, |ui| {
                let mut ext = c.nb.backend == Backend::Claude;
                if ui.checkbox(&mut ext, "Claude (extern) erlauben").changed()
                    && let Err(e) = self.st.set_backend(&mut c.nb, if ext { Backend::Claude } else { Backend::Local }) {
                        self.err = Some(e.to_string());
                    }
                ui.add(egui::Label::new(RichText::new("Achtung: Dann verlassen Quelltexte dieses Notebooks den Mac. Standard ist das lokale Modell.").weak().small()).wrap());
                ui.add(egui::Label::new(RichText::new(format!("LLM: {}", self.st.cfg.llm.url)).weak().small()).truncate());
                if ui.small_button("Datenordner öffnen").clicked() {
                    let _ = Command::new("open").arg(self.st.dir(&c.nb.id)).spawn();
                }
            });
        });
        if let Some(v) = view {
            self.view = Some(v);
        }
        if stop {
            self.stop();
        }
        if let Some(p) = play {
            self.play(&p);
        }
        if let Some((label, o)) = job {
            self.spawn(ctx, label, move |st, nb, p| Ok(Out::Pod(podcast::run(st, nb, &o, p)?)));
        }
        if let Some(k) = doc {
            self.spawn(ctx, format!("Dokument: {}", make::title(&k)), move |st, nb, p| Ok(Out::Doc(make::make(st, nb, &k, p)?)));
        }
    }

    fn viewer(&mut self, ctx: &egui::Context) {
        let Some(v) = &self.view else { return };
        let mut open = true;
        let mut q = None;
        let asking = self.busy("Frage");
        let scr = ctx.content_rect().size();
        let max = scr * 0.9;
        egui::Window::new(&v.title)
            .open(&mut open)
            .default_size([560f32.min(max.x), 480f32.min(max.y)])
            .max_size(max)
            .constrain(true)
            .resizable(true)
            .show(ctx, |ui| {
            ui.set_max_width(ui.available_width());
            if let Some(p) = &v.path {
                rtl_row(ui, |ui| {
                    if ui.small_button("Im Finder").clicked() {
                        let _ = Command::new("open").arg("-R").arg(p).spawn();
                    }
                    ui.add(egui::Label::new(RichText::new(p.to_string_lossy()).weak().small()).truncate()).on_hover_text(p.to_string_lossy());
                });
            }
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                if !v.questions.is_empty() {
                    ui.label(RichText::new("Fragen an die Quelle").strong());
                    for x in &v.questions {
                        if ui.add_enabled(!asking, egui::Button::new(x).wrap()).clicked() {
                            q = Some(x.clone());
                        }
                    }
                    ui.add_space(8.0);
                }
                rich(ui, &v.text, &[]);
            });
        });
        if !open {
            self.view = None;
        }
        if let Some(x) = q {
            self.view = None;
            self.ask(ctx, x);
        }
    }
}

/// One widget row laid out right-to-left (button first, then a field filling the rest).
/// Unlike `with_layout`, this doesn't claim (and vertically center in) the whole remaining height.
fn rtl_row<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let size = egui::vec2(ui.available_width(), ui.spacing().interact_size.y);
    ui.allocate_ui_with_layout(size, egui::Layout::right_to_left(egui::Align::Center), add).inner
}

/// Frame that exactly fills the available width. Setting the inner width from *outside* the frame
/// (minus margins and stroke) keeps panels from growing a few pixels every frame.
fn card<R>(ui: &mut egui::Ui, f: egui::Frame, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let w = (ui.available_width() - f.total_margin().sum().x).max(0.0);
    f.show(ui, |ui| {
        ui.set_width(w);
        add(ui)
    })
    .inner
}

/// `WER|GAP|TEXT` → readable lines with host names; B = interjection by the other host.
fn script_text(raw: &str, host: &(String, String)) -> String {
    let Ok(lines) = funkhaus_audio::script::parse(raw) else { return raw.to_string() };
    lines
        .iter()
        .map(|l| {
            let n = if l.who == "O" { &host.0 } else { &host.1 };
            if l.over { format!("({n}, Einwurf) {}", l.text) } else { format!("{n}: {}", l.text) }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn bubble(ui: &mut egui::Ui, text: &str, user: bool) {
    ui.with_layout(egui::Layout::right_to_left(egui::Align::TOP), |ui| {
        let pal = theme::pal(ui);
        egui::Frame::new().fill(if user { pal.bubble } else { Color32::TRANSPARENT }).corner_radius(10.0).inner_margin(8.0).show(ui, |ui| {
            ui.set_max_width(ui.available_width() * 0.75);
            ui.add(egui::Label::new(RichText::new(text).color(pal.bubble_text)).wrap());
        });
    });
}

/// Renders light Markdown with `[n]` citations as links; returns the clicked citation number.
fn rich(ui: &mut egui::Ui, text: &str, nums: &[usize]) -> Option<usize> {
    let mut hit = None;
    for line in text.lines() {
        let t = line.trim_end();
        if t.is_empty() {
            ui.add_space(6.0);
            continue;
        }
        let (t, head) = match t.trim_start_matches('#') {
            s if s.len() < t.len() => (s.trim(), true),
            _ => (t, false),
        };
        let t = t.replace("**", "");
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            let mut rest = t.as_str();
            while !rest.is_empty() {
                let (plain, cite, after) = match rest.find('[').and_then(|a| rest[a..].find(']').map(|b| (a, a + b))) {
                    Some((a, b)) => {
                        let inner = &rest[a + 1..b];
                        let ns = funkhaus_core::ask::cite_nums(&rest[a..=b]);
                        if !ns.is_empty() && inner.chars().all(|c| c.is_ascii_digit() || ", -–".contains(c)) {
                            (&rest[..a], Some(ns), &rest[b + 1..])
                        } else {
                            (&rest[..=b], None, &rest[b + 1..])
                        }
                    }
                    None => (rest, None, ""),
                };
                for w in plain.split_inclusive(' ') {
                    ui.label(if head { RichText::new(w).strong().size(16.0) } else { RichText::new(w) });
                }
                for n in cite.unwrap_or_default() {
                    let known = nums.is_empty() || nums.contains(&n);
                    let r = ui.add(egui::Label::new(RichText::new(format!("[{n}]")).color(theme::pal(ui).accent).strong()).sense(egui::Sense::click()));
                    if known && r.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                        hit = Some(n);
                    }
                }
                rest = after;
            }
        });
    }
    hit
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _: &mut eframe::Frame) {
        self.poll(ctx);
        egui::TopBottomPanel::top("top").show(ctx, |ui| {
            ui.horizontal(|ui| {
                if self.cur.is_some() {
                    if ui.button("⬅ Notebooks").clicked() {
                        self.cur = None;
                        self.reload();
                    }
                    if let Some(c) = &self.cur {
                        ui.label(RichText::new(&c.nb.name).strong().size(16.0));
                    }
                } else {
                    ui.label(RichText::new("📻 Funkhaus").strong().size(16.0));
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let cur = ctx.options(|o| o.theme_preference);
                    let mut sel = cur;
                    egui::ComboBox::from_id_salt("theme")
                        .selected_text(match sel {
                            egui::ThemePreference::System => "🖥 System",
                            egui::ThemePreference::Light => "☀ Hell",
                            egui::ThemePreference::Dark => "🌙 Dunkel",
                        })
                        .width(110.0)
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut sel, egui::ThemePreference::System, "🖥 System");
                            ui.selectable_value(&mut sel, egui::ThemePreference::Light, "☀ Hell");
                            ui.selectable_value(&mut sel, egui::ThemePreference::Dark, "🌙 Dunkel");
                        });
                    if sel != cur {
                        ctx.set_theme(sel);
                        self.st.cfg.theme = theme::name(sel).into();
                        if let Err(e) = self.st.cfg.save(&self.st.root.join("config.toml")) {
                            self.err = Some(e.to_string());
                        }
                    }
                    for j in &self.jobs {
                        ui.label(RichText::new(funkhaus_sys::clip(&format!("{}: {}", j.label, j.last), 70)).small());
                        ui.spinner();
                    }
                });
            });
        });
        let mut clear = false;
        if self.err.is_some() || self.info.is_some() {
            egui::TopBottomPanel::bottom("msg").show(ctx, |ui| {
                ui.horizontal(|ui| {
                    if let Some(e) = &self.err {
                        ui.label(RichText::new(format!("Fehler: {e}")).color(theme::pal(ui).err));
                    } else if let Some(i) = &self.info {
                        ui.label(RichText::new(i).color(theme::pal(ui).ok));
                    }
                    if ui.small_button("✖").clicked() {
                        clear = true;
                    }
                });
            });
        }
        if clear {
            self.err = None;
            self.info = None;
        }
        if self.cur.is_some() { self.notebook(ctx) } else { self.home(ctx) }
        self.viewer(ctx);
        if !self.jobs.is_empty() || self.player.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(300));
        }
    }

    fn on_exit(&mut self, _: Option<&eframe::glow::Context>) {
        self.stop();
    }
}

fn main() -> eframe::Result {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("--write-icon") {
        let p = PathBuf::from(args.get(2).map(String::as_str).unwrap_or("icon.png"));
        icon::write_png(&p, 1024).expect("icon");
        return Ok(());
    }
    if args.get(1).map(String::as_str) == Some("--shot") {
        if let Err(e) = shot::run(&args[1..]) {
            eprintln!("Fehler: {e:#}");
            std::process::exit(1);
        }
        return Ok(());
    }
    if let Some(a) = args.get(1).filter(|a| !a.starts_with("-psn")) {
        // never open a window for unknown arguments
        println!("Funkhaus GUI\n  funkhaus-gui                 Fenster öffnen\n  {}\n  funkhaus-gui --write-icon OUT.png", shot::USAGE);
        std::process::exit(if a == "-h" || a == "--help" { 0 } else { 2 });
    }
    util::fix_path();
    let st = match Store::open() {
        Ok(st) => st,
        Err(e) => {
            rfd::MessageDialog::new()
                .set_level(rfd::MessageLevel::Error)
                .set_title("Funkhaus")
                .set_description(format!("Datenordner oder config.toml nicht lesbar:\n{e:#}"))
                .show();
            std::process::exit(1);
        }
    };
    let n = 256;
    let opts = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Funkhaus")
            .with_inner_size([1280.0, 820.0])
            .with_min_inner_size([900.0, 560.0])
            .with_icon(egui::IconData { rgba: icon::rgba(n), width: n, height: n }),
        ..Default::default()
    };
    eframe::run_native("Funkhaus", opts, Box::new(|cc| {
        theme::apply(&cc.egui_ctx, theme::pref(&st.cfg.theme));
        Ok(Box::new(App::new(st)))
    }))
}
