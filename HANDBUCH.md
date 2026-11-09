# Funkhaus – Handbuch

## Was ist Funkhaus?

Funkhaus ist dein eigenes, lokales NotebookLM. Du sammelst Quellen (Dokumente, Ordner, Webseiten, YouTube-Links), stellst Fragen dazu und bekommst Antworten mit Belegen. Daraus werden Zusammenfassungen, FAQs und ein deutscher Zwei-Stimmen-Podcast.

- **Alles läuft auf deinem Mac.** Quellen verlassen den Rechner nicht.
- Die KI kann sich irren. Prüfe Zitate, bevor du dich darauf verlässt.

## App starten

1. **Funkhaus** über den Schreibtisch-Alias öffnen.
2. Im Hintergrund muss das Sprachmodell **gemma-serve** laufen. Wenn Antworten ausbleiben, siehe „Typische Probleme“.
3. Du landest in der **Notebook-Liste**.

## Notebook anlegen

1. Im Feld „Name des neuen Notebooks“ einen Namen tippen, z. B. `Kunde Müller`.
2. **+ Neues Notebook** klicken (oder Enter).

Ein Notebook ist ein Ordner für ein Thema oder einen Kunden. Quellen, Chat, Notizen und Podcasts bleiben darin getrennt. Löschen: **Löschen** → **Wirklich löschen**.

## Quellen hinzufügen

Die linke Spalte heißt **Quellen**.

- **+ Dateien…** wählt eine oder mehrere Dateien, **+ Ordner…** nimmt einen ganzen Ordner (inkl. Unterordner).
- Unterstützt: `.md`, `.txt`, `.pdf`, `.srt`, `.vtt`, `.html`, `.htm`, `.docx`.
- **Drag & Drop:** Dateien oder Ordner aus dem Finder in die linke Spalte ziehen.
- **Webseiten und YouTube:** Adresse (beginnt mit `https://`) ins Feld einfügen, **Laden** klicken.

**Was bei Videos passiert:** Funkhaus holt nur Text: Titel, Beschreibung, Kapitel und Untertitel. Das Video wird nicht heruntergeladen. Manuelle Untertitel gehen vor automatischen, Originalsprache vor Deutsch vor Englisch.
**Ohne Untertitel** gibt es nur die Beschreibung, mit der Warnung „kein Transkript – nur Beschreibung“. Antworten dazu sind dann dünn.

**Quellen verwalten:**
- Häkchen: an/aus. Nur aktive Quellen fließen in Antworten ein.
- Klick auf den Namen: zeigt den extrahierten Text.
- Rechtsklick: Quelle entfernen.
- **Kartieren**: geht lange Quellen einmal durch, für genauere Antworten und Podcasts. Im Terminal: `funkhaus map <nb>` (mit `--fast` für das kleine Modell, `--source <id>` für eine Quelle).

## Fragen stellen und Belege lesen

1. Frage unten im Chat eintippen, **Fragen** klicken (oder Enter).
2. Die Antwort enthält Zitate wie **[1]**, **[2]**. Ein Klick öffnet den Originalausschnitt.
3. Bei Videos und Audio steht ein **Zeitstempel** im Zitat, z. B. `[3] Interview · s3#2, ab 12:40`.

Steht nichts Passendes in den Quellen, sagt Funkhaus: „Das steht nicht in den Quellen.“
**Fragen vorschlagen** (💡) liefert drei Fragen zu deinen Quellen.

## Notizen

Unter jeder Antwort: **📌 Als Notiz speichern**. Die Notiz landet rechts unter **Notizen**. Klick zeigt sie, **✖** löscht sie.

## Dokumente (Zusammenfassung, FAQ …)

Rechts im Bereich **Studio** unter **Dokumente** (nur aktiv, wenn Quellen da sind):

- **Zusammenfassung**: Überblick über alle Quellen
- **FAQ**: häufige Fragen mit Antworten
- **Briefing**: Kurzbriefing für Entscheidung oder Gespräch
- **Lernhilfe**: Fragen und Kernbegriffe
- **Zeitleiste**: Ereignisse in zeitlicher Reihenfolge

Das Dokument öffnet sich im Fenster. **Im Finder** zeigt die Markdown-Datei im Ordner `out/`.

- **Quellenübersicht**: eine Übersicht pro Quelle mit Verweisen.

## Podcast erzeugen

Im Studio-Bereich:

1. **Fokus** (optional): z. B. `nur die Kosten`. Leer lassen für alles.
2. **Länge** wählen (Standard in der App: kurz):
   - **kurz**: ca. 1,5 Min Audio
   - **mittel**: ca. 5 Min
   - **lang**: ca. 12 Min
3. **Podcast erzeugen** klicken.

**Dauer:** Die Erzeugung läuft im Hintergrund, du kannst weiterarbeiten. **kurz** braucht etwa **5 bis 8 Minuten**, mittel und lang entsprechend länger. Nie mehrere Podcasts gleichzeitig starten, die Grafikkarte wird sonst zu voll. Läuft gerade ein anderer Job auf der Grafikkarte (Frage, Dokument, Kartieren), zeigt Funkhaus **„wartet auf GPU …“** und startet automatisch, sobald sie frei ist.

**Ergebnis** in der Liste **Podcasts**:
- **▶ Abspielen** / **⏹ Stopp**
- **Skript**: Text, wer was sagt (standardmäßig Oskar und Fable, Namen und Stimmen sind einstellbar, siehe „Stimmen und Aussprache“)
- **Im Finder**: zeigt die MP3
- **⚠ Hinweise im Skript**: Skript prüfen. Zahlen darin könnten nicht aus deinen Quellen stammen.

**Erneut rendern:** erscheint, wenn der Podcast ohne MP3 abgebrochen ist. Fertige Teile werden wiederverwendet.

**Drei-Stufen-Podcast:** für sehr lange Quellen. Erst Gliederung, dann Skript pro Abschnitt, dann Stimme.

### Lautheit

Die MP3 wird auf ein Ziel normalisiert (Terminal: `--loudness`, sonst `loudness` in `config.toml`). Gemessen liegt jedes Preset innerhalb ±0,5 LU:

| Preset | Ziel | True Peak | Einsatz |
|---|---|---|---|
| `podcast` (Standard) | −16 LUFS | −1,5 dBTP | Podcast-Apps |
| `hoerbuch` | −19 LUFS | −3 dBTP | Hörbuch, leise Umgebung |
| `video` | −14 LUFS | −1 dBTP | Video-Ton |

### KI-Hinweis

Am Ende spricht die Stimme einen Hinweis, dass die Stimmen künstlich erzeugt sind (Text: `disclosure_text` in `config.toml`). Zusätzlich steht er als ID3-Kommentar samt Tag `AI_GENERATED` in der MP3. Weglassen nur im Terminal: `--no-disclosure` (lässt den gesprochenen Satz weg). Steht der Satz schon als letzte Skriptzeile, wird er nicht doppelt gesprochen.

### Kapitel

Marker `# SEG n | Titel | Ziel` im Skript ergeben Kapitel. Neben der MP3 liegen `chapters.txt` (Format `HH:MM:SS<TAB>Titel`, z. B. für YouTube), `chapters.json` und `shownotes.md`; die Kapitel stehen auch in der MP3 (ID3). Mit Jingle verschieben sich die Zeiten um den Vorlauf.

### Skript bearbeiten: Pausen, Tags, Cache

Terminal: `funkhaus podcast <nb> --script-only`, `script.txt` ändern, dann `--from-script <pfad>`.

- **Pausen:** `[pause]` (0,35 s), `[pause 500ms]`, `[pause 1.5s]`. Maximal 10 s, sie sind echte Stille.
- **Nicht-verbale Tags** (nur diese 13, andere eckige Klammern sind Prüffehler): `[laughter] [sigh] [confirmation-en] [question-en] [question-ah] [question-oh] [question-ei] [question-yi] [surprise-ah] [surprise-oh] [surprise-wa] [surprise-yo] [dissatisfaction-hnn]`. Nie `[laughter]` in zwei Zeilen hintereinander.
- **Cache nach Zeileninhalt:** Ändert, löscht oder fügt man eine Zeile ein, wird nur diese Zeile neu gesprochen, die anderen Stimmen bleiben gleich. Nur eine Pause ändern braucht keine Sprachausgabe. Stimme, Sprache oder Qualität ändern rendert alles neu. `--fresh` ignoriert den Cache.
- **Eine Zeile neu würfeln:** klingt eine Zeile schlecht, `funkhaus reroll <nb> <N>` (N = Zeilennummer; `--dir <ordner>` oder `latest` für den neuesten Podcast). Nur diese Zeile bekommt einen neuen Anlauf, der Rest kommt aus dem Cache. Identische Zeilen teilen sich einen Take (Neuwürfeln wirkt auf alle). Lautheit, Jingle/Bett und Stimmen des Originals bleiben erhalten (`render.json`). `--undo` stellt die vorherige Version wieder her.

## Musik (Jingle und Musikbett)

Intro, Outro und Bett erzeugt **acestep** lokal, einmal (nicht pro Folge).

- `funkhaus music make` rendert nach `<Datenordner>/music/`. Unverändertes wird übersprungen (`--force` erzwingt), läuft unter dem GPU-Lock. `funkhaus music show` listet Dateien, Dauer und Cache-Schlüssel.
- `funkhaus podcast <nb> --jingle`: Intro und Outro (Fehler, wenn die Dateien fehlen). `--no-jingle` schaltet aus. Ohne Flag wird der Jingle genutzt, sobald die Dateien existieren.
- `--bed`: Musikbett unter der Sprache, in Sprechpausen lauter, beim Sprechen abgesenkt (Ducking).
- Einstellungen im Block `[music]` der `config.toml`: Pegel (`bed_db`, `duck_db` …), Tempo und Tonart je Teil (`intro_bpm`, `intro_key` …) und `[music.captions]` (Beschreibung des Klangs, Englisch, mindestens 250 Zeichen, keine Zahlen). Vier fertige Stile (ruhig-warm, modern-elektronisch, akustisch-folk, seriös-nachrichtlich) stehen in `docs/m3/music-captions.md`. Nach Änderungen `funkhaus music make` erneut ausführen.

## Stimmen und Aussprache

**Hosts:** in `config.toml` je Sprecher ein Block mit Anzeigename und Stimme:

```toml
[hosts.o]
name = "Oskar"
persona = "gt"
[hosts.f]
name = "Fable"
persona = "fable"
```

Die alten Felder `[tts] voice_o/voice_f/host_o/host_f` werden noch gelesen, mit Warnung. Für einen Lauf: `--voices gt,fable`.

**Personas** liegen unter `<Datenordner>/voices/` (eine TOML-Datei je Stimme): `funkhaus voice ls`, `show <name>`, `add <name> --kind builtin|design|clone …`, `test <name> "Satz"` (rendert einen Satz, braucht die GPU).
- **Klonen nur mit Einwilligung.** Ein Klon braucht `--consent-who`, `--consent-statement` und Datum; ohne Einwilligung wird er abgelehnt. Herkunft, Einwilligung und der SHA-256 des Referenz-Audios werden mit der Persona gespeichert; die Einwilligung ist an dieses Audio gebunden – wird die Datei ersetzt, verweigert Funkhaus das Rendern ("Referenz-Audio geändert"), und die Stimme muss neu angelegt werden. Klone ohne gespeicherten Hash werden ebenfalls abgelehnt. Wird die Einwilligung widerrufen (`revoked` mit Datum), weigert sich Funkhaus, mit dieser Stimme zu rendern.

**Aussprache-Lexikon** je Notebook (`notebooks/<id>/lexicon.tsv`): `funkhaus lex <nb> ls`, `add <wort> <aussprache>` (setzt oder ersetzt), `rm <wort>`. Beispiel: `funkhaus lex kleinholm add Haffrot "Haff-root"`. Nach einer Änderung werden nur die betroffenen Zeilen neu gesprochen.

**Grafikkarte:** LLM, Stimme und Musik teilen sich eine Sperre (`.gpu.lock` in `~/Library/Application Support/funkhaus/`, gilt für alle Funkhaus-Prozesse). Kurze Anfragen wie Fragen haben Vorrang vor langen Renderläufen.

## Hell / Dunkel

Oben rechts **Darstellung**: **🖥 System** (Standard, folgt dem Mac), **☀ Hell**, **🌙 Dunkel**. Die Wahl wird gespeichert.

## Datenschutz

- **Alles lokal:** Sprachmodell (gemma-serve) und Stimme (omnivoice) laufen auf dem Mac. Funkhaus schickt keine Quelltexte ins Netz.
- **Ausnahmen:** Webadressen, die du selbst eingibst, werden abgerufen.
- **Claude (extern) erlauben:** unter **⚙ Einstellungen** im Studio (im Terminal: `funkhaus nb backend <nb> claude`). Ist es an, gehen die Quelltexte **dieses Notebooks** an Anthropic. Standard ist aus (`local`). Bei Kundendaten aus lassen.

**Gespeichert wird unter** `~/Library/Application Support/funkhaus/`:
- `config.toml`: Einstellungen
- `notebooks/<id>/`: `notebook.json` (Name, Quellen), `sources/` (Text der Quellen), `chat.jsonl` (Verlauf), `notes/`, `out/` (Dokumente, Podcasts)

Ordner öffnen: **⚙ Einstellungen → Datenordner öffnen**.

## Typische Probleme und Lösung

| Problem | Lösung |
|---|---|
| „Gemma (GPU) ist abgestürzt“ oder Antworten bleiben aus | Im Terminal `gemma-serve stop && gemma-serve start`, dann Frage erneut stellen. |
| YouTube-Quelle hat nur die Beschreibung | Video hat keine Untertitel. Transkript selbst als `.txt` oder `.srt` hochladen. |
| Meldung „wartet auf GPU …“ bleibt lange stehen | Ein anderer Job hält die Grafikkarte. Warten, oder laufende Aufgabe prüfen. Bei Stillstand siehe „abgestürzt“ oben. |
| App zeigt alte Version oder spinnt | **Cmd+Q** (ganz beenden), dann neu öffnen. |
| Webseite liefert nur Cookie-Abfrage | Text der Seite in eine `.txt` kopieren und als Datei hinzufügen. |
| Podcast hat „⚠ Hinweise im Skript“ | Skript lesen, Zahlen prüfen, ggf. Fokus ändern und neu erzeugen. |

## Befehlszeile (kurz)

Alles geht auch im Terminal mit `funkhaus` (Ausgabe als JSON, Fortschritt auf stderr). Die fünf wichtigsten:

```bash
funkhaus nb new "Kunde Müller"
funkhaus src add kunde-muller ~/Dokumente/angebot.pdf ~/Projekte/notizen/ https://example.org/artikel
funkhaus ask kunde-muller "Was kostet das Angebot?"
funkhaus make kunde-muller faq
funkhaus podcast kunde-muller --len kurz --focus "Kosten"
```

Weitere Befehle:
- `funkhaus src list|show|toggle|rm` (Quellen an/aus, Text zeigen, entfernen), `funkhaus suggest` (drei Fragen)
- `funkhaus chat <nb>` (Verlauf, `--clear` löscht ihn), `funkhaus chunk <nb> s1#3` (Text zu einem Zitat)
- `funkhaus note add <nb> --last` (letzte Antwort als Notiz), `funkhaus note list|rm`
- `funkhaus podcasts <nb>` (bisherige Podcasts), `funkhaus podcast <nb> --script-only` (nur Skript), `funkhaus reroll <nb> <N>` (Zeile neu würfeln)
- `funkhaus music make|show`, `funkhaus lex <nb> ls|add|rm`, `funkhaus voice ls|show|add|test`
- `funkhaus map <nb>` (Kartieren, LLM, zwischengespeichert)
- `funkhaus eval <nb> --set <datei>` (Retrieval-Test gegen Gold-Zitate, ohne LLM; Entwickler-Werkzeug)

Hilfe: `funkhaus --help` bzw. `funkhaus <befehl> --help`. Prüfen, ob alles läuft: `funkhaus doctor`.
