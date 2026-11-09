use crate::script::Line;
use anyhow::{Context, Result, bail};
use std::path::Path;

/// Mono f32 samples + sample rate (stereo is averaged).
pub fn read(p: &Path) -> Result<(Vec<f32>, u32)> {
    let mut r = hound::WavReader::open(p).with_context(|| p.display().to_string())?;
    let s = r.spec();
    let x: Vec<f32> = match s.sample_format {
        hound::SampleFormat::Float => r.samples::<f32>().collect::<Result<_, _>>()?,
        hound::SampleFormat::Int => {
            let k = (1i64 << (s.bits_per_sample - 1)) as f32;
            r.samples::<i32>().map(|v| v.map(|v| v as f32 / k)).collect::<Result<_, _>>()?
        }
    };
    let ch = s.channels.max(1) as usize;
    let x = if ch == 1 { x } else { x.chunks(ch).map(|f| f.iter().sum::<f32>() / ch as f32).collect() };
    Ok((x, s.sample_rate))
}

/// Cuts leading/trailing samples below `th`, keeping `pad` seconds.
pub fn trim(x: &[f32], sr: u32, th: f32, pad: f32) -> &[f32] {
    let p = (pad * sr as f32) as usize;
    match (x.iter().position(|v| v.abs() > th), x.iter().rposition(|v| v.abs() > th)) {
        (Some(a), Some(b)) => &x[a.saturating_sub(p)..(b + p).min(x.len())],
        _ => x,
    }
}

/// Start sample per line and total length (as in ref/mix.py, plus 0.5 s tail).
pub fn timeline(lines: &[Line], lens: &[usize], sr: u32) -> (Vec<usize>, usize) {
    let mut t = 0usize;
    let mut prev = (0usize, 0usize);
    let mut end = 0;
    let starts = lines
        .iter()
        .zip(lens)
        .map(|(l, &n)| {
            let s = if l.over {
                prev.0 + ((prev.1 - prev.0) as f64 * l.gap as f64).round() as usize
            } else {
                let s = (t as f64 + l.gap as f64 * sr as f64).round().max(0.0) as usize;
                prev = (s, s + n);
                t = s + n;
                s
            };
            end = end.max(s + n);
            s
        })
        .collect();
    (starts, end + sr as usize / 2)
}

/// Lays out trimmed clips; B lines at 0.8 gain; scales down to 0.95 peak if needed.
pub fn mix(lines: &[Line], clips: &[Vec<f32>], sr: u32) -> Vec<f32> {
    let cl: Vec<&[f32]> = clips.iter().map(|c| trim(c, sr, 0.01, 0.03)).collect();
    layout(lines, &cl, sr)
}

/// Like `mix` for clips that are already trimmed (keeps deliberate edge silence from `[pause]`).
pub fn layout(lines: &[Line], cl: &[&[f32]], sr: u32) -> Vec<f32> {
    layout_k(lines, cl, sr).0
}

/// `layout` plus the divisor `k` (≥ 1) it scaled the sum by: line i sits in the mix as `clip * gain(i) / k`.
pub fn layout_k(lines: &[Line], cl: &[&[f32]], sr: u32) -> (Vec<f32>, f32) {
    let (starts, len) = timeline(lines, &cl.iter().map(|c| c.len()).collect::<Vec<_>>(), sr);
    place(&starts, cl, &lines.iter().map(line_gain).collect::<Vec<_>>(), len)
}

/// Sums clips at absolute sample `starts` (clip i at `gains[i]`) into a buffer of at least `len` samples (longer if a clip
/// ends later); scales down to 0.95 peak like `layout_k` and returns the divisor `k` (≥ 1).
pub fn place(starts: &[usize], cl: &[&[f32]], gains: &[f32], len: usize) -> (Vec<f32>, f32) {
    let mut out = place_raw(starts, cl, gains, len);
    let k = norm(&mut out);
    (out, k)
}

/// `place` without the peak scaling (Stimmwerk adds music cues first, then normalizes once).
pub fn place_raw(starts: &[usize], cl: &[&[f32]], gains: &[f32], len: usize) -> Vec<f32> {
    let len = starts.iter().zip(cl).map(|(s, c)| s + c.len()).fold(len, usize::max);
    let mut out = vec![0f32; len];
    for ((c, &s), &g) in cl.iter().zip(starts).zip(gains) {
        for (o, v) in out[s..s + c.len()].iter_mut().zip(c.iter()) {
            *o += v * g;
        }
    }
    out
}

/// Scales `out` down to 0.95 peak if needed; returns the divisor `k` (≥ 1).
pub fn norm(out: &mut [f32]) -> f32 {
    let peak = out.iter().fold(0f32, |m, v| m.max(v.abs()));
    let k = (peak / 0.95).max(1.0);
    out.iter_mut().for_each(|v| *v /= k);
    k
}

/// (frames per channel, sample rate, channels) from a WAV header.
pub fn info(p: &Path) -> Result<(usize, u32, u16)> {
    let r = hound::WavReader::open(p).with_context(|| p.display().to_string())?;
    let s = r.spec();
    Ok((r.duration() as usize, s.sample_rate, s.channels))
}

/// Mix gain of a line: overlays (B / `+ID`) at 0.8.
pub fn line_gain(l: &Line) -> f32 {
    if l.over { 0.8 } else { 1.0 }
}

/// 32-bit float mono WAV (no clipping above 0 dBFS).
pub fn write_f32(p: &Path, x: &[f32], sr: u32) -> Result<()> {
    let spec = hound::WavSpec { channels: 1, sample_rate: sr, bits_per_sample: 32, sample_format: hound::SampleFormat::Float };
    let mut w = hound::WavWriter::create(p, spec)?;
    x.iter().try_for_each(|v| w.write_sample(*v))?;
    w.finalize()?;
    Ok(())
}

pub fn write(p: &Path, x: &[f32], sr: u32) -> Result<()> {
    let spec = hound::WavSpec { channels: 1, sample_rate: sr, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
    let mut w = hound::WavWriter::create(p, spec)?;
    for v in x {
        w.write_sample((v.clamp(-1.0, 1.0) * 32767.0) as i16)?;
    }
    w.finalize()?;
    Ok(())
}

/// Limiter ceiling below the spec TP: MP3 encoding adds overshoot (funkhaus path, unchanged).
const MP3_HEADROOM: f32 = 1.0;
/// PCM keeps the samples, but alimiter limits sample peaks: real speech overshoots ~0.5 dB true peak (measured, leuchtturm).
const WAV_HEADROOM: f32 = 0.8;

/// Loudness target (EBU R128 loudnorm).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Loudness {
    #[default]
    Podcast,
    Hoerbuch,
    Video,
}

impl Loudness {
    /// (I, TP, LRA)
    /// Spec targets (I, true peak, LRA) of the delivered file.
    pub fn target(self) -> (f32, f32, f32) {
        match self {
            Self::Podcast => (-16.0, -1.5, 11.0),
            Self::Hoerbuch => (-19.0, -3.0, 11.0),
            Self::Video => (-14.0, -1.0, 11.0),
        }
    }
    /// Pass 1: measure only (loudnorm analysis output, JSON).
    fn measure_filter(self) -> String {
        let (i, tp, lra) = self.target();
        format!("loudnorm=I={i}:TP={tp}:LRA={lra}:print_format=json")
    }
    /// Pass 2: constant gain to the target I, then a lookahead limiter at the spec TP minus MP3 headroom.
    /// (loudnorm's linear mode silently misses I when the measured TP/LRA forbid it.)
    pub fn gain_filter(self, input_i: f32) -> String {
        self.chain(self.target().0 - input_i)
    }
    /// Limiter ceiling in dBTP for the delivered format.
    pub fn limit_db_for(self, fmt: Fmt) -> f32 {
        self.target().1 - if fmt == Fmt::Mp3 { MP3_HEADROOM } else { WAV_HEADROOM }
    }
    /// alimiter `limit` (linear sample peak) for `fmt`, exactly as used in the chain.
    pub fn limit_lin(self, fmt: Fmt) -> f32 {
        let l = 10f32.powf(self.limit_db_for(fmt) / 20.0).clamp(0.0625, 1.0);
        (l * 1e4).round() / 1e4
    }
    /// Does the peak (`input_tp` dBTP) exceed the limiter ceiling after `gain_db`?
    pub fn limiter_active(self, input_tp: f32, gain_db: f32) -> bool {
        self.limiter_active_for(input_tp, gain_db, Fmt::Mp3)
    }
    fn limiter_active_for(self, input_tp: f32, gain_db: f32, fmt: Fmt) -> bool {
        input_tp + gain_db >= self.limit_db_for(fmt)
    }
    fn chain(self, gain_db: f32) -> String {
        self.chain_lat(gain_db, false, Fmt::Mp3)
    }
    /// `lat`: alimiter compensates its lookahead delay.
    fn chain_lat(self, gain_db: f32, lat: bool, fmt: Fmt) -> String {
        format!("volume={gain_db:.2}dB,alimiter=limit={:.4}:level=0{}", self.limit_lin(fmt), if lat { ":latency=1" } else { "" })
    }
}

impl std::str::FromStr for Loudness {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        match s.trim().to_lowercase().as_str() {
            "podcast" => Ok(Self::Podcast),
            "hoerbuch" => Ok(Self::Hoerbuch),
            "video" => Ok(Self::Video),
            x => Err(format!("unbekanntes Loudness-Preset '{x}' (podcast|hoerbuch|video)")),
        }
    }
}

/// loudnorm's JSON block from ffmpeg stderr (log lines follow it).
fn loudnorm_json(err: &str) -> Option<serde_json::Value> {
    let (a, b) = (err.rfind('{')?, err.rfind('}')?);
    serde_json::from_str(err.get(a..=b)?).ok()
}

/// Output container of `master`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fmt {
    /// libmp3lame q:a 3, 44.1 kHz mono, ID3 tags
    Mp3,
    /// 16-bit PCM at the source rate, RIFF INFO tags (no TXXX)
    Wav,
}

/// `comment` also sets the `AI_GENERATED=true` TXXX tag (MP3).
#[derive(Clone, Copy, Debug, Default)]
pub struct Tags<'a> {
    pub title: &'a str,
    pub artist: &'a str,
    pub comment: Option<&'a str>,
}

fn ff(bin: &Path, args: &[&str]) -> Result<std::process::Output> {
    let o = std::process::Command::new(bin).args(args).output()?;
    if !o.status.success() {
        bail!("ffmpeg: {}", String::from_utf8_lossy(&o.stderr));
    }
    Ok(o)
}

/// Gain (dB) that brings `wav` to `ld` through `volume + alimiter`: loudnorm measurement, then (only if the limiter
/// works) re-measure the real chain and correct, max 3x, 0.1 LU tolerance. `lat`: limiter delay compensated.
fn plan_gain(bin: &Path, wav: &Path, ld: Loudness, lat: bool, fmt: Fmt) -> Result<f32> {
    let w = wav.to_string_lossy();
    let measure = |af: &str| -> Result<(f32, f32)> {
        let o = ff(bin, &["-hide_banner", "-nostats", "-i", &w, "-af", af, "-f", "null", "-"])?;
        loudnorm_json(&String::from_utf8_lossy(&o.stderr))
            .and_then(|m| Some((m["input_i"].as_str()?.parse::<f32>().ok()?, m["input_tp"].as_str()?.parse::<f32>().ok()?)))
            .filter(|v| v.0.is_finite() && v.1.is_finite())
            .context("loudnorm-Messung ohne Ergebnis (stille Datei?)")
    };
    let (ti, ..) = ld.target();
    let (mi, mtp) = measure(&ld.measure_filter())?;
    let mut gain = ti - mi;
    // the limiter takes loudness away when it works hard: re-measure the real chain and correct the gain (max 3x); skipped if it stays idle
    for _ in 0..if ld.limiter_active_for(mtp, gain, fmt) { 3 } else { 0 } {
        let got = measure(&format!("{},{}", ld.chain_lat(gain, lat, fmt), ld.measure_filter()))?.0;
        if (ti - got).abs() < 0.1 {
            break;
        }
        gain += ti - got;
    }
    Ok(gain)
}

/// Two-pass master of `wav` to `ld` (see `plan_gain`) as `fmt`. `lat`: compensate the limiter's lookahead delay
/// (output sample-aligned with the input). Returns the applied gain in dB.
/// MP3: ID3 COMM + TXXX `AI_GENERATED=true`. WAV: RIFF keeps only INFO ICMT, so the flag goes into the comment text.
pub fn master(ffmpeg: &str, wav: &Path, out: &Path, ld: Loudness, fmt: Fmt, tags: Tags, lat: bool) -> Result<f32> {
    let bin = funkhaus_sys::which(ffmpeg).context("ffmpeg nicht gefunden")?;
    let gain = plan_gain(&bin, wav, ld, lat, fmt)?;
    let (w, o) = (wav.to_string_lossy(), out.to_string_lossy());
    let af = ld.chain_lat(gain, lat, fmt);
    let (t, a) = (format!("title={}", tags.title), format!("artist={}", tags.artist));
    let mut args = vec!["-y", "-loglevel", "error", "-i", &w, "-af", &af];
    args.extend(match fmt {
        Fmt::Mp3 => ["-ar", "44100", "-ac", "1", "-c:a", "libmp3lame", "-q:a", "3"].as_slice(),
        Fmt::Wav => ["-ac", "1", "-c:a", "pcm_s16le"].as_slice(),
    });
    args.extend(["-metadata", &t, "-metadata", &a]);
    let c = tags.comment.map(|c| if fmt == Fmt::Wav { format!("comment={c} [{WAV_AI_FLAG}]") } else { format!("comment={c}") });
    if let Some(c) = &c {
        args.extend(["-metadata", c]);
        if fmt == Fmt::Mp3 {
            args.extend(["-metadata", "AI_GENERATED=true"]);
        }
    }
    args.push(&o);
    ff(&bin, &args)?;
    Ok(gain)
}

/// Marker appended to the WAV comment (ICMT) – RIFF has no custom tags.
pub const WAV_AI_FLAG: &str = "AI_GENERATED=true";

/// Applies `master`'s chain (`volume` + `alimiter`, ceiling for `fmt`) with a fixed `gain_db` to `inp` -> 32-bit float
/// WAV `out` (e.g. stems of a mastered mix: same processing, so a stem matches the master where it plays alone).
pub fn chain_file(ffmpeg: &str, inp: &Path, out: &Path, ld: Loudness, fmt: Fmt, gain_db: f32, lat: bool) -> Result<()> {
    let bin = funkhaus_sys::which(ffmpeg).context("ffmpeg nicht gefunden")?;
    let (i, o, af) = (inp.to_string_lossy(), out.to_string_lossy(), ld.chain_lat(gain_db, lat, fmt));
    ff(&bin, &["-y", "-loglevel", "error", "-i", &i, "-af", &af, "-ac", "1", "-c:a", "pcm_f32le", "-f", "wav", &o]).map(drop)
}

/// Two-pass to `ld`, MP3: loudnorm measurement, then measured gain + limiter. `comment` also sets the `AI_GENERATED=true` TXXX tag.
pub fn mp3(ffmpeg: &str, wav: &Path, out: &Path, title: &str, artist: &str, ld: Loudness, comment: Option<&str>) -> Result<()> {
    master(ffmpeg, wav, out, ld, Fmt::Mp3, Tags { title, artist, comment }, false).map(drop)
}

/// EBU R128 of a finished file (ffmpeg `ebur128=peak=true`).
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize)]
pub struct Measure {
    /// integrated loudness, LUFS
    pub lufs: f64,
    /// true peak, dBTP
    pub tp: f64,
}

/// Integrated loudness and true peak of `p` (any format ffmpeg reads).
pub fn measure_file(ffmpeg: &str, p: &Path) -> Result<Measure> {
    let bin = funkhaus_sys::which(ffmpeg).context("ffmpeg nicht gefunden")?;
    let o = ff(&bin, &["-hide_banner", "-nostats", "-i", &p.to_string_lossy(), "-filter_complex", "ebur128=peak=true", "-f", "null", "-"])?;
    ebur128_summary(&String::from_utf8_lossy(&o.stderr)).with_context(|| format!("ebur128 ohne Ergebnis: {}", p.display()))
}

/// `I:` and `Peak:` from the last ebur128 `Summary:` block.
fn ebur128_summary(err: &str) -> Option<Measure> {
    let sum = &err[err.rfind("Summary:")?..];
    let num = |key: &str| sum.lines().map(str::trim).find_map(|l| l.strip_prefix(key)).and_then(|r| r.split_whitespace().next()?.parse::<f64>().ok());
    Some(Measure { lufs: num("I:")?, tp: num("Peak:")? })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loudnorm_json_with_trailing_log() {
        let e = "[Parsed_loudnorm_0 @ 0x1] \n{\n\t\"input_i\" : \"-17.10\",\n\t\"input_tp\" : \"-1.30\"\n}\n[out#0/null @ 0x2] video:0KiB audio:1KiB\n";
        assert_eq!(loudnorm_json(e).unwrap()["input_i"], "-17.10");
    }

    #[test]
    fn ebur128_parse() {
        let e = "[Parsed_ebur128_0 @ 0x1] t: 1.2 M: -20 S: -21 I: -30 LUFS\n[Parsed_ebur128_0 @ 0x1] Summary:\n\n  Integrated loudness:\n    I:         -16.2 LUFS\n    Threshold: -26.5 LUFS\n\n  Loudness range:\n    LRA:         3.1 LU\n\n  True peak:\n    Peak:       -1.9 dBFS\n";
        assert_eq!(ebur128_summary(e), Some(Measure { lufs: -16.2, tp: -1.9 }));
        assert_eq!(ebur128_summary("I: -3 LUFS"), None);
        assert!(Loudness::Podcast.chain_lat(0.5, true, Fmt::Mp3).ends_with("level=0:latency=1"));
    }

    #[test]
    fn limiter_decision() {
        // podcast ceiling = -2.5 dB
        assert!(!Loudness::Podcast.limiter_active(-10.0, 5.0));
        assert!(Loudness::Podcast.limiter_active(-3.0, 1.0));
        assert!(Loudness::Podcast.limiter_active(-2.5, 0.0));
        // WAV: 0.8 dB below spec TP; MP3 unchanged (funkhaus)
        assert_eq!((Loudness::Podcast.limit_db_for(Fmt::Wav), Loudness::Podcast.limit_db_for(Fmt::Mp3)), (-2.3, -2.5));
        assert!(Loudness::Podcast.limiter_active_for(-2.4, 0.0, Fmt::Mp3) && !Loudness::Podcast.limiter_active_for(-2.4, 0.0, Fmt::Wav));
        assert_eq!(Loudness::Podcast.chain_lat(0.0, false, Fmt::Mp3), Loudness::Podcast.gain_filter(Loudness::Podcast.target().0));
        assert_eq!(Loudness::Podcast.limit_lin(Fmt::Mp3), 0.7499);
    }

    #[test]
    fn loudness_presets() {
        assert_eq!(Loudness::default(), Loudness::Podcast);
        assert_eq!(Loudness::Hoerbuch.measure_filter(), "loudnorm=I=-19:TP=-3:LRA=11:print_format=json");
        // limit = lin(TP - 1 dB headroom)
        assert_eq!(Loudness::Podcast.gain_filter(-17.1), "volume=1.10dB,alimiter=limit=0.7499:level=0");
        assert_eq!(Loudness::Video.gain_filter(-15.6), "volume=1.60dB,alimiter=limit=0.7943:level=0");
        assert_eq!(Loudness::Hoerbuch.gain_filter(-12.0), "volume=-7.00dB,alimiter=limit=0.6310:level=0");
        assert_eq!("Video".parse::<Loudness>().unwrap(), Loudness::Video);
        assert!("x".parse::<Loudness>().is_err());
        assert_eq!(serde_json::to_string(&Loudness::Hoerbuch).unwrap(), "\"hoerbuch\"");
    }

    fn l(w: char, g: f32) -> Line {
        Line::ofb(w, g, "x")
    }

    #[test]
    fn timeline_like_ref() {
        let sr = 100;
        let lines = [l('O', 0.0), l('F', 0.1), l('B', 0.5), l('O', -0.2), l('F', 0.05)];
        let (s, end) = timeline(&lines, &[100, 200, 30, 50, 10], sr);
        // O 0..100, F 110..310, B at 110+0.5*200=210, O 290..340 (overlap), F 345..355
        assert_eq!(s, vec![0, 110, 210, 290, 345]);
        assert_eq!(end, 355 + 50);
        let (s, _) = timeline(&[l('O', -1.0)], &[10], sr);
        assert_eq!(s, vec![0]);
    }

    #[test]
    fn trim_and_mix() {
        let sr = 1000;
        let mut c = vec![0f32; 100];
        c[50] = 0.5;
        assert_eq!(trim(&c, sr, 0.01, 0.01).len(), 20); // same slice as ref/mix.py
        assert_eq!(trim(&[0.0; 5], sr, 0.01, 0.01).len(), 5);
        let lines = [l('O', 0.0), l('B', 0.0)];
        let out = mix(&lines, &[vec![1.0; 10], vec![1.0; 10]], sr);
        assert_eq!(out.len(), 10 + 500);
        assert!((out[0] - 0.95).abs() < 1e-6); // 1.8 peak scaled to 0.95
        let out = mix(&[l('O', 0.0)], &[vec![0.5; 10]], sr);
        assert_eq!(out[0], 0.5);
    }

    #[test]
    fn pause_line_is_silence_in_timeline() {
        let sr = 24_000;
        let l = |w, t: &str| Line::ofb(w, 0.0, t);
        let lines = [l('O', "Hallo."), l('O', "[pause 1.5s]"), l('F', "Ja.")];
        let n = crate::script::pause_samples("[pause 1.5s]", sr).unwrap().unwrap();
        assert_eq!(n, 36_000);
        let clips = [vec![0.5f32; 100], vec![0.0; n], vec![0.5; 100]];
        let cl: Vec<&[f32]> = clips.iter().map(|c| c.as_slice()).collect();
        let (s, _) = timeline(&lines, &[100, n, 100], sr);
        assert_eq!(s, vec![0, 100, 36_100]);
        let out = layout(&lines, &cl, sr);
        assert!(out[100..36_100].iter().all(|v| *v == 0.0));
        assert_eq!(out[36_100], 0.5);
    }

    #[test]
    fn place_absolute_no_gap() {
        let one = vec![0.5f32; 24_000];
        let (out, k) = place(&[0, 48_000], &[&one, &one], &[1.0, 1.0], 0);
        assert_eq!((out.len(), k), (72_000, 1.0));
        assert_eq!((out[23_999], out[24_000], out[47_999], out[48_000]), (0.5, 0.0, 0.0, 0.5));
        // overlapping clips sum and are scaled like layout_k; len is a minimum
        let full = [1.0f32; 20];
        let (out, k) = place(&[0, 10], &[&full, &full], &[1.0, 0.8], 100);
        assert_eq!(out.len(), 100);
        assert!((k - 1.8 / 0.95).abs() < 1e-6 && (out[15] - 0.95).abs() < 1e-6 && (out[5] - 1.0 / k).abs() < 1e-6);
        // place = place_raw + norm; a quiet sum is untouched (k = 1)
        let mut raw = place_raw(&[0, 10], &[&full, &full], &[1.0, 0.8], 100);
        assert_eq!(raw[15], 1.8);
        assert_eq!(norm(&mut raw), k);
        assert_eq!(raw, out);
        assert_eq!(norm(&mut [0.5f32, -0.9]), 1.0);
    }

    #[test]
    fn wav_roundtrip() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("a.wav");
        write(&p, &[0.0, 0.5, -0.5], 24000).unwrap();
        let (x, sr) = read(&p).unwrap();
        assert_eq!(sr, 24000);
        assert!((x[1] - 0.5).abs() < 1e-3);
    }
}
