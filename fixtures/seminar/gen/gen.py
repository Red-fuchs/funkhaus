import json
import pathlib
import sys

SRC = pathlib.Path(__file__).parent
OUT = pathlib.Path("./fixtures/seminar")
NAMES = {"Mara": "Mara Lindqvist", "Jonas": "Jonas", "Selma": "Selma", "Thorsten": "Thorsten", "Ilse": "Ilse"}
MAXLEN = 95
CPS = 12.5
GAP = 0.25


def split(text):
    out = []
    rest = text.strip()
    while len(rest) > MAXLEN:
        for sep in (". ", "? ", "! ", ", ", " "):
            i = rest.rfind(sep, 0, MAXLEN + 1)
            if i >= 30:
                break
        out.append(rest[: i + len(sep.rstrip())].strip())
        rest = rest[i + len(sep):].strip()
    out.append(rest)
    return out


def ts(t):
    h, r = divmod(int(round(t * 1000)), 3600_000)
    m, r = divmod(r, 60_000)
    s, ms = divmod(r, 1000)
    return f"{h:02d}:{m:02d}:{s:02d}.{ms:03d}"


def parse():
    items = []  # ("note", text) | ("cue", speaker, text)

    def emit(line):
        spk, _, txt = line.partition(": ")
        assert spk in NAMES and txt, line
        items.extend(("cue", NAMES[spk], c) for c in split(txt))

    for n in range(1, 7):
        for line in (SRC / f"b{n}.txt").read_text(encoding="utf-8").splitlines():
            line = line.strip()
            if not line:
                continue
            if line.startswith("@@BLOCK"):
                items.append(("note", "Block: " + line[len("@@BLOCK "):]))
            elif line == "@@OUTCUT_START":
                items.append(("note", "outcut:start (private Nebengespraech, nicht fuer Quellen)"))
            elif line == "@@OUTCUT_END":
                items.append(("note", "outcut:end"))
            elif line.startswith("@@INSERT "):
                for name in line.split()[1:]:
                    for sub in (SRC / f"{name}.txt").read_text(encoding="utf-8").splitlines():
                        if sub.strip():
                            emit(sub.strip())
            else:
                emit(line)
    return items


def build():
    t = 0.0
    cues = []
    lines = ["WEBVTT", "", "NOTE Fiktives Testfixture fuer Funkhaus. Keine realen Personen oder Organisationen.", ""]
    for it in parse():
        if it[0] == "note":
            lines += [f"NOTE {it[1]}", ""]
            continue
        _, spk, txt = it
        dur = min(8.0, max(3.0, len(txt) / CPS))
        start, end = t, t + dur
        cues.append({"start": round(start, 3), "end": round(end, 3), "speaker": spk, "text": txt})
        lines += [f"{ts(start)} --> {ts(end)}", f"<v {spk}>{txt}</v>", ""]
        t = end + GAP
    return lines, cues


def main():
    OUT.mkdir(parents=True, exist_ok=True)
    lines, cues = build()
    (OUT / "seminar.vtt").write_text("\n".join(lines), encoding="utf-8")
    chars = sum(len(c["text"]) for c in cues)
    dur = cues[-1]["end"]
    print(f"cues={len(cues)} chars={chars} duration={dur:.1f}s ({dur/60:.1f} min)")
    print(f"min_dur={min(c['end']-c['start'] for c in cues):.2f} max_dur={max(c['end']-c['start'] for c in cues):.2f}")
    print("samples:")
    for c in (cues[0], cues[len(cues) // 2], cues[-1]):
        print(f"  {c['start']:.3f}-{c['end']:.3f} [{c['speaker']}] {c['text']}")
    json.dump(cues, open(SRC / "cues.json", "w", encoding="utf-8"), ensure_ascii=False)


if __name__ == "__main__":
    sys.exit(main())
