# Stimmwerk Hörspiel-Fixture (Etappe 9)

Synthetisch, keine personenbezogenen Daten. Kein GPU-Lauf nötig: Tests mit Fake-TTS und generierten WAVs (tempdir).
Ergänzt `demo.studio-script.txt` (alte Syntax mit `# CUE`-Zeilen, bleibt unverändert; wird von `script.rs` als Notiz gelesen).

## Dateien
- `script.txt`: 25 Datenzeilen, 3 `# SEG` (Der Hafen 1–8, Der Fund 9–17, Die Treppe 18–25). Overlays: Zeile 5 `+Lea` (auf Otto 4), Zeile 16 `+Lea` (auf Otto 15), Zeile 23 `+Lea` (auf Otto 22).
- `project.toml`: format=1, Besetzung, `[music]`, 2 `[[cues]]`. Schema geprüft gegen `crates/stimmwerk-core/src/project.rs` (`Meta`, `deny_unknown_fields`) und `cues.rs` (`Cue`, `MusicCfg`, `Kind`).
- Keine `assets/`: Dateien werden im Test erzeugt bzw. sind Platzhalter (siehe Cues).

## Schema-Abgleich (Stand 2026-10-14, Code-Lesung, nicht ausgeführt)
- `[settings]`: `loudness`, `gap`, `disclosure` sind in `Settings` bekannt.
- `[speakers.*]`: nur `persona` und `label` (`Speaker`). IDs ERZ/Lea/Otto erfüllen `script::id_ok`.
- `[music]`: alle sechs Keys in `MusicCfg` (`project.rs` `Meta.music`, `cues.rs`). Werte = Defaults.
- `[[cues]]`: nur bekannte Keys. `kind` lowercase (`music`/`ambience`/`sfx`). Cue 2 `loop_until_line = 26` ist laut `cues.rs` lint erlaubt (≤ n+1).
- `persona`: `gt`, `kn`, `gerstaecker` stehen in `voice.rs` `KNOWN_BUILTINS`.
- Zeilenbezüge gegen `script.txt` geprüft: at_line 1 = ERZ „Im Hafen“, at_line 18 = ERZ „Die Tür ließ sich öffnen“, Overlays auf 5, 16, 23 bestätigt.

Ergebnis: `project.toml` ist mit dem aktuellen Schema ladbar. Änderungen nur an Kommentaren (`[music]` = Defaults, `loop_until_line` = Ende der Sprache). Keine Keys geändert.

## Besetzung (Figur -> Persona, builtin omnivoice)
- ERZ -> `gt` (Erzähler)
- Lea -> `kn`
- Otto -> `gerstaecker`
- `fable` bewusst nicht genutzt: nicht in omnivoice SKILL.md gelistet (Herkunft unklar).

## Cues (Schema docs/stimmwerk/etappe8-card.md §2)
| # | at_line | Zeile (Start) | Datei | kind | Ende | duck |
|---|---|---|---|---|---|---|
| 1 | 1 | ERZ "Im Hafen" | assets/music/intro.wav (acestep, >= 10 s) | music | Dateilänge | ja (-12 dB via `[music].duck_db`) |
| 2 | 18 | ERZ "Die Tür ließ sich öffnen" | assets/ambience/meer_cc0.wav (CC0-Import) | ambience | loop bis virtuell 26 | nein |

- Cue 2 Datei ist Platzhalter: Quelle (CC0, z. B. Freesound-CC0 Meer) noch nicht gewählt, nicht im Repo. Lint soll "Datei fehlt" melden, bis importiert.
- acestep-Aufruf für Cue 1 (nur Planung, nicht ausführen; `-d` >= 10, `-i` Pflicht, kein `--lyrics`):
  `acestep gen "ruhiges maritimes Intro, Akustikgitarre, Hafenstimmung" -i -d 12 --bpm 70 --key "A minor" --timesig 4 --seed 1 -f wav24 -o raw.wav`
  Alternativ als `[cues.gen]` (prompt, dur 10..=600, seed, bpm, key) statt `file`; dann Cache `takes/music/<key>.wav`.
- Cue-Schema: `at_line` 1-basiert über `Script::lines()` (Overlays mitgezählt). Kein `# CUE` im Skript (Plan §6 Vorschlag 2).

## Timeline-Assertions (für Etappe 9 Tests T1–T9, Muster etappe9-card.md §4)
Fake-TTS-Längen `lens[0..25]` aus Tabelle im Test. Symbolisch:
- T1 Timeline: `starts[i] == mix::timeline(...).0[i]`. Overlay Zeile 5: `start == starts[3] + round((end4 - starts4) * 0.7)`; Zeile 16: Faktor 0.5; Zeile 23: Faktor 0.3.
- T2 Cue 1 Start: erstes Nicht-Null-Sample der Musikspur == `starts[0]` (= 0); Musik-Fade-in 2 s linear.
- T3 Cue 2 Start: erstes Nicht-Null-Sample der Ambience-Spur == `starts[17]` (Zeile 18); davor exakt 0.
- T4 Cue 2 Loop: Ende == `starts[25]` (virtuell 26 = Gesamtende ohne Tail, geclampt auf Mixlänge). Naht-Diff unter Schwelle (`loop_bed_seam_and_len`-Muster).
- T5 Ducking Cue 1: Tiefe unter Sprache bei 150 ms nach Zeilenstart = -12 dB ±1 dB (`duck_depth_within_1db`). Wortlücke < 300 ms (hold) löst Duck nicht (`word_gap_does_not_release_duck`).
- T6 Ducking Cue 2: keine Änderung unter Sprache (duck=false): Ausgabe bit-gleich zu reiner Gain/Fade-Kurve mit/ohne Sprache.
- T7 Kapitel: 3 Einträge, Titel "Der Hafen" / "Der Fund" / "Die Treppe"; Start 0 / ms(starts[8]) / ms(starts[17]); letztes Ende = ms(Gesamtlänge); Rundung `(s*1000 + sr/2)/sr`.
- T8 Manifest: 25 Einträge; Overlay-Zeilen auf Track `speakers.len() + idx(Lea)+1`; keine zwei überlappenden Zeilen auf demselben Track; `file == parts/NN-who.wav`.
- T9 Länge: `mix.len() == total` (Timeline + 0.5 s Tail) ±1 Sample; Cue-Ende geclampt.
- Negativ: `# SEG` als letzte Zeile -> Fehler oder Verwerfen, kein Panic. Cue mit `kind="sfx"` + `loop_until_line` -> lint-Fehler. `at_line = 27` -> lint-Fehler (> N = 25).

## Lint-Erwartung (Test-Projekt, ohne Musikdatei)
- 0 Fehler bei Personas gt/kn/gerstaecker.
- Fehler: `assets/music/intro.wav` und `assets/ambience/meer_cc0.wav` fehlen (bis Import/Generierung).
- Test `cues.rs::hoerspiel_fixture_lint` erwartet genau diese zwei `cue_src`-Fehler.

## Offene Fragen
1. `docs/stimmwerk/r14-etappe9-check.md` (Befund §1 und Frage 5: „`[music]` nicht ladbar“) ist veraltet: `Meta` hat `music` und `cues` seit `project.rs:98-102`. Dokument nicht geändert (außerhalb dieser Datei).
2. `fixtures/stimmwerk/cues/README.md` Fall 02 sagt „unbekanntes Feld `at_sec`“, `Cue` kennt `at_sec` aber (`cues.rs:80-81`). Dort korrigieren, nicht hier.
3. Alte `demo.studio-script.txt` hat `# CUE 20` auf Zeile 20 (Off-by-one, Plan §6 Vorschlag: 19). Hier nicht übernommen.
4. Ambience-Quelle (CC0-Datei) bestimmt Testlänge; Musik-Länge >= 10 s (acestep-Minimum).
5. Bitgleiches Roundtrip-Verhalten gilt nur für `project.rs`-Fixture `leuchtturm.studio`, nicht für dieses Hörspiel-Projekt. Hier nicht geprüft, da kein cargo.
6. Keine `disclosure`-Abweichung getestet; `disclosure = true` (Default) gesetzt.
