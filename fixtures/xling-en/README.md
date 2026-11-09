# xling-en fixture (Funkhaus)

Fictional cross-lingual eval set: German questions against an English source talk.

- All people, boats, ports and companies are invented (Ines Halvorsen, Moth Wing, Port Arvel). No real persons or personal data.
- Purpose: test whether retrieval/QA over an English transcript answers German questions, both with lexical overlap (`direct`) and with German paraphrase that shares no content words with the source (`oblique`). Two `none` items have no answer in the source.

Files:
- `transcript.vtt`: WebVTT, ~6.6k chars of talk text, 91 cues of 3-6 s, speaker tags `<v Ines Halvorsen>` and `<v Interviewer>`.
- `eval.jsonl`: 22 lines. Fields: `q` (German question), `quote` (verbatim English substring of the concatenated cue text, no tags or timestamps), `kind` (`direct` x10, `oblique` x10). `none: true` x2 with `quote: ""` are unanswerable.

Generation:
- Transcript written by a throwaway python3 script (outside the repo) that splits the speaker turns into word-bounded chunks of at most 80 chars and sets each cue duration to len/14 s, clamped to 3-6 s.
- `eval.jsonl` was hand-written. The same script then checks that every non-`none` quote is a substring of the cue texts joined with a single space, and that the file has 22 rows with 10/10/2.
