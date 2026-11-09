# Stimmwerk Prose-Fixtures (Etappe 7: Prosa → Rollenskript, Set 2)

Ergänzt `fixtures/stimmwerk/prosa/` (Kapitel 1, Referenz im Zeilenformat `ID|GAP|TEXT`) um vier kurze Fälle im JSON-Format `{cast, lines}`, wie in `fixtures/stimmwerk/llm/README.md` („Prose→Script-JSON“) und `docs/stimmwerk/prompts.md` §1 beschrieben.

Alle Texte sind Eigenkompositionen. Keine realen Personen, keine fremden Texte, keine Kundendaten. Namen und Orte sind frei erfunden.

## Dateien

- `p1.txt` / `p1.expected.json`: dialoglastig, 126 Wörter, 2 Figuren (Rike, Lena) + Erzähler.
- `p2.txt` / `p2.expected.json`: nur Erzähler, 171 Wörter, keine wörtliche Rede. `cast` enthält nur `ERZ`.
- `p3.txt` / `p3.expected.json`: 147 Wörter, 3 Figuren (Oskar, Marta, Ben) + Erzähler, Interjektionen (Ach, Hm, Na so was, Oje, Psst), ein `[sigh]`-Tag.
- `p4.txt` / `p4.expected.json`: 174 Wörter, 2 Figuren (Frau Ahrens, Tobias Kern) + Erzähler, Zahlen und Daten: `12. Oktober 2026`, `15. Oktober 2026`, `8:00 bis 12:00 Uhr`, `1.250 Euro`, `14,6`, `18,90 Euro`, `849,50 Euro`, `4471-B`, `M6`, `9:15 Uhr`.

## Format der Expected-Dateien

```json
{"cast": [{"id": "ERZ", "name": "Erzähler", "desc": "..."}],
 "lines": [{"who": "ERZ", "text": "..."}]}
```

- Keine Zusatzfelder (`gap`, `over`, `suggested_voice_instruct`).
- `who` ist immer eine `cast`-ID.
- Eine Zeile pro Satz. Wörtliche Rede ohne Anführungszeichen. Sprecherattribution („sagte Lena“) entfällt, wie in `prosa/README.md`.
- Erzählsätze ohne Rede gehören zu `ERZ`.
- Zahlen und Daten stehen verbatim wie im Prosatext. Keine Ausschreibung.
- Tags nur, wenn der Prosatext den Laut beschreibt (`[sigh]` bei „seufzte schwer“). Das Tag steht am Anfang der Figurenzeile.

## Was ein Scorer prüfen soll

0. Normalisierung wie in `prosa/README.md` §0. Tags vor dem Textvergleich entfernen.
1. Format: JSON parst. Top-Keys sind exakt `cast` und `lines`. Cast-Einträge haben genau `id`, `name`, `desc`. Line-Einträge haben genau `who`, `text`. Jedes `who` ist eine Cast-ID.
2. Cast-Vollständigkeit: Jede Cast-ID kommt in `lines` vor. Ausnahme ist `p2`, dort ist nur `ERZ` vorgesehen.
3. Zeilen-Recall und Precision gegen die Expected-Datei, mit dem Alignment aus `prosa/README.md` §2 (ID gleich, Wort-F1 ≥ 0,8). Ziel ≥ 0,9.
4. Wörtliche Rede: Jede Nicht-ERZ-Zeile enthält den normalisierten Wortlaut der Rede. Ziel 100 %.
5. Sprecher-Genauigkeit: Gematchte Zeilen mit gleicher ID. Ziel 100 %. Besonders prüfen:
   - p2: keine Figurenzeile. Jede erfundene Figur ist ein Fehler.
   - p3: Interjektionen (`Hm`, `Oje`, `Psst`, `Ach`) gehören zur Figur, nicht zu `ERZ`.
   - p1: „Lena lachte kurz“ und „Rike drehte sich um“ sind Erzählung, nicht Rede.
6. Zahlen-Verbatim (p4): Jede Zahl und jedes Datum aus der Expected-Datei kommt als exakter Substring im Output vor. Konkret: `12. Oktober 2026`, `15. Oktober 2026`, `8:00`, `12:00`, `1.250`, `14,6`, `18,90`, `4,50`, `39,50`, `849,50`, `894,50`, `4471-B`, `M6`, `9:15`. Ausgeschriebene Formen oder umgeformte Zahlen sind Fehler.
7. Halluzination: Tags, die nicht im Expected stehen, zählen als Fehler. Erfundene Zeilen zählen als `extra`.
8. Kein Ziffern-Verbot in diesen Fällen. Die Regel „nur Datumszeile enthält Ziffern“ aus `prosa/` gilt hier nicht, p4 testet gerade Zahlen.

## Offene Punkte für den Senior-Coder

- Key-Name `desc` (Auftrag) vs. `description` plus `suggested_voice_instruct` (Schema in `prompts.md` §1). Hier ist `desc` verwendet. Entscheidung treffen. Bei Schema-Vorgabe die Dateien umbauen.
- ID-Stil: Hier GROSS (`RIKE`). `prosa/cast.toml` nutzt `Mara`, `Jonas`. Das Schema erlaubt beides laut Karte (C5). Scorer muss beides akzeptieren.
- Ordner heißt `prose/` (Auftrag). Bestehende Fixture heißt `prosa/`. Zusammenlegen oder beide behalten.
- Attribution entfällt in den Expected-Dateien. Ein LLM, das `sagte X.` als ERZ-Zeile ausgibt, verliert nur beim Alignment, nicht bei der wörtlichen Rede.
- Tag-Abdeckung ist dünn: nur ein `[sigh]` in p3, kein `[laughter]`. Für Tag-F1 reicht das nicht. Ggf. ein fünfter Fall.

---

## Set 3: harte Fälle (p5–p8)

Eigenkompositionen, freie Namen, keine Kundendaten. Gleiches Format `{cast, lines}`, gleiche Regeln wie oben (Attribution entfällt, `who` ist Cast-ID, Zahlen verbatim).

- `p5.txt` / `p5.expected.json`: Schilder, Durchsage und Zettel stehen in »…«, sind aber keine Figurenrede. Sie gehen zu `ERZ`. Eine Figur (`JULE`) spricht zweimal. 95 Wörter, 12 Zeilen.
- `p6.txt` / `p6.expected.json`: eine lange Rede von `HANNE` in einem »…«-Block mit fünf Sätzen. Erwartet: fünf `HANNE`-Zeilen, eine pro Satz. Danach `TOM` mit einem Satz. 73 Wörter, 8 Zeilen.
- `p7.txt` / `p7.expected.json`: unbenannte Sprecher. `MANN` („der Mann“) und `STIMME` („Stimme aus dem Nebenzimmer“) sprechen mehrfach und bekommen Rollen-IDs. Die einmalige Rufe-Zeile „eine Frau“ ist `ERZ`. 90 Wörter, 12 Zeilen.
- `p8.txt` / `p8.expected.json`: Abkürzungen und Dezimalzahlen: „Dr. Kern“, „z. B.“, „S. 12“, „Nr. 4“, „Nr. 7“, „ca. 20“, „3,5 Grad“, „0,75“, „1,20“. Kein Satzende an diesen Punkten. 104 Wörter, 11 Zeilen.

### Was der Scorer für p5–p8 prüfen muss

- **p5 (Schild → ERZ):** Jede Zeile mit Schild-, Durchsage- oder Zettel-Wortlaut muss `ERZ` sein, auch wenn der Text in »…« steht. Fehler, wenn `JULE` diese Zeilen bekommt. Erwartete `JULE`-Zeilen: nur „Wie lange noch?“ und „Das ist ja ein Witz.“. Der Text „Achtung, der Aufzug …“ bleibt in `ERZ`, nicht gesplittet.
- **p6 (Langrede):** Zahl der `HANNE`-Zeilen = 5 (exakt). Jede Zeile ist ein vollständiger Satz, wortgleich. Zusammenlegung zu einer Zeile = 4 `missing` plus 1 `extra`, zählt als Alignment-Fehler. Satzzeichen innerhalb des Satzes („Die Miete …, und zwar …“) darf nicht zum Split führen.
- **p7 (unbenannte Sprecher):** Keine erfundenen Eigennamen. `name` für `MANN` und `STIMME` darf nur die Rollenbeschreibung sein. Die Einmal-Rede „Von der Treppe rief eine Frau …“ ist `ERZ`. Kein `FRAU`-Cast-Eintrag. Die Wörter „der Mann“ im Erzähltext sind keine Figurenzeile.
- **p8 (Abkürzungen, Dezimalzahlen):** Sieben Vorkommen von fünf Abkürzungen (`Dr.` ×2, `z. B.`, `S.`, `Nr.` ×2, `ca.`) dürfen keinen Satz beenden. Splitter-Liste oder Regex muss sie kennen. Anzahl Zeilen = 11. Jede Zeile ist wortgleich. Dezimalzahlen bleiben verbatim: `3,5` darf nicht `3.5` oder `3 Komma 5` werden. Die Ziffern-Regel aus Set 1 gilt hier nicht.
- **Alle vier:** Kein Tag im Output, der nicht im Expected steht. Keine Zeilen mit leerem `text`.

### Offene Punkte (Set 3)

- Regel für unbenannte Sprecher: Die Fixture nimmt an, dass wiederkehrende unbenannte Sprecher eine Rollen-ID bekommen und einmalige unbenannte Rede zu `ERZ` geht. Das ist eine Annahme. Entscheidung von the owner nötig, bevor der Scorer das als hart prüft.
- Schild/Durchsage vs. Sprechanlage: p5 legt Durchsagen und Schilder zu `ERZ`. Eine Person, die über eine Sprechanlage zu jemandem spricht, ist dagegen Figur (p7 mit „Nebenzimmer“). Grenzfall: eine automatische Ansage mit Stimme. Hier nicht getestet.
- p5–p8 sind kürzer als Set 1 (73–104 gegen 126–174 Wörter). Längere Varianten nur bei Bedarf.
- Tag-Abdeckung bleibt dünn: kein `[laughter]` in Set 3.
