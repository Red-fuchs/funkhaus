# asr-lexicon

Domain lexicon for German seminar audio (Autismus, Eingliederungshilfe, Jugendhilfe, BTHG, Fallbesprechung). 100 terms.

Files: `lexicon.tsv` (term, misheard, category). Header row first, tab-separated, misheard list comma-separated.

Status: `misheard` column is hypothesis, not measured ASR output. Verify each entry with a TTS→ASR round-trip before trusting it. Fixture contains no personal data and no real person names.

## (a) Hotword / initial prompt for ASR

- whisper.cpp: `--prompt "<terms>"` sets initial decoder context, not a hard boost.
  - Token budget: Whisper text ctx is 448 tokens. Prompt conditioning keeps only the last ~224 tokens (src: https://github.com/openai/whisper/blob/main/whisper/transcribe.py, from memory, verify). Keep prompt ≤ ~150 tokens, ≈ 25–35 terms.
  - Carry across 30 s windows: `--carry-initial-prompt` (src: https://github.com/ggml-org/whisper.cpp README, verify flag with `whisper-cli -h`).
  - Select terms per session (top-N by seminar topic), not all 100.
  - Sentence form ("Thema: Reizfilter, Echolalie, TEACCH …") vs bare list: unverified, A/B on one recording.
- Qwen3-ASR: context/hotword support not verified. Check model card (src: https://huggingface.co/Qwen, search Qwen3-ASR). Treat as unknown until read.
- NVIDIA Parakeet / NeMo: NeMo has context-biasing / word-boosting for transducer and CTC decoding (src: https://github.com/NVIDIA/NeMo, search "context biasing" / "boosting"). Weak: from memory, not checked for parakeet-tdt specifically. Boosting list = `term` column only.
- Hotwords do not fix acronyms reliably (ABA, SGB IX, BTHG). Use (b) for those.

## (b) Post-correction regex table

Build patterns from `misheard`, map to `term`:

- Boundaries: `(?<![\wÄÖÜäöüß-])(?:alt1|alt2)(?![\wÄÖÜäöüß-])` (Python `re`, Unicode). Hyphen excluded so `Autismus-Spektrum` is not split.
- Order: sort alternatives by length desc, so multi-word keys win (`Autismus Spektrum` before `Autismus`).
- Case: match case-insensitive for words, replacement takes canonical `term` casing. Acronyms (ABA, SGB IX, BTHG, WfbM, SPFH, ICD-10): case-sensitive match only.
- Min length: skip misheard forms < 5 chars unless multi-word.
- Over-correction risks (review, do not auto-apply):
  - `Teach`, `Pex`, `Peks`, `Melt Down`, `Shut down`: real English/German tokens.
  - `Hypersensibilität` ↔ `Hyposensibilität`: minimal pair, only correct listed forms.
  - `Spezial Interesse`, `Fall Arbeit`, `Heim Erziehung`: split compounds can be legit.
  - `Stimming` ← `Stiming`/`Stemming`: `Stemming` is a real word, flag for review.
  - Real canonical forms are never in the misheard list (e.g. `Routine` singular, `Stereotyp`, `Bindung`, `Jugendamts`, `Leistungserbringerin` left out on purpose).
- Log hits (line, old, new) and apply after manual review; idempotent because canonical forms are not in `misheard`.

Minimal sketch:

```python
import re, csv
rules = []
for r in csv.DictReader(open("lexicon.tsv", encoding="utf-8"), delimiter="\t"):
    for alt in r["misheard"].split(", "):
        if len(alt) >= 5 or " " in alt:
            rules.append((alt, r["term"]))
rules.sort(key=lambda x: -len(x[0]))
pat = re.compile(r"(?<![\wÄÖÜäöüß-])(" + "|".join(re.escape(a) for a, _ in rules) + r")(?![\wÄÖÜäöüß-])", re.I)
canon = {a.lower(): t for a, t in rules}
fix = lambda s: pat.sub(lambda m: canon[m.group(1).lower()], s)
```

Acronym rule (case-sensitive, skipped above) needs a separate pass.

## (c) omnivoice TTS pronunciation

- Target format is `word<TAB>speakable<TAB>note` (src: `~/Projects/omnivoice/lexicon/de.tsv`, read-only). Whole-word, case-sensitive, longest key wins, applied after rule normaliser.
- `lexicon.tsv` cannot be fed to omnivoice directly. Derive speakable forms per term. Suggested (all unverified, run `omnivoice check --asr`):

| word | speakable | note |
|---|---|---|
| BTHG | Be-Te-Ha-Ge | unverified |
| SGB IX | Es-Ge-Be neun | unverified |
| SGB VIII | Es-Ge-Be acht | unverified |
| SPFH | Es-Pe-Ef-Ha | unverified |
| WfbM | Ve-Ef-Be-Em | unverified |
| ABA | A-Be-A | unverified |
| ICD-10 | I-Ce-De zehn | unverified |
| TEACCH | Tieatsch | unverified |
| PECS | Peks | unverified |
| Echolalie | Ech-o-la-lie | stress hint |
| Propriozeption | Proprio-zep-zion | unverified |
| Interozeption | Intero-zep-zion | unverified |
| Monotropismus | Mono-tro-pismus | unverified |
| Hyperfokus | Hy-per-fokus | unverified |
| Meltdown | Melt-daun | unverified |
| Shutdown | Schat-daun | unverified |

- Adding to omnivoice lexicon: write only after user decision, per `speakable-de` skill (maintains pronunciation lexicon).

## Sources and flags

- whisper.cpp README: https://github.com/ggml-org/whisper.cpp (primary, flags may change).
- OpenAI Whisper source: https://github.com/openai/whisper (prompt token limit, memory-based, weak until checked).
- NVIDIA NeMo: https://github.com/NVIDIA/NeMo (context biasing, weak, not checked for Parakeet).
- Qwen3-ASR: https://huggingface.co/Qwen (no verified hotword API).
- Misheard lists: hypothesis from German phonetics and English-spelling interference, no corpus. Weak.
