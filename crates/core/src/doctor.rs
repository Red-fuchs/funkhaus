use crate::{Store, llm, util};
use serde_json::{Value, json};
use std::process::Command;

/// Checks LLM endpoints, omnivoice, ffmpeg, claude. Returns (all required ok, report).
pub fn run(st: &Store) -> (bool, Value) {
    let c = &st.cfg;
    let llm_chk = |l: &crate::config::Llm| match llm::models(l) {
        Ok(m) => {
            let has = l.model.is_empty() || m.contains(&l.model);
            json!({"ok": has, "url": l.url, "model": l.model, "models": m})
        }
        Err(e) => json!({"ok": false, "url": l.url, "error": e.to_string()}),
    };
    let primary = llm_chk(&c.llm);
    let fallback = c.fallback.as_ref().map(llm_chk);
    let llm_ok = primary["ok"] == true || fallback.as_ref().is_some_and(|f| f["ok"] == true);
    let tool = |cmd: &str, args: &[&str]| match util::which(cmd) {
        Some(p) => match Command::new(&p).args(args).output() {
            Ok(o) if o.status.success() => {
                let s = String::from_utf8_lossy(&o.stdout);
                json!({"ok": true, "path": p, "info": s.lines().next().unwrap_or("").trim()})
            }
            Ok(o) => json!({"ok": false, "path": p, "error": String::from_utf8_lossy(&o.stderr).trim()}),
            Err(e) => json!({"ok": false, "path": p, "error": e.to_string()}),
        },
        None => json!({"ok": false, "error": format!("{cmd} nicht gefunden")}),
    };
    let tts = tool(&c.tts.cmd, &["status"]);
    let ff = tool(&c.ffmpeg, &["-version"]);
    let claude = json!({"available": util::which(&c.claude).is_some(), "note": "nur pro Notebook erlaubt (Backend claude)"});
    let ok = llm_ok && ff["ok"] == true && util::which(&c.tts.cmd).is_some();
    (
        ok,
        json!({
            "ok": ok,
            "home": st.root,
            "config": st.root.join("config.toml"),
            "llm": primary,
            "fallback": fallback,
            "omnivoice": tts,
            "ffmpeg": ff,
            "claude": claude,
        }),
    )
}
