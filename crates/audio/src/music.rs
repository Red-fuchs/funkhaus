//! Jingle + music bed placement and ducking (docs/m3/music-spec.md). Pure DSP, no subprocess.
use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Captions {
    pub intro: String,
    pub outro: String,
    pub bed: String,
}

impl Default for Captions {
    fn default() -> Self {
        Self {
            intro: "Short warm instrumental jingle for a German knowledge podcast. Soft felt piano motif over gentle plucked strings, light vibraphone accents, airy analog pads, clean studio sound, confident but calm, clear simple melody that resolves cleanly, no vocals.".into(),
            outro: "Gentle instrumental sign-off for a German knowledge podcast. Warm felt piano and soft strings returning to the intro motif in a relaxed, slowing manner, natural decay and gentle room reverb, a calm and friendly close that ends softly on a long fading chord, no vocals, no drums.".into(),
            bed: "Calm instrumental background bed for spoken German knowledge podcast. Soft evolving warm analog pads, sparse low piano chords, subtle slow arpeggios kept in the lower register, no drums, no clear melody that pulls attention, minimal dynamics, sustained tones, gentle warm texture under speech, no vocals.".into(),
        }
    }
}

/// `[music]` in config.toml. File names are relative to `<FUNKHAUS_HOME>/music/`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Music {
    pub enabled: bool,
    /// intro + outro (CLI --jingle/--no-jingle); only used if the files exist
    pub jingle: bool,
    /// bed under speech (CLI --bed)
    pub bed: bool,
    pub acestep: String,
    pub intro: String,
    pub outro: String,
    pub bed_file: String,
    /// intro +0, outro +1, bed +2
    pub seed: u64,
    /// intro/outro level in dB relative to the voice RMS (jingle is first matched to the speech RMS, then offset)
    pub intro_db: f32,
    pub outro_db: f32,
    /// bed level in dB re full scale of the bed file
    pub bed_db: f32,
    /// acestep tempo/key per part (part of the music cache key)
    pub intro_bpm: u32,
    pub outro_bpm: u32,
    pub bed_bpm: u32,
    pub intro_key: String,
    pub outro_key: String,
    pub bed_key: String,
    /// extra dip while speech is present
    pub duck_db: f32,
    pub duck_thresh_db: f32,
    pub duck_attack_ms: f32,
    pub duck_release_ms: f32,
    /// duck stays down this long after speech fell below the threshold (bridges word gaps)
    pub duck_hold_ms: f32,
    /// voice starts this long before the intro ends
    pub intro_overlap_ms: f32,
    /// silence after the last voice line
    pub outro_gap_ms: f32,
    /// bed loop seam
    pub crossfade_ms: f32,
    pub captions: Captions,
}

impl Default for Music {
    fn default() -> Self {
        Self {
            enabled: true,
            jingle: true,
            bed: false,
            acestep: "acestep".into(),
            intro: "intro.wav".into(),
            outro: "outro.wav".into(),
            bed_file: "bed.wav".into(),
            seed: 42,
            intro_db: -3.0,
            outro_db: -3.0,
            bed_db: -20.0,
            intro_bpm: 90,
            outro_bpm: 80,
            bed_bpm: 70,
            intro_key: "D major".into(),
            outro_key: "D major".into(),
            bed_key: "C major".into(),
            duck_db: -12.0,
            duck_thresh_db: -50.0,
            duck_attack_ms: 20.0,
            duck_release_ms: 800.0,
            duck_hold_ms: 300.0,
            intro_overlap_ms: 1500.0,
            outro_gap_ms: 1000.0,
            crossfade_ms: 2000.0,
            captions: Captions::default(),
        }
    }
}

pub fn db(x: f32) -> f32 {
    10f32.powf(x / 20.0)
}

fn ms(sr: u32, v: f32) -> usize {
    (v.max(0.0) * sr as f32 / 1000.0).round() as usize
}

/// Samples before the voice starts: intro minus overlap.
pub fn lead_samples(intro_len: usize, sr: u32, c: &Music) -> usize {
    intro_len.saturating_sub(ms(sr, c.intro_overlap_ms))
}

/// RMS over the active samples (|x| > -50 dBFS), so pauses do not count; 0 if there are none.
pub fn active_rms(x: &[f32]) -> f32 {
    let th = db(-50.0);
    let (s, n) = x.iter().filter(|v| v.abs() > th).fold((0f64, 0usize), |(s, n), v| (s + (*v as f64).powi(2), n + 1));
    if n == 0 { 0.0 } else { (s / n as f64).sqrt() as f32 }
}

/// Gain that brings `x` to the voice RMS `vr`, times `offset_db`; 1 if either is silent.
fn match_gain(x: &[f32], vr: f32, offset_db: f32) -> f32 {
    let r = active_rms(x);
    if vr > 0.0 && r > 0.0 { vr / r * db(offset_db) } else { db(offset_db) }
}

/// Ducking parameters: funkhaus takes them from `Music`, Stimmwerk per cue (`depth_db` override).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DuckParams {
    /// dip while speech is present (negative dB; the sign is ignored)
    pub depth_db: f32,
    pub thresh_db: f32,
    pub attack_ms: f32,
    pub release_ms: f32,
    pub hold_ms: f32,
}

impl From<&Music> for DuckParams {
    fn from(c: &Music) -> Self {
        Self { depth_db: c.duck_db, thresh_db: c.duck_thresh_db, attack_ms: c.duck_attack_ms, release_ms: c.duck_release_ms, hold_ms: c.duck_hold_ms }
    }
}

/// Gain per sample: a fixed `duck_db` dip while the 30 ms RMS of `v` is above the threshold,
/// held for `duck_hold_ms` after it drops below.
pub fn duck(v: &[f32], sr: u32, c: &Music) -> Vec<f32> {
    duck_with(v, sr, &c.into())
}

/// `duck` with explicit parameters.
pub fn duck_with(v: &[f32], sr: u32, d: &DuckParams) -> Vec<f32> {
    let k = |t: f32| (-1.0 / (t.max(1e-3) * 1e-3 * sr as f32)).exp();
    let (rms_k, att, rel) = (k(30.0), k(d.attack_ms), k(d.release_ms));
    let (thr, lo) = (db(d.thresh_db), db(-d.depth_db.abs()));
    let hold = ms(sr, d.hold_ms);
    let (mut m, mut g, mut quiet) = (0f32, 1f32, usize::MAX / 2);
    v.iter()
        .map(|&s| {
            m = rms_k * m + (1.0 - rms_k) * s * s;
            quiet = if m.sqrt() > thr { 0 } else { quiet + 1 };
            let want = if quiet <= hold { lo } else { 1.0 };
            let a = if want < g { att } else { rel };
            g = a * g + (1.0 - a) * want;
            g
        })
        .collect()
}

/// Loop body of `b` for a crossfade of `x` samples: `b.len() - x` samples whose first `x` blend the tail in, so
/// `body[i % body.len()]` repeats without a seam.
pub fn loop_body(b: &[f32], x: usize) -> Result<Vec<f32>> {
    if b.len() <= 2 * x {
        bail!("Musikbett zu kurz ({} Samples) für Crossfade {x}", b.len());
    }
    let p = b.len() - x;
    Ok((0..p)
        .map(|i| if i < x { let w = i as f32 / x as f32; b[i] * w + b[p + i] * (1.0 - w) } else { b[i] })
        .collect())
}

/// Seamless loop of `b` over `span` samples at `bed_db`, 1 s fade in/out.
pub fn loop_bed(b: &[f32], span: usize, sr: u32, c: &Music) -> Result<Vec<f32>> {
    let body = loop_body(b, ms(sr, c.crossfade_ms))?;
    let p = body.len();
    let (g, f) = (db(c.bed_db), sr as usize);
    Ok((0..span)
        .map(|i| {
            let fade = (i.min(span - 1 - i) as f32 / f as f32).min(1.0);
            body[i % p] * g * fade
        })
        .collect())
}

/// One Stimmwerk cue: `len` samples of `src` (looped seamlessly, or cut to `min(len, src.len())`), at `gain_db`,
/// with linear fades (fade-in starts at 0, fade-out reaches 0 on the last sample). Ducking is applied by the caller.
#[derive(Clone, Debug, PartialEq)]
pub struct CueRender {
    pub len: usize,
    pub loop_: bool,
    pub gain_db: f32,
    /// s
    pub fade_in: f64,
    pub fade_out: f64,
    /// loop seam crossfade
    pub xfade_ms: f32,
}

pub fn render_cue(src: &[f32], c: &CueRender, sr: u32) -> Result<Vec<f32>> {
    let body: std::borrow::Cow<[f32]> = if c.loop_ { loop_body(src, ms(sr, c.xfade_ms))?.into() } else { src.into() };
    let len = if c.loop_ { c.len } else { c.len.min(src.len()) };
    if body.is_empty() || len == 0 {
        return Ok(vec![]);
    }
    let g = db(c.gain_db);
    let (fi, fo) = ((c.fade_in.max(0.0) * sr as f64).round() as usize, (c.fade_out.max(0.0) * sr as f64).round() as usize);
    Ok((0..len)
        .map(|i| {
            let a = if i < fi { i as f32 / fi as f32 } else { 1.0 };
            let b = if len - 1 - i < fo { (len - 1 - i) as f32 / fo as f32 } else { 1.0 };
            body[i % body.len()] * g * a * b
        })
        .collect())
}

/// Voice (from `mix::layout`, incl. 0.5 s tail) + intro/outro/bed. Intro/outro are level-matched to the voice's active RMS, then offset by `intro_db`/`outro_db`. Returns (mix, lead) with lead = voice offset.
/// Bed and the intro tail overlapping speech are ducked by the same envelope.
pub fn place(voice: &[f32], intro: Option<&[f32]>, outro: Option<&[f32]>, bed: Option<&[f32]>, c: &Music, sr: u32) -> Result<(Vec<f32>, usize)> {
    if intro.is_none() && outro.is_none() && bed.is_none() {
        return Ok((voice.to_vec(), 0));
    }
    let lead = intro.map_or(0, |i| lead_samples(i.len(), sr, c));
    let vend = lead + voice.len().saturating_sub(sr as usize / 2);
    let ostart = vend + ms(sr, c.outro_gap_ms);
    let bend = vend + sr as usize;
    let len = [lead + voice.len(), intro.map_or(0, |i| i.len()), outro.map_or(0, |o| ostart + o.len()), if bed.is_some() { bend } else { 0 }].into_iter().max().unwrap_or(0);
    let mut vt = vec![0f32; len];
    vt[lead..lead + voice.len()].copy_from_slice(voice);
    let env = duck(&vt, sr, c);
    let vr = active_rms(voice);
    let mut out = vt;
    if let Some(i) = intro {
        let g = match_gain(i, vr, c.intro_db);
        out.iter_mut().zip(i).zip(&env).for_each(|((o, s), e)| *o += s * g * e);
    }
    if let Some(o) = outro {
        let g = match_gain(o, vr, c.outro_db);
        out[ostart..ostart + o.len()].iter_mut().zip(o).for_each(|(d, s)| *d += s * g);
    }
    if let Some(b) = bed {
        let l = loop_bed(b, bend - lead, sr, c)?;
        out[lead..bend].iter_mut().zip(&l).zip(&env[lead..bend]).for_each(|((o, s), e)| *o += s * e);
    }
    let k = (out.iter().fold(0f32, |m, v| m.max(v.abs())) / 0.95).max(1.0);
    out.iter_mut().for_each(|v| *v /= k);
    Ok((out, lead))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: u32 = 24_000;
    fn sine(n: usize, amp: f32) -> Vec<f32> {
        (0..n).map(|i| amp * (2.0 * std::f32::consts::PI * 1000.0 * i as f32 / SR as f32).sin()).collect()
    }

    #[test]
    fn config_defaults_and_partial() {
        let c: Music = serde_json::from_str("{}").unwrap();
        assert_eq!(c, Music::default());
        let c: Music = serde_json::from_str(r#"{"bed":true}"#).unwrap();
        assert!(c.bed && Music { bed: false, ..c.clone() } == Music::default());
        let s = serde_json::to_string(&c).unwrap();
        assert_eq!(serde_json::from_str::<Music>(&s).unwrap(), c);
    }
    #[test]
    fn lead_numbers() {
        let c = Music::default();
        assert_eq!(lead_samples(192_000, SR, &c), 156_000);
        assert_eq!(lead_samples(10_000, SR, &c), 0);
        assert_eq!((db(0.0), db(-20.0)), (1.0, 0.1));
    }

    #[test]
    fn placement() {
        let c = Music { bed_db: -20.0, intro_db: 0.0, outro_db: 0.0, ..Music::default() };
        let voice = vec![0.1f32; 252_000];
        let (intro, outro) = (vec![0.1f32; 192_000], vec![0.1f32; 144_000]);
        let (m, lead) = place(&voice, Some(&intro), Some(&outro), None, &c, SR).unwrap();
        assert_eq!((lead, m.len()), (156_000, 564_000));
        assert_eq!(m[419_999], 0.0); // gap before outro_start 420_000
        assert!(m[420_000] > 0.08);
        let (m, l) = place(&voice, None, None, None, &c, SR).unwrap();
        assert_eq!((m, l), (voice.clone(), 0));
    }

    #[test]
    fn intro_only_is_verbatim_before_voice() {
        let c = Music::default();
        let intro = sine(192_000, 0.5);
        let (m, lead) = place(&vec![0.0; 24_000], Some(&intro), None, None, &c, SR).unwrap(); // silent voice: no matching
        assert_eq!(lead, 156_000);
        assert!(m[..lead].iter().zip(&intro).all(|(a, b)| (a - b * db(c.intro_db)).abs() < 1e-7));
    }

    #[test]
    fn intro_tail_is_ducked() {
        let c = Music { intro_db: 0.0, ..Music::default() };
        let intro = vec![0.4f32; 192_000];
        let voice = sine(48_000, 0.5);
        let (m, lead) = place(&voice, Some(&intro), None, None, &c, SR).unwrap();
        let k = active_rms(&voice) / 0.4; // intro matched to voice RMS
        // 1 s into speech the intro tail (still playing) is ~12 dB down
        let i = lead + 24_000;
        let tail = m[i] - voice[24_000];
        assert!(tail.abs() < 0.4 * k * db(-12.0) + 0.03, "{tail}");
        assert!((m[lead - 10] - 0.4 * k).abs() < 1e-6);
    }

    #[test]
    fn jingle_matched_to_voice_rms_with_offset() {
        let c = Music::default(); // intro_db -3
        let voice = sine(48_000, 0.1);
        let intro = sine(192_000, 0.8);
        let (m, lead) = place(&voice, Some(&intro), None, None, &c, SR).unwrap();
        let r = active_rms(&m[..lead]) / active_rms(&voice);
        assert!((20.0 * r.log10() + 3.0).abs() < 0.1, "{r}");
    }

    #[test]
    fn default_captions_long_enough() {
        let c = Captions::default();
        assert!([&c.intro, &c.outro, &c.bed].iter().all(|s| s.len() >= 250));
    }

    #[test]
    fn loop_bed_seam_and_len() {
        let c = Music::default();
        let b: Vec<f32> = (0..1_440_000).map(|i| (2.0 * std::f32::consts::PI * 220.0 * i as f32 / SR as f32).sin()).collect();
        let l = loop_bed(&b, 2_880_000, SR, &c).unwrap();
        assert_eq!(l.len(), 2_880_000);
        let p = 1_392_000;
        let mut st: Vec<f32> = l[1_000_000..1_100_000].windows(2).map(|w| (w[1] - w[0]).abs()).collect();
        st.sort_by(|a, b| a.total_cmp(b));
        assert!((l[p - 1] - l[p]).abs() <= 2.0 * st[st.len() / 2].max(1e-6) * 1.5 + 1e-4, "{}", (l[p - 1] - l[p]).abs());
        assert!(l[0].abs() < 1e-6 && l[l.len() - 1].abs() < 1e-3);
        assert!(loop_bed(&vec![0.0; 96_000], 1000, SR, &c).is_err());
    }

    #[test]
    fn duck_depth_within_1db() {
        let c = Music::default();
        let mut v = sine(12_000, 0.5);
        v.resize(48_000, 0.0);
        let g = duck(&v, SR, &c);
        let d = 20.0 * (g[3_600] / db(-12.0)).log10(); // 150 ms in
        assert!(d.abs() < 1.0, "{d}");
        let mut v2 = sine(12_000, 0.5);
        v2.resize(120_000, 0.0);
        assert!(duck(&v2, SR, &c)[12_000 + 96_000] >= 0.95); // 4 s after the burst
        assert!(duck(&vec![0.0; 1000], SR, &c).iter().all(|g| *g == 1.0));
    }

    #[test]
    fn word_gap_does_not_release_duck() {
        let c = Music::default();
        let mut v = sine(7_200, 0.5); // word, 300 ms
        v.resize(7_200 + 4_800, 0.0); // 200 ms gap
        v.extend(sine(7_200, 0.5));
        v.resize(v.len() + 24_000, 0.0);
        let g = duck(&v, SR, &c);
        let lo = db(-12.0);
        assert!(g[7_200 + 4_800..7_200 + 4_800 + 100].iter().all(|x| *x < lo * 1.15), "{}", g[12_000]);
        assert!(g[3_000..19_000].iter().all(|x| *x < lo * 1.15));
    }

    /// Refactor gate: the parametrised functions are bit-identical to the `Music`-driven ones (funkhaus golden).
    #[test]
    fn duck_with_and_loop_body_match_music_versions() {
        let c = Music { duck_db: -9.0, crossfade_ms: 1500.0, bed_db: -17.0, ..Music::default() };
        let mut v = sine(12_000, 0.5);
        v.resize(60_000, 0.0);
        assert_eq!(duck(&v, SR, &c), duck_with(&v, SR, &DuckParams::from(&c)));
        let b: Vec<f32> = (0..240_000).map(|i| (i as f32 * 0.013).sin()).collect();
        let x = (1.5 * SR as f32) as usize;
        let body = loop_body(&b, x).unwrap();
        assert_eq!(body.len(), b.len() - x);
        let l = loop_bed(&b, 500_000, SR, &c).unwrap();
        let g = db(c.bed_db);
        assert!(l.iter().enumerate().skip(SR as usize).take(1000).all(|(i, v)| *v == body[i % body.len()] * g * 1.0));
        assert!(loop_body(&vec![0.0; 2 * x], x).is_err() && loop_body(&vec![0.0; 2 * x + 1], x).is_ok());
    }

    #[test]
    fn render_cue_gain_fades_loop_cut() {
        let src = vec![0.5f32; 48_000];
        // gain -6 dB, no fades: factor 0.5012
        let r = render_cue(&src, &CueRender { len: 1000, loop_: false, gain_db: -6.0, fade_in: 0.0, fade_out: 0.0, xfade_ms: 0.0 }, SR).unwrap();
        assert_eq!(r.len(), 1000);
        assert!((r[0] / 0.5 - 0.5012).abs() < 1e-4);
        // cut: never longer than the source
        assert_eq!(render_cue(&src, &CueRender { len: 100_000, loop_: false, gain_db: 0.0, fade_in: 0.0, fade_out: 0.0, xfade_ms: 0.0 }, SR).unwrap().len(), 48_000);
        // fades: linear 0 -> 1 over fade_in, 0 on the last sample
        let r = render_cue(&src, &CueRender { len: 48_000, loop_: false, gain_db: 0.0, fade_in: 1.0, fade_out: 0.5, xfade_ms: 0.0 }, SR).unwrap();
        assert_eq!((r[0], r[12_000], r[24_000], r[47_999]), (0.0, 0.25, 0.5, 0.0));
        assert!((r[48_000 - 6_000] - 0.25).abs() < 1e-4);
        // loop: exact length, seam bounded, needs > 2x crossfade
        let b: Vec<f32> = (0..1_440_000).map(|i| (2.0 * std::f32::consts::PI * 220.0 * i as f32 / SR as f32).sin()).collect();
        let l = render_cue(&b, &CueRender { len: 2_880_000, loop_: true, gain_db: 0.0, fade_in: 0.0, fade_out: 0.0, xfade_ms: 2000.0 }, SR).unwrap();
        assert_eq!(l.len(), 2_880_000);
        let p = 1_392_000;
        assert!((l[p - 1] - l[p]).abs() < 0.1, "{}", (l[p - 1] - l[p]).abs());
        assert!(render_cue(&vec![0.0; 96_000], &CueRender { len: 10, loop_: true, gain_db: 0.0, fade_in: 0.0, fade_out: 0.0, xfade_ms: 2000.0 }, SR).is_err());
        assert!(render_cue(&[], &CueRender { len: 10, loop_: false, gain_db: 0.0, fade_in: 0.0, fade_out: 0.0, xfade_ms: 0.0 }, SR).unwrap().is_empty());
    }
}
