# Stimmwerk Cue-Edge-Fixtures (Etappe 8)

Synthetisch, keine personenbezogenen Daten, keine Kundendaten. Kein GPU, kein acestep-Aufruf.
Schema: docs/stimmwerk/etappe8-card.md §2 (Lint), §3 (Mix), §11 (Tests 8–27).

## Gemeinsame Annahmen
- Skript: `fixtures/stimmwerk/hoerspiel/script.txt`, N = 25 Datenzeilen (Overlays mitgezählt: Index 5, 16, 23).
- Zeilen-Index 1-basiert über `Script::lines()`. Index 18 = "Die Tür ließ sich öffnen" (`starts[17]`).
- `starts`, `end` aus `mix::timeline(lines, lens, sr)`; Testlängen `lens` aus Tabelle im Test (Fake-TTS).
- Alle `assets/…`-Dateien existieren nur im Test (tempdir, generierte WAVs), außer Fälle 07 (bewusst fehlend) und 02/08 (Lint bricht vorher ab).
- `[music]`: duck_db -12, thresh -50, attack 20, release 800, hold 300, crossfade 2000 ms (Loop-Mindestquelle > 4 s).
- Sample rate Projekt = 24 kHz.

## Fälle

| # | Datei | Fall | Erwartetes Lint-Ergebnis | Timeline-/Mix-Assertion |
|---|---|---|---|---|
| 01 | 01-at-line.toml | cue at line (gültige Baseline) | 0 Fehler, 0 Warnungen (Datei 12 s, sr 24 kHz) | `starts[0] == 0`; erstes Nicht-Null-Sample der Musikspur bei 0; Fade-in linear 0→1 über 2 s; Cue-Ende = 0 + 12 s ±1 Sample; Duck −12 dB ±1 dB bei +150 ms unter Sprache (T5); Hold 300 ms |
| 02 | 02-at-sec.toml | cue at seconds (`at_sec` = 12.0) | Fehler: unbekanntes Feld `at_sec` (`deny_unknown_fields`); Stage 8 nur `at_line` (C1) | Projekt-Load = Err; kein Mix, kein Render |
| 03 | 03-loop-past-end.toml | `loop_until_line = 27` (> N+1 = 26) | Fehler: `loop_until_line` > virtuelles Ende 26 | Lint = Err. Falls erzwungen gerendert: Cue-Ende = `starts[25]` (virtuell 26), geclampt auf Mixlänge; Loop-Naht-Diff unter Schwelle |
| 04 | 04-overlap-ducked.toml | zwei überlappende `duck = true` Cues (Zeilen 8–9) | Warnung: überlappende Duck-Cues (Doppel-Duck). Keine Fehler | Überlappungsfenster = `[starts[7], starts[9])`. Jeder Cue hat eigene Duck-Hüllkurve, −12 dB ±1 dB bei +150 ms (Summe-Pegel im Fenster um beide Dips reduziert). Wortlücke < 300 ms löst Duck nicht |
| 05 | 05-fade-longer-than-cue.toml | `fade_in + fade_out` = 16 s > Cue-Länge 10 s | Fehler: Fades länger als Cue | Lint = Err. Falls erzwungen: linear Fade-in bis 8 s, Fade-out ab 2 s, kein Abbruch durch Überlappung (Mix nicht NaN) |
| 06 | 06-gain-out-of-range.toml | `gain_db = -75.0` (|gain| > 60) | Fehler: gain_db außerhalb ±60 | Lint = Err. Gain-Faktor wäre 10^(−75/20) ≈ 1.8e-4 (nicht gerendert) |
| 07 | 07-missing-file.toml | Datei `assets/music/intro_not_generated.wav` fehlt | Fehler: Datei fehlt (`exists`-Check); keine weiteren Fehler für diesen Cue | Lint = Err. Kein Mix-Lauf |
| 08 | 08-unknown-line.toml | `at_line = 26` (N+1) | Fehler: `at_line` außerhalb 1..=25 (virtuelles Ende gilt nur für `loop_until_line`) | Lint = Err. Kein Start-Sample definierbar |
| 09 | 09-music-shorter-than-10s.toml | music-Datei 6 s (< acestep-Minimum 10 s) | Keine Fehler; Warnung "kürzer als 10 s" (Vorschlag, C5 offen) | Cue-Ende = `starts[0] + 6.0 s` ±1 Sample (kein Trim, kein Loop); Fade-out 1 s endet bei 6 s; kein Stille-Padding |

## Hinweise
- 02 und 08: Lint bricht bei erstem Fehler nicht ab? Erwartung: alle Fehler sammeln; Test prüft nur Vorhandensein des jeweiligen Fehlers.
- 07 bleibt bis Import/Generierung rot (hoerspiel/README.md Erwartung analog).
- 09 Abnahme hängt an Entscheidung C5 (Trim-Feld `trim_s` vs. nur Dateilänge).

## Offene Fragen
1. Q1 (C1): `at_sec` in Stage 8 wirklich hart abweisen (Fall 02), oder Warnung + ignorieren?
2. Q2: `loop_until_line` = N+2 (Fall 03) Fehler oder Clamp mit Warnung? Card sagt Fehler (§2).
3. Q3 (C5): Kurze Musik-Cues (Fall 09) per Warnung oder Trim-Feld `trim_s`? Derzeit nur Warnung als Vorschlag.
4. Q4: Duck-Doppelüberlappung (Fall 04) nur Warnung, oder Summen-Dip begrenzen (z. B. max −12 dB)? Card §2 sagt Warnung.
5. Q5: Fall 01 setzt sr 24 kHz voraus. Stimmt die Projekt-Rate im Test-Setup?
