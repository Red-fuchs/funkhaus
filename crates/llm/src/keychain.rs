//! API keys in the macOS login keychain (generic passwords) via `/usr/bin/security`. The key never appears in argv:
//! `add-generic-password` runs inside `security -i` with the command on stdin. `$FUNKHAUS_SECURITY` replaces the
//! binary (tests point it at a fake that stores in a temp dir; the real keychain is never touched by tests).
use anyhow::{Context, Result, bail};
use std::io::Write;
use std::process::{Command, Stdio};

pub const ENV: &str = "FUNKHAUS_SECURITY";
/// Keychain service of Stimmwerk's LLM keys; account = backend name.
pub const SERVICE: &str = "stimmwerk-llm";
const NOT_FOUND: i32 = 44; // errSecItemNotFound as exit status

pub fn bin() -> String {
    std::env::var(ENV).ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "/usr/bin/security".into())
}

/// Service/account/key must tokenize unambiguously in `security -i` (no whitespace, quotes, backslashes, control chars).
fn plain(what: &str, s: &str) -> Result<()> {
    if s.is_empty() || s.chars().any(|c| c.is_whitespace() || c.is_control() || matches!(c, '"' | '\'' | '\\')) {
        bail!("Schlüsselbund: {what} darf nicht leer sein und keine Leerzeichen, Anführungszeichen oder Backslashes enthalten");
    }
    Ok(())
}

pub fn save(svc: &str, acct: &str, key: &str) -> Result<()> {
    save_with(&bin(), svc, acct, key)
}
pub fn load(svc: &str, acct: &str) -> Result<Option<String>> {
    load_with(&bin(), svc, acct)
}
pub fn clear(svc: &str, acct: &str) -> Result<()> {
    clear_with(&bin(), svc, acct)
}

/// Create or update (`-U`). The key goes through stdin of `security -i`, not through argv.
pub fn save_with(bin: &str, svc: &str, acct: &str, key: &str) -> Result<()> {
    plain("Dienst", svc)?;
    plain("Konto", acct)?;
    plain("Schlüssel", key)?;
    let mut ch = Command::new(bin).arg("-i").stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().with_context(|| format!("{bin} nicht startbar"))?;
    ch.stdin.take().context("stdin")?.write_all(format!("add-generic-password -U -s {svc} -a {acct} -w {key}\n").as_bytes())?;
    let o = ch.wait_with_output()?;
    let out = format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr)).replace(key, "***");
    // interactive mode exits 0 even on failure: errors are reported as "<command>: returned <code>"
    if !o.status.success() || out.contains("returned") || out.contains("Usage") {
        bail!("Schlüsselbund: Speichern fehlgeschlagen: {}", funkhaus_sys::clip(out.trim(), 300));
    }
    Ok(())
}

/// `Ok(None)` when no item exists.
pub fn load_with(bin: &str, svc: &str, acct: &str) -> Result<Option<String>> {
    plain("Dienst", svc)?;
    plain("Konto", acct)?;
    let o = Command::new(bin).args(["find-generic-password", "-s", svc, "-a", acct, "-w"]).output().with_context(|| format!("{bin} nicht startbar"))?;
    let err = String::from_utf8_lossy(&o.stderr);
    if o.status.success() {
        return Ok(Some(String::from_utf8_lossy(&o.stdout).trim().to_string()).filter(|k| !k.is_empty()));
    }
    if o.status.code() == Some(NOT_FOUND) || err.contains("could not be found") {
        return Ok(None);
    }
    bail!("Schlüsselbund ({svc}/{acct}): {}", funkhaus_sys::clip(err.trim(), 300))
}

/// Missing item is fine.
pub fn clear_with(bin: &str, svc: &str, acct: &str) -> Result<()> {
    plain("Dienst", svc)?;
    plain("Konto", acct)?;
    let o = Command::new(bin).args(["delete-generic-password", "-s", svc, "-a", acct]).output().with_context(|| format!("{bin} nicht startbar"))?;
    let err = String::from_utf8_lossy(&o.stderr);
    if o.status.success() || o.status.code() == Some(NOT_FOUND) || err.contains("could not be found") {
        return Ok(());
    }
    bail!("Schlüsselbund ({svc}/{acct}) löschen: {}", funkhaus_sys::clip(err.trim(), 300))
}

/// Shell script that mimics the three `security` subcommands on files in `dir` (tests only). Returns the script path.
#[cfg(any(test, feature = "testsrv"))]
pub fn fake(dir: &std::path::Path) -> std::path::PathBuf {
    let p = dir.join("fake-security");
    let s = format!(
        "#!/bin/sh\nD='{d}'\nif [ \"$1\" = -i ]; then read -r line; set -- $line; fi\ncmd=$1; shift\nwhile [ $# -gt 0 ]; do case \"$1\" in -s) s=$2; shift;; -a) a=$2; shift;; -w) [ -n \"$2\" ] && {{ w=$2; shift; }};; esac; shift; done\nf=\"$D/$s--$a\"\ncase $cmd in\n add-generic-password) printf '%s' \"$w\" > \"$f\";;\n find-generic-password) [ -f \"$f\" ] && cat \"$f\" || {{ echo 'security: SecKeychainSearchCopyNext: The specified item could not be found in the keychain.' >&2; exit 44; }};;\n delete-generic-password) [ -f \"$f\" ] && rm \"$f\" || {{ echo 'could not be found' >&2; exit 44; }};;\n *) echo Usage >&2; exit 2;;\nesac\n",
        d = dir.display()
    );
    std::fs::write(&p, s).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_on_fake() {
        let d = tempfile::tempdir().unwrap();
        let b = fake(d.path()).display().to_string();
        assert_eq!(load_with(&b, SERVICE, "claude").unwrap(), None);
        save_with(&b, SERVICE, "claude", "sk-ant-test-123").unwrap();
        assert_eq!(load_with(&b, SERVICE, "claude").unwrap().as_deref(), Some("sk-ant-test-123"));
        save_with(&b, SERVICE, "claude", "sk-ant-neu").unwrap();
        assert_eq!(load_with(&b, SERVICE, "claude").unwrap().as_deref(), Some("sk-ant-neu"));
        clear_with(&b, SERVICE, "claude").unwrap();
        clear_with(&b, SERVICE, "claude").unwrap();
        assert_eq!(load_with(&b, SERVICE, "claude").unwrap(), None);
        // nothing of the key in the fake's store path except the file content (never in argv): the dir holds no other file
        assert!(std::fs::read_dir(d.path()).unwrap().count() == 1);
        for bad in ["", "a b", "x\"y", "k\\n"] {
            assert!(save_with(&b, SERVICE, "claude", bad).is_err(), "{bad:?}");
        }
        assert!(save_with(&b, SERVICE, "mit leer", "k").is_err());
        let e = save_with("/nonexistent/security", SERVICE, "a", "k").unwrap_err().to_string();
        assert!(e.contains("nicht startbar"), "{e}");
    }
}
