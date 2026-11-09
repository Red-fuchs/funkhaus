//! Cue-precise citation times: a citation keeps its chunk id but shows the start of the cue inside the
//! chunk that best matches the claim citing it (token overlap, tie → earliest).
use crate::ask::{Cite, cite_nums};
use crate::ingest::Cue;
use crate::retrieval::{Chunk, tokens};
use crate::{Notebook, Store, util};
use std::collections::HashSet;

/// Cues inside the chunk's time span, without excluded (outcut) ones.
pub fn chunk_cues(st: &Store, nb: &Notebook, c: &Chunk) -> Vec<Cue> {
    let (Some(a), Some(b)) = (c.t0, c.t1) else { return vec![] };
    let oc = st.outcuts(nb, &c.src);
    st.cues(nb, &c.src).unwrap_or_default().into_iter().filter(|x| x.t0 >= a && x.t1 <= b && !oc.iter().any(|o| o.hits(x.t0, x.t1))).collect()
}

/// Best cue by shared tokens with `claim` (a cue plus its successor, since claims often span two cues);
/// tie → earliest; None without any overlap.
pub fn best_cue<'a>(cues: &'a [Cue], claim: &str) -> Option<&'a Cue> {
    let q: HashSet<String> = tokens(claim).into_iter().collect();
    let mut best: Option<(usize, &Cue)> = None;
    for (i, c) in cues.iter().enumerate() {
        let next = cues.get(i + 1).map(|n| n.text.as_str()).unwrap_or("");
        let have: HashSet<String> = tokens(&format!("{} {next}", c.text)).into_iter().collect();
        let own = tokens(&c.text).iter().filter(|t| q.contains(*t)).count();
        let s = q.intersection(&have).count();
        // the window must start with a matching cue
        if own > 0 && best.is_none_or(|b| s > b.0) {
            best = Some((s, c));
        }
    }
    best.map(|b| b.1)
}

/// Sentences of `text` citing [n]; a bare "[n]" sentence takes the sentence before it.
pub fn claims(text: &str, n: usize) -> String {
    let segs: Vec<&str> = text.split_inclusive(['.', '!', '?', '\n']).collect();
    let mut out = vec![];
    for (i, s) in segs.iter().enumerate() {
        if cite_nums(s).contains(&n) {
            let bare = !s.split_whitespace().any(|w| !w.starts_with('[') && w.chars().any(char::is_alphabetic));
            if bare && i > 0 {
                out.push(segs[i - 1]);
            }
            out.push(s);
        }
    }
    out.concat()
}

/// Sets `at`/`t0` of each citation to the best-matching cue of its chunk (chunk start otherwise).
pub fn refine(st: &Store, nb: &Notebook, text: &str, cites: &mut [Cite]) {
    for c in cites {
        let Ok(ch) = st.chunk(nb, &c.chunk) else { continue };
        let cues = chunk_cues(st, nb, &ch);
        if let Some(x) = best_cue(&cues, &claims(text, c.n)) {
            c.t0 = Some(x.t0);
            c.at = Some(util::mmss(x.t0));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cue(t0: f64, t: &str) -> Cue {
        Cue { t0, t1: t0 + 3.0, speaker: None, text: t.into() }
    }

    #[test]
    fn best_cue_by_overlap() {
        let cs = vec![cue(10.0, "Guten Morgen zusammen."), cue(14.0, "Wir haben von neun Uhr"), cue(18.0, "bis sechzehn Uhr Zeit."), cue(22.0, "Um sechzehn Uhr ist Schluss.")];
        assert_eq!(best_cue(&cs, "Der Workshop läuft von neun bis sechzehn Uhr.").map(|c| c.t0), Some(14.0));
        assert_eq!(best_cue(&cs, "Kaffee?"), None);
        // tie → earliest
        assert_eq!(best_cue(&cs, "Uhr").map(|c| c.t0), Some(14.0));
    }

    #[test]
    fn claim_sentences() {
        let t = "Der Kurs dauert sieben Stunden [1]. Pausen gibt es zwei. [2]\nEnde [1, 3].";
        assert_eq!(claims(t, 1), "Der Kurs dauert sieben Stunden [1].Ende [1, 3].");
        assert_eq!(claims(t, 2), " Pausen gibt es zwei. [2]\n");
    }
}
