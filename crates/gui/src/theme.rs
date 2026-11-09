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

const ACCENT_DARK: Color32 = Color32::from_rgb(232, 140, 72);
const ACCENT_LIGHT: Color32 = Color32::from_rgb(0xA8, 0x50, 0x1A);

fn dark() -> Visuals {
    let mut v = Visuals::dark();
    v.panel_fill = Color32::from_rgb(32, 34, 38);
    v.window_fill = Color32::from_rgb(38, 41, 46);
    v.extreme_bg_color = Color32::from_rgb(24, 26, 29);
    v.faint_bg_color = Color32::from_rgb(40, 43, 48);
    v.code_bg_color = Color32::from_rgb(44, 47, 53);
    v.override_text_color = Some(Color32::from_rgb(222, 219, 214));
    v.hyperlink_color = ACCENT_DARK;
    v.selection.bg_fill = Color32::from_rgb(120, 70, 36);
    v.selection.stroke.color = Color32::from_rgb(250, 220, 195);
    v.weak_text_color = Some(Color32::from_rgb(0xA9, 0xA7, 0xA2));
    let stroke = Color32::from_rgb(0x4A, 0x4F, 0x57);
    v.widgets.noninteractive.bg_stroke.color = stroke;
    v.widgets.inactive.bg_stroke.color = Color32::from_rgb(0x6E, 0x73, 0x7B); // ≥3:1 for controls
    v.widgets.inactive.bg_stroke.width = 1.0;
    v
}

fn light() -> Visuals {
    let mut v = Visuals::light();
    v.panel_fill = Color32::from_rgb(242, 239, 234);
    v.window_fill = Color32::from_rgb(0xEF, 0xEC, 0xE6);
    v.extreme_bg_color = Color32::from_rgb(251, 250, 247);
    v.faint_bg_color = Color32::from_rgb(234, 230, 223);
    v.code_bg_color = Color32::from_rgb(232, 228, 221);
    v.override_text_color = Some(Color32::from_rgb(48, 44, 40));
    v.hyperlink_color = ACCENT_LIGHT;
    v.selection.bg_fill = Color32::from_rgb(244, 205, 170);
    v.selection.stroke.color = Color32::from_rgb(90, 45, 10);
    v.weak_text_color = Some(Color32::from_rgb(0x6B, 0x66, 0x5F));
    let stroke = Color32::from_rgb(0xBD, 0xB6, 0xAB);
    let btn = Color32::from_rgb(0xE4, 0xDF, 0xD6);
    v.widgets.noninteractive.bg_stroke.color = stroke;
    v.widgets.inactive.bg_stroke.color = Color32::from_rgb(0x8C, 0x85, 0x7A); // ≥3:1 for controls
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
    pub bubble: Color32,
    pub bubble_text: Color32,
    pub drop: Color32,
    pub warn: Color32,
    pub ok: Color32,
    pub err: Color32,
}

pub fn pal(ui: &egui::Ui) -> Pal {
    if ui.visuals().dark_mode {
        Pal {
            accent: ACCENT_DARK,
            bubble: Color32::from_rgb(70, 52, 38),
            bubble_text: Color32::from_rgb(240, 225, 210),
            drop: Color32::from_rgb(78, 56, 38),
            warn: Color32::from_rgb(230, 170, 70),
            ok: Color32::from_rgb(120, 200, 130),
            err: Color32::from_rgb(240, 110, 100),
        }
    } else {
        Pal {
            accent: ACCENT_LIGHT,
            bubble: Color32::from_rgb(248, 224, 199),
            bubble_text: Color32::from_rgb(60, 40, 20),
            drop: Color32::from_rgb(252, 230, 206),
            warn: Color32::from_rgb(170, 100, 0),
            ok: Color32::from_rgb(30, 120, 50),
            err: Color32::from_rgb(180, 30, 30),
        }
    }
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
        for v in [light(), dark()] {
            let bg = v.panel_fill;
            assert!(ratio(v.override_text_color.unwrap(), bg) >= 4.5);
            assert!(ratio(v.weak_text_color.unwrap(), bg) >= 4.5, "weak {:.2}", ratio(v.weak_text_color.unwrap(), bg));
            assert!(ratio(v.hyperlink_color, bg) >= 4.5, "accent {:.2}", ratio(v.hyperlink_color, bg));
            assert!(ratio(v.widgets.inactive.bg_stroke.color, bg) >= 3.0, "stroke {:.2}", ratio(v.widgets.inactive.bg_stroke.color, bg));
        }
    }
}
