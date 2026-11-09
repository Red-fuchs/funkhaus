use crate::{Backend, Config};
use anyhow::{Context, Result, bail};
use serde_json::Value;
use std::io::Write;

pub use funkhaus_llm::{GPU_DEAD, Msg, asst, gpu_error, parse_json, sys, user};

/// `GET /models` of a configured endpoint (config.toml is the user's own: online allowed).
pub fn models(b: &funkhaus_llm::Backend) -> Result<Vec<String>> {
    funkhaus_llm::models(b, true)
}
/// One chat completion. With `schema`, requests structured JSON output.
/// Local = `funkhaus_llm::complete` (shared GPU lock for loopback endpoints + fallback).
pub fn chat(cfg: &Config, be: Backend, msgs: &[Msg], schema: Option<&Value>) -> Result<String> {
    match be {
        Backend::Claude => claude(cfg, msgs, schema),
        // funkhaus' endpoints come from the user's own config.toml (never from a notebook): that file is the switch
        Backend::Local => funkhaus_llm::complete(&cfg.llm, cfg.fallback.as_ref(), true, msgs, schema),
    }
}

/// Structured output, parsed robustly; one retry on invalid JSON.
pub fn json(cfg: &Config, be: Backend, msgs: &[Msg], schema: &Value) -> Result<Value> {
    let out = chat(cfg, be, msgs, Some(schema))?;
    if let Some(v) = parse_json(&out) {
        return Ok(v);
    }
    json_repair(cfg, be, &out, schema)
}

/// Repair pass: sees only the broken output and the schema, not the (long) source prompt.
pub fn json_repair(cfg: &Config, be: Backend, out: &str, schema: &Value) -> Result<Value> {
    let m = [
        sys("Du reparierst fehlerhaftes JSON. Antworte ausschließlich mit gültigem JSON nach dem Schema, ohne Text davor oder danach."),
        user(format!("Schema:\n{schema}\n\nFehlerhafte Ausgabe:\n{}", crate::util::clip(out, 16_000))),
    ];
    let out = chat(cfg, be, &m, Some(schema))?;
    parse_json(&out).context("LLM lieferte kein gültiges JSON")
}

fn claude(cfg: &Config, msgs: &[Msg], schema: Option<&Value>) -> Result<String> {
    let mut p = String::new();
    for m in msgs {
        let tag = match m.role {
            "system" => "Anweisung",
            "assistant" => "Assistent",
            _ => "Nutzer",
        };
        p.push_str(&format!("### {tag}\n{}\n\n", m.content));
    }
    if let Some(s) = schema {
        p.push_str(&format!("Antworte ausschließlich mit JSON nach diesem Schema:\n{s}\n"));
    }
    let bin = crate::util::which(&cfg.claude).context("claude CLI nicht gefunden")?;
    let mut ch = std::process::Command::new(bin)
        // no tools, no CLAUDE.md/skills/MCP, neutral cwd: only the prompt leaves the Mac
        .args(["-p", "--output-format", "text", "--tools", "", "--safe-mode", "--strict-mcp-config"])
        .current_dir(std::env::temp_dir())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;
    ch.stdin.take().unwrap().write_all(p.as_bytes())?;
    let o = ch.wait_with_output()?;
    if !o.status.success() {
        bail!("claude -p fehlgeschlagen: {}", String::from_utf8_lossy(&o.stderr));
    }
    Ok(String::from_utf8_lossy(&o.stdout).trim().to_string())
}
