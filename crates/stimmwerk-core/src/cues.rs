//! Music/ambience cues: `[music]` + `[[cues]]` in project.toml (schema, lint) and their placement in the export mix.
//!
//! A cue starts at a script line (`at_line`, 1-based over spoken + overlay lines) or at `at_sec`; its audio is a file under
//! `assets/` or an acestep generation (`gen`, cached in `takes/music/`, see `crate::music`). Loops (`loop` / `loop_until_line`)
//! are crossfaded (`[music].crossfade_ms`) and end at the start of `loop_until_line` or with the mix (bed: runs under the
//! spoken AI disclosure, fades out over the last `BED_FADE` s, `mix_len`); `duck`
//! dips the cue under speech (`duck_db` per cue, else `[music].duck_db`). Speech is never ducked. Mixing is pure Rust
//! (`funkhaus_audio::music::{render_cue, duck_with}`); ffmpeg only resamples a source whose rate differs from the mix.
use crate::Project;
use crate::lint::Issue;
use anyhow::{Context, Result, bail};
use funkhaus_audio::music::{self as am, CueRender, DuckParams};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Music,
    Ambience,
    Sfx,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Music => "music",
            Kind::Ambience => "ambience",
            Kind::Sfx => "sfx",
        }
    }
}

/// acestep generation of a cue (always instrumental); cache file `takes/music/<key>.wav` (`crate::music::key`).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Gen {
    /// English caption, no digits (bpm/key are fields)
    pub prompt: String,
    /// seconds, 10..=600 (acestep minimum 10)
    pub dur: u32,
    pub seed: u64,
    /// 0 = free
    #[serde(default, skip_serializing_if = "is_zero")]
    pub bpm: u32,
    /// e.g. "D major"
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    /// acestep `--dit` (turbo | sft | base | xl-turbo | …); unset = acestep default
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

pub const DUR_MIN: u32 = 10;
pub const DUR_MAX: u32 = 600;

impl Gen {
    pub fn check(&self) -> Result<()> {
        if self.prompt.trim().is_empty() {
            bail!("gen.prompt fehlt");
        }
        if !(DUR_MIN..=DUR_MAX).contains(&self.dur) {
            bail!("gen.dur {} außerhalb {DUR_MIN}..={DUR_MAX} s (acestep-Minimum {DUR_MIN} s)", self.dur);
        }
        Ok(())
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Cue {
    /// 1-based index into spoken + overlay lines (not the script.txt line); cue starts with that line
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at_line: Option<usize>,
    /// seconds from mix start (alternative to `at_line`)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at_sec: Option<f64>,
    /// `assets/…`, relative to the project dir
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(default, rename = "gen", skip_serializing_if = "Option::is_none")]
    pub gen_: Option<Gen>,
    pub kind: Kind,
    /// absolute dB re full scale of the source
    #[serde(default)]
    pub gain_db: f32,
    /// s, linear
    #[serde(default)]
    pub fade_in: f64,
    #[serde(default)]
    pub fade_out: f64,
    /// seamless loop until `loop_until_line` or the end of the last script line
    #[serde(default, rename = "loop")]
    pub loop_: bool,
    /// cue ends at the start of this line (exclusive); `n_lines + 1` = with the mix (bed). Implies `loop`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub loop_until_line: Option<usize>,
    /// dips under speech; depth = `duck_db` else `[music].duck_db`
    #[serde(default)]
    pub duck: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duck_db: Option<f32>,
}

impl Cue {
    pub fn new(kind: Kind) -> Self {
        Cue { at_line: None, at_sec: None, file: None, gen_: None, kind, gain_db: 0.0, fade_in: 0.0, fade_out: 0.0, loop_: false, loop_until_line: None, duck: false, duck_db: None }
    }

    pub fn looping(&self) -> bool {
        self.loop_ || self.loop_until_line.is_some()
    }

    /// Short label for reports: the asset path or `gen:<key>`.
    pub fn label(&self) -> String {
        match (&self.file, &self.gen_) {
            (Some(f), _) => f.clone(),
            (None, Some(g)) => format!("gen:{}", crate::music::key(g)),
            (None, None) => "?".into(),
        }
    }

    /// Where the cue starts in the mix: `at_sec` or the start of `at_line`. None if neither resolves.
    pub fn start(&self, starts: &[usize], sr: u32) -> Option<usize> {
        match (self.at_line, self.at_sec) {
            (Some(l), None) => starts.get(l.checked_sub(1)?).copied(),
            (None, Some(s)) if s.is_finite() && s >= 0.0 => Some((s * sr as f64).round() as usize),
            _ => None,
        }
    }
}

/// `[music]`: ducking and loop crossfade of all cues.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct MusicCfg {
    /// default dip for `duck = true` (negative dB)
    pub duck_db: f32,
    pub duck_thresh_db: f32,
    pub duck_attack_ms: f32,
    pub duck_release_ms: f32,
    /// duck stays down this long after speech (bridges word gaps)
    pub duck_hold_ms: f32,
    /// loop seam crossfade
    pub crossfade_ms: f32,
}

impl Default for MusicCfg {
    fn default() -> Self {
        Self { duck_db: -12.0, duck_thresh_db: -50.0, duck_attack_ms: 20.0, duck_release_ms: 800.0, duck_hold_ms: 300.0, crossfade_ms: 2000.0 }
    }
}

impl MusicCfg {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
    pub fn duck_params(&self, depth: Option<f32>) -> DuckParams {
        DuckParams { depth_db: depth.unwrap_or(self.duck_db), thresh_db: self.duck_thresh_db, attack_ms: self.duck_attack_ms, release_ms: self.duck_release_ms, hold_ms: self.duck_hold_ms }
    }
}

/// `assets/<rest>` → `<rest>` (normal components only, no `..`, not absolute); None otherwise.
pub fn rel(f: &str) -> Option<&Path> {
    let p = Path::new(f);
    match p.components().next()? {
        Component::Normal(a) if a == "assets" => {}
        _ => return None,
    }
    let rest = p.strip_prefix("assets").ok()?;
    (!rest.as_os_str().is_empty() && rest.components().all(|c| matches!(c, Component::Normal(_)))).then_some(rest)
}

/// Absolute path of a cue's source: the asset file or the gen cache file (which may not exist yet).
pub fn source_path(p: &Project, c: &Cue) -> Option<PathBuf> {
    match (&c.file, &c.gen_) {
        (Some(f), _) => rel(f).map(|r| p.dir.join("assets").join(r)),
        (None, Some(g)) => Some(crate::music::path(p, g)),
        _ => None,
    }
}

/// Source samples of a cue at the mix rate `sr` (resampled through ffmpeg if the file's rate differs).
pub fn source(p: &Project, c: &Cue, ffmpeg: &str, sr: u32) -> Result<Vec<f32>> {
    let f = source_path(p, c).context("Cue ohne Quelle (file oder gen)")?;
    funkhaus_voice::music::load(ffmpeg, &f, sr).with_context(|| format!("Cue-Quelle {}", f.display()))
}

fn issue(level: &'static str, code: &'static str, i: usize, msg: String) -> Issue {
    Issue { level, code, line: None, msg: format!("Cue {i}: {msg}") }
}

/// Structural checks of all cues (no timeline): errors stop the export, warnings are advice. `n` = script lines.
pub fn lint(p: &Project) -> Vec<Issue> {
    let n = p.script.lines().count();
    let cfg = &p.meta.music;
    // estimated length of each script line (13 chars/s + pauses + gap): where a non-looping cue of known length ends
    let est: Vec<f64> = p.script.lines().map(|l| crate::project::est_secs(&l.text) + f64::from(l.gap.max(0.0))).collect();
    let mut is = vec![];
    // (start line, end line exclusive) of line-anchored cues with `duck`, for the double-dip warning
    let mut ducked: Vec<(usize, usize, usize)> = vec![];
    for (k, c) in p.meta.cues.iter().enumerate() {
        let i = k + 1;
        let (mut errs, mut warns) = (vec![], vec![]);
        let mut e = |code: &'static str, msg: String| errs.push(issue("error", code, i, msg));
        match (c.at_line, c.at_sec) {
            (Some(l), None) if !(1..=n).contains(&l) => e("cue_at", format!("at_line {l} außerhalb 1..={n}")),
            (None, Some(s)) if !(s.is_finite() && s >= 0.0) => e("cue_at", format!("at_sec {s} muss ≥ 0 sein")),
            (Some(_), None) | (None, Some(_)) => {}
            _ => e("cue_at", "genau eins von at_line oder at_sec angeben".into()),
        }
        // source: length in s (None = unknown) for the loop/fade checks
        let mut secs: Option<f64> = None;
        match (&c.file, &c.gen_) {
            (Some(f), None) => match rel(f) {
                None => e("cue_src", format!("file „{f}“: nur relativ unter assets/, ohne .. oder /")),
                Some(r) => {
                    let path = p.dir.join("assets").join(r);
                    if !path.is_file() {
                        e("cue_src", format!("{f} fehlt – `stimmwerk cue import` oder Datei nach assets/ legen"));
                    } else {
                        match funkhaus_audio::mix::info(&path) {
                            Err(x) => e("cue_src", format!("{f} nicht lesbar ({x:#}) – `stimmwerk cue import` wandelt um")),
                            Ok((_, _, ch)) if ch > 2 => e("cue_src", format!("{f}: {ch} Kanäle – nur mono oder stereo")),
                            Ok((fr, rate, _)) => secs = Some(fr as f64 / rate.max(1) as f64),
                        }
                    }
                }
            },
            (None, Some(g)) => match g.check() {
                Err(x) => e("cue_src", format!("{x}")),
                Ok(()) => {
                    let path = crate::music::path(p, g);
                    match funkhaus_audio::mix::info(&path) {
                        Ok((fr, rate, _)) => secs = Some(fr as f64 / rate.max(1) as f64),
                        Err(_) => {
                            secs = Some(g.dur as f64);
                            warns.push(issue("warn", "cue_gen", i, format!("Musik noch nicht erzeugt (gen:{}) – `stimmwerk music gen` oder der Export ruft acestep", crate::music::key(g))));
                        }
                    }
                }
            },
            _ => e("cue_src", "genau eins von file oder gen angeben".into()),
        }
        if let Some(end) = c.loop_until_line {
            match c.at_line {
                None => e("cue_loop", "loop_until_line braucht at_line".into()),
                Some(at) if end <= at || end > n + 1 => e("cue_loop", format!("loop_until_line {end} muss in {}..={} liegen ({} = Ende)", at + 1, n + 1, n + 1)),
                _ => {}
            }
        }
        if c.looping() {
            if c.kind == Kind::Sfx {
                e("cue_loop", "sfx wird nicht geloopt (kind music oder ambience)".into());
            }
            let x = cfg.crossfade_ms as f64 / 1000.0;
            if secs.is_some_and(|s| s <= 2.0 * x) {
                e("cue_loop", format!("Loop-Quelle {:.1} s ≤ 2 × Crossfade {x} s", secs.unwrap_or(0.0)));
            }
        }
        if !(c.fade_in.is_finite() && c.fade_in >= 0.0 && c.fade_out.is_finite() && c.fade_out >= 0.0) {
            e("cue_fade", format!("fade_in {} / fade_out {} müssen ≥ 0 sein", c.fade_in, c.fade_out));
        } else if !c.looping() && secs.is_some_and(|s| c.fade_in + c.fade_out > s) {
            e("cue_fade", format!("fade_in + fade_out {} s länger als die Quelle {:.1} s", c.fade_in + c.fade_out, secs.unwrap_or(0.0)));
        }
        if !(c.gain_db.is_finite() && (GAIN_MIN..=GAIN_MAX).contains(&c.gain_db)) {
            e("cue_gain", format!("gain_db {} außerhalb −{}..+{}", c.gain_db, -GAIN_MIN, GAIN_MAX));
        }
        if let Some(d) = c.duck_db.filter(|d| !(d.is_finite() && (-60.0..0.0).contains(d))) {
            e("cue_duck", format!("duck_db {d} muss in −60..0 liegen"));
        }
        if c.duck && c.kind == Kind::Sfx {
            warns.push(issue("warn", "cue_duck", i, "Ducking auf einem Geräusch (sfx) ist selten gewollt".into()));
        }
        if let (true, Some(at)) = (c.duck, c.at_line) {
            let end = c.loop_until_line.unwrap_or(if c.loop_ { n + 1 } else { secs.map_or(at + 1, |s| end_line(&est, at, s)) });
            if let Some((j, ..)) = ducked.iter().find(|(_, s, e)| at < *e && *s < end) {
                warns.push(issue("warn", "cue_overlap", i, format!("duckt gleichzeitig mit Cue {j} (Zeilen {at}–{}) – doppelter Dip", end.saturating_sub(1))));
            }
            ducked.push((i, at, end));
        }
        is.extend(errs);
        is.extend(warns);
    }
    is
}

/// Cue gain in dB re full scale of the source (GUI, lint, CLI share this range).
pub const GAIN_MIN: f32 = -60.0;
pub const GAIN_MAX: f32 = 6.0;

/// Line (exclusive, 1-based) where a non-looping cue of `secs` starting at line `at` ends, by the estimated line lengths
/// `est`; at least `at + 1`, at most `n + 1`.
fn end_line(est: &[f64], at: usize, secs: f64) -> usize {
    let mut rest = secs;
    for (k, d) in est.iter().enumerate().skip(at.saturating_sub(1)) {
        rest -= d;
        if rest <= 0.0 {
            return (k + 2).max(at + 1);
        }
    }
    (est.len() + 1).max(at + 1)
}

/// A cue in the finished mix (manifest + report).
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Placed {
    /// 1-based cue index
    pub i: usize,
    pub kind: Kind,
    pub source: String,
    /// s in the mix
    pub start: f64,
    pub end: f64,
    pub duck: bool,
    /// the cue was cut at the mix end (or skipped entirely when it starts after it)
    pub clamped: bool,
}

/// A bed (loop to the end) runs under the spoken AI disclosure and fades out over the last `BED_FADE` s of the mix; the
/// mix then ends that long after its last clip instead of the usual 0.5 s tail (`mix_len`).
pub const BED_FADE: f64 = 1.5;

/// Loops to the end of the mix: `loop` without `loop_until_line`, or `loop_until_line` past the last of `n` lines.
pub fn is_bed(c: &Cue, n: usize) -> bool {
    c.looping() && c.loop_until_line.is_none_or(|l| l > n)
}

/// Mix length for `cues` over `n` script lines whose clips (disclosure included) end at sample `end`: `BED_FADE` s after
/// `end` when a bed plays, else `total` (the timeline's own tail).
pub fn mix_len<'a>(cues: impl IntoIterator<Item = &'a Cue>, n: usize, end: usize, total: usize, sr: u32) -> usize {
    if cues.into_iter().any(|c| is_bed(c, n)) { end + (BED_FADE * sr as f64).round() as usize } else { total }
}

/// Cues `render` can play right now: lint-clean, source loadable – gen cues only from the cache, render never calls
/// acestep. `(1-based index → samples at sr)` plus one warning per skipped cue.
pub fn playable(p: &Project, ffmpeg: &str, sr: u32) -> (std::collections::BTreeMap<usize, Vec<f32>>, Vec<String>) {
    let bad: Vec<String> = lint(p).into_iter().filter(|i| i.level == "error").map(|i| i.msg).collect();
    let (mut ok, mut warn) = (std::collections::BTreeMap::new(), vec![]);
    for (k, c) in p.meta.cues.iter().enumerate() {
        let i = k + 1;
        let pre = format!("Cue {i}:");
        if let Some(m) = bad.iter().find(|m| m.starts_with(&pre)) {
            warn.push(format!("{m} – übersprungen"));
        } else if let Some(g) = c.gen_.as_ref().filter(|g| crate::music::lookup(p, g).is_none()) {
            warn.push(format!("Cue {i}: Musik noch nicht erzeugt (gen:{}) – übersprungen; `stimmwerk music gen` oder Musik-Reiter", crate::music::key(g)));
        } else {
            match source(p, c, ffmpeg, sr) {
                Ok(s) => drop(ok.insert(i, s)),
                Err(e) => warn.push(format!("Cue {i}: {e:#} – übersprungen")),
            }
        }
    }
    (ok, warn)
}

/// Adds all cues of `p` into `out`, the un-normalized speech mix (`out.len()` = mix length incl. tail, see `mix_len`).
/// `starts`: start sample of each script line (overlays included, no disclosure). `src(cue)` loads a cue's samples at `sr`.
pub fn mix_into(p: &Project, out: &mut [f32], starts: &[usize], sr: u32, mut src: impl FnMut(&Cue) -> Result<Vec<f32>>) -> Result<Vec<Placed>> {
    mix_cues(&p.meta.music, p.meta.cues.iter().enumerate().map(|(k, c)| (k + 1, c)), out, starts, sr, |_, c| src(c))
}

/// `mix_into` for a selection of cues `(1-based index, cue)` (render: only `playable` ones). Loops end at
/// `loop_until_line`, beds (`is_bed`) with the mix, fading out over at least `BED_FADE` s.
pub fn mix_cues<'a>(cfg: &MusicCfg, cues: impl IntoIterator<Item = (usize, &'a Cue)>, out: &mut [f32], starts: &[usize], sr: u32, mut src: impl FnMut(usize, &Cue) -> Result<Vec<f32>>) -> Result<Vec<Placed>> {
    let total = out.len();
    let voice = out.to_vec();
    let mut envs: HashMap<u32, Vec<f32>> = HashMap::new();
    let mut res = vec![];
    let secs = |s: usize| s as f64 / sr as f64;
    for (i, c) in cues {
        let start = c.start(starts, sr).with_context(|| format!("Cue {i}: at_line/at_sec ungültig"))?;
        let s = src(i, c).with_context(|| format!("Cue {i}"))?;
        let bed = is_bed(c, starts.len());
        let want = match c.loop_until_line {
            Some(l) if l <= starts.len() => starts[l - 1],
            _ if c.looping() => total,
            _ => start.saturating_add(s.len()),
        };
        let end = want.min(total);
        let mut pl = Placed { i, kind: c.kind, source: c.label(), start: secs(start), end: secs(start.max(end)), duck: c.duck, clamped: want > total };
        if end > start {
            let fade_out = if bed { c.fade_out.max(BED_FADE) } else { c.fade_out };
            let a = am::render_cue(&s, &CueRender { len: end - start, loop_: c.looping(), gain_db: c.gain_db, fade_in: c.fade_in, fade_out, xfade_ms: cfg.crossfade_ms }, sr).with_context(|| format!("Cue {i}"))?;
            let env = c.duck.then(|| {
                let d = cfg.duck_params(c.duck_db);
                &*envs.entry(d.depth_db.to_bits()).or_insert_with(|| am::duck_with(&voice, sr, &d))
            });
            for (j, v) in a.iter().enumerate() {
                out[start + j] += v * env.map_or(1.0, |e| e[start + j]);
            }
            pl.end = secs(start + a.len());
        }
        res.push(pl);
    }
    Ok(res)
}

/// Human warnings about placed cues (truncated / skipped).
pub fn warnings(placed: &[Placed]) -> Vec<String> {
    placed.iter().filter(|c| c.clamped).map(|c| if c.end > c.start { format!("Cue {}: reicht über das Ende der Sprache hinaus – am Mix-Ende ({} s) abgeschnitten", c.i, c.end) } else { format!("Cue {}: beginnt nach dem Mix-Ende ({} s) – nicht zu hören", c.i, c.start) }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::Meta;
    use funkhaus_audio::{mix, script::Line};

    const FIX: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/stimmwerk/hoerspiel");
    const EDGE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/stimmwerk/cues");
    const SR: u32 = 24_000;

    /// Hörspiel fixture (25 lines, 3 overlays) in a tempdir with `toml` as project.toml.
    fn proj(toml: &str) -> (tempfile::TempDir, Project) {
        let t = tempfile::tempdir().unwrap();
        let d = t.path().join("h.studio");
        std::fs::create_dir_all(d.join("assets")).unwrap();
        std::fs::write(d.join("project.toml"), toml).unwrap();
        std::fs::copy(Path::new(FIX).join("script.txt"), d.join("script.txt")).unwrap();
        let p = Project::load(&d).unwrap();
        (t, p)
    }

    fn wav(p: &Project, rel: &str, secs: f64, level: f32) {
        let f = p.dir.join(rel);
        std::fs::create_dir_all(f.parent().unwrap()).unwrap();
        mix::write_f32(&f, &vec![level; (secs * SR as f64) as usize], SR).unwrap();
    }

    fn codes(v: &[Issue]) -> Vec<(&'static str, &'static str)> {
        v.iter().map(|i| (i.level, i.code)).collect()
    }

    fn parse(s: &str) -> Result<Vec<Cue>, anyhow::Error> {
        Meta::parse(&format!("format = 1\ntitle = \"x\"\n{s}")).map(|m| m.cues)
    }

    #[test]
    fn parse_schema() {
        let c = &parse("[[cues]]\nat_line = 3\nfile = \"assets/a.wav\"\nkind = \"music\"\nloop_until_line = 9\nduck = true\nduck_db = -12.0\n").unwrap()[0];
        assert_eq!((c.at_line, c.kind, c.loop_until_line, c.duck, c.duck_db, c.looping()), (Some(3), Kind::Music, Some(9), true, Some(-12.0), true));
        assert_eq!((c.gain_db, c.fade_in, c.loop_, c.at_sec), (0.0, 0.0, false, None));
        let c = &parse("[[cues]]\nat_sec = 15.5\nkind = \"sfx\"\n[cues.gen]\nprompt = \"calm pad\"\ndur = 12\nseed = 3\n").unwrap()[0];
        assert_eq!((c.at_sec, c.gen_.as_ref().map(|g| g.dur), c.file.is_none()), (Some(15.5), Some(12), true));
        assert_eq!(c.label(), format!("gen:{}", crate::music::key(c.gen_.as_ref().unwrap())));
        // old `at`, unknown fields, missing kind, bad kind, unknown gen field
        for bad in ["[[cues]]\nat = 3\nfile = \"assets/a.wav\"\nkind = \"music\"\n", "[[cues]]\nat_line = 3\nfile = \"assets/a.wav\"\n", "[[cues]]\nat_line = 3\nfile = \"assets/a.wav\"\nkind = \"foo\"\n", "[[cues]]\nat_line = 3\nfile = \"a\"\nkind = \"music\"\ntrim_s = 2.0\n", "[[cues]]\nat_line = 1\nkind = \"music\"\n[cues.gen]\nprompt = \"x\"\ndur = 10\nseed = 1\nlyrics = \"la\"\n"] {
            assert!(parse(bad).is_err(), "{bad}");
        }
        assert!(parse("").unwrap().is_empty());
        assert!(Meta::parse("format = 1\ntitle = \"x\"\n[music]\nduck_db = -9.0\nbed_db = 1.0\n").is_err(), "unknown [music] key");
    }

    #[test]
    fn roundtrip_bit_identical() {
        let mut m = Meta::new("x", "de");
        m.music.duck_db = -9.0;
        let mut a = Cue::new(Kind::Music);
        (a.at_line, a.gain_db, a.fade_in, a.loop_until_line, a.duck) = (Some(1), -20.0, 2.0, Some(26), true);
        a.gen_ = Some(Gen { prompt: "calm maritime intro".into(), dur: 12, seed: 1, bpm: 70, key: Some("A minor".into()), model: None });
        let mut b = Cue::new(Kind::Ambience);
        (b.at_sec, b.file, b.loop_, b.duck_db) = (Some(15.5), Some("assets/ambience/meer.wav".into()), true, Some(-6.0));
        m.cues = vec![a, b];
        let s = m.to_toml().unwrap();
        assert!(s.contains("[music]\nduck_db = -9.0\n") && s.contains("[[cues]]\nat_line = 1\n") && s.contains("[cues.gen]\nprompt") && s.contains("loop = true"), "{s}");
        let m2 = Meta::parse(&s).unwrap();
        assert_eq!(m2, m);
        assert_eq!(m2.to_toml().unwrap(), s);
        // defaults are not written
        assert!(!Meta::new("x", "de").to_toml().unwrap().contains("music"));
    }

    #[test]
    fn rel_paths() {
        assert_eq!(rel("assets/m/a.wav"), Some(Path::new("m/a.wav")));
        for f in ["/assets/a.wav", "assets/../a.wav", "./assets/a.wav", "music/a.wav", "assets", "assets/", "x/assets/a.wav"] {
            assert_eq!(rel(f), None, "{f}");
        }
    }

    /// The Hörspiel fixture: both cue files missing → 2 errors; present → clean; the `[music]` block parses.
    #[test]
    fn hoerspiel_fixture_lint() {
        let (_t, mut p) = proj(&std::fs::read_to_string(Path::new(FIX).join("project.toml")).unwrap());
        assert_eq!((p.meta.cues.len(), p.meta.music.duck_db, p.script.lines().count()), (2, -12.0, 25));
        assert_eq!(codes(&lint(&p)), [("error", "cue_src"), ("error", "cue_src")]);
        wav(&p, "assets/music/intro.wav", 12.0, 0.3);
        wav(&p, "assets/ambience/meer_cc0.wav", 8.0, 0.2);
        assert_eq!(lint(&p), []);
        p.meta.cues[1].at_line = Some(26);
        assert_eq!(codes(&lint(&p)), [("error", "cue_at"), ("error", "cue_loop")]);
    }

    /// fixtures/stimmwerk/cues/*.toml: expected lint result per case (README table).
    #[test]
    fn edge_fixtures() {
        let case = |n: &str| std::fs::read_to_string(Path::new(EDGE).join(n)).unwrap();
        let (_t, mut p) = proj(&case("01-at-line.toml"));
        wav(&p, "assets/music/intro.wav", 12.0, 0.3);
        wav(&p, "assets/ambience/wind_cc0.wav", 6.0, 0.1);
        wav(&p, "assets/ambience/meer_cc0.wav", 6.0, 0.1);
        wav(&p, "assets/music/sting_10s.wav", 10.0, 0.3);
        wav(&p, "assets/music/sting_6s.wav", 6.0, 0.3);
        let run = |p: &mut Project, n: &str| {
            p.meta = Meta::parse(&case(n)).unwrap();
            codes(&lint(p))
        };
        assert_eq!(run(&mut p, "01-at-line.toml"), []);
        assert_eq!(run(&mut p, "02-at-sec.toml"), [("error", "cue_at")]);
        assert_eq!(run(&mut p, "03-loop-past-end.toml"), [("error", "cue_loop")]);
        assert_eq!(run(&mut p, "04-overlap-ducked.toml"), [("warn", "cue_overlap")]);
        assert_eq!(run(&mut p, "05-fade-longer-than-cue.toml"), [("error", "cue_fade")]);
        assert_eq!(run(&mut p, "06-gain-out-of-range.toml"), [("error", "cue_gain")]);
        assert_eq!(run(&mut p, "07-missing-file.toml"), [("error", "cue_src")]);
        assert_eq!(run(&mut p, "08-unknown-line.toml"), [("error", "cue_at")]);
        assert_eq!(run(&mut p, "09-music-shorter-than-10s.toml"), []);
    }

    #[test]
    fn lint_rules() {
        let (_t, mut p) = proj(&std::fs::read_to_string(Path::new(FIX).join("project.toml")).unwrap());
        wav(&p, "assets/short.wav", 3.0, 0.3); // ≤ 2 × 2 s crossfade
        wav(&p, "assets/long.wav", 4.1, 0.3);
        let mk = |f: Option<&str>| {
            let mut c = Cue::new(Kind::Music);
            c.at_line = Some(2);
            c.file = f.map(String::from);
            c
        };
        let mut both = mk(Some("assets/long.wav"));
        both.at_sec = Some(1.0);
        let mut none = mk(None);
        none.at_line = None;
        let mut sfx = mk(Some("assets/long.wav"));
        (sfx.kind, sfx.loop_, sfx.duck) = (Kind::Sfx, true, true);
        let mut short = mk(Some("assets/short.wav"));
        short.loop_until_line = Some(26);
        let mut ok_loop = mk(Some("assets/long.wav"));
        ok_loop.loop_ = true;
        let mut nan = mk(Some("assets/long.wav"));
        (nan.gain_db, nan.duck_db, nan.fade_in) = (f32::NAN, Some(0.0), -1.0);
        let mut until_sec = mk(Some("assets/long.wav"));
        (until_sec.at_line, until_sec.at_sec, until_sec.loop_until_line) = (None, Some(2.0), Some(5));
        let mut badgen = mk(None);
        badgen.gen_ = Some(Gen { prompt: " ".into(), dur: 5, seed: 1, bpm: 0, key: None, model: None });
        let mut g9 = mk(None);
        g9.gen_ = Some(Gen { prompt: "pad".into(), dur: 12, seed: 1, bpm: 0, key: None, model: None });
        p.meta.cues = vec![both, none, sfx, short, ok_loop, nan, until_sec, badgen, g9];
        let is = lint(&p);
        let by = |n: usize| is.iter().filter(|i| i.msg.starts_with(&format!("Cue {n}:"))).map(|i| (i.level, i.code)).collect::<Vec<_>>();
        assert_eq!(by(1), [("error", "cue_at")]);
        assert_eq!(by(2), [("error", "cue_at"), ("error", "cue_src")]);
        assert_eq!(by(3), [("error", "cue_loop"), ("warn", "cue_duck")]);
        assert_eq!(by(4), [("error", "cue_loop")]);
        assert_eq!(by(5), []);
        assert_eq!(by(6), [("error", "cue_fade"), ("error", "cue_gain"), ("error", "cue_duck")]);
        assert_eq!(by(7), [("error", "cue_loop")]);
        assert_eq!(by(8), [("error", "cue_src")]);
        assert_eq!(by(9), [("warn", "cue_gen")]);
        assert!(is.iter().find(|i| i.code == "cue_gen").unwrap().msg.contains("music gen"));
        assert!(is.iter().find(|i| i.code == "cue_gain").unwrap().msg.contains("−60..+6"));
        let mut hot = mk(Some("assets/long.wav"));
        hot.gain_db = 6.5;
        p.meta.cues = vec![hot];
        assert_eq!(codes(&lint(&p)), [("error", "cue_gain")]);
    }

    /// Double-dip warning uses the real cue length: a long non-looping cue reaches later ducked cues, a short one not.
    #[test]
    fn overlap_by_source_length() {
        let (_t, mut p) = proj(&std::fs::read_to_string(Path::new(FIX).join("project.toml")).unwrap());
        wav(&p, "assets/long.wav", 4.1, 0.3);
        let est: Vec<f64> = p.script.lines().map(|l| crate::project::est_secs(&l.text) + f64::from(l.gap.max(0.0))).collect();
        assert_eq!(end_line(&est, 1, 0.1), 2);
        assert_eq!(end_line(&est, 1, 1e9), est.len() + 1);
        assert_eq!(end_line(&est, 3, est[2] + est[3] + 0.01), 6);
        assert_eq!(end_line(&est, 3, est[2] + est[3]), 5);
        let at = |l: usize, long: bool| {
            let mut c = Cue::new(Kind::Music);
            (c.at_line, c.duck) = (Some(l), true);
            if long {
                c.gen_ = Some(Gen { prompt: "pad".into(), dur: 600, seed: 1, bpm: 0, key: None, model: None });
            } else {
                c.file = Some("assets/long.wav".into());
            }
            c
        };
        p.meta.cues = vec![at(1, true), at(20, false)];
        assert_eq!(codes(&lint(&p)), [("warn", "cue_gen"), ("warn", "cue_overlap")], "600 s from line 1 still plays at line 20");
        p.meta.cues = vec![at(1, false), at(20, false)];
        assert_eq!(lint(&p), [], "4.1 s from line 1 is over long before line 20");
    }

    /// Script lines of the fixture with fake clip lengths: cue start == `timeline` start of its line (overlay line 5
    /// = prev start + round(prev_len · 0.7)); `at_sec` is sample-exact; zeros before the start.
    #[test]
    fn cue_start_is_line_start() {
        let (_t, mut p) = proj(&std::fs::read_to_string(Path::new(FIX).join("project.toml")).unwrap());
        let lines: Vec<Line> = p.script.lines().cloned().collect();
        let lens: Vec<usize> = (0..lines.len()).map(|i| 20_000 + i * 1000).collect();
        let (starts, total) = mix::timeline(&lines, &lens, SR);
        assert!(lines[4].over && starts[4] == starts[3] + ((lens[3] as f64) * 0.7f64).round() as usize);
        let mk = |at: Option<usize>, sec: Option<f64>| {
            let mut c = Cue::new(Kind::Music);
            (c.at_line, c.at_sec, c.file) = (at, sec, Some("assets/c.wav".into()));
            c
        };
        p.meta.cues = vec![mk(Some(1), None), mk(Some(5), None), mk(Some(18), None), mk(None, Some(15.5))];
        let src = |_: &Cue| Ok(vec![0.25f32; 2 * SR as usize]);
        for (k, want) in [(0, starts[0]), (1, starts[4]), (2, starts[17]), (3, 372_000)] {
            let mut out = vec![0f32; total];
            let one = Project { meta: crate::project::Meta { cues: vec![p.meta.cues[k].clone()], ..p.meta.clone() }, ..p.clone() };
            let pl = mix_into(&one, &mut out, &starts, SR, src).unwrap();
            assert_eq!(out.iter().position(|v| *v != 0.0), Some(want), "cue {k}");
            assert_eq!((pl[0].start, pl[0].clamped), (want as f64 / SR as f64, false));
            assert_eq!(out.iter().rposition(|v| *v != 0.0), Some(want + 2 * SR as usize - 1));
        }
    }

    /// Duck: −12 dB ±1 dB at 150 ms into speech, released 4 s after; per-cue `duck_db`; `duck = false` identical with
    /// and without speech; loop ends at `loop_until_line`, a bed with the mix (BED_FADE fade); fades; one peak divisor afterwards.
    #[test]
    fn ducking_loop_and_clamp() {
        let (_t, mut p) = proj(&std::fs::read_to_string(Path::new(FIX).join("project.toml")).unwrap());
        let n = p.script.lines().count();
        let cue = |duck: bool, duck_db: Option<f32>, loop_: bool, until: Option<usize>| {
            let mut c = Cue::new(Kind::Music);
            (c.at_line, c.file, c.duck, c.duck_db, c.loop_, c.loop_until_line) = (Some(1), Some("assets/c.wav".into()), duck, duck_db, loop_, until);
            c
        };
        let starts: Vec<usize> = (0..n).map(|i| i * SR as usize).collect(); // 1 s per line
        let speech_end = n * SR as usize;
        let total = mix_len(&[cue(false, None, true, None)], n, speech_end, speech_end + SR as usize / 2, SR);
        assert_eq!(total, speech_end + (1.5 * SR as f64) as usize);
        assert_eq!(mix_len(&[cue(false, None, false, Some(18))], n, speech_end, 7, SR), 7);
        assert!(is_bed(&cue(false, None, false, Some(n + 1)), n) && !is_bed(&cue(false, None, false, Some(n)), n));
        let mut voice = vec![0f32; total];
        // speech: 0.5 s burst at line 1, silence afterwards
        voice.iter_mut().take(12_000).enumerate().for_each(|(i, v)| *v = 0.5 * (2.0 * std::f32::consts::PI * 1000.0 * i as f32 / SR as f32).sin());
        let src = |_: &Cue| Ok(vec![0.3f32; 10 * SR as usize]);
        let run = |p: &mut Project, cues: Vec<Cue>, v: &[f32]| {
            p.meta.cues = cues;
            let mut out = v.to_vec();
            let pl = mix_into(p, &mut out, &starts, SR, src).unwrap();
            (out, pl)
        };
        let (out, _) = run(&mut p, vec![cue(true, None, true, None)], &voice);
        let d = 20.0 * ((out[3_600] - voice[3_600]) / (0.3 * am::db(-12.0))).log10();
        assert!(d.abs() < 1.0, "duck depth {d} dB");
        assert!((out[12_000 + 4 * SR as usize] - 0.3).abs() < 0.3 * 0.06, "released 4 s after speech");
        let (o6, _) = run(&mut p, vec![cue(true, Some(-6.0), true, None)], &voice);
        let d6 = 20.0 * ((o6[3_600] - voice[3_600]) / (0.3 * am::db(-6.0))).log10();
        assert!(d6.abs() < 1.0, "per-cue depth {d6} dB");
        // duck = false: cue part identical with and without speech
        let (a, _) = run(&mut p, vec![cue(false, None, true, None)], &voice);
        let (b, pl) = run(&mut p, vec![cue(false, None, true, None)], &vec![0.0; total]);
        assert!(a.iter().zip(&voice).zip(&b).all(|((a, v), b)| (a - v - b).abs() < 1e-6));
        assert_eq!((pl[0].start, pl[0].end, pl[0].clamped), (0.0, total as f64 / SR as f64, false), "bed ends with the mix");
        let fo = (BED_FADE * SR as f64) as usize;
        assert!((b[total - 1 - fo] - 0.3).abs() < 1e-6 && (b[total - 1 - fo / 2] - 0.15).abs() < 1e-3 && b[total - 1] == 0.0, "bed fades over the last {BED_FADE} s");
        // `loop_until_line = n + 1` is the same bed; a longer own fade_out wins
        let mut long = cue(false, None, false, Some(n + 1));
        long.fade_out = 3.0;
        let (b2, pl) = run(&mut p, vec![long], &vec![0.0; total]);
        assert_eq!(pl[0].end, total as f64 / SR as f64);
        assert!((b2[total - 1 - fo] - 0.15).abs() < 1e-3);
        // loop_until_line 18: ends at starts[17]; non-loop 10 s file: 10 s; clamp past the mix end
        let (b, pl) = run(&mut p, vec![cue(false, None, false, Some(18))], &vec![0.0; total]);
        assert_eq!((pl[0].end, b[starts[17] - 1], b[starts[17]]), (17.0, 0.3, 0.0));
        let (b, pl) = run(&mut p, vec![cue(false, None, false, None)], &vec![0.0; total]);
        assert_eq!((pl[0].end, b[10 * SR as usize - 1], b[10 * SR as usize]), (10.0, 0.3, 0.0));
        let mut late = cue(false, None, false, None);
        late.at_line = Some(n); // 10 s file 1 s before the end → clamped
        let (b, pl) = run(&mut p, vec![late.clone()], &vec![0.0; total]);
        assert!(pl[0].clamped && pl[0].end == total as f64 / SR as f64 && b[total - 1] == 0.3);
        assert_eq!(warnings(&pl).len(), 1);
        late.at_sec = Some(100.0);
        late.at_line = None;
        let (_, pl) = run(&mut p, vec![late], &vec![0.0; total]);
        assert!(pl[0].clamped && pl[0].end == pl[0].start && warnings(&pl)[0].contains("nicht zu hören"));
        // fades + gain: fade_in starts at 0, fade_out ends at 0; peak divisor only once (C7)
        let mut f = cue(false, None, false, None);
        (f.fade_in, f.fade_out, f.gain_db) = (1.0, 1.0, -6.0);
        let (mut b, _) = run(&mut p, vec![f], &vec![0.0; total]);
        assert_eq!((b[0], b[10 * SR as usize - 1]), (0.0, 0.0));
        assert!((b[5 * SR as usize] / 0.3 - 0.5012).abs() < 1e-3);
        b[1] = 1.9;
        assert!((mix::norm(&mut b) - 2.0).abs() < 1e-6 && (b[1] - 0.95).abs() < 1e-6);
    }
}
