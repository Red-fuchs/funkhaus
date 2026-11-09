# sync fixtures

Test input for the future TTS-to-subtitle fit ("sync") feature. Synthetic German text only.

Assumptions: German TTS ~13 chars/s. `needed = chars / 13`, `ratio = needed / slot`.
Status rules (limit = 1.12, matches PLAN-stimmwerk.md and atempo factor limit):
- `overlap`: cue window or start collides with a neighbour (`start_i < end_{i-1}` or `end_i > start_{i+1}`), checked first
- `fits`: ratio <= 1.00
- `atempo`: 1.00 < ratio <= 1.12 (speed-up via ffmpeg atempo)
- `too_long`: ratio > 1.12 (LLM shortening suggestion, else warning + overhang until next cue start)

Chars counted after stripping HTML/VTT tags and joining lines with a space.

## fit3.srt / fit3.vtt (same cues, two formats)

| # | slot | text | chars | needed | ratio | expected |
|---|------|------|-------|--------|-------|----------|
| 1 | 2.00 s (01.000-03.000) | `Alles klar.` (2 words) | 11 | 0.85 s | 0.42 | fits (slack ~1.15 s) |
| 2 | 3.28 s (04.000-07.280) | `Die Lieferung kommt morgen früh am Vormittag.` | 45 | 3.46 s | 1.06 | atempo (~6 % speed-up) |
| 3 | 2.00 s (08.000-10.000) | `Bitte denken Sie daran, die Unterlagen am Montag vollständig mitzubringen.` | 74 | 5.69 s | 2.85 | too_long |

## edge.srt

| # | timing | text | expected | why |
|---|--------|------|----------|-----|
| 1 | 00:00:00,000 --> 00:00:03.500 | `Willkommen zurück.` | overlap | ends 3.5 s, cue 2 starts 3.0 s (`end_i > start_{i+1}`); fits on its own (18 chars, 1.38 s, ratio 0.40) |
| 2 | 00:00:03,000 --> 00:00:06,000 | `Das ist ein Test` / `mit zwei Zeilen.` (2 lines) | overlap | starts before cue 1 ends (`start_i < end_{i-1}`); 33 chars = 2.54 s in 3.0 s slot (ratio 0.85, would fit) |
| 3 | 00:00:06,000 --> 00:00:09,000 | `<i>Das</i> ist <i>kursiv</i>.` | fits | tags stripped → `Das ist kursiv.` 15 chars = 1.15 s, ratio 0.38; raw length would be 29 |
| 4 | 00:00:09.200 --> 00:00:10,000 | `Ende.` | fits | mixed `.`/`,` separators in one line must parse; 5 chars = 0.38 s in 0.80 s slot (ratio 0.48) |

Timestamp variants in edge.srt: cue 1 mixes `,` and `.` (`00,000 --> 03.500`); cues 2 and 3 use comma only; cue 4 mixes both separators in one line (`09.200 --> 10,000`).

Cue timings unchanged: no fixture ratio falls in the 1.12–1.15 band, so the limit change alters no status.

## ok2.srt

Cues 1 and 2 of fit3.srt only: `[fits, atempo]`, summary ok (CLI exit 0).

## voice.vtt

Speaker tags and script edge cases for `sync import`:

| # | text | expected |
|---|------|----------|
| 1 | `<v Anna Müller>Hallo</v>` | speaker `Anna_Müller` (label "Anna Müller"), text `Hallo` |
| 2 | `<v.loud Bernd>Guten Tag, Anna.` | speaker `Bernd` (class ignored) |
| 3 | `#1 ist die Antwort.` | no `<v>`: fallback speaker; leading `#` stays text (`ERZ|0|#1 …` is no comment) |
| 4 | (no text) | empty cue: `[pause 0s]` in script.txt, silence in its slot, no TTS |
