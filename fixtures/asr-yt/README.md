# asr-yt: YouTube auto-caption (ASR) parsing regression fixture

Fictional German two-person chat about a fictional library. No network, no real persons.

- `talk.vtt`: 60 cues, YouTube ASR style (`align:start position:0%`, inline `<hh:mm:ss.mmm><c> word</c>` tags, rolling 2-line cues, 10 ms flush cues, `>>` turn markers without names).
- `expected.cues.json`: 30 ideal cues `{t0, t1, speaker, text}` (seconds). `speaker` starts at "A" and flips at each `>>`. `text` has no `>>`.

## Cue numbering

Cues are numbered 1..60 in file order. Utterance k (1-based, = `expected.cues.json[k-1]`) is:
- odd cue `2k-1`: typing cue (the visible caption, line1 = rolled previous line, line2 = new text with word tags). Its end time is `t1`.
- even cue `2k`: 10 ms flush cue (plain text line, then a blank line). Duplicate of the typing cue text.

## Tricky cases

- Rolling dupes (all typing cues): line1 of cue `2k-1` repeats the text of utterance k-1 (e.g. cue 3 line1 = utt 1). Must be dropped. Flush cues `2k` duplicate their own typing cue (e.g. cue 2 = cue 1 text). Must be dropped.
- Real repeat "Ja." across speakers: utt 5 (B, cue 9-10) and utt 6 (A, cue 11-12, `>> Ja.`). Cue 11 line1 = "Ja." (roll, drop) but line2 = ">> Ja." is real speech. A parser that drops line2 because it equals the previous cue text loses utt 6. Expected keeps both, speakers B then A.
- Number-only line "2027": utt 11 (B, cues 21-22, `>> 2027`). Cue 23 line1 = "2027" (roll, drop). Expected keeps "2027" once. Must not be treated as an SRT index or dropped as a number.
- Speaker flips at `>>` (no names): flip cues 3, 7, 9, 11, 15, 19, 21, 25, 29, 33, 37, 41, 45, 49, 53, 57 (odd = typing cue of the flipped utt). Speaker alternates A/B on each. Non-flip cues keep speaker.
- Pause without roll: utt 15 (cue 29, line1 = " " after 0.8 s gap), utt 19 (cue 37, 0.6 s), utt 27 (cue 53, 0.5 s). Line1 is blank, so there is no roll. Text must still be kept, t0 = block start.
- Numbers in real speech: "84 Stimmen" (utt 12, cues 23-24; utt 13, cues 25-26), "zwölf", "Zwölf Vereine" (utt 20). Must survive.
- Inline word tags: every typing cue, e.g. cue 1 `Hallo<00:00:00.590><c> und</c>...`. Tags stripped, words joined with single spaces.
- Timing: typing cue start = `t0`, typing cue end = `t1`. Flush cues must not change `t0`/`t1` of the kept cue.
