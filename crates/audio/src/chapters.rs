//! Chapters from `# SEG` markers (docs/m2/podcast-chapters.md): start offsets from the mix timeline,
//! FFMETADATA1 → ID3 CHAP via ffmpeg, chapters.json and shownotes.md.
use crate::{mix, script::Line};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Chapter {
    pub n: usize,
    pub title: String,
    pub goal: String,
    pub start_ms: u64,
    pub end_ms: u64,
    /// "s1#3 12:04"
    #[serde(default)]
    pub refs: Vec<String>,
}

/// (first line index, title, goal) per `# SEG n | title | goal` marker.
pub fn marks(script: &str) -> Vec<(usize, String, String)> {
    let (mut v, mut n) = (vec![], 0);
    for l in script.lines().map(str::trim) {
        if let Some(r) = l.strip_prefix("# SEG") {
            let p: Vec<&str> = r.splitn(3, '|').map(str::trim).collect();
            v.push((n, p.get(1).unwrap_or(&"").to_string(), p.get(2).unwrap_or(&"").to_string()));
        } else if !l.is_empty() && !l.starts_with('#') {
            n += 1;
        }
    }
    v
}

/// Why a `# SEG` marker yields no chapter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SegReason {
    /// no speech line follows (script end)
    Orphan,
    /// the next SEG comes before any speech line
    Empty,
}

/// (1-based script line, SEG text, reason)
pub type SegIssue = (usize, String, SegReason);

/// `# SEG` markers that yield no chapter: (1-based script line, SEG text, reason).
/// Same line counting as `marks`; `spans` drops exactly these.
pub fn seg_issues(script: &str) -> Vec<SegIssue> {
    let (mut v, mut pending): (Vec<SegIssue>, Vec<(usize, String)>) = (vec![], vec![]);
    // the first SEG also covers speech before it (start 0), so it is never empty when speech precedes it
    let (mut data, mut nseg, mut prelude) = (false, 0usize, false);
    for (i, l) in script.lines().map(str::trim).enumerate() {
        if l.starts_with("# SEG") {
            if nseg == 0 {
                prelude = data;
            }
            nseg += 1;
            pending.push((i + 1, l.to_string()));
        } else if !l.is_empty() && !l.starts_with('#') {
            data = true;
            let n = pending.len().saturating_sub(1);
            let first = nseg == pending.len() && prelude;
            v.extend(pending.drain(..n).enumerate().filter(|(j, _)| !(first && *j == 0)).map(|(_, (n, s))| (n, s, SegReason::Empty)));
            pending.clear();
        }
    }
    v.extend(pending.into_iter().map(|(n, s)| (n, s, SegReason::Orphan)));
    v
}

/// German one-liner per `seg_issues` entry (funkhaus `warnings.txt`, stimmwerk lint).
pub fn seg_warning(seg: &str, reason: SegReason) -> String {
    match reason {
        SegReason::Orphan => format!("{seg}: keine Sprechzeile danach – Kapitel entfällt"),
        SegReason::Empty => format!("{seg}: keine Sprechzeile bis zum nächsten SEG – Kapitel entfällt"),
    }
}

/// Chapter spans from trimmed clip lengths (same timeline as `mix::mix`); first chapter starts at 0;
/// everything else (and the end) shifts by `lead` samples (intro before the voice).
/// Markers without a speech line (index past the timeline), markers followed by another marker on the same
/// line index (empty, except the first one when speech precedes it) and zero-length chapters are dropped
/// (`seg_issues` names them); ffmpeg never sees START == END.
pub fn spans(marks: &[(usize, String, String)], lines: &[Line], lens: &[usize], sr: u32, lead: usize) -> Vec<Chapter> {
    let (starts, total) = mix::timeline(lines, lens, sr);
    let ms = |s: usize| (s as u64 * 1000 + sr as u64 / 2) / sr as u64;
    let marks: Vec<_> = marks.iter().filter(|m| m.0 < starts.len()).collect();
    // an empty SEG would otherwise span the lead/initial gap before its successor (lead > 0 or first line GAP > 0)
    let marks: Vec<_> = marks.iter().enumerate().filter(|(k, m)| (*k == 0 && m.0 > 0) || marks.get(k + 1).is_none_or(|n| n.0 != m.0)).map(|(_, m)| *m).collect();
    let st: Vec<u64> = marks.iter().enumerate().map(|(k, m)| if k == 0 { 0 } else { ms(starts[m.0] + lead) }).collect();
    let end = ms(total + lead);
    let mut chs: Vec<Chapter> = marks
        .iter()
        .enumerate()
        .map(|(k, m)| Chapter { n: 0, title: m.1.clone(), goal: m.2.clone(), start_ms: st[k], end_ms: st.get(k + 1).copied().unwrap_or(end), refs: vec![] })
        .filter(|c| c.end_ms > c.start_ms)
        .collect();
    chs.iter_mut().enumerate().for_each(|(k, c)| c.n = k + 1);
    chs
}

fn esc(s: &str) -> String {
    s.chars().fold(String::new(), |mut o, c| {
        if "=;#\\\n".contains(c) {
            o.push('\\');
        }
        o.push(c);
        o
    })
}

pub fn ffmeta(title: &str, artist: &str, chs: &[Chapter]) -> String {
    let mut s = format!(";FFMETADATA1\ntitle={}\nartist={}\n", esc(title), esc(artist));
    for c in chs {
        s.push_str(&format!("\n[CHAPTER]\nTIMEBASE=1/1000\nSTART={}\nEND={}\ntitle={}\n", c.start_ms, c.end_ms, esc(&c.title)));
    }
    s
}

/// "m:ss" / "h:mm:ss", floored.
pub fn stamp(ms: u64) -> String {
    funkhaus_sys::mmss((ms / 1000) as f64)
}

/// Cue sheet: `HH:MM:SS<TAB>Title` per chapter (hours unbounded, seconds floored, tabs/newlines in titles → space).
pub fn cue_sheet(spans: &[Chapter]) -> String {
    spans
        .iter()
        .map(|c| {
            let s = c.start_ms / 1000;
            let t: String = c.title.chars().map(|x| if matches!(x, '\t' | '\n' | '\r') { ' ' } else { x }).collect();
            format!("{:02}:{:02}:{:02}\t{}\n", s / 3600, s / 60 % 60, s % 60, t)
        })
        .collect()
}

/// Show notes (German): overview, chapters with one-liner + refs, source list (title + file name/URL only).
pub fn shownotes(title: &str, overview: &str, chs: &[Chapter], sources: &[(String, String, String)], footer: &str) -> String {
    let ov: String = overview.split_inclusive(['.', '!', '?']).take(3).collect::<String>();
    let mut s = format!("# {title}\n\n{}\n\n## Kapitel\n\n", ov.trim());
    for c in chs {
        s.push_str(&format!("- {} {}: {} {}\n", stamp(c.start_ms), c.title, c.goal, c.refs.iter().map(|r| format!("[{r}]")).collect::<Vec<_>>().join(" ")));
    }
    s.push_str("\n## Quellenliste\n\n");
    for (id, t, origin) in sources.iter().filter(|(id, ..)| chs.iter().flat_map(|c| &c.refs).any(|r| r.starts_with(&format!("{id}#")))) {
        let o = if origin.contains("://") { origin.clone() } else { Path::new(origin).file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default() };
        s.push_str(&format!("- {id}: {t} ({o})\n"));
    }
    s.push_str(&format!("\n## Hinweis\n\n{footer}\n"));
    s
}

/// Writes CHAP frames into `mp3` (no re-encode) and checks them with ffprobe (±30 ms).
pub fn embed(ffmpeg: &str, mp3: &Path, meta: &Path, chs: &[Chapter]) -> Result<()> {
    let bin = funkhaus_sys::which(ffmpeg).context("ffmpeg nicht gefunden")?;
    // unique per file and process: two exports into one dir never share the temp file
    let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.subsec_nanos());
    let tmp = mp3.with_file_name(format!(".{}.ch-{}-{nonce}.mp3", mp3.file_stem().map(|s| s.to_string_lossy()).unwrap_or_default(), std::process::id()));
    let o = std::process::Command::new(&bin)
        .args(["-y", "-loglevel", "error", "-i"])
        .arg(mp3)
        .arg("-i")
        .arg(meta)
        .args(["-map", "0:a", "-map_metadata", "0", "-map_chapters", "1", "-c", "copy", "-id3v2_version", "3"])
        .arg(&tmp)
        .output()?;
    if !o.status.success() {
        bail!("ffmpeg Kapitel: {}", String::from_utf8_lossy(&o.stderr));
    }
    if let Some(probe) = bin.parent().map(|d| d.join("ffprobe")).filter(|p| p.exists()).or_else(|| funkhaus_sys::which("ffprobe")) {
        let o = std::process::Command::new(probe).args(["-v", "error", "-show_chapters", "-of", "json"]).arg(&tmp).output()?;
        let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap_or_default();
        let got: Vec<f64> = v["chapters"].as_array().into_iter().flatten().filter_map(|c| c["start_time"].as_str()?.parse().ok()).collect();
        let ok = got.len() == chs.len() && got.iter().zip(chs).all(|(g, c)| (g * 1000.0 - c.start_ms as f64).abs() <= 30.0);
        if !ok {
            let _ = std::fs::remove_file(&tmp);
            bail!("Kapitel-Prüfung fehlgeschlagen: ffprobe {got:?}");
        }
    }
    std::fs::rename(&tmp, mp3)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marks_spans_meta() {
        let txt = "# SEG 1 | Auftakt | Hook\nO|0|a\nF|0.1|b\n# SEG 2 | Kosten = Geld | Mitte\nF|0.8|c\nB|0.5|Mhm.\nO|0.1|d\n";
        let m = marks(txt);
        assert_eq!(m.iter().map(|x| x.0).collect::<Vec<_>>(), vec![0, 2]);
        let lines = crate::script::parse(txt).unwrap();
        let sr = 1000;
        // a 0..100, b 200..300, c 1100..1200 (0.8 s gap), B over c, d 1300..1400, tail 500
        let chs = spans(&m, &lines, &[100, 100, 100, 10, 100], sr, 0);
        assert_eq!((chs[0].start_ms, chs[0].end_ms, chs[1].start_ms, chs[1].end_ms), (0, 1100, 1100, 1900));
        let sh = spans(&m, &lines, &[100, 100, 100, 10, 100], sr, 6500);
        assert_eq!((sh[0].start_ms, sh[0].end_ms, sh[1].start_ms, sh[1].end_ms), (0, 1100 + 6500, 1100 + 6500, 1900 + 6500));
        let meta = ffmeta("T;1", "Funkhaus", &chs);
        assert!(meta.starts_with(";FFMETADATA1\ntitle=T\\;1\n") && meta.contains("START=1100\nEND=1900\ntitle=Kosten \\= Geld\n"));
        assert_eq!(stamp(61_999), "1:01");
    }

    /// docs/stimmwerk/seg-panic.md: SEG at script end / only SEGs used to index past the timeline.
    #[test]
    fn seg_edge_cases_no_panic() {
        let sr = 1000;
        let run_lead = |t: &str, lead: usize| {
            let lines = crate::script::parse(t).unwrap_or_default(); // only SEGs: "leeres Skript"
            let lens = vec![100; lines.len()];
            spans(&marks(t), &lines, &lens, sr, lead)
        };
        let run = |t: &str| run_lead(t, 0);
        let ns = |c: &[Chapter]| c.iter().map(|c| c.n).collect::<Vec<_>>();
        // (a) SEG as last line after a SEG
        let a = run("# SEG 1 | A\nO|0|a\n# SEG 2 | B\n");
        assert_eq!((a.len(), a[0].start_ms, a[0].end_ms, &a[0].title[..]), (1, 0, 600, "A"));
        // (a') single trailing SEG: nothing to cover
        assert!(run("O|0|a\n# SEG 1 | A\n").is_empty());
        // (b') two SEGs at the end
        assert_eq!(run("# SEG 1 | A\nO|0|a\n# SEG 2 | B\n# SEG 3 | C\n").len(), 1);
        // (b) consecutive SEGs mid-script: the zero-length one goes, numbering stays dense
        let b = run("# SEG 1 | A\nO|0|a\n# SEG 2 | B\n# SEG 3 | C\nF|0.1|b\n");
        assert_eq!((ns(&b), &b[1].title[..], b[1].start_ms, b[1].end_ms), (vec![1, 2], "C", 200, 800));
        // (c) two SEGs before the first line
        let c = run("# SEG 1 | A\n# SEG 2 | B\nO|0|a\nF|0.1|b\n");
        assert_eq!((c.len(), &c[0].title[..], c[0].start_ms), (1, "B", 0));
        // (c') first line not at 0 s / intro before the voice (--jingle): the empty SEG must not cover the gap
        let c = run("# SEG 1 | A\n# SEG 2 | B\nO|0.8|a\nF|0.1|b\n");
        assert_eq!((c.len(), &c[0].title[..], c[0].start_ms, c[0].end_ms), (1, "B", 0, 1600));
        let c = run_lead("# SEG 1 | A\n# SEG 2 | B\nO|0|a\nF|0.1|b\n", 1000);
        assert_eq!((c.len(), &c[0].title[..], c[0].start_ms, c[0].end_ms), (1, "B", 0, 1800));
        let c = run_lead("# SEG 1 | A\n# SEG 2 | B\nO|0.8|a\nF|0.1|b\n", 1000);
        assert_eq!((c.len(), &c[0].title[..]), (1, "B"));
        // (b) with lead: the zero-length SEG goes, successor starts at its line
        let b = run_lead("# SEG 1 | A\nO|0|a\n# SEG 2 | B\n# SEG 3 | C\nF|0.1|b\n", 1000);
        assert_eq!((b.len(), &b[1].title[..], b[1].start_ms), (2, "C", 1200));
        // (d) only SEGs
        assert!(run("# SEG 1 | A\n# SEG 2 | B\n").is_empty());
        assert!(run("# SEG 1 | A\n").is_empty());
        // speech before the first SEG belongs to it: SEG 1 is not empty
        let p = run("O|0|a\n# SEG 1 | A\n# SEG 2 | B\nF|0.1|b\n");
        assert_eq!((p.len(), &p[0].title[..], p[0].end_ms, &p[1].title[..]), (2, "A", 200, "B"));
        let p = run_lead("O|0.8|a\n# SEG 1 | A\n# SEG 2 | B\nF|0.1|b\n", 1000);
        assert_eq!((p.len(), &p[0].title[..], p[0].end_ms, &p[1].title[..]), (2, "A", 2000, "B"));
        // lens shorter than lines (caller bug) must not panic either
        let lines = crate::script::parse("# SEG 1 | A\nO|0|a\n# SEG 2 | B\nF|0.1|b\n").unwrap();
        assert_eq!(spans(&marks("# SEG 1 | A\nO|0|a\n# SEG 2 | B\nF|0.1|b\n"), &lines, &[100], sr, 0).len(), 1);
    }

    #[test]
    fn seg_issues_match_dropped_chapters() {
        use SegReason::{Empty as E, Orphan as O};
        let i = |t: &str| seg_issues(t).into_iter().map(|(n, _, r)| (n, r)).collect::<Vec<_>>();
        assert_eq!(i("# SEG 1 | A\nO|0|a\n# SEG 2 | B\n"), [(3, O)]);
        assert_eq!(i("O|0|a\n# SEG 1 | A\n"), [(2, O)]);
        assert_eq!(i("# SEG 1 | A\nO|0|a\n\n# SEG 2 | B\n# SEG 3 | C\n"), [(4, O), (5, O)]);
        assert_eq!(i("# SEG 1 | A\nO|0|a\n# SEG 2 | B\n# SEG 3 | C\nF|0.1|b\n"), [(3, E)]);
        assert_eq!(i("# SEG 1 | A\n# SEG 2 | B\nO|0|a\nF|0.1|b\n"), [(1, E)]);
        assert_eq!(i("# SEG 1 | A\n# SEG 2 | B\n"), [(1, O), (2, O)]);
        assert_eq!(i("O|0|a\n# SEG 1 | A\n# SEG 2 | B\nF|0.1|b\n"), []);
        assert_eq!(i("# SEG 1 | A\nO|0|a\n# note\nF|0.1|b\n# SEG 2 | B\nO|0|c\n"), []);
        assert_eq!(seg_issues("# SEG 1 | A\n")[0].1, "# SEG 1 | A");
        assert!(seg_warning("# SEG 1 | A", O).contains("Kapitel entfällt"));
    }

    #[test]
    fn cue_sheet_format() {
        let c = |ms, t: &str| Chapter { n: 1, title: t.into(), goal: String::new(), start_ms: ms, end_ms: ms, refs: vec![] };
        assert_eq!(cue_sheet(&[c(0, "A"), c(3_723_999, "B")]), "00:00:00\tA\n01:02:03\tB\n");
        assert_eq!(cue_sheet(&[c(360_000_000, "X")]), "100:00:00\tX\n");
        assert_eq!(cue_sheet(&[c(1000, "a\tb\nc\r\nd")]), "00:00:01\ta b c  d\n");
        assert_eq!(cue_sheet(&[]), "");
    }

    #[test]
    fn notes_list_only_cited_sources() {
        let ch = Chapter { n: 1, title: "A".into(), goal: "Ziel.".into(), start_ms: 0, end_ms: 1, refs: vec!["s1#2 3:04".into()] };
        let src = vec![("s1".into(), "Seminar".into(), "/Users/x/geheim/seminar.vtt".into()), ("s2".into(), "Andere".into(), "https://e.org/a".into())];
        let n = shownotes("Folge", "Eins. Zwei. Drei. Vier.", &[ch], &src, "Fuß.");
        assert!(n.contains("Eins. Zwei. Drei.") && !n.contains("Vier"));
        assert!(n.contains("- 0:00 A: Ziel. [s1#2 3:04]") && n.contains("- s1: Seminar (seminar.vtt)") && !n.contains("/Users") && !n.contains("s2:"));
    }
}
