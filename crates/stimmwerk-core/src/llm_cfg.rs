//! App-level LLM settings: `<home>/settings.toml`, table `[llm]` with named backends and their online permission,
//! plus the resolution of a project's `[llm]` (name + consent) to a `Backend`. Keys live in the keychain (`KeyRef`),
//! never in this file. Built-in presets: local servers (default) and online providers (templates, disabled until the
//! user enables them and stores a key).
use anyhow::{Context, Result, bail};
use funkhaus_llm::{Backend, KeyRef, Kind, ONLINE_TIMEOUT_S, keychain};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub const FILE: &str = "settings.toml";
pub const DEFAULT: &str = "gemma";

/// One configured backend (same fields as `Backend`, spelled out so unknown keys are refused).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Entry {
    pub kind: Kind,
    pub url: String,
    /// Empty = first model from /models.
    pub model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key: Option<KeyRef>,
    /// Text of consenting projects may go to this host (only meaningful for non-loopback URLs).
    pub allow_online: bool,
    pub timeout_s: u64,
    pub max_tokens: u32,
    pub temperature: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gpu: Option<bool>,
}

impl Default for Entry {
    fn default() -> Self {
        let b = Backend::default();
        Entry { kind: b.kind, url: b.url, model: b.model, key: None, allow_online: false, timeout_s: b.timeout_s, max_tokens: b.max_tokens, temperature: b.temperature, gpu: b.gpu }
    }
}

impl Entry {
    pub fn backend(&self) -> Backend {
        Backend { kind: self.kind, url: self.url.clone(), model: self.model.clone(), api_key: None, key: self.key.clone(), timeout_s: self.timeout_s, max_tokens: self.max_tokens, temperature: self.temperature, gpu: self.gpu }
    }
    pub fn is_local(&self) -> bool {
        self.backend().is_local()
    }
    /// `keychain` | `env:VAR` | None – never the key.
    pub fn key_info(&self) -> Option<String> {
        self.key.as_ref().map(|k| match k {
            KeyRef::Keychain { .. } => "keychain".to_string(),
            KeyRef::Env(v) => format!("env:{v}"),
        })
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default, deny_unknown_fields)]
pub struct AppLlm {
    /// Backend for projects without `[llm] backend`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
    pub backends: BTreeMap<String, Entry>,
}

/// Whole settings file; other sections may follow, so only `[llm]` is strict.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default)]
pub struct Settings {
    pub llm: AppLlm,
}

pub struct Preset {
    pub name: &'static str,
    pub label: &'static str,
    pub kind: Kind,
    pub url: &'static str,
    pub model: &'static str,
}

/// Templates. Local ones are configured by default; online ones only via `add` (+ `allow_online` + key).
pub const PRESETS: &[Preset] = &[
    Preset { name: "gemma", label: "gemma-serve (lokal, :8095)", kind: Kind::OpenAi, url: "http://127.0.0.1:8095/v1", model: "models/gemma-4-26B-A4B-it-JANG_4M" },
    Preset { name: "vmlx", label: "vMLX (lokal, :8080)", kind: Kind::OpenAi, url: "http://127.0.0.1:8080/v1", model: "" },
    Preset { name: "lmstudio", label: "LM Studio (lokal, :1234)", kind: Kind::OpenAi, url: "http://127.0.0.1:1234/v1", model: "" },
    Preset { name: "ollama", label: "Ollama (lokal, :11434)", kind: Kind::OpenAi, url: "http://127.0.0.1:11434/v1", model: "" },
    Preset { name: "anthropic", label: "Anthropic (online, Schlüssel nötig)", kind: Kind::Anthropic, url: funkhaus_llm::ANTHROPIC_URL, model: funkhaus_llm::ANTHROPIC_MODEL },
    Preset { name: "openai", label: "OpenAI (online, Schlüssel nötig)", kind: Kind::OpenAi, url: "https://api.openai.com/v1", model: "gpt-4o-mini" },
    Preset { name: "openrouter", label: "OpenRouter (online, Schlüssel nötig; Modell aus `llm test`)", kind: Kind::OpenAi, url: "https://openrouter.ai/api/v1", model: "" },
];

impl Preset {
    pub fn get(name: &str) -> Option<&'static Preset> {
        PRESETS.iter().find(|p| p.name == name)
    }
    pub fn online(&self) -> bool {
        !Backend { url: self.url.into(), ..Backend::default() }.is_local()
    }
    pub fn entry(&self) -> Entry {
        Entry { kind: self.kind, url: self.url.into(), model: self.model.into(), timeout_s: if self.online() { ONLINE_TIMEOUT_S } else { 900 }, ..Entry::default() }
    }
}

/// Backend names double as keychain accounts: letters, digits, `_ - .`.
pub fn name_ok(n: &str) -> bool {
    !n.is_empty() && n.len() <= 40 && n.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
}

impl AppLlm {
    /// Fresh settings: all local presets, `gemma` as default.
    pub fn builtin() -> Self {
        AppLlm { default: Some(DEFAULT.into()), backends: PRESETS.iter().filter(|p| !p.online()).map(|p| (p.name.to_string(), p.entry())).collect() }
    }

    pub fn check(&self) -> Result<()> {
        for (n, e) in &self.backends {
            if !name_ok(n) {
                bail!("Backend-Name {n:?} ungültig (Buchstaben, Ziffern, _ - .)");
            }
            if e.url.trim().is_empty() {
                bail!("Backend {n}: url fehlt");
            }
            if e.kind == Kind::Anthropic && e.model.is_empty() {
                bail!("Backend {n}: Anthropic braucht ein Modell (z. B. {})", funkhaus_llm::ANTHROPIC_MODEL);
            }
        }
        if let Some(d) = &self.default
            && !self.backends.contains_key(d)
        {
            bail!("default = {d:?} ist kein konfiguriertes Backend");
        }
        Ok(())
    }

    pub fn get(&self, name: &str) -> Result<&Entry> {
        self.backends.get(name).with_context(|| format!("LLM-Backend {name:?} ist nicht konfiguriert – `stimmwerk llm ls` zeigt Backends und Vorlagen, `stimmwerk llm add {name}` legt es an"))
    }

    /// Name of the backend a project uses.
    pub fn name_for(&self, p: Option<&crate::project::ProjectLlm>) -> Result<String> {
        p.and_then(|p| p.backend.clone()).or_else(|| self.default.clone()).context("kein LLM-Backend gewählt: `stimmwerk llm use <projekt> <backend>` oder `default` in settings.toml")
    }
}

pub fn path() -> PathBuf {
    crate::home().join(FILE)
}

pub fn load() -> Result<Settings> {
    load_from(&path())
}

/// Missing file = built-in local presets.
pub fn load_from(p: &Path) -> Result<Settings> {
    let s = match std::fs::read_to_string(p) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Settings { llm: AppLlm::builtin() }),
        Err(e) => return Err(e).with_context(|| p.display().to_string()),
    };
    let st: Settings = toml::from_str(&s).with_context(|| p.display().to_string())?;
    st.llm.check().with_context(|| p.display().to_string())?;
    Ok(st)
}

pub fn save(st: &Settings) -> Result<()> {
    save_to(&path(), st)
}

pub fn save_to(p: &Path, st: &Settings) -> Result<()> {
    st.llm.check()?;
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d)?;
    }
    let new = toml::to_string_pretty(st)?;
    let out = std::fs::read_to_string(p).ok().and_then(|old| crate::project::merge_toml(&old, &new)).filter(|m| toml::from_str::<Settings>(m).is_ok_and(|m| m == *st)).unwrap_or(new);
    crate::project::write(p, &out)
}

/// A project's backend with the effective online consent (project `online_ok` AND backend `allow_online`).
#[derive(Clone, Debug)]
pub struct Resolved {
    pub name: String,
    pub entry: Entry,
    pub backend: Backend,
    /// Project switch.
    pub project_ok: bool,
    /// Effective: both switches, or loopback.
    pub online_ok: bool,
}

impl Resolved {
    pub fn is_local(&self) -> bool {
        self.backend.is_local()
    }

    /// Refusal with the switch that is missing (loopback always passes). Run before any LLM call.
    pub fn allowed(&self) -> Result<()> {
        if self.is_local() {
            return Ok(());
        }
        let (n, host) = (&self.name, self.backend.host());
        if !self.project_ok {
            bail!("Online-Anbieter „{n}“ ({host}) ist für dieses Projekt nicht freigegeben – der Text würde den Mac verlassen. Freigeben: `stimmwerk llm use <projekt> {n} --online-ok` (keine Personendaten senden)");
        }
        if !self.entry.allow_online {
            bail!("Online-Anbieter „{n}“ ({host}) ist in den Einstellungen nicht erlaubt – erlauben: `stimmwerk llm add {n} --allow-online`, Schlüssel: `stimmwerk llm key {n}`");
        }
        funkhaus_llm::allowed(&self.backend, self.online_ok)
    }

    /// Personal-data warning for online use (None for loopback).
    pub fn warning(&self) -> Option<String> {
        (!self.is_local()).then(|| format!("Der Text geht an {} („{}“) und verlässt den Mac – keine Personendaten senden", self.backend.host(), self.name))
    }
}

/// Project `[llm]` (or the app default) -> backend. Unknown names are an error, never a URL fallback.
pub fn resolve(app: &AppLlm, p: Option<&crate::project::ProjectLlm>) -> Result<Resolved> {
    let name = app.name_for(p)?;
    let entry = app.get(&name)?.clone();
    let backend = entry.backend();
    let project_ok = p.is_some_and(|p| p.online_ok);
    let online_ok = backend.is_local() || (project_ok && entry.allow_online);
    Ok(Resolved { name, entry, backend, project_ok, online_ok })
}

/// Stores `key` for backend `name` in the keychain (service `stimmwerk-llm`, account = name) and points the entry at it.
pub fn set_key(st: &mut Settings, name: &str, key: &str) -> Result<()> {
    set_key_with(&keychain::bin(), st, name, key)
}

/// `set_key` through the `security` binary `bin` (tests: the fake).
pub fn set_key_with(bin: &str, st: &mut Settings, name: &str, key: &str) -> Result<()> {
    let e = st.llm.backends.get_mut(name).with_context(|| format!("LLM-Backend {name:?} ist nicht konfiguriert"))?;
    keychain::save_with(bin, keychain::SERVICE, name, key.trim())?;
    e.key = Some(KeyRef::Keychain { service: keychain::SERVICE.into(), account: name.into() });
    Ok(())
}

/// Drops the key reference of `name`; the keychain item is deleted only if the entry points at one (`security` is never
/// called for a backend without a key, e.g. a local one). Unknown backend: nothing to do.
pub fn clear_key(st: &mut Settings, name: &str) -> Result<()> {
    clear_key_with(&keychain::bin(), st, name)
}

pub fn clear_key_with(bin: &str, st: &mut Settings, name: &str) -> Result<()> {
    let Some(e) = st.llm.backends.get_mut(name) else { return Ok(()) };
    if let Some(KeyRef::Keychain { service, account }) = &e.key {
        keychain::clear_with(bin, service, account)?;
    }
    e.key = None;
    Ok(())
}

/// A settings file that does not parse is copied to `settings.toml.broken` (first free name), so a later save never
/// destroys the user's text; returns the backup path.
pub fn backup_broken(p: &Path) -> Result<PathBuf> {
    let b = (0..).map(|k| if k == 0 { p.with_extension("toml.broken") } else { p.with_extension(format!("toml.broken.{k}")) }).find(|b| !b.exists()).expect("free name");
    std::fs::copy(p, &b).with_context(|| b.display().to_string())?;
    Ok(b)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::ProjectLlm;

    fn app(toml_src: &str) -> Result<AppLlm> {
        let st: Settings = toml::from_str(toml_src)?;
        st.llm.check()?;
        Ok(st.llm)
    }

    #[test]
    fn builtin_and_presets() {
        let a = AppLlm::builtin();
        a.check().unwrap();
        assert_eq!(a.default.as_deref(), Some("gemma"));
        assert!(a.backends.values().all(|e| e.is_local() && !e.allow_online && e.key.is_none()));
        assert_eq!(a.backends.len(), 4);
        let on: Vec<_> = PRESETS.iter().filter(|p| p.online()).map(|p| p.name).collect();
        assert_eq!(on, ["anthropic", "openai", "openrouter"]);
        let c = Preset::get("anthropic").unwrap().entry();
        assert_eq!((c.kind, c.timeout_s, c.allow_online, c.model.as_str()), (Kind::Anthropic, ONLINE_TIMEOUT_S, false, "claude-haiku-5-5"));
        assert_eq!(Preset::get("gemma").unwrap().entry().timeout_s, 900);
        assert!(Preset::get("nope").is_none());
    }

    #[test]
    fn settings_parse_check_roundtrip() {
        let a = app("[llm]\ndefault = \"claude\"\n[llm.backends.claude]\nkind = \"anthropic\"\nurl = \"https://api.anthropic.com/v1\"\nmodel = \"claude-haiku-5-5\"\nallow_online = true\nkey = { keychain = { service = \"stimmwerk-llm\", account = \"claude\" } }\n").unwrap();
        let c = &a.backends["claude"];
        assert!(c.allow_online && !c.is_local() && c.key_info().as_deref() == Some("keychain"));
        let b = c.backend();
        assert_eq!((b.kind, b.key.clone()), (Kind::Anthropic, Some(KeyRef::Keychain { service: "stimmwerk-llm".into(), account: "claude".into() })));
        assert!(!app("[llm]\n[llm.backends.x]\nurl = \"http://h/v1\"\n").unwrap().backends["x"].allow_online);
        for bad in [
            "[llm]\n[llm.backends.x]\nurl = \"http://h/v1\"\nallow_onlin = true\n",
            "[llm]\n[llm.backends.x]\nurl = \"http://h/v1\"\napi_key = \"sk-x\"\n",
            "[llm]\n[llm.backends.x]\nurl = \"http://h/v1\"\nkind = \"gpt-x\"\n",
            "[llm]\n[llm.backends.x]\nurl = \"\"\n",
            "[llm]\ndefault = \"nope\"\n[llm.backends.x]\nurl = \"http://h/v1\"\n",
            "[llm]\n[llm.backends.x]\nurl = \"http://h/v1\"\n[llm.backends.x]\nurl = \"http://h2/v1\"\n",
            "[llm]\n[llm.backends.\"a b\"]\nurl = \"http://h/v1\"\n",
            "[llm]\n[llm.backends.a]\nkind = \"anthropic\"\nurl = \"https://api.anthropic.com/v1\"\nmodel = \"\"\n",
            "[llm]\nfoo = 1\n",
        ] {
            assert!(app(bad).is_err(), "{bad}");
        }
        // save/load: no key material, missing file = builtin
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("s").join(FILE);
        assert_eq!(load_from(&p).unwrap().llm, AppLlm::builtin());
        let mut st = Settings { llm: a };
        st.llm.backends.get_mut("claude").unwrap().key = Some(KeyRef::Env("STIMMWERK_TEST_KEY".into()));
        save_to(&p, &st).unwrap();
        let s = std::fs::read_to_string(&p).unwrap();
        assert!(s.contains("allow_online = true") && s.contains("env = \"STIMMWERK_TEST_KEY\"") && !s.contains("sk-"), "{s}");
        assert_eq!(load_from(&p).unwrap(), st);
        // comments survive a save
        std::fs::write(&p, s.replace("[llm]", "# meine Einstellungen\n[llm]")).unwrap();
        save_to(&p, &st).unwrap();
        assert!(std::fs::read_to_string(&p).unwrap().starts_with("# meine Einstellungen\n"));
    }

    #[test]
    fn resolve_needs_both_switches() {
        let mut a = AppLlm::builtin();
        a.backends.insert("claude".into(), Preset::get("anthropic").unwrap().entry());
        let p = |b: Option<&str>, ok: bool| ProjectLlm { backend: b.map(String::from), online_ok: ok };
        // default = local gemma, no project switch needed
        let r = resolve(&a, None).unwrap();
        assert!(r.name == "gemma" && r.is_local() && r.online_ok && r.allowed().is_ok() && r.warning().is_none());
        assert_eq!(resolve(&a, Some(&p(None, false))).unwrap().name, "gemma");
        // online: project switch missing
        let r = resolve(&a, Some(&p(Some("claude"), false))).unwrap();
        assert!(!r.online_ok);
        let e = r.allowed().unwrap_err().to_string();
        assert!(e.contains("für dieses Projekt nicht freigegeben") && e.contains("--online-ok"), "{e}");
        // project yes, app no
        let r = resolve(&a, Some(&p(Some("claude"), true))).unwrap();
        assert!(!r.online_ok);
        let e = r.allowed().unwrap_err().to_string();
        assert!(e.contains("Einstellungen nicht erlaubt") && e.contains("--allow-online"), "{e}");
        assert!(funkhaus_llm::allowed(&r.backend, r.online_ok).is_err());
        // both
        a.backends.get_mut("claude").unwrap().allow_online = true;
        let r = resolve(&a, Some(&p(Some("claude"), true))).unwrap();
        assert!(r.online_ok && r.allowed().is_ok());
        assert!(r.warning().unwrap().contains("api.anthropic.com"));
        // app default online, project without [llm] -> refused (C7); unknown name -> error, no fallback
        a.default = Some("claude".into());
        assert!(resolve(&a, None).unwrap().allowed().is_err());
        assert!(resolve(&a, Some(&p(Some("nope"), true))).unwrap_err().to_string().contains("nicht konfiguriert"));
        a.default = None;
        assert!(resolve(&a, Some(&p(None, false))).is_err());
    }

    /// Fake `security` passed explicitly (no process-wide env var: tests run in parallel).
    #[test]
    fn key_set_clear_via_fake_keychain() {
        let d = tempfile::tempdir().unwrap();
        let fake = keychain::fake(d.path()).display().to_string();
        let mut st = Settings { llm: AppLlm::builtin() };
        st.llm.backends.insert("claude".into(), Preset::get("anthropic").unwrap().entry());
        assert!(set_key_with(&fake, &mut st, "nope", "k").is_err());
        set_key_with(&fake, &mut st, "claude", " sk-ant-geheim \n").unwrap();
        assert_eq!(st.llm.backends["claude"].key, Some(KeyRef::Keychain { service: "stimmwerk-llm".into(), account: "claude".into() }));
        assert_eq!(keychain::load_with(&fake, keychain::SERVICE, "claude").unwrap().as_deref(), Some("sk-ant-geheim"));
        let p = d.path().join(FILE);
        save_to(&p, &st).unwrap();
        assert!(!std::fs::read_to_string(&p).unwrap().contains("geheim"));
        clear_key_with(&fake, &mut st, "claude").unwrap();
        assert!(st.llm.backends["claude"].key.is_none() && keychain::load_with(&fake, keychain::SERVICE, "claude").unwrap().is_none());
        // no key reference (local backend, env ref, unknown name): `security` is never started
        clear_key_with("/nonexistent/security", &mut st, "gemma").unwrap();
        clear_key_with("/nonexistent/security", &mut st, "nope").unwrap();
        st.llm.backends.get_mut("claude").unwrap().key = Some(KeyRef::Env("X".into()));
        clear_key_with("/nonexistent/security", &mut st, "claude").unwrap();
        assert!(st.llm.backends["claude"].key.is_none());
        st.llm.backends.get_mut("claude").unwrap().key = Some(KeyRef::Keychain { service: "stimmwerk-llm".into(), account: "claude".into() });
        assert!(clear_key_with("/nonexistent/security", &mut st, "claude").unwrap_err().to_string().contains("nicht startbar"));
        // broken file: backup copy, original untouched
        std::fs::write(&p, "[llm]\nfoo = 1\n").unwrap();
        assert!(load_from(&p).is_err());
        let b = backup_broken(&p).unwrap();
        assert!(b.ends_with("settings.toml.broken") && std::fs::read_to_string(&b).unwrap() == "[llm]\nfoo = 1\n");
        assert!(backup_broken(&p).unwrap().ends_with("settings.toml.broken.1"));
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "[llm]\nfoo = 1\n");
    }
}
