//! Visuals (pattern of funkhaus' gui/theme.rs, own brass/walnut accent) + speaker chip palette + status colours.
use eframe::egui::{self, Color32, Theme, ThemePreference, Visuals};

pub fn pref(s: &str) -> ThemePreference {
    match s {
        "hell" | "light" => ThemePreference::Light,
        "dunkel" | "dark" => ThemePreference::Dark,
        _ => ThemePreference::System,
    }
}

pub fn name(p: ThemePreference) -> &'static str {
    match p {
        ThemePreference::Light => "hell",
        ThemePreference::Dark => "dunkel",
        ThemePreference::System => "system",
    }
}

const ACCENT_DARK: Color32 = Color32::from_rgb(226, 176, 96);
const ACCENT_LIGHT: Color32 = Color32::from_rgb(0x84, 0x4E, 0x1A);

fn dark() -> Visuals {
    let mut v = Visuals::dark();
    v.panel_fill = Color32::from_rgb(33, 32, 30);
    v.window_fill = Color32::from_rgb(40, 38, 35);
    v.extreme_bg_color = Color32::from_rgb(24, 23, 21);
    v.faint_bg_color = Color32::from_rgb(42, 40, 37);
    v.code_bg_color = Color32::from_rgb(46, 44, 40);
    v.override_text_color = Some(Color32::from_rgb(224, 219, 210));
    v.hyperlink_color = ACCENT_DARK;
    v.selection.bg_fill = Color32::from_rgb(110, 78, 38);
    v.selection.stroke.color = Color32::from_rgb(250, 228, 196);
    v.weak_text_color = Some(Color32::from_rgb(0xAA, 0xA5, 0x9C));
    v.widgets.noninteractive.bg_stroke.color = Color32::from_rgb(0x50, 0x4C, 0x46);
    v.widgets.inactive.bg_stroke.color = Color32::from_rgb(0x74, 0x70, 0x68); // ≥3:1 for controls
    v.widgets.inactive.bg_stroke.width = 1.0;
    v
}

fn light() -> Visuals {
    let mut v = Visuals::light();
    v.panel_fill = Color32::from_rgb(244, 240, 233);
    v.window_fill = Color32::from_rgb(0xF0, 0xEC, 0xE4);
    v.extreme_bg_color = Color32::from_rgb(252, 250, 246);
    v.faint_bg_color = Color32::from_rgb(235, 229, 219);
    v.code_bg_color = Color32::from_rgb(233, 227, 217);
    v.override_text_color = Some(Color32::from_rgb(46, 40, 34));
    v.hyperlink_color = ACCENT_LIGHT;
    v.selection.bg_fill = Color32::from_rgb(240, 210, 160);
    v.selection.stroke.color = Color32::from_rgb(84, 46, 12);
    v.weak_text_color = Some(Color32::from_rgb(0x6A, 0x64, 0x5A));
    let btn = Color32::from_rgb(0xE6, 0xDF, 0xD2);
    v.widgets.noninteractive.bg_stroke.color = Color32::from_rgb(0xC0, 0xB8, 0xAA);
    v.widgets.inactive.bg_stroke.color = Color32::from_rgb(0x8C, 0x84, 0x77); // ≥3:1 for controls
    v.widgets.inactive.bg_stroke.width = 1.0;
    v.widgets.inactive.bg_fill = btn;
    v.widgets.inactive.weak_bg_fill = btn;
    v
}

/// Installs both palettes; egui picks one per the preference (System follows macOS live).
pub fn apply(ctx: &egui::Context, p: ThemePreference) {
    ctx.set_visuals_of(Theme::Dark, dark());
    ctx.set_visuals_of(Theme::Light, light());
    ctx.set_theme(p);
}

/// Semantic colours for the current theme.
pub struct Pal {
    pub accent: Color32,
    pub warn: Color32,
    pub ok: Color32,
    pub err: Color32,
    pub card: Color32,
}

const PAL_DARK: Pal = Pal {
    accent: ACCENT_DARK,
    warn: Color32::from_rgb(232, 176, 80),
    ok: Color32::from_rgb(124, 200, 132),
    err: Color32::from_rgb(246, 124, 112),
    card: Color32::from_rgb(44, 42, 38),
};
const PAL_LIGHT: Pal = Pal {
    accent: ACCENT_LIGHT,
    warn: Color32::from_rgb(150, 88, 0),
    ok: Color32::from_rgb(28, 112, 48),
    err: Color32::from_rgb(176, 30, 30),
    card: Color32::from_rgb(250, 247, 241),
};

pub fn pal(ui: &egui::Ui) -> &'static Pal {
    if ui.visuals().dark_mode { &PAL_DARK } else { &PAL_LIGHT }
}

/// Speaker chip fills (cast order, cycling); text colour per theme.
const CHIPS_LIGHT: [Color32; 8] = [
    Color32::from_rgb(247, 212, 160),
    Color32::from_rgb(190, 222, 200),
    Color32::from_rgb(196, 214, 240),
    Color32::from_rgb(236, 196, 214),
    Color32::from_rgb(220, 206, 240),
    Color32::from_rgb(200, 228, 230),
    Color32::from_rgb(236, 224, 176),
    Color32::from_rgb(226, 206, 190),
];
const CHIPS_DARK: [Color32; 8] = [
    Color32::from_rgb(122, 82, 36),
    Color32::from_rgb(50, 100, 68),
    Color32::from_rgb(52, 80, 124),
    Color32::from_rgb(118, 58, 84),
    Color32::from_rgb(88, 66, 128),
    Color32::from_rgb(38, 96, 102),
    Color32::from_rgb(104, 90, 40),
    Color32::from_rgb(104, 70, 56),
];
const CHIP_TEXT_LIGHT: Color32 = Color32::from_rgb(40, 30, 20);
const CHIP_TEXT_DARK: Color32 = Color32::from_rgb(246, 240, 230);

/// (fill, text) of speaker chip `i` (cast index; unknown speakers get the last colour).
pub fn speaker(ui: &egui::Ui, i: usize) -> (Color32, Color32) {
    if ui.visuals().dark_mode { (CHIPS_DARK[i % 8], CHIP_TEXT_DARK) } else { (CHIPS_LIGHT[i % 8], CHIP_TEXT_LIGHT) }
}

/// `a` mixed with `b` (share `t` of `b`), opaque.
pub fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let m = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgb(m(a.r(), b.r()), m(a.g(), b.g()), m(a.b(), b.b()))
}

/// Fill of the selected script row: selection colour at 35 % over the panel (opaque, so text contrast is testable).
pub fn row_sel(ui: &egui::Ui) -> Color32 {
    let v = ui.visuals();
    mix(v.panel_fill, v.selection.bg_fill, 0.35)
}

/// macOS appearance, for headless rendering where no system theme is reported.
pub fn system_is_dark() -> bool {
    std::process::Command::new("defaults")
        .args(["read", "-g", "AppleInterfaceStyle"])
        .output()
        .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).trim() == "Dark")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lum(c: Color32) -> f64 {
        let f = |v: u8| {
            let v = v as f64 / 255.0;
            if v <= 0.03928 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
        };
        0.2126 * f(c.r()) + 0.7152 * f(c.g()) + 0.0722 * f(c.b())
    }

    fn ratio(a: Color32, b: Color32) -> f64 {
        let (x, y) = (lum(a), lum(b));
        (x.max(y) + 0.05) / (x.min(y) + 0.05)
    }

    #[test]
    fn wcag() {
        for (v, p, chips, ct) in [(light(), &PAL_LIGHT, CHIPS_LIGHT, CHIP_TEXT_LIGHT), (dark(), &PAL_DARK, CHIPS_DARK, CHIP_TEXT_DARK)] {
            let bg = v.panel_fill;
            assert!(ratio(v.override_text_color.unwrap(), bg) >= 4.5);
            assert!(ratio(v.weak_text_color.unwrap(), bg) >= 4.5, "weak {:.2}", ratio(v.weak_text_color.unwrap(), bg));
            assert!(ratio(v.widgets.inactive.bg_stroke.color, bg) >= 3.0, "stroke {:.2}", ratio(v.widgets.inactive.bg_stroke.color, bg));
            for (n, c) in [("accent", p.accent), ("warn", p.warn), ("ok", p.ok), ("err", p.err)] {
                assert!(ratio(c, bg) >= 4.5, "{n} {:.2}", ratio(c, bg));
                assert!(ratio(c, p.card) >= 4.5, "{n} on card {:.2}", ratio(c, p.card));
            }
            for (i, c) in chips.iter().enumerate() {
                assert!(ratio(ct, *c) >= 4.5, "chip {i} {:.2}", ratio(ct, *c));
            }
            let text = v.override_text_color.unwrap();
            // selected script row: text, weak text and state colours on the row fill
            let sel = mix(bg, v.selection.bg_fill, 0.35);
            for (n, c) in [("text", text), ("weak", v.weak_text_color.unwrap()), ("ok", p.ok), ("err", p.err), ("warn", p.warn), ("accent", p.accent)] {
                assert!(ratio(c, sel) >= 4.5, "{n} on selected row {:.2}", ratio(c, sel));
            }
            // button text on button fills (idle, hover, pressed) and selected tabs/toggles (selection stroke on fill)
            for (n, f) in [("idle", v.widgets.inactive.weak_bg_fill), ("hover", v.widgets.hovered.weak_bg_fill), ("active", v.widgets.active.weak_bg_fill)] {
                assert!(ratio(text, f) >= 4.5, "button text {n} {:.2}", ratio(text, f));
            }
            assert!(ratio(text, v.selection.bg_fill) >= 4.5, "text on selection {:.2}", ratio(text, v.selection.bg_fill));
            assert!(ratio(v.selection.stroke.color, v.selection.bg_fill) >= 4.5, "selection stroke {:.2}", ratio(v.selection.stroke.color, v.selection.bg_fill));
        }
    }
}
