---
name: funkhaus
description: Local NotebookLM via the `funkhaus` CLI - notebooks of sources (md, txt, pdf, srt/vtt, html, docx, URLs, folders), cited Q&A over them, summaries/FAQ/briefing/study guide/timeline, and German two-host podcasts ("Projekt-Funk" style, omnivoice TTS, MP3). Use for "Notebook", "Quellen fragen", "Audio-Zusammenfassung", "Podcast aus Dokumenten", "Funkhaus", "fasse die Quellen zusammen", the client seminar archives as audio. Fully local (gemma-serve), CLI only, no MCP.
---

# funkhaus — lokales NotebookLM

Binary `~/.local/bin/funkhaus` (source `~/Projects/funkhaus`, rebuild + app: `scripts/build-app.sh`). GUI: `~/Applications/Funkhaus.app`.
**Stand 2026-10-13:** installiertes Binary + App = v1 (Tag `v1-gut`). M3-Funktionen (`reroll`, `music`, `lex`, `voice`, `--loudness`, `--jingle/--bed`, `--no-disclosure`, `--fresh`, Tags/`[pause]`) gibt es nur in `~/Projects/funkhaus/target/release/funkhaus`, bis the owner den neuen Build freigibt. Im Zweifel `funkhaus --help` prüfen.
stdout = one JSON object `{"ok": true, "result": …}` or `{"ok": false, "error": …}`; progress on stderr. Exit 0 ok, 1 failed, 2 usage.
Data: `~/Library/Application Support/funkhaus/` (`config.toml`, `notebooks/<id>/{notebook.json,sources/,chat.jsonl,notes/,out/}`); override with `FUNKHAUS_HOME`.
NB = notebook id, exact name, or unique id prefix. SRC = source id (`s1`) or exact title.

```bash
funkhaus doctor                                   # LLM endpoint(s), omnivoice, ffmpeg
funkhaus nb new "Kunde X" | nb list | nb show NB | nb rm NB
funkhaus src add NB file.pdf notes/ https://example.org/artikel   # files, folders (recursive), URLs
funkhaus src list NB | src show NB SRC | src rm NB SRC | src toggle NB SRC on|off
funkhaus ask NB "Frage"            # → text with [n] + cites[{n, chunk:"s1#0", src, title, excerpt}]
funkhaus chunk NB s1#0             # full text behind a citation
funkhaus suggest NB                # three suggested questions
funkhaus chat NB [--clear]
funkhaus note add NB "Text" [--title T] | note add NB --last | note list NB | note rm NB ID
funkhaus make NB summary|faq|briefing|studyguide|timeline        # → out/<kind>-<ts>.md (+ Quellen list)
funkhaus podcast NB [--focus "…"] [--len kurz|mittel|lang] [--voices gt,fable] [--lang de] [--script-only] [--from-script s.txt] [--loudness podcast|hoerbuch|video] [--no-disclosure] [--fresh] [--jingle|--no-jingle] [--bed]
funkhaus reroll NB N [--dir D|latest] [--undo]   # re-roll line N (new seed), rest from cache
funkhaus music make [--force] | music show        # jingle + bed via acestep, once → <HOME>/music/
funkhaus lex NB ls | add KEY SPEAKABLE | rm KEY   # pronunciation lexicon (notebooks/<id>/lexicon.tsv)
funkhaus voice ls | show NAME | add NAME --kind builtin|design|clone … | test NAME "Satz"
funkhaus podcasts NB               # previous podcast dirs
```

## Podcast
- Output dir `out/podcast-<ts>/`: `script.txt` (`WER|GAP|TEXT`; O/F hosts, B = backchannel over previous line at fraction GAP), `<hash>.wav` (one take per distinct line text+voice), `rerolls.json` (alternative takes, `funkhaus reroll`), `render.json` (voices + loudness/jingle/bed/disclosure used; reruns and rerolls in the dir reuse it unless overridden), `mix.wav`, `podcast.mp3`, `chapters.txt` (`HH:MM:SS<TAB>Titel`, from `# SEG n | Titel | Ziel` markers), `chapters.json`, `shownotes.md`.
- Loudness `--loudness` (else config `loudness`): podcast −16 LUFS/TP −1.5 (default), hoerbuch −19/−3, video −14/−1; measured ±0.5 LU.
- AI disclosure: spoken closing sentence (`disclosure_text`) + ID3 comment + `AI_GENERATED=true`; `--no-disclosure` drops the spoken line.
- Script tags: 13 non-verbal only (`[laughter] [sigh] [confirmation-en] [question-en|ah|oh|ei|yi] [surprise-ah|oh|wa|yo] [dissatisfaction-hnn]`; any other bracket = check error; no `[laughter]` twice in a row). Pauses `[pause]` (0.35 s), `[pause 500ms]`, `[pause 1.5s]`, max 10 s; pauses never hit TTS.
- Cache by line content: edit/insert/delete a line → only that line re-renders (seed derives from content). Voice/lang/quality change → all. `--fresh` ignores cache. Lexicon change → only affected lines.
- Music: `--jingle` (error if files missing), `--no-jingle`, `--bed` (ducked under speech). Without flags the jingle is used only if the files exist. Config `[music]` (`bed_db`, `duck_db`, `*_bpm`, `*_key`, `[music.captions]`); captions English, ≥250 chars, no digits; sets in `docs/m3/music-captions.md`.
- Hosts: config `[hosts.o]` / `[hosts.f]` (`name`, `persona`; default Oskar=gt, Fable=fable). Legacy `[tts] voice_o/voice_f/host_o/host_f` still read, with warning. `--voices` overrides personas.
- Personas in `<HOME>/voices/*.toml`. Clones need documented consent (`--consent-who/-statement/-date`); `revoked` set → render refused. Don't clone anyone without the user's explicit OK.
- GPU: one global lock `.gpu.lock` in `~/Library/Application Support/funkhaus/` (LLM, TTS, acestep); jobs queue ("wartet auf GPU").
- Lengths: kurz ≈ 1.5 min (~5–8 min render), mittel ≈ 5 min, lang ≈ 12 min. TTS ≈ 10–25 s per line — run in background; don't start several at once (GPU).
- Workflow for edits: `--script-only` → edit script.txt → `--from-script path/script.txt` (if the script lies in an existing podcast dir, only changed lines are re-rendered there).
- `result.warnings` lists remaining non-speakable text and numbers not found in the sources — check them.

## Rules
- Answers come only from enabled sources; "Das steht nicht in den Quellen." when nothing fits. Always quote citations to the user.
- Sources may be customer data: keep everything local. Backend `claude` (`funkhaus nb backend NB claude`) sends source text to Anthropic — only when the user explicitly allows it for that notebook.
- No network except localhost LLM and URLs the user adds.
- Video URLs (YouTube, Vimeo, Mediatheken …) are imported via `yt-dlp` (config `ytdlp`): title, channel, description, chapters and captions (manual before automatic; original language > de > en). Only text is downloaded. Without captions the source holds only the description → warning `kein Transkript – nur Beschreibung`; check `added[].warnings`.
- Web pages that come back as cookie/consent walls (< 500 chars or consent keywords) are imported with a warning; podcast/make warn "Quellen sehr dünn" when enabled sources total < 1500 chars.
- Local audio/video files need `asr_cmd` in config.toml (e.g. `"whisper-cli -nt -f {in}"`).
