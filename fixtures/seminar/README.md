# Fixture: seminar (fiktiv)

Testfixture für Funkhaus (lokales NotebookLM). Alle Personen, Orte, Zahlen, Methoden und Dokumente sind erfunden.

- Workshop: "Achtsam kommunizieren im Pflegealltag" (eintägig, fiktiv)
- Trainerin: Mara Lindqvist
- Teilnehmende (nur Vornamen): Jonas, Selma, Thorsten, Ilse
- Fiktive Methoden: Drei-Spiegel-Methode, Kiesel-Modell, Wegweiser-Raster

## Inhalt

- `seminar.vtt`: WebVTT, ca. 90 min, 1020 Cues (3 bis 8 s), Sprecher über `<v ...>`. Sechs Blöcke: Begrüßung, Theorie Wahrnehmung, Übung Rollenspiel, Pause und Technik-Störung, Fallbesprechung, Abschluss. Enthält eine Technik-Störung (Mikrofon) und ein Pausengespräch.
- `NOTE outcut:start` / `NOTE outcut:end` markieren ein privates Pausengespräch, das vor dem Indexieren entfernt werden soll. Die Cues dazwischen sind im VTT enthalten.
- `QUESTIONS.jsonl`: 25 Testfragen. `kind` ist `direct` (10, Wortüberlappung mit dem Zitat), `oblique` (10, keine inhaltlichen Wörter gemeinsam mit dem Zitat) oder `global` (5, Fallbeispiel insgesamt). Jedes `quote` ist ein wörtlicher Teilstring des zusammengefügten Cue-Textes (Cues mit Leerzeichen verbunden, ohne Tags und Zeitstempel).

## Erzeugung

Der Text wurde als Dialog-Quelle von Hand verfasst. Ein kleines Python-Skript (`gen.py`) liegt außerhalb des Repos im Scratchpad und erzeugt daraus die VTT-Datei. Zeitstempel: Dauer je Cue = Zeichenzahl / 12,5, begrenzt auf 3 bis 8 s, Lücke 0,25 s. Lange Äußerungen werden an Satzgrenzen in Cues von höchstens 95 Zeichen zerlegt.

Prüfung: `verify.py` prüft, dass alle Zitate im Cue-Text enthalten sind, die Typenverteilung 10/10/5 stimmt und bei `oblique` keine inhaltlichen Wörter überlappen.

Neuerzeugung: `python3 gen.py && python3 verify.py` im Scratchpad-Verzeichnis (nicht im Repo).
