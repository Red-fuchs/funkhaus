import json
import pathlib
import re
from collections import Counter

SRC = pathlib.Path(__file__).parent
OUT = pathlib.Path("./fixtures/seminar")
STOP = set("und oder aber dass dann denn wenn wie was wer wir sie die der das den dem des ein eine einen einem ist sind war sich auch noch nur schon mal mit von für auf aus bei nach über unter wird wurde hat haben kann mehr sehr gibt wieviel welche welcher welches".split())

cues = json.load(open(SRC / "cues.json", encoding="utf-8"))
texts = [c["text"] for c in cues]
joined = " ".join(texts)
rows = [json.loads(l) for l in (OUT / "QUESTIONS.jsonl").read_text(encoding="utf-8").splitlines()]

assert len(rows) == 25, len(rows)
print("kinds:", Counter(r["kind"] for r in rows))
bad = 0
for i, r in enumerate(rows, 1):
    assert set(r) == {"q", "quote", "kind"}, r
    ok = r["quote"] in joined
    single = any(r["quote"] in t for t in texts)
    if not ok:
        bad += 1
        print(f"FAIL {i}: {r['quote']!r}")
    elif not single:
        print(f"WARN {i}: quote spans cue boundary: {r['quote']!r}")
    if r["kind"] == "oblique":
        tok = lambda s: {w for w in re.findall(r"\w+", s.lower()) if len(w) >= 4 and w not in STOP}
        ov = tok(r["q"]) & tok(r["quote"])
        if ov:
            bad += 1
            print(f"OVERLAP {i}: {ov}")
    if r["kind"] == "global" and r["q"] != "Worum ging es im Fallbeispiel insgesamt?":
        print(f"NOTE {i}: global question differs")
print("failures:", bad)
