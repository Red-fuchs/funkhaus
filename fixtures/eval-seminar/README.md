# Eval: seminar (fiktiv)

Eval-Set zur Fixture `../seminar/seminar.vtt` (Workshop "Achtsam kommunizieren im Pflegealltag", Trainerin Mara Lindqvist). Alle Personen, Orte, Zahlen und Methoden sind erfunden. Keine realen Personen.

Ergänzt `../seminar/QUESTIONS.jsonl` (25 Fragen). Keine Frage und kein Zitat aus dieser Datei wird wiederholt.

## Format `questions.jsonl`

Eine Zeile pro Frage:

- `q`: Frage (Archivnutzer, Deutsch)
- `quote`: wörtlicher Teilstring des zusammengefügten Cue-Textes (Cues mit Leerzeichen verbunden, ohne `<v>`-Tags und Zeitstempel). Leer bei `none`.
- `ts`: Startzeit `HH:MM:SS` des Cues, in dem `quote` beginnt. `null` bei `none`.
- `kind`: `factual` (10), `wo_sagt` (8, Frage "Wo sagt die Trainerin …", Zitat muss von Mara Lindqvist stammen), `oblique` (6, Frage ohne inhaltliche Wortüberlappung mit dem Zitat), `oos` (3, Antwort steht nicht im Material), `outcut` (3, berührt das private Pausengespräch)
- `hard_negative_quote`: verbatim Passage, die ähnlich klingt und falsch wäre. Bei `outcut` stammt sie aus dem Pausengespräch (VTT-Cues zwischen `NOTE outcut:start` und `NOTE outcut:end`, Cue 526–550, 00:46:18–00:48:35).
- `none: true`: nur bei `oos`. Korrekt ist Abstention.
- `avoid_outcut: true`: nur bei `outcut`. `quote` liegt außerhalb des Pausengespräches. Ein Zitat aus dem Pausengespräch zählt als Leak.

Prüfung: Jedes `quote` und `hard_negative_quote` ist im Cue-Text genau einmal enthalten. `ts` ist aus dem VTT abgeleitet. `wo_sagt`-Zitate stammen von der Trainerin. `oblique` hat keine Stämme (5 Zeichen) gemeinsam mit der Frage. Geprüft mit `/private/tmp/verify_seminar_eval.py` (außerhalb des Repos, liest das VTT).

## Scoring

Grundlage: 27 beantwortbare Fragen (alle außer `oos`) und 3 `oos`-Fragen.

- Retrieval, `recall@1`, `recall@3`, `recall@5`: Treffer, wenn einer der Top-k-Chunks `quote` enthält (Whitespace normalisiert, Groß/Klein egal).
- `MRR`: Mittel über 1/Rang des ersten Treffers (0 ohne Treffer), über die 27 beantwortbaren Fragen.
- Zitatzeit: Zitat-`ts` gilt als korrekt, wenn `|ts_zitiert - ts| <= 15 s`. Bei `wo_sagt` ohne Zeitstempel: falsch. Angabe pro `kind`.
- Abstention `oos`: richtig, wenn keine Quelle zitiert wird und explizit gesagt wird, dass die Antwort nicht im Material steht. Zitiert das System eine Passage (auch die aus `hard_negative_quote`), ist es falsch.
- False-Abstention (Rate über die 27 beantwortbaren Fragen): Anteil, bei dem das System abstiniert oder keine Quelle liefert.
- Outcut-Leak-Rate: Anteil der Antworten (alle 30), bei denen mindestens ein Zitat aus dem Pausengespräch stammt, d. h. Zeit im Bereich 00:46:18 bis 00:48:35 oder Text aus `hard_negative_quote` bei `outcut`. Pflicht-Ziel 0 bei `avoid_outcut`. Die Index-Pipeline soll das Pausengespräch vor dem Indexieren entfernen. Ein Leak deutet auf fehlende Entfernung oder fehlerhaftes Ranking hin.
- Hard-Negative-Verwechslung: Anteil der Fragen, deren Top-1-Zitat `hard_negative_quote` enthält. Pro `kind` ausweisen.

Ausgabe pro Lauf: Tabelle mit Metrik, Wert, n. Keine Gesamtpunktzahl.
