//! Audio rendered from a ref voice (clone or anchored design), so `voice revoke|rm` can delete all of it later.
//! Index `voices.tsv` in each folder that collects such audio (`<proj>/takes/`, the probe cache): one line
//! `<sha256 of the ref clip>\t<path>\t<len>\t<mtime_ns>\t<persona>` per file as it was when recorded (older lines lack
//! the persona column and match any name). Purge deletes a file only if it is unchanged since then (an export
//! overwritten later with other voices stays). Every read-modify-write of an index runs under `<dir>/.voices.lock`
//! (`Lock`), so concurrent recorders (probe, CLI + GUI) and a purge neither lose nor resurrect lines.
use anyhow::{Context, Result};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

pub const INDEX: &str = "voices.tsv";
pub const LOCK: &str = ".voices.lock";

fn stamp(p: &Path) -> Option<(u64, u128)> {
    let m = std::fs::metadata(p).ok()?;
    Some((m.len(), m.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_nanos()))
}

/// Exclusive lock on the index of one folder (blocking; held only for short index edits, or by a purge for its whole
/// run so a recorder can re-check its voice after it).
pub struct Lock {
    dir: PathBuf,
    _f: std::fs::File,
}

impl Lock {
    pub fn take(dir: &Path) -> Result<Lock> {
        std::fs::create_dir_all(dir)?;
        let path = dir.join(LOCK);
        let f = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(&path).with_context(|| format!("Index-Sperre {} nicht anlegbar", path.display()))?;
        f.lock().with_context(|| format!("Index-Sperre {} fehlgeschlagen", path.display()))?;
        Ok(Lock { dir: dir.to_path_buf(), _f: f })
    }

    /// `record` under this lock.
    pub fn record(&self, sha: &str, name: &str, files: &[PathBuf]) -> Result<()> {
        record_in(&self.dir, sha, name, files)
    }

    /// `purge` under this lock.
    pub fn purge(&self, shas: &[String], name: &str) -> Result<(Vec<PathBuf>, Refused)> {
        purge_in(&self.dir, shas, name)
    }
}

/// Records `files` (existing ones, current size/mtime) of persona `name` under `sha` in `<dir>/voices.tsv`; a file
/// already listed under the same sha gets its line replaced (one line per sha + path).
pub fn record(dir: &Path, sha: &str, name: &str, files: &[PathBuf]) -> Result<()> {
    if sha.is_empty() {
        return Ok(());
    }
    Lock::take(dir)?.record(sha, name, files)
}

fn record_in(dir: &Path, sha: &str, name: &str, files: &[PathBuf]) -> Result<()> {
    if sha.is_empty() {
        return Ok(());
    }
    let idx = dir.join(INDEX);
    let old = std::fs::read_to_string(&idx).unwrap_or_default();
    // absolute: a later purge may run from another working dir (CLI with a relative project path, GUI)
    let files: Vec<PathBuf> = files.iter().map(|f| std::path::absolute(f).unwrap_or_else(|_| f.clone())).collect();
    let new: Vec<(String, String)> = files
        .iter()
        .filter(|f| !f.to_string_lossy().contains(['\t', '\n']))
        .filter_map(|f| stamp(f).map(|(l, m)| (f.display().to_string(), format!("{sha}\t{}\t{l}\t{m}\t{name}", f.display()))))
        .collect();
    let have: HashSet<&str> = old.lines().collect();
    if new.iter().all(|(_, l)| have.contains(l.as_str())) {
        return Ok(());
    }
    let paths: HashSet<&str> = new.iter().map(|(p, _)| p.as_str()).collect();
    let mut out: Vec<String> = old
        .lines()
        .filter(|l| {
            let f: Vec<&str> = l.split('\t').collect();
            !(f.len() >= 4 && f[0] == sha && paths.contains(f[1]))
        })
        .map(String::from)
        .collect();
    let mut seen = HashSet::new();
    out.extend(new.into_iter().filter(|(p, _)| seen.insert(p.clone())).map(|(_, l)| l));
    crate::render::atomic(&idx, |t| Ok(std::fs::write(t, out.iter().map(|l| format!("{l}\n")).collect::<String>())?))
}

/// Files a purge kept, with the reason.
pub type Refused = Vec<(PathBuf, String)>;

/// Extensions a purge may delete (takes, mixes, exports – also outside the data root with `--out`).
pub const AUDIO: &[&str] = &["wav", "mp3", "m4a", "flac"];

/// What `check` found at a path a purge wants to delete.
#[derive(Debug, PartialEq)]
pub enum Found {
    /// nothing there (also no dangling link)
    Gone,
    /// plain file with an allowed extension, one link: may be deleted
    Plain,
    /// must not be deleted (reason)
    Refused(String),
}

/// Whether purge may delete `p`: only a plain file (no symlink – also a dangling one –, no hard link) with one of the
/// extensions `exts`. Guards against a corrupted or hand-edited index pointing at arbitrary files; exports may
/// legitimately live anywhere.
pub fn check(p: &Path, exts: &[&str]) -> Found {
    use std::os::unix::fs::MetadataExt;
    let m = match std::fs::symlink_metadata(p) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Found::Gone,
        Err(e) => return Found::Refused(format!("nicht lesbar: {e}")),
    };
    if !m.file_type().is_file() {
        return Found::Refused("keine normale Datei (Link/Ordner)".into());
    }
    if !p.extension().and_then(|x| x.to_str()).is_some_and(|x| exts.contains(&x.to_ascii_lowercase().as_str())) {
        return Found::Refused(format!("keine erlaubte Datei ({})", exts.join("/")));
    }
    if m.nlink() != 1 {
        return Found::Refused("hat weitere harte Links".into());
    }
    Found::Plain
}

/// Deletes `p` if `check` allows it: `Ok(true)` deleted, `Ok(false)` nothing there, `Err(reason)` kept.
pub fn remove(p: &Path, exts: &[&str]) -> std::result::Result<bool, String> {
    match check(p, exts) {
        Found::Gone => Ok(false),
        Found::Refused(why) => Err(why),
        Found::Plain => std::fs::remove_file(p).map(|_| true).map_err(|e| format!("nicht löschbar: {e}")),
    }
}

/// Deletes every file recorded under one of `shas` (and persona `name`, if the line names one) in `<dir>/voices.tsv`
/// that is unchanged since (size + mtime) and a plain audio file (`check`). Lines of deleted or vanished files are
/// dropped; refused ones stay in the index (a later purge tries again) and come back with the reason.
pub fn purge(dir: &Path, shas: &[String], name: &str) -> Result<(Vec<PathBuf>, Refused)> {
    if !dir.join(INDEX).exists() {
        return Ok((vec![], vec![]));
    }
    Lock::take(dir)?.purge(shas, name)
}

fn purge_in(dir: &Path, shas: &[String], name: &str) -> Result<(Vec<PathBuf>, Refused)> {
    let idx = dir.join(INDEX);
    let Ok(s) = std::fs::read_to_string(&idx) else { return Ok((vec![], vec![])) };
    let (mut keep, mut gone, mut refused) = (String::new(), vec![], vec![]);
    for l in s.lines() {
        let f: Vec<&str> = l.split('\t').collect();
        let ours = (f.len() == 4 || (f.len() == 5 && f[4] == name)) && shas.iter().any(|x| x == f[0]);
        let p = PathBuf::from(f[1]);
        let why = if !ours {
            None
        } else {
            match check(&p, AUDIO) {
                Found::Gone => continue,
                Found::Refused(why) => Some(why),
                Found::Plain => match stamp(&p) {
                    Some((n, m)) if n.to_string() == f[2] && m.to_string() == f[3] => match std::fs::remove_file(&p) {
                        Ok(()) => {
                            gone.push(p);
                            continue;
                        }
                        Err(e) => Some(format!("nicht löschbar: {e}")),
                    },
                    _ => Some("seit der Aufnahme geändert (z. B. neu exportiert)".into()),
                },
            }
        };
        if let Some(why) = why {
            refused.push((p, why));
        }
        keep.push_str(l);
        keep.push('\n');
    }
    if keep.is_empty() {
        match std::fs::remove_file(&idx) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
            _ => {}
        }
    } else {
        crate::render::atomic(&idx, |t| Ok(std::fs::write(t, &keep)?))?;
    }
    Ok((gone, refused))
}

/// After a folder move `old` → `new`: every index line of `<dir>/voices.tsv` whose path lay under `old` now points
/// under `new` (`dir` is the index folder at its new place). Returns the number of rewritten lines.
pub fn rewrite_prefix(dir: &Path, old: &Path, new: &Path) -> Result<usize> {
    let idx = dir.join(INDEX);
    let Ok(s) = std::fs::read_to_string(&idx) else { return Ok(0) };
    let _l = Lock::take(dir)?;
    let mut n = 0;
    let out: String = s
        .lines()
        .map(|l| {
            let mut f: Vec<&str> = l.split('\t').collect();
            let moved = f.get(1).and_then(|p| Path::new(p).strip_prefix(old).ok()).map(|r| new.join(r).display().to_string());
            match moved {
                Some(p) if f.len() >= 4 => {
                    n += 1;
                    f[1] = &p;
                    f.join("\t") + "\n"
                }
                _ => format!("{l}\n"),
            }
        })
        .collect();
    if n > 0 {
        crate::render::atomic(&idx, |t| Ok(std::fs::write(t, &out)?))?;
    }
    Ok(n)
}

/// Existing files recorded in `<dir>/voices.tsv` that lie outside `inside` (exports with `--out`): they would lose
/// their only index entry when the folder goes.
pub fn outside(dir: &Path, inside: &Path) -> Vec<PathBuf> {
    let s = std::fs::read_to_string(dir.join(INDEX)).unwrap_or_default();
    s.lines().filter_map(|l| l.split('\t').nth(1)).map(PathBuf::from).filter(|p| !p.starts_with(inside) && p.exists()).collect()
}

/// Data-root index folder `<home>/derived`: lines of files that lost their project index (`project trash` with
/// exports on another volume). `voice revoke|rm` scans it like a project's `takes/`.
pub const HOME_DIR: &str = "derived";

pub fn home_index(home: &Path) -> PathBuf {
    home.join(HOME_DIR)
}

/// After single files moved (`fs::rename`, stamp unchanged): every line of `<dir>/voices.tsv` whose path is an `old`
/// in `moves` now carries its `new`. Returns the number of rewritten lines.
pub fn relocate(dir: &Path, moves: &[(PathBuf, PathBuf)]) -> Result<usize> {
    if moves.is_empty() {
        return Ok(0);
    }
    let _l = Lock::take(dir)?;
    let idx = dir.join(INDEX);
    let Ok(s) = std::fs::read_to_string(&idx) else { return Ok(0) };
    let map: std::collections::HashMap<String, String> = moves.iter().map(|(o, n)| (o.display().to_string(), n.display().to_string())).collect();
    let mut n = 0;
    let out: String = s
        .lines()
        .map(|l| {
            let mut f: Vec<&str> = l.split('\t').collect();
            match f.get(1).and_then(|p| map.get(*p)) {
                Some(p) if f.len() >= 4 => {
                    n += 1;
                    f[1] = p;
                    f.join("\t") + "\n"
                }
                _ => format!("{l}\n"),
            }
        })
        .collect();
    if n > 0 {
        crate::render::atomic(&idx, |t| Ok(std::fs::write(t, &out)?))?;
    }
    Ok(n)
}

/// Moves the lines of `files` from `<from>/voices.tsv` into `<to>/voices.tsv` (a folder about to go keeps its
/// elsewhere-exports findable at the data root). Returns the number of moved lines.
pub fn transfer(from: &Path, to: &Path, files: &[PathBuf]) -> Result<usize> {
    if files.is_empty() {
        return Ok(0);
    }
    let _lf = Lock::take(from)?;
    let fi = from.join(INDEX);
    let Ok(s) = std::fs::read_to_string(&fi) else { return Ok(0) };
    let set: HashSet<String> = files.iter().map(|p| p.display().to_string()).collect();
    let (mut keep, mut mv) = (String::new(), vec![]);
    for l in s.lines() {
        if l.split('\t').nth(1).is_some_and(|p| set.contains(p)) {
            mv.push(l);
        } else {
            keep.push_str(l);
            keep.push('\n');
        }
    }
    if mv.is_empty() {
        return Ok(0);
    }
    {
        let _lt = Lock::take(to)?;
        let ti = to.join(INDEX);
        let old = std::fs::read_to_string(&ti).unwrap_or_default();
        let have: HashSet<&str> = old.lines().collect();
        let mut out = old.clone();
        if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
        mv.iter().filter(|l| !have.contains(*l)).for_each(|l| {
            out.push_str(l);
            out.push('\n');
        });
        crate::render::atomic(&ti, |t| Ok(std::fs::write(t, &out)?))?;
    }
    crate::render::atomic(&fi, |t| Ok(std::fs::write(t, &keep)?))?;
    Ok(mv.len())
}

/// omnivoice's `--ref` prompt cache key: `sha1("<resolved wav>|<mtime_ns>|<ref_text>")[:16]` (cli/omnivoice_cli.py `vkey`).
pub fn omnivoice_key(wav: &Path, ref_text: &str) -> Option<String> {
    use sha1::{Digest, Sha1};
    let r = std::fs::canonicalize(wav).ok()?;
    let m = std::fs::metadata(&r).ok()?.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_nanos();
    let h = Sha1::digest(format!("{}|{m}|{ref_text}", r.display()).as_bytes());
    Some(h.iter().take(8).map(|b| format!("{b:02x}")).collect())
}

/// `<cache>/*.npz` whose stored `ref_text` is exactly `ref_text` (numpy `<U{n}` = UTF-32LE, uncompressed npz), so an
/// entry is found even when the clip's path or mtime changed since it was encoded.
pub fn npz_with_text(cache: &Path, ref_text: &str) -> Vec<PathBuf> {
    let n = ref_text.chars().count();
    let (descr, body): (String, Vec<u8>) = (format!("'<U{n}'"), ref_text.chars().flat_map(|c| (c as u32).to_le_bytes()).collect());
    let has = |hay: &[u8], needle: &[u8]| !needle.is_empty() && hay.windows(needle.len()).any(|w| w == needle);
    std::fs::read_dir(cache)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "npz"))
        .filter(|p| std::fs::read(p).is_ok_and(|b| has(&b, &body) && has(&b, descr.as_bytes())))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_purge_only_unchanged() {
        let t = tempfile::tempdir().unwrap();
        let (a, b, c) = (t.path().join("a.wav"), t.path().join("b.wav"), t.path().join("c.wav"));
        for f in [&a, &b, &c] {
            std::fs::write(f, b"x").unwrap();
        }
        record(t.path(), "s1", "p", &[a.clone(), b.clone()]).unwrap();
        record(t.path(), "s1", "p", std::slice::from_ref(&a)).unwrap();
        record(t.path(), "s2", "p", std::slice::from_ref(&c)).unwrap();
        assert_eq!(std::fs::read_to_string(t.path().join(INDEX)).unwrap().lines().count(), 3);
        std::thread::sleep(std::time::Duration::from_millis(10));
        std::fs::write(&b, b"later, other voices").unwrap();
        assert_eq!(purge(t.path(), &["s1".into()], "p").unwrap().0, std::slice::from_ref(&a));
        assert!(!a.exists() && b.exists() && c.exists());
        assert_eq!(purge(t.path(), &["s2".into()], "p").unwrap().0, [c]);
        // the changed file's line stays (refused, reported again later); once it is gone the index goes too
        assert_eq!(std::fs::read_to_string(t.path().join(INDEX)).unwrap().lines().count(), 1);
        std::fs::remove_file(&b).unwrap();
        assert_eq!(purge(t.path(), &["s1".into()], "p").unwrap(), (vec![], vec![]));
        assert!(!t.path().join(INDEX).exists());
        // no index (any more): nothing to do, no error
        assert_eq!(purge(t.path(), &["s2".into()], "p").unwrap(), (vec![], vec![]));
    }

    /// Lines name their persona: a purge for one name leaves another persona with the same clip alone; old 4-column
    /// lines match any name.
    #[test]
    fn purge_matches_persona_name() {
        let t = tempfile::tempdir().unwrap();
        let (a, b, c) = (t.path().join("a.wav"), t.path().join("b.wav"), t.path().join("c.wav"));
        for f in [&a, &b, &c] {
            std::fs::write(f, b"x").unwrap();
        }
        record(t.path(), "s", "eins", std::slice::from_ref(&a)).unwrap();
        record(t.path(), "s", "zwei", std::slice::from_ref(&b)).unwrap();
        let (l, m) = stamp(&c).unwrap();
        let idx = t.path().join(INDEX);
        let old = std::fs::read_to_string(&idx).unwrap();
        std::fs::write(&idx, format!("{old}s\t{}\t{l}\t{m}\n", c.display())).unwrap();
        assert_eq!(purge(t.path(), &["s".into()], "eins").unwrap().0, [a.clone(), c.clone()]);
        assert!(b.exists());
    }

    /// Concurrent recorders (threads standing in for CLI + GUI + probe) never lose each other's lines.
    #[test]
    fn concurrent_records_keep_every_line() {
        let t = tempfile::tempdir().unwrap();
        let files: Vec<PathBuf> = (0..40).map(|i| t.path().join(format!("{i}.wav"))).collect();
        files.iter().for_each(|f| std::fs::write(f, b"x").unwrap());
        std::thread::scope(|s| {
            for f in &files {
                s.spawn(|| record(t.path(), "s", "p", std::slice::from_ref(f)).unwrap());
            }
        });
        assert_eq!(std::fs::read_to_string(t.path().join(INDEX)).unwrap().lines().count(), 40);
    }

    #[test]
    fn check_and_remove() {
        let t = tempfile::tempdir().unwrap();
        let (n, w) = (t.path().join("k.npz"), t.path().join("k.wav"));
        std::fs::write(&n, b"x").unwrap();
        std::fs::write(&w, b"x").unwrap();
        let l = t.path().join("l.npz");
        std::os::unix::fs::symlink(&n, &l).unwrap();
        assert_eq!(remove(&l, &["npz"]), Err("keine normale Datei (Link/Ordner)".into()));
        assert!(remove(&w, &["npz"]).is_err() && w.exists());
        assert_eq!(remove(&n, &["npz"]), Ok(true));
        assert_eq!(remove(&n, &["npz"]), Ok(false));
        assert_eq!(check(&l, &["npz"]), Found::Refused("keine normale Datei (Link/Ordner)".into()), "dangling link");
    }

    #[test]
    fn omnivoice_key_and_npz_scan() {
        let t = tempfile::tempdir().unwrap();
        let w = t.path().join("r.wav");
        std::fs::write(&w, b"x").unwrap();
        let k = omnivoice_key(&w, "Hallo").unwrap();
        assert_eq!(k.len(), 16);
        assert_ne!(k, omnivoice_key(&w, "Hallo!").unwrap());
        let u = |s: &str| s.chars().flat_map(|c| (c as u32).to_le_bytes()).collect::<Vec<u8>>();
        std::fs::write(t.path().join("a.npz"), [b"{'descr': '<U5'".as_slice(), &u("Hallo")].concat()).unwrap();
        std::fs::write(t.path().join("b.npz"), [b"{'descr': '<U6'".as_slice(), &u("Hallo!")].concat()).unwrap();
        assert_eq!(npz_with_text(t.path(), "Hallo"), [t.path().join("a.npz")]);
    }

    /// `relocate` rewrites exactly the moved paths (stamp kept); `transfer` moves lines into another index without
    /// duplicating ones already there and leaves the rest.
    #[test]
    fn relocate_and_transfer_lines() {
        let t = tempfile::tempdir().unwrap();
        let (own, home) = (t.path().join("takes"), t.path().join("derived"));
        std::fs::create_dir_all(&own).unwrap();
        let (a, b, c) = (t.path().join("a.wav"), t.path().join("b.wav"), t.path().join("c.wav"));
        for f in [&a, &b, &c] {
            std::fs::write(f, b"x").unwrap();
        }
        record(&own, "s", "p", &[a.clone(), b.clone(), c.clone()]).unwrap();
        let a2 = own.join("extern/a.wav");
        std::fs::create_dir_all(a2.parent().unwrap()).unwrap();
        std::fs::rename(&a, &a2).unwrap();
        assert_eq!(relocate(&own, &[(a.clone(), a2.clone())]).unwrap(), 1);
        assert_eq!(relocate(&own, &[(a.clone(), a2.clone())]).unwrap(), 0, "already rewritten");
        let idx = std::fs::read_to_string(own.join(INDEX)).unwrap();
        assert!(idx.contains(&format!("\t{}\t", a2.display())) && !idx.contains(&format!("\t{}\t", a.display())), "{idx}");
        assert_eq!(transfer(&own, &home, std::slice::from_ref(&b)).unwrap(), 1);
        assert_eq!(transfer(&own, &home, std::slice::from_ref(&b)).unwrap(), 0, "gone from the source");
        assert_eq!(std::fs::read_to_string(own.join(INDEX)).unwrap().lines().count(), 2);
        let h = std::fs::read_to_string(home.join(INDEX)).unwrap();
        assert_eq!(h.lines().count(), 1);
        assert!(h.contains(&format!("\t{}\t", b.display())), "{h}");
        // the moved line keeps its stamp: a purge at the data root deletes the file
        assert_eq!(purge(&home, &["s".into()], "p").unwrap().0, std::slice::from_ref(&b));
        assert!(!b.exists() && c.exists() && a2.exists());
        assert_eq!(purge(&own, &["s".into()], "p").unwrap().0, [a2, c]);
    }

    #[test]
    fn record_replaces_line_of_same_path() {
        let t = tempfile::tempdir().unwrap();
        let a = t.path().join("a.wav");
        std::fs::write(&a, b"x").unwrap();
        record(t.path(), "s1", "p", std::slice::from_ref(&a)).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(10));
        std::fs::write(&a, b"rewritten").unwrap();
        record(t.path(), "s1", "p", std::slice::from_ref(&a)).unwrap();
        record(t.path(), "s2", "p", std::slice::from_ref(&a)).unwrap();
        let idx = std::fs::read_to_string(t.path().join(INDEX)).unwrap();
        assert_eq!(idx.lines().count(), 2, "{idx}");
        assert!(idx.lines().all(|l| l.contains("\t9\t")), "{idx}");
        assert_eq!(purge(t.path(), &["s1".into()], "p").unwrap().0, [a]);
    }

    /// A corrupted / hand-edited index deletes only plain audio files: no other extensions, symlinks, hard links;
    /// exports outside the data root stay deletable. Every refused entry is reported.
    #[test]
    fn purge_only_plain_audio() {
        let t = tempfile::tempdir().unwrap();
        let (own, other) = (t.path().join("proj"), t.path().join("elsewhere"));
        for d in [&own, &other] {
            std::fs::create_dir_all(d).unwrap();
        }
        let (doc, wav, mp3, hard) = (other.join("wichtig.txt"), other.join("export.wav"), own.join("mix.MP3"), own.join("hart.wav"));
        for f in [&doc, &wav, &mp3, &hard] {
            std::fs::write(f, b"x").unwrap();
        }
        std::fs::hard_link(&hard, other.join("hart-kopie.wav")).unwrap();
        let link = own.join("link.wav");
        std::os::unix::fs::symlink(&wav, &link).unwrap();
        record(&own, "s", "p", &[doc.clone(), wav.clone(), mp3.clone(), hard.clone(), link.clone()]).unwrap();
        let (gone, refused) = purge(&own, &["s".into()], "p").unwrap();
        assert_eq!(gone, [wav.clone(), mp3.clone()]);
        assert!(doc.exists() && hard.exists() && !wav.exists() && !mp3.exists());
        let r: Vec<(&Path, &str)> = refused.iter().map(|(p, w)| (p.as_path(), w.as_str())).collect();
        // the symlink's target is gone by now (deleted as export.wav): a dangling link is still refused, not "gone"
        let link_why = "keine normale Datei (Link/Ordner)";
        assert_eq!(r, [(doc.as_path(), "keine erlaubte Datei (wav/mp3/m4a/flac)"), (hard.as_path(), "hat weitere harte Links"), (link.as_path(), link_why)]);
        assert!(std::fs::symlink_metadata(&link).is_ok());
        // refused lines stay in the index: the next purge reports them again
        assert_eq!(std::fs::read_to_string(own.join(INDEX)).unwrap().lines().count(), 3);
        assert_eq!(purge(&own, &["s".into()], "p").unwrap().1.len(), 3);
    }
}
