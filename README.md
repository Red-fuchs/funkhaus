# Funkhaus

A local, privacy-first take on NotebookLM, written in Rust. Collect sources, ask questions and get cited answers, generate
briefings/FAQs/study guides, and turn the material into a German two-host podcast. Everything runs on one Mac: a local
OpenAI-compatible LLM server, local TTS and ffmpeg. Source texts never leave the machine unless you explicitly allow an
online backend per notebook.

Also in this workspace: **Stimmwerk**, a second app on the same audio/voice crates for scripted multi-voice audio
(voice library, voice cloning with documented consent, music cues, sync to subtitles, LLM-assisted prose-to-script).

> Status: personal project, built for my own use on Apple Silicon (macOS). Documentation is partly German. No guarantees,
> but issues and ideas are welcome.

## What it does

- **Sources**: md, txt, pdf, srt/vtt, html, docx, URLs, whole folders, YouTube captions via `yt-dlp` (text only).
- **Ask**: answers with clickable `[n]` citations into the source text. Retrieval is BM25 plus German-aware fields; optional embeddings.
- **Make**: briefing, FAQ, study guide, timeline, saved as notes.
- **Podcast**: LLM writes a two-host script (JSON-schema constrained), an automatic checker flags digits, abbreviations and
  numbers that are not in the sources, TTS renders line by line with a content-hash cache (edit one line, only that line re-renders),
  mix with loudness presets, optional jingle and ducked music bed, chapters, show notes, ID3 tags, spoken AI disclosure.
- **Agent friendly**: the CLI prints JSON on stdout; a Claude Code skill is included in `skills/funkhaus/`.

## Architecture

```
crates/core      store, import, chunking + BM25, LLM client orchestration, ask/make, podcast pipeline
crates/audio     script format (tags, [pause]), mix timeline, loudness presets, MP3/ID3, chapters, music placement
crates/voice     TTS invocation, content-hash cache, pronunciation lexicon, personas, GPU lock, music generation call
crates/llm       backends (OpenAI-compatible local servers, optional online), keychain-based API keys
crates/cli       `funkhaus` binary
crates/gui       `funkhaus-gui` (eframe/egui)
crates/stimmwerk-{core,cli,gui}   Stimmwerk app and CLI
```

External tools it calls (not included): a local OpenAI-compatible LLM server (default `http://127.0.0.1:8095/v1`),
an `omnivoice` TTS CLI, `ffmpeg`, optionally `yt-dlp` and an ACE-Step CLI for music.

## Measured numbers (from the repo's own docs)

- Loudness presets, measured with ffmpeg ebur128: `podcast` -16 LUFS / -1.5 dBTP, `hoerbuch` -19 / -3, `video` -14 / -1;
  all within +/-0.5 LU of target, true peak within spec, with and without jingle.
- Retrieval eval (44 questions on two fictional fixture sets, bootstrap CIs): adding German map fields to BM25 raised
  recall@10 on the `solarboot` set from 0.364 to 0.909 and MRR from 0.273 to 0.675; on the tiny `kleinholm` set the
  difference is within noise.

## Quick start

```bash
cargo build --release --workspace
cargo test --workspace
./target/release/funkhaus doctor        # checks LLM server, TTS, ffmpeg

funkhaus nb new "Kleinholm"
funkhaus src add kleinholm fixtures/haiku/kleinholm.md fixtures/haiku/chronik.txt
funkhaus ask kleinholm "Wie viele Einwohner hat Kleinholm?"
funkhaus make kleinholm briefing
funkhaus podcast kleinholm --len kurz
```

`scripts/build-app.sh` additionally builds a macOS app bundle. Data lives in `~/Library/Application Support/funkhaus/`
(override with `FUNKHAUS_HOME`). The full German manual is `HANDBUCH.md`.

Requires Rust >= 1.85 (edition 2024).

## Fixtures

All fixtures are fictional or synthetic (the village "Kleinholm", sailing transcript "Solarboot", generated seminar texts).

## Credits and licenses

Built on open-source Rust crates (see `Cargo.lock`).
Models, TTS voices and music models you plug in come with their own licenses, which apply to their output.

## License

MIT, see `LICENSE`.
