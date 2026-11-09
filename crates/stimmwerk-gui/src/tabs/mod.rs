//! Tabs: Skript | Stimmen | Stimme erzeugen | Musik | Export | Einstellungen (texts: docs/stimmwerk/ui-strings-de.md).
pub mod einstellungen;
pub mod erzeugen;
pub mod export;
pub mod llm;
pub mod musik;
pub mod prose;
pub mod skript;
pub mod start;
pub mod stimmen;

use crate::app::App;
use crate::theme;
use eframe::egui::{self, Color32, RichText};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tab {
    Skript,
    Stimmen,
    Erzeugen,
    Musik,
    Export,
    Einstellungen,
}

impl Tab {
    pub const ALL: [Tab; 6] = [Tab::Skript, Tab::Stimmen, Tab::Erzeugen, Tab::Musik, Tab::Export, Tab::Einstellungen];

    pub fn label(self) -> &'static str {
        match self {
            Tab::Skript => "Skript",
            Tab::Stimmen => "Stimmen",
            Tab::Erzeugen => "Stimme erzeugen",
            Tab::Musik => "Musik",
            Tab::Export => "Export",
            Tab::Einstellungen => "Einstellungen",
        }
    }

    pub fn tip(self) -> &'static str {
        match self {
            Tab::Skript => "Zeilen schreiben, Sprecher zuordnen, rendern (⌘1)",
            Tab::Stimmen => "Stimmen den Sprechern zuweisen und vorhören (⌘2)",
            Tab::Erzeugen => "Stimmen entwerfen oder klonen (⌘3)",
            Tab::Musik => "Musik-Cues auf Zeilen legen und mischen (⌘4)",
            Tab::Export => "Mix als WAV, MP3 oder Einzeldateien exportieren (⌘5)",
            Tab::Einstellungen => "Darstellung, Datenordner, omnivoice-Status, KI-Anbieter (⌘6)",
        }
    }

    /// `--tab` names for shots.
    pub fn parse(s: &str) -> Option<Tab> {
        Some(match s {
            "skript" => Tab::Skript,
            "stimmen" => Tab::Stimmen,
            "erzeugen" | "stimme-erzeugen" => Tab::Erzeugen,
            "musik" => Tab::Musik,
            "export" => Tab::Export,
            "einstellungen" | "einst" => Tab::Einstellungen,
            _ => return None,
        })
    }
}

pub fn num_key(k: usize) -> egui::Key {
    [egui::Key::Num1, egui::Key::Num2, egui::Key::Num3, egui::Key::Num4, egui::Key::Num5, egui::Key::Num6][k]
}

/// Rounded label chip.
pub fn chip(ui: &mut egui::Ui, text: &str, fill: Color32, fg: Color32, min_w: f32) -> egui::Response {
    egui::Frame::new()
        .fill(fill)
        .corner_radius(9.0)
        .inner_margin(egui::Margin::symmetric(8, 2))
        .show(ui, |ui| {
            ui.set_min_width(min_w);
            ui.label(RichText::new(text).color(fg).strong())
        })
        .inner
}

/// Card with fixed width (does not grow frame by frame).
pub fn card<R>(ui: &mut egui::Ui, f: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let w = ui.available_width();
    egui::Frame::new()
        .fill(theme::pal(ui).card)
        .stroke(ui.visuals().widgets.noninteractive.bg_stroke)
        .corner_radius(8.0)
        .inner_margin(14.0)
        .show(ui, |ui| {
            ui.set_width(w - 30.0);
            f(ui)
        })
        .inner
}

/// Display name of a voice: (label, tooltip). omnivoice builtins get a readable German label (id in the tooltip),
/// own voices their description if they have one.
pub fn voice_label(app: &App, name: &str) -> (String, String) {
    let bi = |l: &str| (l.to_string(), format!("omnivoice-Standard-Stimme „{name}“"));
    match name {
        _ if app.personas.iter().any(|p| p == name) => {
            let e = app.lib.iter().find(|e| e.name == name);
            let kind = match e.map(|e| e.kind.as_str()) {
                Some("design") => "entworfen",
                Some("clone") => "geklont",
                _ => "eigene Stimme",
            };
            match e.and_then(|e| e.description.clone()).filter(|d| !d.trim().is_empty()) {
                Some(d) => (format!("{name} – {}", clip_chars(&d, 28)), format!("Eigene Stimme „{name}“ ({kind}): {d}")),
                None => (name.to_string(), format!("Eigene Stimme „{name}“ ({kind})")),
            }
        }
        "gt" => bi("Oskar (tief, männlich)"),
        "fable" => bi("Fable (weiblich)"),
        "gerstaecker" => bi("Gerstäcker (Vorleser, tief)"),
        "kn" => bi("Standard (mittlere Lage)"),
        _ => (name.to_string(), format!("omnivoice-Stimme „{name}“")),
    }
}

/// German decimal comma, one digit (−16,0).
pub fn de1(x: f64) -> String {
    format!("{x:.1}").replace('-', "−").replace('.', ",")
}

/// Path with `~` for the home dir.
pub fn tilde(p: &std::path::Path) -> String {
    let h = funkhaus_sys::home();
    p.strip_prefix(&h).map_or_else(|_| p.display().to_string(), |r| format!("~/{}", r.display()))
}

/// At most `n` chars, `…` when cut.
pub fn clip_chars(s: &str, n: usize) -> String {
    if s.chars().count() <= n { s.into() } else { format!("{}…", s.chars().take(n - 1).collect::<String>()) }
}
