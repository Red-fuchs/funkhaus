use crate::{Tts, lexicon::Lexicon, persona::{Kind, VoiceSpec}};
use funkhaus_audio::script::{Line, Seg, segments};
use funkhaus_sys::{clip, which};
use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use std::process::Command;

/// One rendered piece of a line: a cached wav or silence (seconds).
#[derive(Clone, Debug, PartialEq)]
pub enum Part {
    Wav(PathBuf),
    Pause(f64),
}

pub fn fnv(parts: &[&str]) -> u64 {
    parts.iter().flat_map(|p| p.bytes().chain([0u8])).fold(0xcbf2_9ce4_8422_2325, |h, b| (h ^ b as u64).wrapping_mul(0x0100_0000_01b3))
}

/// Bump when the rendering pipeline changes in a way that makes old clips wrong.
const CACHE_V: &str = "v1";

/// Builtin voices hash exactly the pre-persona fields (existing cached clips stay valid);
/// design/clone append instruct, ref content hash and ref_text.
pub fn key(t: &Tts, v: &VoiceSpec, lang: &str, text: &str) -> u64 {
    let seed = v.seed.unwrap_or(t.seed).to_string();
    let mut p = vec![CACHE_V, &t.cache_salt, &seed, &v.voice, lang, &t.quality, text];
    if v.kind != Kind::Builtin {
        p.extend(["instruct", &v.instruct, "ref", &v.ref_sha, &v.ref_text]);
    }
    fnv(&p)
}

/// Per-line seed from content (not index), so inserting/deleting lines keeps other lines' voices.
pub fn seed_of(t: &Tts, v: &VoiceSpec, lang: &str, text: &str) -> u64 {
    key(t, v, lang, text) & 0x7fff_ffff
}

/// Speaker id -> voice (funkhaus: `O`, `F`).
pub type Voices = std::collections::BTreeMap<String, VoiceSpec>;

/// The two funkhaus hosts as a speaker map.
pub fn hosts(o: VoiceSpec, f: VoiceSpec) -> Voices {
    Voices::from([("O".into(), o), ("F".into(), f)])
}

fn voice_of<'a>(voices: &'a Voices, l: &Line, n: usize) -> Result<&'a VoiceSpec> {
    voices.get(&l.who).with_context(|| format!("Zeile {n}: keine Stimme für Sprecher {:?}", l.who))
}

/// Per-line takes: `active` maps a base key to the salt of its chosen take; `keep` lists every take's
/// file hash so pruning never deletes alternates (that is what makes undo free).
#[derive(Clone, Debug, Default)]
pub struct Rerolls {
    pub active: std::collections::HashMap<u64, u64>,
    pub keep: std::collections::HashSet<u64>,
}

/// Hash (file name + seed source) of a rerolled take; the salt only affects its own base key.
pub fn salted(base: u64, salt: u64) -> u64 {
    fnv(&[&format!("{base:016x}"), "reroll", &salt.to_string()])
}

/// Base keys of the text segments of line `idx` (0-based), exactly as `render_with` computes them.
pub fn line_keys(t: &Tts, lines: &[Line], voices: &Voices, lex: &Lexicon, lang: &str, idx: usize) -> Result<Vec<u64>> {
    let l = lines.get(idx).with_context(|| format!("Zeile {} existiert nicht", idx + 1))?;
    let voice = voice_of(voices, l, idx + 1)?;
    Ok(segments(&l.text)?.into_iter().filter_map(|s| if let Seg::Text(x) = s { Some(key(t, voice, lang, &lex.apply(&x))) } else { None }).collect())
}

/// The real `omnivoice say` call under the GPU lock: `(wav, text, voice, seed)`.
pub fn say_cmd<'a>(t: &'a Tts, lang: &'a str, prog: &'a dyn Fn(&str)) -> Result<impl FnMut(&Path, &str, &VoiceSpec, u64) -> Result<()> + 'a> {
    let bin = which(&t.cmd).context("omnivoice nicht gefunden")?;
    Ok(move |wav: &Path, text: &str, v: &VoiceSpec, seed: u64| {
        let o = crate::gpu::with_lock(prog, || Command::new(&bin).args(say_args(text, wav, v, seed, lang, &t.quality)).env("OMNIVOICE_CACHE", funkhaus_sys::funkhaus_omnivoice_cache()).output())??;
        if !o.status.success() || !wav.exists() {
            bail!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
        }
        Ok(())
    })
}

/// Everything `render` needs besides the `say` callback.
pub struct RenderReq<'a> {
    pub t: &'a Tts,
    pub lines: &'a [Line],
    pub voices: &'a Voices,
    pub lex: &'a Lexicon,
    pub lang: &'a str,
    pub dir: &'a Path,
    pub fresh: bool,
    pub rr: &'a Rerolls,
    pub prog: &'a dyn Fn(&str),
    /// Checked before each line: set -> stops with `CANCELLED` (no pruning, finished clips stay cached).
    pub cancel: Option<&'a std::sync::atomic::AtomicBool>,
    /// Per-line state for structured progress: `(0-based index into lines, state)`; funkhaus passes `None`.
    pub on_line: Option<&'a dyn Fn(usize, LineState)>,
}

/// State of one line during `render_with`: `Rendering` before its first TTS call, then `Done`;
/// `Cached` if no TTS was needed; `Failed` if its TTS/validation failed (the error follows).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineState {
    Rendering,
    Cached,
    Done,
    Failed,
}

pub const CANCELLED: &str = "abgebrochen";

/// Renders each line's text segments to `<hash>.wav` via `omnivoice say`, one `Vec<Part>` per line in script order.
/// Text goes through the notebook lexicon first (hash and seed follow the post-lexicon text).
/// Pause tags never reach TTS or the cache key. Clips with an existing valid wav are reused unless `fresh`.
/// Each omnivoice call runs under the GPU lock (per line: other processes' interactive LLM calls wait at most one line).
pub fn render(q: &RenderReq) -> Result<Vec<Vec<Part>>> {
    let mut say = say_cmd(q.t, q.lang, q.prog)?;
    let guard = crate::gpu::PrioGuard::hold(); // whole render is interactive; dropped on error/panic too
    render_with(q, |w, x, v, s| {
        if let Some(g) = &guard {
            g.touch();
        }
        say(w, x, v, s)
    })
}

fn valid(p: &Path) -> bool {
    funkhaus_audio::mix::read(p).is_ok_and(|(x, _)| !x.is_empty())
}

/// `say(wav, text, voice, seed)`; injectable for tests. Re-validates all voices (clone consent) before any render.
pub fn render_with(q: &RenderReq, mut say: impl FnMut(&Path, &str, &VoiceSpec, u64) -> Result<()>) -> Result<Vec<Vec<Part>>> {
    let RenderReq { t, lines, voices, lex, lang, dir, fresh, rr, prog, cancel, on_line } = *q;
    let ev = |k: usize, s: LineState| {
        if let Some(f) = on_line {
            f(k, s)
        }
    };
    voices.values().try_for_each(VoiceSpec::validate)?;
    lines.iter().enumerate().try_for_each(|(k, l)| voice_of(voices, l, k + 1).map(drop))?;
    let mut out = vec![];
    let mut done = std::collections::HashSet::new();
    for (k, l) in lines.iter().enumerate() {
        if cancel.is_some_and(|c| c.load(std::sync::atomic::Ordering::Relaxed)) {
            bail!("{CANCELLED}");
        }
        let n = k + 1;
        let mut spoke = false;
        let mut line = || -> Result<Vec<Part>> {
            let voice = voice_of(voices, l, n)?;
            let mut parts = vec![];
            for seg in segments(&l.text).with_context(|| format!("Zeile {n}"))? {
                let text = match seg {
                    Seg::Pause(s) => {
                        parts.push(Part::Pause(s));
                        continue;
                    }
                    Seg::Text(x) => lex.apply(&x),
                };
                let base = key(t, voice, lang, &text);
                let h = rr.active.get(&base).map_or(base, |&s| salted(base, s));
                let wav = dir.join(format!("{h:016x}.wav"));
                if wav.exists() && !done.contains(&h) && (fresh || !valid(&wav)) {
                    let _ = std::fs::remove_file(&wav); // forced or corrupt: re-render
                }
                if !wav.exists() {
                    if !spoke {
                        spoke = true;
                        ev(k, LineState::Rendering);
                    }
                    prog(&format!("TTS {n}/{} [{}] {}", lines.len(), voice.label(), clip(&text, 60)));
                    let tmp = dir.join(format!("{h:016x}.part.wav"));
                    let r = say(&tmp, &text, voice, h & 0x7fff_ffff).with_context(|| format!("omnivoice Zeile {n}"));
                    if r.is_ok() && !valid(&tmp) {
                        let _ = std::fs::remove_file(&tmp);
                        bail!("omnivoice Zeile {n}: leere oder ungültige Ausgabe");
                    }
                    if let Err(e) = r {
                        let _ = std::fs::remove_file(&tmp);
                        return Err(e);
                    }
                    std::fs::rename(&tmp, &wav)?;
                }
                done.insert(h);
                parts.push(Part::Wav(wav));
            }
            Ok(parts)
        };
        let parts = line().inspect_err(|_| ev(k, LineState::Failed))?;
        ev(k, if spoke { LineState::Done } else { LineState::Cached });
        out.push(parts);
    }
    prune_orphans(dir, &out, &rr.keep);
    Ok(out)
}

/// One clip per line from rendered parts: speech trimmed (0.01 / 30 ms pad), `[pause]` as exact silence.
/// Returns the clips and their common sample rate (24 kHz if nothing was spoken).
pub fn clips(parts: &[Vec<Part>]) -> Result<(Vec<Vec<f32>>, u32)> {
    let mut sr0 = None;
    let mut raw = vec![];
    for w in parts.iter().flatten() {
        if let Part::Wav(w) = w {
            let (x, sr) = funkhaus_audio::mix::read(w)?;
            if *sr0.get_or_insert(sr) != sr {
                bail!("unterschiedliche Abtastraten in {}", w.display());
            }
            raw.push(x);
        }
    }
    let sr = sr0.unwrap_or(24_000);
    let mut raw = raw.into_iter();
    let out = parts
        .iter()
        .map(|ps| {
            ps.iter()
                .flat_map(|p| match p {
                    Part::Wav(_) => funkhaus_audio::mix::trim(&raw.next().unwrap(), sr, 0.01, 0.03).to_vec(),
                    Part::Pause(s) => vec![0f32; (s * sr as f64).round() as usize],
                })
                .collect()
        })
        .collect();
    Ok((out, sr))
}

/// Deletes `<16 hex>.wav` clips in `dir` that no line references (best effort).
fn prune_orphans(dir: &Path, parts: &[Vec<Part>], keep: &std::collections::HashSet<u64>) {
    let used: std::collections::HashSet<&Path> = parts.iter().flatten().filter_map(|p| if let Part::Wav(w) = p { Some(w.as_path()) } else { None }).collect();
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let p = e.path();
        let hex = p.file_name().and_then(|n| n.to_str()).and_then(|n| n.strip_suffix(".wav")).is_some_and(|s| s.len() == 16 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')));
        let kept = p.file_stem().and_then(|n| n.to_str()).and_then(|n| u64::from_str_radix(n, 16).ok()).is_some_and(|h| keep.contains(&h));
        if hex && !kept && !used.contains(p.as_path()) {
            let _ = std::fs::remove_file(&p);
        }
    }
}

/// Options first, then `--`, so text starting with "-" is never parsed as a flag.
pub fn say_args(text: &str, wav: &Path, v: &VoiceSpec, seed: u64, lang: &str, quality: &str) -> Vec<String> {
    let mut a: Vec<String> = ["say", "-o"].map(String::from).to_vec();
    a.push(wav.to_string_lossy().into_owned());
    match (v.kind, &v.ref_wav) {
        (Kind::Clone | Kind::Design, Some(r)) => a.extend(["--ref".into(), r.to_string_lossy().into_owned(), "--ref-text".into(), v.ref_text.clone()]),
        (Kind::Design, _) => a.extend(["--instruct".into(), v.instruct.clone()]),
        _ => a.extend(["--voice".into(), v.voice.clone()]),
    }
    a.extend(["--seed", &seed.to_string(), "--lang", lang].map(String::from));
    if !quality.is_empty() {
        a.extend(["--quality".to_string(), quality.to_string()]);
    }
    a.extend(["--".to_string(), text.to_string()]);
    a
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    fn tts() -> Tts {
        Tts { seed: 42, ..Tts::default() }
    }
    fn lines(n: usize) -> Vec<Line> {
        (1..=n).map(|i| Line::ofb(if i % 2 == 1 { 'O' } else { 'F' }, 0.0, format!("Satz {i}."))).collect()
    }
    fn rw(ls: &[Line], voices: &Voices, lex: &Lexicon, d: &Path, fresh: bool, say: impl FnMut(&Path, &str, &VoiceSpec, u64) -> Result<()>) -> Result<Vec<Vec<Part>>> {
        render_with(&RenderReq { t: &tts(), lines: ls, voices, lex, lang: "de", dir: d, fresh, rr: &Rerolls::default(), prog: &|_: &str| {}, cancel: None, on_line: None }, say)
    }
    fn run(d: &Path, ls: &[Line], n: &Cell<usize>) -> Vec<Vec<Part>> {
        let say = |w: &Path, _: &str, _: &VoiceSpec, _: u64| -> Result<()> {
            n.set(n.get() + 1);
            funkhaus_audio::mix::write(w, &[0.1f32; 100], 24_000)
        };
        rw(ls, &hosts(VoiceSpec::builtin("gt"), VoiceSpec::builtin("anna")), &Lexicon::default(), d, false, say).unwrap()
    }

    #[test]
    fn cancel_stops_before_next_line() {
        let (d, n) = (tempfile::tempdir().unwrap(), Cell::new(0));
        let stop = std::sync::atomic::AtomicBool::new(false);
        let say = |w: &Path, _: &str, _: &VoiceSpec, _: u64| -> Result<()> {
            n.set(n.get() + 1);
            stop.store(true, std::sync::atomic::Ordering::Relaxed);
            funkhaus_audio::mix::write(w, &[0.1f32; 100], 24_000)
        };
        let vs = hosts(VoiceSpec::builtin("gt"), VoiceSpec::builtin("anna"));
        let q = RenderReq { t: &tts(), lines: &lines(3), voices: &vs, lex: &Lexicon::default(), lang: "de", dir: d.path(), fresh: false, rr: &Rerolls::default(), prog: &|_: &str| {}, cancel: Some(&stop), on_line: None };
        assert_eq!(render_with(&q, say).unwrap_err().to_string(), CANCELLED);
        assert_eq!((n.get(), wavs(d.path()).len()), (1, 1));
    }

    #[test]
    fn line_states() {
        let d = tempfile::tempdir().unwrap();
        let ls: Vec<Line> = ["Eins.", "[pause 1s]", "Eins.", "Zwei."].iter().map(|t| Line::ofb('O', 0.0, *t)).collect();
        let vs = hosts(VoiceSpec::builtin("gt"), VoiceSpec::builtin("anna"));
        let got = std::cell::RefCell::new(vec![]);
        let on = |k: usize, s: LineState| got.borrow_mut().push((k, s));
        let mut fail_on_zwei = |w: &Path, x: &str, _: &VoiceSpec, _: u64| -> Result<()> {
            if x == "Zwei." {
                bail!("nope")
            }
            funkhaus_audio::mix::write(w, &[0.1f32; 100], 24_000)
        };
        let q = RenderReq { t: &tts(), lines: &ls, voices: &vs, lex: &Lexicon::default(), lang: "de", dir: d.path(), fresh: false, rr: &Rerolls::default(), prog: &|_: &str| {}, cancel: None, on_line: Some(&on) };
        assert!(render_with(&q, &mut fail_on_zwei).is_err());
        use LineState::*;
        assert_eq!(*got.borrow(), [(0, Rendering), (0, Done), (1, Cached), (2, Cached), (3, Rendering), (3, Failed)]);
    }

    #[test]
    fn clips_trim_and_pause() {
        let d = tempfile::tempdir().unwrap();
        let w = d.path().join("a.wav");
        funkhaus_audio::mix::write(&w, &[[0f32; 2400].as_slice(), &[0.5; 240], &[0.0; 2400]].concat(), 24_000).unwrap();
        let (c, sr) = clips(&[vec![Part::Wav(w.clone()), Part::Pause(0.5)], vec![]]).unwrap();
        assert_eq!((sr, c.len(), c[0].len(), c[1].len()), (24_000, 2, 720 + 240 + 720 - 1 + 12_000, 0));
    }

    #[test]
    fn delete_line_3_of_10_renders_nothing() {
        let (d, n) = (tempfile::tempdir().unwrap(), Cell::new(0));
        let ls = lines(10);
        run(d.path(), &ls, &n);
        assert_eq!(n.get(), 10);
        n.set(0);
        // removing 3 and 4 keeps every following speaker's voice (alternation preserved)
        let mut cut = ls.clone();
        cut.drain(2..4);
        run(d.path(), &cut, &n);
        assert_eq!(n.get(), 0);
    }

    #[test]
    fn insert_line_renders_exactly_one() {
        let (d, n) = (tempfile::tempdir().unwrap(), Cell::new(0));
        let first = run(d.path(), &lines(10), &n);
        n.set(0);
        let mut ins = lines(10);
        ins.insert(4, Line::ofb('O', 0.0, "Neuer Satz."));
        let second = run(d.path(), &ins, &n);
        assert_eq!(n.get(), 1);
        assert_eq!(second.iter().flatten().filter(|p| first.iter().flatten().any(|q| q == *p)).count(), 10);
    }

    #[test]
    fn insert_backchannel_renders_exactly_one() {
        let (d, n) = (tempfile::tempdir().unwrap(), Cell::new(0));
        let first = run(d.path(), &lines(10), &n);
        n.set(0);
        let mut ins = lines(10);
        ins.insert(4, Line::ofb('B', 0.5, "Mhm.")); // B after F (line 4): voiced by O
        funkhaus_audio::script::resolve(&mut ins);
        let second = run(d.path(), &ins, &n);
        assert_eq!(n.get(), 1);
        assert_eq!(second.iter().flatten().filter(|p| first.iter().flatten().any(|q| q == *p)).count(), 10);
    }

    #[test]
    fn speaker_change_rerenders_that_line() {
        let (d, n) = (tempfile::tempdir().unwrap(), Cell::new(0));
        let mut ls = lines(4);
        ls[2] = Line::ofb('B', 0.5, "Mhm."); // line 3 spoken by the other host than line 2 ('F') -> 'O'
        funkhaus_audio::script::resolve(&mut ls);
        assert_eq!(ls[2].who, "O");
        run(d.path(), &ls, &n);
        n.set(0);
        ls[1].who = "O".into(); // line 2 now O, so the B in line 3 is voiced by F
        ls[2].who.clear();
        funkhaus_audio::script::resolve(&mut ls);
        run(d.path(), &ls, &n);
        assert!(n.get() >= 1);
        n.set(0);
        run(d.path(), &ls, &n);
        assert_eq!(n.get(), 0);
    }

    #[test]
    fn pause_not_rendered_nor_hashed() {
        let (d, n) = (tempfile::tempdir().unwrap(), Cell::new(0));
        let l = |t: &str| vec![Line::ofb('O', 0.0, t)];
        let a = run(d.path(), &l("Hallo. [pause 1.5s] Ja."), &n);
        assert_eq!(n.get(), 2);
        assert_eq!(a[0].len(), 3);
        assert_eq!(a[0][1], Part::Pause(1.5));
        let b = run(d.path(), &l("Hallo. [pause 2s] Ja."), &n);
        assert_eq!(n.get(), 2);
        assert_eq!((&a[0][0], &a[0][2]), (&b[0][0], &b[0][2]));
        let c = run(d.path(), &l("[pause 300ms]"), &n);
        assert_eq!((n.get(), c[0].clone()), (2, vec![Part::Pause(0.3)]));
    }

    #[test]
    fn orphans_pruned_only_hex_wavs() {
        let (d, n) = (tempfile::tempdir().unwrap(), Cell::new(0));
        run(d.path(), &lines(3), &n);
        let keep = ["mix.wav", "script.txt", "01.wav", "0123456789abcdef.part.wav", "0123456789ABCDEF.wav", "podcast.mp3"];
        for k in keep {
            std::fs::write(d.path().join(k), b"x").unwrap();
        }
        std::fs::write(d.path().join("0123456789abcdef.wav"), b"x").unwrap(); // orphan
        run(d.path(), &lines(2), &n); // line 3 dropped
        let mut left: Vec<String> = std::fs::read_dir(d.path()).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        left.sort();
        assert!(!left.contains(&"0123456789abcdef.wav".to_string()));
        assert!(keep.iter().all(|k| left.contains(&k.to_string())));
        assert_eq!(left.iter().filter(|f| f.len() == 20 && f.ends_with(".wav") && f.bytes().take(16).all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())).count(), 2);
    }

    #[test]
    fn old_nn_files_do_not_crash() {
        let (d, n) = (tempfile::tempdir().unwrap(), Cell::new(0));
        std::fs::write(d.path().join("01.wav"), b"x").unwrap();
        std::fs::write(d.path().join("01.key"), "old").unwrap();
        run(d.path(), &lines(2), &n);
        assert_eq!(n.get(), 2);
    }

    fn wavs(d: &Path) -> Vec<PathBuf> {
        std::fs::read_dir(d).unwrap().map(|e| e.unwrap().path()).collect()
    }

    #[test]
    fn invalid_output_is_not_cached() {
        let d = tempfile::tempdir().unwrap();
        let ls = lines(1);
        for bad in [&b""[..], b"garbage"] {
            let say = |w: &Path, _: &str, _: &VoiceSpec, _: u64| -> Result<()> { Ok(std::fs::write(w, bad)?) };
            let e = rw(&ls, &hosts(VoiceSpec::builtin("gt"), VoiceSpec::builtin("anna")), &Lexicon::default(), d.path(), false, say).unwrap_err().to_string();
            assert!(e.contains("ungültige"), "{e}");
            assert!(wavs(d.path()).is_empty());
        }
    }

    #[test]
    fn corrupt_cached_wav_rerendered_once() {
        let (d, n) = (tempfile::tempdir().unwrap(), Cell::new(0));
        let ls = lines(2);
        run(d.path(), &ls, &n);
        n.set(0);
        let p = wavs(d.path()).pop().unwrap();
        std::fs::write(&p, b"junk").unwrap();
        run(d.path(), &ls, &n);
        assert_eq!(n.get(), 1);
        assert!(valid(&p));
    }

    #[test]
    fn fresh_rerenders_all_and_salt_changes_key() {
        let (d, n) = (tempfile::tempdir().unwrap(), Cell::new(0));
        let ls = lines(3);
        run(d.path(), &ls, &n);
        n.set(0);
        let say = |w: &Path, _: &str, _: &VoiceSpec, _: u64| -> Result<()> {
            n.set(n.get() + 1);
            funkhaus_audio::mix::write(w, &[0.1f32; 100], 24_000)
        };
        rw(&ls, &hosts(VoiceSpec::builtin("gt"), VoiceSpec::builtin("anna")), &Lexicon::default(), d.path(), true, say).unwrap();
        assert_eq!(n.get(), 3);
        assert_eq!(wavs(d.path()).len(), 3);
        let salted = Tts { cache_salt: "m2".into(), ..tts() };
        let gt = VoiceSpec::builtin("gt");
        assert_ne!(key(&tts(), &gt, "de", "x"), key(&salted, &gt, "de", "x"));
    }

    #[test]
    fn seed_depends_on_content_not_index() {
        let (t, gt) = (tts(), VoiceSpec::builtin("gt"));
        let a = seed_of(&t, &gt, "de", "Hallo.");
        assert_eq!(a, seed_of(&t, &gt, "de", "Hallo."));
        assert_ne!(a, seed_of(&t, &VoiceSpec::builtin("anna"), "de", "Hallo."));
        assert_ne!(a, seed_of(&t, &gt, "de", "Hallo!"));
        assert_ne!(a, seed_of(&Tts { seed: 8, ..t }, &gt, "de", "Hallo."));
    }

    #[test]
    fn builtin_key_is_golden() {
        // value of the pre-persona key (v1, salt, seed, voice, lang, quality, text): existing cached clips stay valid
        assert_eq!(key(&tts(), &VoiceSpec::builtin("gt"), "de", "Hallo."), 0x7002206acf103046);
        // persona seed equal to config seed does not change it
        assert_eq!(key(&tts(), &VoiceSpec { seed: Some(42), ..VoiceSpec::builtin("gt") }, "de", "Hallo."), key(&tts(), &VoiceSpec::builtin("gt"), "de", "Hallo."));
    }

    fn clone_spec(sha: &str) -> VoiceSpec {
        VoiceSpec { kind: Kind::Clone, ref_wav: Some("/r.wav".into()), ref_text: "t".into(), ref_sha: sha.into(), ref_ok: true, ..Default::default() }
    }

    #[test]
    fn key_fields_per_kind() {
        let t = tts();
        let d = |i: &str| VoiceSpec { kind: Kind::Design, instruct: i.into(), ..Default::default() };
        assert_ne!(key(&t, &d("female"), "de", "x"), key(&t, &d("male"), "de", "x"));
        assert_ne!(key(&t, &clone_spec("a"), "de", "x"), key(&t, &clone_spec("b"), "de", "x"));
        let mut c = clone_spec("a");
        c.ref_text = "u".into();
        assert_ne!(key(&t, &clone_spec("a"), "de", "x"), key(&t, &c, "de", "x"));
        assert_ne!(key(&t, &VoiceSpec::builtin("gt"), "de", "x"), key(&t, &d(""), "de", "x"));
    }

    #[test]
    fn clone_without_consent_refused_at_render() {
        let d = tempfile::tempdir().unwrap();
        let bad = VoiceSpec { ref_ok: false, ..clone_spec("a") };
        let say = |_: &Path, _: &str, _: &VoiceSpec, _: u64| -> Result<()> { panic!("must not render") };
        let e = rw(&lines(2), &hosts(bad, VoiceSpec::builtin("gt")), &Lexicon::default(), d.path(), false, say).unwrap_err().to_string();
        assert!(e.contains("Einwilligung"), "{e}");
    }

    #[test]
    fn lexicon_edit_rerenders_only_affected_lines() {
        let (d, n) = (tempfile::tempdir().unwrap(), Cell::new(0));
        let ls: Vec<Line> = ["Lewa ist schön.", "Ruse auch.", "Nichts."].iter().map(|t| Line::ofb('O', 0.0, *t)).collect();
        let go = |lex: &str| {
            let say = |w: &Path, _: &str, _: &VoiceSpec, _: u64| -> Result<()> {
                n.set(n.get() + 1);
                funkhaus_audio::mix::write(w, &[0.1f32; 100], 24_000)
            };
            let (g, a) = (VoiceSpec::builtin("gt"), VoiceSpec::builtin("anna"));
            rw(&ls, &hosts(g, a), &Lexicon::parse(lex), d.path(), false, say).unwrap();
        };
        go("Lewa\tLehwah\nRuse\tRusse\n");
        assert_eq!(n.get(), 3);
        n.set(0);
        go("# Kommentar\nLewa\tLehwah\nRuse\tRusse\n");
        assert_eq!(n.get(), 0);
        go("Lewa\tLewah\nRuse\tRusse\n");
        assert_eq!(n.get(), 1);
    }

    #[test]
    fn say_args_per_kind() {
        let w = std::path::Path::new("a.wav");
        let a = say_args("-minus", w, &VoiceSpec::builtin("gt"), 7, "de", "");
        assert_eq!(&a[a.len() - 2..], ["--", "-minus"]);
        assert_eq!(a[..5], ["say", "-o", "a.wav", "--voice", "gt"]);
        let d = VoiceSpec { kind: Kind::Design, instruct: "female".into(), ..Default::default() };
        let a = say_args("x", w, &d, 7, "de", "");
        assert!(a.windows(2).any(|p| p == ["--instruct", "female"]) && !a.contains(&"--voice".to_string()));
        let a = say_args("-x", w, &clone_spec("a"), 7, "de", "hq");
        assert!(a.windows(4).any(|p| p == ["--ref", "/r.wav", "--ref-text", "t"]) && !a.contains(&"--voice".to_string()));
        assert_eq!(&a[a.len() - 2..], ["--", "-x"]);
    }

    #[test]
    fn n_speakers_map_and_missing_voice() {
        let d = tempfile::tempdir().unwrap();
        let ls = funkhaus_audio::script::parse("Anna|0|Hallo.\nBen|0.1|Hallo.\n+Cleo|0.5|Mhm.").unwrap();
        let vs: Voices = [("Anna", "gt"), ("Ben", "anna"), ("Cleo", "fable")].map(|(k, v)| (k.to_string(), VoiceSpec::builtin(v))).into();
        let used = std::cell::RefCell::new(vec![]);
        let say = |w: &Path, _: &str, v: &VoiceSpec, _: u64| -> Result<()> {
            used.borrow_mut().push(v.voice.clone());
            funkhaus_audio::mix::write(w, &[0.1f32; 100], 24_000)
        };
        let p = rw(&ls, &vs, &Lexicon::default(), d.path(), false, say).unwrap();
        assert_eq!(*used.borrow(), ["gt", "anna", "fable"]);
        assert_ne!(p[0], p[1]); // same text, other voice
        assert_eq!(line_keys(&tts(), &ls, &vs, &Lexicon::default(), "de", 2).unwrap(), vec![key(&tts(), &VoiceSpec::builtin("fable"), "de", "Mhm.")]);
        let e = rw(&ls, &hosts(VoiceSpec::builtin("gt"), VoiceSpec::builtin("anna")), &Lexicon::default(), d.path(), false, |_: &Path, _: &str, _: &VoiceSpec, _: u64| -> Result<()> { panic!("must not render") }).unwrap_err().to_string();
        assert!(e.contains("keine Stimme") && e.contains("Anna"), "{e}");
    }

    #[test]
    fn funkhaus_ofb_keys_unchanged() {
        // B voiced by the other host: same key as that host saying the text (old speakers() mapping)
        let ls = funkhaus_audio::script::parse("O|0|Eins.\nF|0.1|Zwei.\nB|0.5|Mhm.").unwrap();
        let (gt, an) = (VoiceSpec::builtin("gt"), VoiceSpec::builtin("anna"));
        let vs = hosts(gt.clone(), an.clone());
        let k = |i| line_keys(&tts(), &ls, &vs, &Lexicon::default(), "de", i).unwrap();
        assert_eq!(k(0), vec![key(&tts(), &gt, "de", "Eins.")]);
        assert_eq!(k(1), vec![key(&tts(), &an, "de", "Zwei.")]);
        assert_eq!(k(2), vec![key(&tts(), &gt, "de", "Mhm.")]);
    }

    /// funkhaus' omnivoice calls keep the `--ref` prompt cache under the funkhaus home (purgeable), not in omnivoice's repo.
    #[test]
    fn say_cmd_sets_omnivoice_cache() {
        let t = tempfile::tempdir().unwrap();
        let fake = t.path().join("fake.sh");
        let env = t.path().join("env.txt");
        // args: say -o WAV ...
        std::fs::write(&fake, format!("#!/bin/sh\nprintf '%s' \"$OMNIVOICE_CACHE\" > '{}'\n: > \"$3\"\n", env.display())).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        let tt = Tts { cmd: fake.display().to_string(), ..tts() };
        let wav = t.path().join("o.wav");
        let mut say = say_cmd(&tt, "de", &|_| {}).unwrap();
        say(&wav, "Hallo.", &VoiceSpec::builtin("gt"), 1).unwrap();
        assert_eq!(std::fs::read_to_string(&env).unwrap(), funkhaus_sys::funkhaus_omnivoice_cache().display().to_string());
    }
}
