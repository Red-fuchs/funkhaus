pub(crate) use funkhaus_sys::{clip, home, mmss, which};
pub use funkhaus_sys::fix_path;

pub fn slug(s: &str) -> String {
    let mut out = String::new();
    for c in s.to_lowercase().chars() {
        let c = match c {
            'ä' => "ae".into(),
            'ö' => "oe".into(),
            'ü' => "ue".into(),
            'ß' => "ss".into(),
            c if c.is_ascii_alphanumeric() => c.to_string(),
            _ => "-".into(),
        };
        if c == "-" && (out.is_empty() || out.ends_with('-')) {
            continue;
        }
        out.push_str(&c);
    }
    let out = out.trim_end_matches('-');
    let out: String = out.chars().take(40).collect();
    if out.is_empty() { "notebook".into() } else { out }
}

pub fn now() -> String {
    chrono::Local::now().format("%Y-%m-%dT%H:%M:%S").to_string()
}

pub fn stamp() -> String {
    chrono::Local::now().format("%Y%m%d-%H%M%S").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn slugs() {
        assert_eq!(slug("Größe & Übung 2"), "groesse-uebung-2");
        assert_eq!(slug("  !! "), "notebook");
        assert_eq!(mmss(75.9), "1:15");
        assert_eq!(mmss(3725.0), "1:02:05");
    }
}
