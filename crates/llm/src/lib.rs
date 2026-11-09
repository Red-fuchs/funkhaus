//! LLM clients: OpenAI-compatible chat (gemma-serve, vMLX, LM Studio, Ollama, online providers with key) and the
//! Anthropic Messages API. `chat`/`models` are pure HTTP; `complete` adds the shared GPU lock (endpoints on this Mac)
//! and the connection fallback. Every path that opens a socket runs `allowed()` first: a non-loopback host needs
//! `online_ok`. Keys come from the keychain or an env var (`KeyRef`) and are never written or logged.
use anyhow::{Context, Result, bail};
use funkhaus_sys::{clip, gpu};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

pub mod keychain;
#[cfg(any(test, feature = "testsrv"))]
pub mod testsrv;

#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    #[default]
    OpenAi,
    Anthropic,
}

impl Kind {
    fn is_default(&self) -> bool {
        *self == Kind::OpenAi
    }
}

/// Where an API key lives. Only this reference is ever written to config/project files, never the key.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum KeyRef {
    /// macOS keychain generic password (`security find-generic-password -s service -a account`).
    Keychain { service: String, account: String },
    /// Environment variable name.
    Env(String),
}

/// One endpoint. Serialises like the old `[llm]` table (kind/key only when set).
#[derive(Serialize, Deserialize, Clone, PartialEq)]
#[serde(default)]
pub struct Backend {
    #[serde(skip_serializing_if = "Kind::is_default")]
    pub kind: Kind,
    pub url: String,
    /// Empty = first model from /models.
    pub model: String,
    /// Runtime only, never serialised. Still read from old configs (`api_key = "…"`): see `legacy_key_warning`.
    #[serde(skip_serializing)]
    pub api_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key: Option<KeyRef>,
    pub timeout_s: u64,
    pub max_tokens: u32,
    pub temperature: f32,
    /// Runs on this Mac's GPU (takes the shared GPU lock). None = `is_local()` (loopback). Set `true` for a server on
    /// this Mac reached via LAN IP, `.local` or Tailscale; `false` for a loopback tunnel to another machine.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gpu: Option<bool>,
}

impl Default for Backend {
    fn default() -> Self {
        Self {
            kind: Kind::OpenAi,
            url: "http://127.0.0.1:8095/v1".into(),
            model: "models/gemma-4-26B-A4B-it-JANG_4M".into(),
            api_key: None,
            key: None,
            timeout_s: 900,
            max_tokens: 8192,
            temperature: 0.7,
            gpu: None,
        }
    }
}

impl std::fmt::Debug for Backend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Backend")
            .field("kind", &self.kind)
            .field("url", &self.url)
            .field("model", &self.model)
            .field("api_key", &self.api_key.as_ref().map(|_| "***"))
            .field("key", &self.key)
            .field("timeout_s", &self.timeout_s)
            .field("max_tokens", &self.max_tokens)
            .field("temperature", &self.temperature)
            .field("gpu", &self.gpu)
            .finish()
    }
}

pub const ANTHROPIC_URL: &str = "https://api.anthropic.com/v1";
pub const ANTHROPIC_VERSION: &str = "2023-06-01";
/// Cheapest current Claude with structured outputs (llm-etappe7.md).
pub const ANTHROPIC_MODEL: &str = "claude-haiku-5-5";
/// Default timeout for online endpoints (local stays at 900 s).
pub const ONLINE_TIMEOUT_S: u64 = 120;

impl Backend {
    /// Anthropic Messages API (`https://api.anthropic.com/v1`), no GPU lock, 120 s timeout.
    pub fn anthropic(model: &str) -> Self {
        Backend { kind: Kind::Anthropic, url: ANTHROPIC_URL.into(), model: model.into(), timeout_s: ONLINE_TIMEOUT_S, ..Backend::default() }
    }

    /// Host of `url` is loopback (localhost, 127.0.0.0/8, ::1): runs on this Mac's GPU, no data leaves it.
    pub fn is_local(&self) -> bool {
        let rest = self.url.split_once("://").map_or(self.url.as_str(), |(_, r)| r);
        let auth = rest.split(['/', '?', '#']).next().unwrap_or("");
        let host = auth.rsplit_once('@').map_or(auth, |(_, h)| h);
        let host = match host.strip_prefix('[') {
            Some(h) => h.split(']').next().unwrap_or(""),
            None => host.split(':').next().unwrap_or(""),
        };
        host.eq_ignore_ascii_case("localhost") || host.parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_loopback())
    }

    /// Host part of `url` for messages (never the key).
    pub fn host(&self) -> String {
        let rest = self.url.split_once("://").map_or(self.url.as_str(), |(_, r)| r);
        let auth = rest.split(['/', '?', '#']).next().unwrap_or("");
        auth.rsplit_once('@').map_or(auth, |(_, h)| h).to_string()
    }

    /// Takes the shared GPU lock: explicit `gpu`, else loopback.
    pub fn uses_gpu(&self) -> bool {
        self.gpu.unwrap_or_else(|| self.is_local())
    }

    /// Key for requests: runtime `api_key`, else `key` resolved (env var or keychain).
    pub fn resolve_key(&self) -> Result<Option<String>> {
        if let Some(k) = self.api_key.as_ref().filter(|k| !k.is_empty()) {
            return Ok(Some(k.clone()));
        }
        match &self.key {
            None => Ok(None),
            Some(KeyRef::Env(v)) => std::env::var(v).ok().filter(|k| !k.is_empty()).map(Some).with_context(|| format!("API-Schlüssel: Umgebungsvariable {v} fehlt oder ist leer")),
            Some(KeyRef::Keychain { service, account }) => {
                keychain::load(service, account)?.map(Some).with_context(|| format!("API-Schlüssel: kein Eintrag im Schlüsselbund ({service}/{account}) – Schlüssel erst hinterlegen"))
            }
        }
    }

    /// Set when an old config carried a plain `api_key` (no longer written back).
    pub fn legacy_key_warning(&self, table: &str) -> Option<String> {
        (self.api_key.is_some() && self.key.is_none())
            .then(|| format!("[{table}] api_key ist veraltet und wird beim Speichern nicht mehr geschrieben – Schlüssel per `key = {{ env = \"VAR\" }}` oder Schlüsselbund angeben"))
    }

    /// `legacy_key_warning`, but at most once per table and process (configs are reloaded, e.g. by the GUI).
    pub fn legacy_key_warning_once(&self, table: &str) -> Option<String> {
        static SEEN: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
        let w = self.legacy_key_warning(table)?;
        let mut seen = SEEN.lock().unwrap_or_else(|e| e.into_inner());
        (!seen.iter().any(|t| t == table)).then(|| {
            seen.push(table.into());
            w
        })
    }

    fn with_key(&self) -> Result<std::borrow::Cow<'_, Backend>> {
        if self.api_key.is_some() || self.key.is_none() {
            return Ok(std::borrow::Cow::Borrowed(self));
        }
        Ok(std::borrow::Cow::Owned(Backend { api_key: self.resolve_key()?, ..self.clone() }))
    }
}

/// Online endpoints only with explicit permission (project + app switch); loopback is always allowed.
pub fn allowed(b: &Backend, online_ok: bool) -> Result<()> {
    if b.is_local() || online_ok {
        return Ok(());
    }
    bail!("Online-LLM {} nicht freigegeben – erst den Schalter „online erlaubt“ setzen (Texte verlassen sonst den Mac)", b.host())
}

#[derive(Clone, Debug)]
pub struct Msg {
    pub role: &'static str,
    pub content: String,
}

pub fn sys(s: impl Into<String>) -> Msg {
    Msg { role: "system", content: s.into() }
}
pub fn user(s: impl Into<String>) -> Msg {
    Msg { role: "user", content: s.into() }
}
pub fn asst(s: impl Into<String>) -> Msg {
    Msg { role: "assistant", content: s.into() }
}

/// A built HTTP request (pure; testable without a server).
#[derive(Debug, PartialEq)]
pub struct Req {
    pub url: String,
    pub headers: Vec<(&'static str, String)>,
    pub body: Value,
}

fn base(b: &Backend) -> &str {
    b.url.trim_end_matches('/')
}

/// Auth headers per kind. Anthropic needs a key (never an empty header); OpenAI-compatible local servers run without.
fn auth(b: &Backend) -> Result<Vec<(&'static str, String)>> {
    let key = b.api_key.as_deref().filter(|k| !k.is_empty());
    Ok(match (b.kind, key) {
        (Kind::OpenAi, Some(k)) => vec![("Authorization", format!("Bearer {k}"))],
        (Kind::OpenAi, None) => vec![],
        (Kind::Anthropic, Some(k)) => vec![("x-api-key", k.to_string()), ("anthropic-version", ANTHROPIC_VERSION.into())],
        (Kind::Anthropic, None) => bail!("Anthropic-Backend {}: kein API-Schlüssel gesetzt", b.host()),
    })
}

/// OpenAI strict mode accepts a schema only if every object lists ALL its properties in `required` and forbids
/// extras; otherwise the API answers 400. Optional fields must be `["T", "null"]` + required. Checked recursively.
pub fn strict_ok(s: &Value) -> bool {
    match s {
        Value::Object(m) => {
            let ok = match m.get("properties").and_then(Value::as_object) {
                Some(p) => {
                    let req: Vec<&str> = m.get("required").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str).collect();
                    m.get("additionalProperties") == Some(&Value::Bool(false)) && p.keys().all(|k| req.contains(&k.as_str()))
                }
                None => true,
            };
            ok && m.values().all(strict_ok)
        }
        Value::Array(a) => a.iter().all(strict_ok),
        _ => true,
    }
}

/// Chat request for `model`; with `schema`, asks for structured JSON output (`strict` only when the schema qualifies).
pub fn request(b: &Backend, model: &str, msgs: &[Msg], schema: Option<&Value>) -> Result<Req> {
    if b.kind == Kind::Anthropic {
        return anthropic_request(b, model, msgs, schema);
    }
    let mut body = json!({
        "model": model,
        "messages": msgs.iter().map(|m| json!({"role": m.role, "content": m.content})).collect::<Vec<_>>(),
        "temperature": b.temperature,
        "max_tokens": b.max_tokens,
    });
    if let Some(s) = schema {
        body["response_format"] = json!({"type": "json_schema", "json_schema": {"name": "out", "strict": strict_ok(s), "schema": s}});
    }
    Ok(Req { url: format!("{}/chat/completions", base(b)), headers: auth(b)?, body })
}

/// `POST {base}/messages`: system messages go top-level, no prefill (last message must be `user`), schema via
/// `output_config.format` (GA, no beta header; llm-etappe7.md).
fn anthropic_request(b: &Backend, model: &str, msgs: &[Msg], schema: Option<&Value>) -> Result<Req> {
    let system: Vec<&str> = msgs.iter().filter(|m| m.role == "system").map(|m| m.content.as_str()).collect();
    let rest: Vec<&Msg> = msgs.iter().filter(|m| m.role != "system").collect();
    if rest.last().is_some_and(|m| m.role == "assistant") {
        bail!("Anthropic: letzte Nachricht darf nicht vom Assistenten sein (kein Prefill)");
    }
    let mut body = json!({
        "model": model,
        "max_tokens": b.max_tokens,
        "temperature": b.temperature.min(1.0),
        "messages": rest.iter().map(|m| json!({"role": m.role, "content": m.content})).collect::<Vec<_>>(),
    });
    if !system.is_empty() {
        body["system"] = json!(system.join("\n\n"));
    }
    if let Some(s) = schema {
        body["output_config"] = json!({"format": {"type": "json_schema", "schema": s}});
    }
    Ok(Req { url: format!("{}/messages", base(b)), headers: auth(b)?, body })
}

/// Text of a 200 reply; truncated or refused output is an error (half a JSON is worse than none). `<think>` stripped.
pub fn parse_reply(kind: Kind, v: &Value) -> Result<String> {
    let text = match kind {
        Kind::OpenAi => {
            let c = &v["choices"][0];
            if c["finish_reason"] == "length" {
                bail!("LLM-Ausgabe abgeschnitten (max_tokens erreicht)");
            }
            c["message"]["content"].as_str().context("leere LLM-Antwort")?.to_string()
        }
        Kind::Anthropic => {
            match v["stop_reason"].as_str() {
                Some("max_tokens") => bail!("LLM-Ausgabe abgeschnitten (max_tokens erreicht)"),
                Some("refusal") => bail!("LLM hat die Anfrage abgelehnt (refusal)"),
                _ => {}
            }
            let t: String = v["content"].as_array().into_iter().flatten().filter(|c| c["type"] == "text").filter_map(|c| c["text"].as_str()).collect();
            if t.is_empty() {
                bail!("leere LLM-Antwort");
            }
            t
        }
    };
    Ok(strip_think(&text))
}

/// Error text from a 4xx/5xx body: `error.type: error.message` (Anthropic and OpenAI shapes), else the raw clip.
pub fn api_err(code: u16, body: &str) -> String {
    let v: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    match (v["error"]["type"].as_str(), v["error"]["message"].as_str()) {
        (Some(t), Some(m)) => format!("LLM-Fehler {code} – {t}: {m}"),
        (None, Some(m)) => format!("LLM-Fehler {code}: {m}"),
        _ => format!("LLM-Fehler {code}: {}", clip(body, 500)),
    }
}

/// No redirects: a key header must never follow a `Location` to another host.
fn agent(timeout: u64) -> ureq::Agent {
    ureq::AgentBuilder::new().redirects(0).timeout_connect(Duration::from_secs(3)).timeout(Duration::from_secs(timeout)).build()
}

/// Retries on 429/503/529 (`retry-after` honoured, capped) and one retry on a transport timeout.
pub const MAX_RETRIES: u32 = 2;
const RETRY_AFTER_CAP: Duration = Duration::from_secs(30);
const BACKOFF: [Duration; 2] = [Duration::from_secs(1), Duration::from_secs(3)];

fn retry_after(r: &ureq::Response) -> Option<Duration> {
    r.header("retry-after")?.trim().parse::<u64>().ok().map(|s| Duration::from_secs(s).min(RETRY_AFTER_CAP))
}

fn check_status(r: ureq::Response) -> Result<ureq::Response> {
    if (300..400).contains(&r.status()) {
        bail!("LLM-Weiterleitung (HTTP {}) nicht erlaubt: {}", r.status(), r.get_url());
    }
    Ok(r)
}

/// `GET {base}/models` → ids. Checks `allowed` before connecting.
pub fn models(b: &Backend, online_ok: bool) -> Result<Vec<String>> {
    allowed(b, online_ok)?;
    let b = &*b.with_key()?;
    let url = format!("{}/models", base(b));
    let r = auth(b)?.into_iter().fold(agent(10).get(&url), |r, (k, v)| r.set(k, &v));
    let v: Value = match r.call() {
        Ok(r) => check_status(r)?.into_json()?,
        Err(ureq::Error::Status(c, r)) => bail!("{}", api_err(c, &r.into_string().unwrap_or_default())),
        Err(e) => return Err(e).with_context(|| format!("LLM nicht erreichbar: {url}")),
    };
    Ok(v["data"].as_array().into_iter().flatten().filter_map(|m| m["id"].as_str().map(String::from)).collect())
}

/// One chat completion. Pure HTTP: no GPU lock, no fallback (see `complete`). Gate, then key, then socket.
pub fn chat(b: &Backend, online_ok: bool, msgs: &[Msg], schema: Option<&Value>) -> Result<String> {
    allowed(b, online_ok)?;
    let b = &*b.with_key()?;
    let model = if b.model.is_empty() { models(b, online_ok)?.into_iter().next().context("kein Modell am Endpoint")? } else { b.model.clone() };
    let q = request(b, &model, msgs, schema)?;
    let mut tries = 0;
    let r = loop {
        tries += 1;
        let r = q.headers.iter().fold(agent(b.timeout_s).post(&q.url), |r, (k, v)| r.set(k, v));
        match r.send_json(q.body.clone()) {
            Ok(r) => break check_status(r)?,
            Err(ureq::Error::Status(c, r)) => {
                let wait = retry_after(&r);
                let body = r.into_string().unwrap_or_default();
                // a crashed Metal backend ignores all further submissions: never retried
                if let Some(m) = gpu_error(c, &body).filter(|_| b.uses_gpu()) {
                    bail!("{m}");
                }
                if tries <= MAX_RETRIES && matches!(c, 429 | 503 | 529) {
                    std::thread::sleep(wait.unwrap_or(BACKOFF[(tries - 1) as usize]));
                    continue;
                }
                bail!("{}", api_err(c, &body));
            }
            // local GPU servers occasionally time out under load: one retry after a short pause
            Err(e) if transport_retry(b, tries) && transient(&e) => std::thread::sleep(Duration::from_secs(3)),
            Err(e) => return Err(e).with_context(|| format!("LLM nicht erreichbar: {}", q.url)),
        }
    };
    parse_reply(b.kind, &r.into_json()?)
}

/// Only a loopback server gets a second try after a transport timeout: an online call that timed out may still be
/// billed, so it is never sent twice.
pub fn transport_retry(b: &Backend, tries: u32) -> bool {
    tries < 2 && b.is_local()
}

/// `chat` under the shared GPU lock (only for endpoints on this Mac: `Backend::uses_gpu`; priority follows `gpu::Batch`);
/// if `b` is unreachable, the same on `fallback`. Both are gated by `allowed(_, online_ok)` before anything happens.
pub fn complete(b: &Backend, fallback: Option<&Backend>, online_ok: bool, msgs: &[Msg], schema: Option<&Value>) -> Result<String> {
    complete_in(&gpu::lock_dir(), b, fallback, online_ok, msgs, schema)
}

fn complete_in(dir: &std::path::Path, b: &Backend, fallback: Option<&Backend>, online_ok: bool, msgs: &[Msg], schema: Option<&Value>) -> Result<String> {
    allowed(b, online_ok)?;
    if let Some(fb) = fallback {
        allowed(fb, online_ok).context("Fallback-LLM")?;
    }
    let go = |b: &Backend| -> Result<String> {
        let _g = b.uses_gpu().then(|| gpu::acquire(dir, gpu::is_batch())).transpose()?;
        chat(b, online_ok, msgs, schema)
    };
    match go(b) {
        Err(e) if is_conn(&e) => match fallback {
            Some(fb) => go(fb).context("auch Fallback-LLM nicht erreichbar"),
            None => Err(e),
        },
        r => r,
    }
}

/// Error text of a cancelled call (same word as the TTS/GPU cancel).
pub const CANCELLED: &str = gpu::CANCELLED;

/// Runs `f` on its own thread and waits for it – unless `cancel` is set first: then `Err(CANCELLED)` at once. A request
/// in flight cannot be aborted (ureq blocks), so the thread is abandoned and its result dropped; `f` gets a flag that
/// mirrors `cancel` (for the GPU-lock wait) and a message sink forwarded to `prog`.
pub fn abandon_on_cancel<T: Send + 'static>(cancel: &AtomicBool, prog: &dyn Fn(&str), f: impl FnOnce(&AtomicBool, &dyn Fn(&str)) -> Result<T> + Send + 'static) -> Result<T> {
    enum Ev<T> {
        Msg(String),
        Done(Result<T>),
    }
    let (tx, rx) = std::sync::mpsc::channel();
    let flag = Arc::new(AtomicBool::new(cancel.load(Ordering::Relaxed)));
    let fl = flag.clone();
    std::thread::spawn(move || {
        let t = tx.clone();
        let r = f(&fl, &move |m: &str| drop(t.send(Ev::Msg(m.into()))));
        let _ = tx.send(Ev::Done(r));
    });
    loop {
        match rx.recv_timeout(Duration::from_millis(50)) {
            Ok(Ev::Msg(m)) => prog(&m),
            Ok(Ev::Done(r)) => return r,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => bail!("LLM-Aufruf unerwartet beendet"),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                if cancel.load(Ordering::Relaxed) {
                    flag.store(true, Ordering::Relaxed);
                    bail!("{CANCELLED}");
                }
            }
        }
    }
}

/// `complete` for interactive jobs: the GPU-lock wait ends on `cancel` (`gpu::with_lock_cancel`), a running request is
/// abandoned on `cancel`. Lock priority follows the calling thread (`gpu::Batch`). No fallback.
pub fn complete_cancel(b: &Backend, online_ok: bool, msgs: &[Msg], schema: Option<&Value>, cancel: &AtomicBool, prog: &dyn Fn(&str)) -> Result<String> {
    complete_cancel_in(&gpu::lock_dir(), b, online_ok, msgs, schema, cancel, prog)
}

fn complete_cancel_in(dir: &std::path::Path, b: &Backend, online_ok: bool, msgs: &[Msg], schema: Option<&Value>, cancel: &AtomicBool, prog: &dyn Fn(&str)) -> Result<String> {
    allowed(b, online_ok)?;
    let (b, msgs, schema, dir, batch) = (b.clone(), msgs.to_vec(), schema.cloned(), dir.to_path_buf(), gpu::is_batch());
    abandon_on_cancel(cancel, prog, move |c, p| {
        let run = || chat(&b, online_ok, &msgs, schema.as_ref());
        if b.uses_gpu() { gpu::with_lock_cancel_in(&dir, batch, p, c, run)? } else { run() }
    })
}

/// `models` whose wait can be abandoned (`abandon_on_cancel`).
pub fn models_cancel(b: &Backend, online_ok: bool, cancel: &AtomicBool) -> Result<Vec<String>> {
    allowed(b, online_ok)?;
    let b = b.clone();
    abandon_on_cancel(cancel, &|_| {}, move |_, _| models(&b, online_ok))
}

/// Accepts raw JSON, fenced JSON or JSON embedded in prose.
pub fn parse_json(s: &str) -> Option<Value> {
    let s = s.trim();
    if let Ok(v) = serde_json::from_str(s) {
        return Some(v);
    }
    let (a, b) = (s.find('{')?, s.rfind('}')?);
    (a < b).then(|| serde_json::from_str(&s[a..=b]).ok()).flatten()
}

/// A timeout/IO error (not "connection refused", which goes to the fallback instead; status errors have their own policy).
fn transient(e: &ureq::Error) -> bool {
    match e {
        ureq::Error::Status(..) => false,
        ureq::Error::Transport(t) => !matches!(t.kind(), ureq::ErrorKind::ConnectionFailed | ureq::ErrorKind::Dns | ureq::ErrorKind::InvalidUrl),
    }
}

/// Endpoint unreachable (transport error anywhere in the chain): worth trying a fallback.
pub fn is_conn(e: &anyhow::Error) -> bool {
    e.chain().any(|c| matches!(c.downcast_ref::<ureq::Error>(), Some(ureq::Error::Transport(_))))
}

pub const GPU_DEAD: &str = "Gemma (GPU) ist abgestürzt – bitte `gemma-serve stop && gemma-serve start` bzw. Neustart; Quellen sind evtl. zu lang";

/// Recognises a crashed MLX/Metal backend behind an HTTP error.
pub fn gpu_error(code: u16, body: &str) -> Option<String> {
    let lo = body.to_lowercase();
    let dead = lo.contains("submissionsignored") || lo.contains("gpu timeout") || (code >= 500 && lo.contains("metal"));
    dead.then(|| format!("{GPU_DEAD} (HTTP {code})"))
}

pub fn strip_think(s: &str) -> String {
    match s.find("</think>") {
        Some(i) => s[i + 8..].trim().to_string(),
        None => s.trim().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use testsrv::{Mock, Reply, err, fixture, ok};

    #[test]
    fn json_parse() {
        assert_eq!(parse_json("{\"a\":1}").unwrap()["a"], 1);
        assert_eq!(parse_json("```json\n{\"a\": [1]}\n```").unwrap()["a"][0], 1);
        assert_eq!(parse_json("Hier: {\"a\":2} fertig").unwrap()["a"], 2);
        assert!(parse_json("nix").is_none());
        assert_eq!(strip_think("<think>x</think> ok"), "ok");
    }

    #[test]
    fn gpu_errors() {
        assert!(gpu_error(500, r#"{"error":"[METAL] Command buffer execution failed: GPU Timeout Error"}"#).unwrap().contains("abgestürzt"));
        assert!(gpu_error(503, "kIOGPUCommandBufferCallbackErrorSubmissionsIgnored").is_some());
        assert!(gpu_error(500, "out of memory in tokenizer").is_none());
        assert!(gpu_error(400, "bad request").is_none());
    }

    #[test]
    fn openai_request_with_and_without_key() {
        let b = Backend { url: "http://x/v1/".into(), ..Backend::default() };
        let q = request(&b, "m", &[sys("s"), user("u")], None).unwrap();
        assert_eq!(q.url, "http://x/v1/chat/completions");
        assert!(q.headers.is_empty());
        assert_eq!(q.body["model"], "m");
        assert_eq!(q.body["messages"][1], json!({"role": "user", "content": "u"}));
        assert_eq!(q.body["max_tokens"], 8192);
        assert!(q.body.get("response_format").is_none());
        let k = Backend { api_key: Some("sk-1".into()), ..b.clone() };
        let q = request(&k, "m", &[user("u")], Some(&json!({"type": "object"}))).unwrap();
        assert_eq!(q.headers, vec![("Authorization", "Bearer sk-1".to_string())]);
        assert_eq!(q.body["response_format"]["json_schema"]["schema"]["type"], "object");
        assert!(request(&Backend { api_key: Some(String::new()), ..b }, "m", &[], None).unwrap().headers.is_empty());
    }

    #[test]
    fn strict_only_when_all_properties_required() {
        let full = json!({"type": "object", "additionalProperties": false, "required": ["a", "b"],
            "properties": {"a": {"type": "string"}, "b": {"type": ["number", "null"]}}});
        let optional = json!({"type": "object", "additionalProperties": false, "required": ["a"],
            "properties": {"a": {"type": "string"}, "b": {"type": "number"}}});
        let nested = json!({"type": "object", "additionalProperties": false, "required": ["xs"],
            "properties": {"xs": {"type": "array", "items": optional.clone()}}});
        let open = json!({"type": "object", "required": ["a"], "properties": {"a": {"type": "string"}}});
        assert!(strict_ok(&full) && !strict_ok(&optional) && !strict_ok(&nested) && !strict_ok(&open));
        let b = Backend::default();
        let sf = request(&b, "m", &[user("u")], Some(&full)).unwrap().body["response_format"]["json_schema"]["strict"].clone();
        let so = request(&b, "m", &[user("u")], Some(&nested)).unwrap().body["response_format"]["json_schema"]["strict"].clone();
        assert_eq!((sf, so), (json!(true), json!(false)));
    }

    #[test]
    fn anthropic_request_shape() {
        let b = Backend { api_key: Some("sk-ant".into()), temperature: 1.5, ..Backend::anthropic(ANTHROPIC_MODEL) };
        assert!(!b.is_local() && !b.uses_gpu() && b.timeout_s == ONLINE_TIMEOUT_S);
        let q = request(&b, ANTHROPIC_MODEL, &[sys("A"), user("u"), asst("a"), sys("B"), user("v")], Some(&json!({"type": "object"}))).unwrap();
        assert_eq!(q.url, "https://api.anthropic.com/v1/messages");
        assert_eq!(q.headers, vec![("x-api-key", "sk-ant".to_string()), ("anthropic-version", ANTHROPIC_VERSION.to_string())]);
        assert!(!q.headers.iter().any(|(k, _)| *k == "Authorization"));
        assert_eq!(q.body["system"], "A\n\nB");
        assert_eq!(q.body["messages"].as_array().unwrap().len(), 3);
        assert!(q.body["messages"].as_array().unwrap().iter().all(|m| m["role"] != "system"));
        assert_eq!((&q.body["max_tokens"], &q.body["temperature"]), (&json!(8192), &json!(1.0)));
        assert_eq!(q.body["output_config"]["format"]["type"], "json_schema");
        assert!(q.body.get("tools").is_none() && q.body.get("tool_choice").is_none() && q.body.get("thinking").is_none());
        // no schema -> no output_config; prefill refused; missing key refused (never an empty header)
        assert!(request(&b, "m", &[user("u")], None).unwrap().body.get("output_config").is_none());
        assert!(request(&b, "m", &[user("u"), asst("x")], None).unwrap_err().to_string().contains("Prefill"));
        let e = request(&Backend::anthropic("m"), "m", &[user("u")], None).unwrap_err().to_string();
        assert!(e.contains("Schlüssel") && !e.contains("Authorization"), "{e}");
    }

    #[test]
    fn parse_replies() {
        let v = json!({"content": [{"type": "thinking", "thinking": "hm"}, {"type": "text", "text": "a"}, {"type": "tool_use", "input": {}}, {"type": "text", "text": "b"}], "stop_reason": "end_turn"});
        assert_eq!(parse_reply(Kind::Anthropic, &v).unwrap(), "ab");
        assert!(parse_reply(Kind::Anthropic, &json!({"content": [], "stop_reason": "end_turn"})).unwrap_err().to_string().contains("leere"));
        let f = |n: &str| serde_json::from_str::<Value>(&fixture(n).body).unwrap();
        assert!(parse_reply(Kind::Anthropic, &f("anthropic/200_max_tokens")).unwrap_err().to_string().contains("abgeschnitten"));
        assert!(parse_reply(Kind::Anthropic, &f("anthropic/200_refusal")).unwrap_err().to_string().contains("abgelehnt"));
        assert!(parse_reply(Kind::Anthropic, &f("anthropic/200_success")).unwrap().starts_with("{\"cast\""));
        assert!(parse_reply(Kind::OpenAi, &f("openai/200_length")).unwrap_err().to_string().contains("abgeschnitten"));
        assert!(parse_reply(Kind::OpenAi, &f("openai/200_success")).unwrap().starts_with("{\"cast\""));
        assert!(parse_json(&parse_reply(Kind::OpenAi, &f("openai/200_malformed_content")).unwrap()).is_none());
        assert!(parse_reply(Kind::OpenAi, &json!({"choices": [{"message": {"content": null}}]})).unwrap_err().to_string().contains("leere"));
        assert_eq!(api_err(401, &fixture("anthropic/401").body), "LLM-Fehler 401 – authentication_error: invalid x-api-key");
        assert_eq!(api_err(401, &fixture("openai/401").body), "LLM-Fehler 401 – invalid_request_error: Incorrect API key provided.");
        assert_eq!(api_err(500, "kaputt"), "LLM-Fehler 500: kaputt");
    }

    #[test]
    fn config_table_compatible() {
        // old `[llm]` tables load as OpenAI without key; defaults serialise without the new fields
        let b: Backend = toml::from_str("url = \"http://h/v1\"\nmodel = \"\"\ntimeout_s = 5\n").unwrap();
        assert_eq!((b.kind, b.api_key.as_deref(), b.url.as_str(), b.timeout_s, b.max_tokens), (Kind::OpenAi, None, "http://h/v1", 5, 8192));
        let s = toml::to_string(&Backend::default()).unwrap();
        assert!(!s.contains("kind") && !s.contains("api_key"), "{s}");
        let a: Backend = toml::from_str("kind = \"anthropic\"").unwrap();
        assert_eq!(a.kind, Kind::Anthropic);
        assert!(toml::to_string(&a).unwrap().contains("kind = \"anthropic\""));
    }

    fn local(m: &Mock, kind: Kind) -> Backend {
        Backend { kind, url: format!("{}/v1", m.url()), model: "m".into(), api_key: Some("sk-test".into()), timeout_s: 5, ..Backend::default() }
    }

    const OK: &str = r#"{"choices":[{"message":{"role":"assistant","content":"<think>hm</think> Hallo"},"finish_reason":"stop"}]}"#;

    #[test]
    fn chat_against_mock_server() {
        let m = Mock::start(vec![ok(OK)]);
        // SAFETY: test-only env var, unique name
        unsafe { std::env::set_var("FUNKHAUS_LLM_TEST_KEY", "sk-env") };
        let b = Backend { api_key: None, key: Some(KeyRef::Env("FUNKHAUS_LLM_TEST_KEY".into())), ..local(&m, Kind::OpenAi) };
        assert_eq!(chat(&b, false, &[user("Hi")], None).unwrap(), "Hallo");
        let r = &m.reqs()[0];
        assert_eq!((r.method.as_str(), r.path.as_str(), r.header("authorization")), ("POST", "/v1/chat/completions", Some("Bearer sk-env")));
        assert!(r.body_str().contains(r#""content":"Hi""#));
        assert_eq!(m.hits(), 1);
    }

    #[test]
    fn anthropic_against_mock_server() {
        let m = Mock::start(vec![fixture("anthropic/200_success")]);
        let b = local(&m, Kind::Anthropic);
        let out = chat(&b, false, &[sys("S"), user("Hi")], Some(&json!({"type": "object"}))).unwrap();
        assert!(out.starts_with("{\"cast\""));
        let r = &m.reqs()[0];
        assert_eq!((r.path.as_str(), r.header("x-api-key"), r.header("anthropic-version"), r.header("authorization")), ("/v1/messages", Some("sk-test"), Some("2023-06-01"), None));
        let body: Value = serde_json::from_str(&r.body_str()).unwrap();
        assert_eq!((body["system"].as_str(), body["output_config"]["format"]["type"].as_str()), (Some("S"), Some("json_schema")));
        // models via GET with the same headers
        let m = Mock::start(vec![ok(r#"{"data":[{"id":"claude-haiku-5-5"},{"id":"claude-sonnet-5-5"}]}"#)]);
        assert_eq!(models(&local(&m, Kind::Anthropic), false).unwrap(), ["claude-haiku-5-5", "claude-sonnet-5-5"]);
        let r = &m.reqs()[0];
        assert_eq!((r.method.as_str(), r.path.as_str(), r.header("x-api-key")), ("GET", "/v1/models", Some("sk-test")));
    }

    /// Every fixture file, replayed through `chat` on its kind: hits (= retries) and the error class as the README says.
    #[test]
    fn fixture_matrix() {
        let cases: &[(&str, Kind, usize, Option<&str>)] = &[
            ("openai/200_success", Kind::OpenAi, 1, None),
            ("openai/200_length", Kind::OpenAi, 1, Some("abgeschnitten")),
            ("openai/200_malformed_content", Kind::OpenAi, 1, None),
            ("openai/401", Kind::OpenAi, 1, Some("Incorrect API key provided.")),
            ("openai/429", Kind::OpenAi, 3, Some("rate limit")),
            ("openai/500", Kind::OpenAi, 1, Some("LLM-Fehler 500 – server_error: internal error")),
            ("openai/500_gpu_timeout", Kind::OpenAi, 1, Some("abgestürzt")),
            ("anthropic/200_success", Kind::Anthropic, 1, None),
            ("anthropic/200_max_tokens", Kind::Anthropic, 1, Some("abgeschnitten")),
            ("anthropic/200_refusal", Kind::Anthropic, 1, Some("abgelehnt")),
            ("anthropic/401", Kind::Anthropic, 1, Some("authentication_error: invalid x-api-key")),
            ("anthropic/429", Kind::Anthropic, 3, Some("rate_limit_error")),
            ("anthropic/529", Kind::Anthropic, 3, Some("overloaded_error")),
        ];
        for (f, kind, hits, err) in cases {
            let m = Mock::start(vec![fixture(f).with("retry-after", "0")]);
            let r = chat(&local(&m, *kind), false, &[user("x")], None);
            assert_eq!(m.hits(), *hits, "{f}");
            match err {
                None => assert!(r.is_ok(), "{f}: {r:?}"),
                Some(e) => {
                    let got = r.unwrap_err().to_string();
                    assert!(got.to_lowercase().contains(&e.to_lowercase()), "{f}: {got}");
                    assert!(!got.contains("sk-test"), "{f}: key leaked: {got}");
                }
            }
        }
    }

    #[test]
    fn retry_then_ok_and_retry_after_honoured() {
        let m = Mock::start(vec![fixture("anthropic/429"), fixture("anthropic/529").with("retry-after", "0"), fixture("anthropic/200_success")]);
        let t = std::time::Instant::now();
        assert!(chat(&local(&m, Kind::Anthropic), false, &[user("x")], None).is_ok());
        assert_eq!(m.hits(), 3);
        // fixture 429 carries `retry-after: 0`: no backoff wait at all
        assert!(t.elapsed() < Duration::from_millis(900), "{:?}", t.elapsed());
        let m = Mock::start(vec![err(503, "busy").with("retry-after", "1"), ok(OK)]);
        let t = std::time::Instant::now();
        assert_eq!(chat(&local(&m, Kind::OpenAi), false, &[user("x")], None).unwrap(), "Hallo");
        assert!(t.elapsed() >= Duration::from_secs(1) && m.hits() == 2);
        // 400 is never retried
        let m = Mock::start(vec![err(400, r#"{"error":{"type":"invalid_request_error","message":"bad"}}"#)]);
        assert!(chat(&local(&m, Kind::Anthropic), false, &[user("x")], None).unwrap_err().to_string().contains("bad"));
        assert_eq!(m.hits(), 1);
        // 503 from a GPU server that died: no retry
        let m = Mock::start(vec![err(503, "kIOGPUCommandBufferCallbackErrorSubmissionsIgnored")]);
        assert!(chat(&local(&m, Kind::OpenAi), false, &[user("x")], None).unwrap_err().to_string().contains(GPU_DEAD));
        assert_eq!(m.hits(), 1);
    }

    #[test]
    fn redirect_not_followed() {
        let target = Mock::start(vec![ok(OK)]);
        let m = Mock::start(vec![Reply::new(302, "").with("Location", &format!("{}/v1/chat/completions", target.url()))]);
        let e = chat(&local(&m, Kind::Anthropic), false, &[user("x")], None).unwrap_err().to_string();
        assert!(e.contains("Weiterleitung"), "{e}");
        assert_eq!((m.hits(), target.hits()), (1, 0));
        let e = models(&local(&m, Kind::OpenAi), false).unwrap_err().to_string();
        assert!(e.contains("Weiterleitung") && target.hits() == 0, "{e}");
    }

    fn dead_url() -> String {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        format!("http://{}/v1", l.local_addr().unwrap()) // dropped: connection refused
    }

    #[test]
    fn complete_falls_back_on_connection_error() {
        let d = tempfile::tempdir().unwrap();
        let m = Mock::start(vec![ok(OK)]);
        let b = Backend { url: dead_url(), model: "m".into(), ..Backend::default() };
        let fb = Backend { url: format!("{}/v1", m.url()), model: "m".into(), ..Backend::default() };
        assert_eq!(complete_in(d.path(), &b, Some(&fb), false, &[user("x")], None).unwrap(), "Hallo");
        assert!(d.path().join(".gpu.lock").exists());
        let e = complete_in(d.path(), &b, None, false, &[user("x")], None).unwrap_err();
        assert!(is_conn(&e), "{e:#}");
    }

    /// Cancel ends the GPU-lock wait (no request sent, prio file gone) and abandons a request in flight within a second.
    #[test]
    fn complete_cancel_wait_and_request() {
        let d = tempfile::tempdir().unwrap();
        let m = Mock::start(vec![ok(OK).delay(Duration::from_secs(2))]);
        let b = Backend { url: format!("{}/v1", m.url()), model: "m".into(), timeout_s: 5, ..Backend::default() };
        let hold = gpu::acquire(d.path(), true).unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let (c2, dd, bb) = (cancel.clone(), d.path().to_path_buf(), b.clone());
        let t = std::thread::spawn(move || {
            let msgs = std::sync::Mutex::new(vec![]);
            let r = complete_cancel_in(&dd, &bb, false, &[user("x")], None, &c2, &|s| msgs.lock().unwrap().push(s.to_string()));
            (r.map_err(|e| e.to_string()), msgs.into_inner().unwrap())
        });
        std::thread::sleep(Duration::from_millis(200));
        cancel.store(true, Ordering::Relaxed);
        let (r, msgs) = t.join().unwrap();
        assert_eq!((r.unwrap_err(), msgs), (CANCELLED.to_string(), vec!["wartet auf GPU …".to_string()]));
        assert_eq!(m.hits(), 0, "never sent while waiting for the GPU");
        std::thread::sleep(Duration::from_millis(100));
        assert!(!gpu::prio_waiting(d.path()), "prio file removed");
        drop(hold);
        // request in flight: the slow mock keeps the socket, cancel returns at once
        let cancel = AtomicBool::new(false);
        let t0 = std::time::Instant::now();
        let bb = b.clone();
        let r = std::thread::scope(|s| {
            s.spawn(|| {
                std::thread::sleep(Duration::from_millis(300));
                cancel.store(true, Ordering::Relaxed);
            });
            complete_cancel_in(d.path(), &bb, false, &[user("x")], None, &cancel, &|_| {})
        });
        assert_eq!(r.unwrap_err().to_string(), CANCELLED);
        assert!(t0.elapsed() < Duration::from_millis(1200), "{:?}", t0.elapsed());
        // not cancelled: normal result; models_cancel too
        let m2 = Mock::start(vec![ok(OK), ok(r#"{"data":[{"id":"a"}]}"#)]);
        let b2 = Backend { url: format!("{}/v1", m2.url()), model: "m".into(), ..Backend::default() };
        assert_eq!(complete_cancel_in(d.path(), &b2, false, &[user("x")], None, &AtomicBool::new(false), &|_| {}).unwrap(), "Hallo");
        assert_eq!(models_cancel(&b2, false, &AtomicBool::new(false)).unwrap(), ["a"]);
        // retry after a transport timeout only on loopback
        assert!(transport_retry(&b2, 1) && !transport_retry(&b2, 2));
        assert!(!transport_retry(&Backend::anthropic("m"), 1));
    }

    #[test]
    fn complete_locks_only_local() {
        let d = tempfile::tempdir().unwrap();
        // remote (unresolvable) endpoint: no lock file created
        let b = Backend { url: "http://invalid.invalid/v1".into(), model: "m".into(), ..Backend::default() };
        assert!(complete_in(d.path(), &b, None, true, &[user("x")], None).is_err());
        assert!(!d.path().join(".gpu.lock").exists());
        // explicit override: a server on this Mac via LAN/.local/Tailscale locks, a loopback tunnel elsewhere does not
        let lan = Backend { url: "http://invalid.invalid/v1".into(), model: "m".into(), gpu: Some(true), ..Backend::default() };
        assert!(lan.uses_gpu() && !lan.is_local());
        assert!(complete_in(d.path(), &lan, None, true, &[user("x")], None).is_err());
        assert!(d.path().join(".gpu.lock").exists());
        assert!(!Backend { gpu: Some(false), ..Backend::default() }.uses_gpu() && Backend::default().uses_gpu());
        let t: Backend = toml::from_str("url = \"http://100.64.0.2:8095/v1\"\ngpu = true\n").unwrap();
        assert_eq!(t.gpu, Some(true));
        assert!(!toml::to_string(&Backend::default()).unwrap().contains("gpu"));
    }

    #[test]
    fn local_and_allowed() {
        let be = |u: &str| Backend { url: u.into(), ..Backend::default() };
        for u in ["http://127.0.0.1:8095/v1", "http://localhost:8080", "http://[::1]:1/v1", "http://127.0.0.2/x", "http://user@localhost/v1"] {
            assert!(be(u).is_local(), "{u}");
            assert!(allowed(&be(u), false).is_ok());
        }
        for u in ["https://api.anthropic.com/v1", "http://192.168.1.5:8080/v1", "http://localhost.evil.com/v1", "http://127.0.0.1.nip.io/v1", "http://localhost@evil.example/v1", "http://127.1/v1", ""] {
            assert!(!be(u).is_local(), "{u}");
        }
        let e = allowed(&be("https://api.openai.com/v1"), false).unwrap_err().to_string();
        assert!(e.contains("nicht freigegeben") && e.contains("api.openai.com"), "{e}");
        assert!(allowed(&be("https://api.openai.com/v1"), true).is_ok());
        assert_eq!(be("https://user:pw@api.openai.com:443/v1").host(), "api.openai.com:443");
    }

    /// The gate runs before the key is resolved and before any socket: a mock on loopback is only reachable through a
    /// non-loopback name, so `online_ok=false` must refuse without connecting (hits stay 0) and without touching the key.
    #[test]
    fn gate_before_key_and_socket() {
        let m = Mock::start(vec![ok(OK)]);
        let port = m.url().rsplit(':').next().unwrap().to_string();
        let online = Backend { url: format!("http://127.0.0.1.nip.io:{port}/v1"), model: "m".into(), key: Some(KeyRef::Env("FUNKHAUS_NOPE_UNSET".into())), ..Backend::default() };
        let d = tempfile::tempdir().unwrap();
        for e in [
            chat(&online, false, &[user("x")], None).unwrap_err(),
            models(&online, false).unwrap_err(),
            complete_in(d.path(), &online, None, false, &[user("x")], None).unwrap_err(),
            complete_in(d.path(), &Backend { url: dead_url(), ..Backend::default() }, Some(&online), false, &[user("x")], None).unwrap_err(),
            complete_in(d.path(), &Backend { model: String::new(), ..online.clone() }, None, false, &[user("x")], None).unwrap_err(),
        ] {
            let s = format!("{e:#}");
            assert!(s.contains("nicht freigegeben") && !s.contains("FUNKHAUS_NOPE_UNSET") && !is_conn(&e), "{s}");
        }
        assert_eq!(m.hits(), 0);
        assert!(!d.path().join(".gpu.lock").exists());
        // gate open, key missing: error names the variable, still no socket
        let e = chat(&online, true, &[user("x")], None).unwrap_err().to_string();
        assert!(e.contains("FUNKHAUS_NOPE_UNSET"), "{e}");
        let e = chat(&Backend { kind: Kind::Anthropic, key: None, ..online.clone() }, true, &[user("x")], None).unwrap_err().to_string();
        assert!(e.contains("Schlüssel") && !e.contains("erreichbar"), "{e}");
        assert_eq!(m.hits(), 0);
    }

    #[test]
    fn key_never_serialised_and_redacted() {
        let kc = KeyRef::Keychain { service: keychain::SERVICE.into(), account: "openai".into() };
        let b = Backend { api_key: Some("sk-secret".into()), key: Some(kc.clone()), ..Backend::default() };
        let s = toml::to_string(&b).unwrap();
        assert!(!s.contains("sk-secret") && !s.contains("api_key"), "{s}");
        assert!(!format!("{b:?}").contains("sk-secret"));
        let back: Backend = toml::from_str(&s).unwrap();
        assert_eq!((back.api_key, back.key), (None, b.key.clone()));
        let a = Backend { api_key: Some("sk-ant-secret".into()), ..Backend::anthropic("m") };
        assert!(!toml::to_string(&a).unwrap().contains("secret") && !format!("{a:?}").contains("secret"));
        let e: Backend = toml::from_str("key = { env = \"X_KEY\" }").unwrap();
        assert_eq!(e.key, Some(KeyRef::Env("X_KEY".into())));
        assert!(toml::to_string(&e).unwrap().contains("[key]\nenv = \"X_KEY\""));
        assert_eq!(b.resolve_key().unwrap().as_deref(), Some("sk-secret"));
        assert!(Backend { key: Some(KeyRef::Env("FUNKHAUS_NOPE_UNSET".into())), ..Backend::default() }.resolve_key().unwrap_err().to_string().contains("FUNKHAUS_NOPE_UNSET"));
    }

    /// Keychain lookups go through the `security` binary; here a fake one (never the login keychain).
    #[test]
    fn keychain_resolve_via_fake() {
        let d = tempfile::tempdir().unwrap();
        let bin = keychain::fake(d.path()).display().to_string();
        let kc = KeyRef::Keychain { service: keychain::SERVICE.into(), account: "claude".into() };
        assert_eq!(keychain::load_with(&bin, keychain::SERVICE, "claude").unwrap(), None);
        keychain::save_with(&bin, keychain::SERVICE, "claude", "sk-ant-x").unwrap();
        assert_eq!(keychain::load_with(&bin, keychain::SERVICE, "claude").unwrap().as_deref(), Some("sk-ant-x"));
        // the settings file carries only the reference
        let s = toml::to_string(&Backend { key: Some(kc), ..Backend::anthropic("m") }).unwrap();
        assert!(s.contains("[key.keychain]") && s.contains("account = \"claude\"") && !s.contains("sk-ant"), "{s}");
    }

    #[test]
    fn legacy_api_key_still_read() {
        let b: Backend = toml::from_str("url = \"https://x/v1\"\napi_key = \"sk-old\"\n").unwrap();
        assert_eq!(b.api_key.as_deref(), Some("sk-old"));
        assert!(b.legacy_key_warning("llm").unwrap().contains("veraltet"));
        assert!(!toml::to_string(&b).unwrap().contains("sk-old"));
        assert!(Backend::default().legacy_key_warning("llm").is_none());
        assert!(b.legacy_key_warning_once("test-once").is_some());
        assert!(b.legacy_key_warning_once("test-once").is_none());
        assert!(Backend::default().legacy_key_warning_once("test-other").is_none());
    }
}
