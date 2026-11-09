# Stimmwerk Prosa-Fixture (Etappe 7: Prosa → Rollenskript)

Synthetische Testdaten, keine echten Personen, keine fremden Texte. Der Text ist Original.

## Dateien

- `kapitel1.md`: ca. 390 Wörter Prosa, Erzähler + drei Figuren (Mara, Jonas, Ilse). Wörtliche Rede in „…“. Ein Lachen („lachte kurz und trocken“), ein Seufzen („seufzte schwer“), ein Datum („Am 3. Oktober 1964“).
- `kapitel1.expected.txt`: Referenz-Rollenskript im Stimmwerk-Format `ID|GAP|TEXT`, zwei Abschnitte `# SEG 1 | Der Kai` und `# SEG 2 | Die Kiste`.
- `cast.toml`: Figuren mit Beschreibung und OmniVoice-Instruct (nur dokumentierte Werte: gender, age, pitch).

## Konventionen der Referenz

- `ERZ` = Erzähler. Alle Erzählsätze ohne wörtliche Rede gehören zu `ERZ`.
- Wörtliche Rede wird ohne Anführungszeichen übernommen, Wortlaut exakt wie im Prosatext (inkl. Satzzeichen innerhalb der Rede).
- Sprechersatzteile („sagte Ilse“, „fragte Mara“, „Sie las sie leise vor:“) entfallen in der Referenz. Sie sind Attribution, keine gesprochenen Worte. Ein LLM-Output, der sie als `ERZ`-Zeile ausgibt, wird bei der wörtlichen Rede nicht bestraft, verliert aber beim Zeilenalignment.
- Beschreibungen nonverbaler Laute werden zu Tags am Anfang bzw. Ende der Figurenzeile, nicht zu `ERZ`-Zeilen:
  - Lachen (Jonas, Zeile „Ich habe ihn nicht gefunden …“): `[laughter]` am Ende der Zeile.
  - Seufzen (Ilse, „seufzte schwer“): `[sigh]` am Anfang der Zeile.
  - Keine anderen Tags. Ein Tag ohne Vorlage im Prosatext ist ein Fehler.
- Zahlen und Daten bleiben verbatim wie in `kapitel1.md`: `3. Oktober 1964` → `3. Oktober 1964`. Keine Ausschreibung, keine Umformung. TTS-Normalisierung passiert später in omnivoice, nicht in der Referenz.
- GAP-Werte sind Richtwerte und werden nicht bewertet, außer: Hauptzeilen haben GAP im Bereich 0 bis 1.

## Scoring eines LLM-Ergebnisses

Eingabe: `out.txt` (LLM-Output, Stimmwerk-Format). Referenz: `kapitel1.expected.txt`. Figurenliste: `cast.toml`.

### 0. Normalisierung (auf beiden Seiten gleich)

1. Zeilen mit `#` oder leer ignorieren, außer `# SEG`-Zeilen (für Abschnittszahl und Titel).
2. Jede Datenzeile in genau drei Felder am ersten und zweiten `|` teilen. Text ist der Rest.
3. Tags `[laughter]`, `[sigh]`, `[pause …]` aus dem Text entfernen, für den Textvergleich.
4. Kleinschreibung, Satzzeichen entfernen, Leerraum zusammenfassen.

### 1. Format (Pass/Fail)

- Jede Datenzeile hat ID, GAP (Zahl) und Text. Fehlerhafte Zeilen zählen als `bad`.
- Jede ID ist `ERZ` oder in `cast.toml` definiert.
- Anzahl `# SEG`-Zeilen: Referenz 2.
- Ziffern im Text: nur die Datumszeile enthält Ziffern (`3. Oktober 1964`, verbatim). Andere Zeilen mit `[0-9]` sind Fehler.
- Anführungszeichen („ oder “) im Text: Anzahl (Ziel 0, da Rede ohne Anführungszeichen übernommen wird).

### 2. Zeilenalignment

- Sequenzvergleich mit `difflib.SequenceMatcher` oder Needleman-Wunsch über die Liste der Zeilen, Schlüssel = (ID, normalisierter Text).
- Zwei Zeilen gelten als gleich, wenn ID gleich ist und das Wort-F1 des normalisierten Textes ≥ 0,8 ist.
- Ausgabe: `matched / expected` (Recall), `matched / produced` (Precision), `extra`, `missing`.
- Ziel: Recall ≥ 0,9, Precision ≥ 0,9.

### 3. Wörtliche Rede

- Referenzzeilen mit ID ≠ `ERZ`: 9 Stück, je eine pro wörtlicher Rede im Prosatext.
- Für jede Referenzzeile: die zugeordnete LLM-Zeile (aus 2) muss den normalisierten Wortlaut enthalten, Wortfolge identisch.
- Score: `wörtlich / 9` in Prozent. Ziel 100 %.
- Zusätzlich: Ausgelassene Wörter aus der Rede (Anzahl) und hinzugefügte Wörter, die nicht im Prosatext stehen.

### 4. Sprecherzuordnung

- Für alle gematchten Zeilen aus 2: Anteil mit gleicher ID. Ziel 100 %.
- Zusatz-Fehlerklassen:
  - Rede als `ERZ` (Figurenrede falsch zugeordnet).
  - Erzählsatz als Figur (Narration falsch zugeordnet).
  - Zwei aufeinanderfolgende Zeilen derselben ID, die in der Referenz getrennt sind (Zusammenführung oder Aufsplitten).

### 5. Tags

- Referenz hat genau zwei Tags: `[laughter]` bei Jonas, `[sigh]` bei Ilse.
- Tag-Precision und Tag-Recall über die Paare (Zeile, Tag) aus den gematchten Zeilen.
- Tags außerhalb dieser Paare zählen als Fehler (Halluzination).
- Ziel: F1 = 1,0.

### 6. Datum-Check (verbatim)

- Zeile „Am 3. Oktober 1964 …“ muss als `ERZ` vorliegen, Datum wortgleich wie in `kapitel1.md`.
- Vergleich auf Rohtext (vor Schritt 0.4): Substring `3. Oktober 1964` muss exakt enthalten sein. Keine Normalisierung, keine Ausschreibung.
- Bestanden nur bei exaktem Treffer. Ausgeschriebene Form (`dritten Oktober neunzehnhundertvierundsechzig`) oder Teiltreffer = fehlgeschlagen.

## Auswertung (Kurzform)

Ausgabe als Tabelle, je Punkt Zahl und Pass/Fail:

| Prüfung | Ziel |
|---|---|
| Format-Fehlerzeilen | 0 |
| SEG-Zeilen | 2 |
| Ziffern im Text | nur Datumszeile |
| Zeilen-Recall / Precision | ≥ 0,9 / ≥ 0,9 |
| Wörtliche Rede | 100 % |
| Sprecher-Genauigkeit | 100 % |
| Tag-F1 | 1,0 |
| Datum verbatim (`3. Oktober 1964`) | ja |

## Grenzen des Tests

- Ein einzelner Prosatext und eine Besetzung mit drei Figuren. Keine Aussage über Mehrfachsprecher oder Overlays (`+ID`/`B`).
- Die Referenz ist eine Setzung, keine einzige richtige Lösung. Der Segmentierung (wo ERZ und Figur getrennt werden) kann ein LLM anders wählen; das Alignment fängt geringe Abweichungen ab.
- Instruct-Strings in `cast.toml` sind nicht im Hörtest geprüft. Sie dienen nur dem Format-Test.
- Die Regel „Attribution entfällt“ ist eine Entscheidung dieser Fixture. Ein LLM, das Attribution als ERZ-Zeile behält, verliert nicht bei der wörtlichen Rede, wohl aber bei Zeilenalignment.
