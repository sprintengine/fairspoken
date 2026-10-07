#!/usr/bin/env python3
"""Evaluate polish candidates on the held-out split, with the bench's metrics plus the tags'.

Serve a candidate with the app's own runtime and flags (see run.sh eval), then:

    python3 -I eval.py --port 48910 --variant tagged --data <data>/test.jsonl --json out.json [--lora 0]
    python3 -I eval.py --port 48910 --variant speakoflow --data <data>/test.jsonl --json out.json
    python3 -I eval.py --score out.json [out2.json ...]      # re-score / tabulate saved runs
    python3 -I eval.py --baseline <data>/test.jsonl          # do-nothing and oracle rows

Variants:
  speakoflow       SpeakoFlow Mini as shipped: its system prompt, the bare transcript, tags dropped
  instructed-tags  the shipped instructed prompt and examples (scripts/polish-bench.py) with the
                   tagged user turn and a format/tone addendum; a stand-in for the formatting
                   work's own instructed layout
  tagged           the tag-trained contract (contract.py): system prompt plus tagged user turn

Metrics, all over the held-out split (exact match after trimming outer whitespace):
  edit accuracy   of transcripts that need a change, the share returned exactly right
  restraint       of transcripts already correct, the share returned untouched
  dropped words   the share of outputs missing a word the reference keeps (lower is better)
  spelling        of tagged terms that were spoken but misheard, the share written as tagged
  unspoken        of transcripts whose <spelling> lists terms never said, the share where one
                  appeared in the output anyway (lower is better)
  format          over transcripts where the format has work to do (several lines, a spoken
                  greeting or sign-off, or code): line structure matches the reference (greeting and sign-off lines, list
                  markers, paragraphs), no greeting or sign-off that was not spoken, and for
                  code every identifier kept verbatim
  bench           the regex suite of scripts/polish-bench.py (CASES + HELD_OUT), plain prompt
"""
import argparse
import importlib.util
import json
import re
import statistics
import sys
import time
import urllib.request
from collections import Counter
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import contract  # noqa: E402

_spec = importlib.util.spec_from_file_location("polish_bench", HERE.parent.parent / "scripts" / "polish-bench.py")
bench = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(bench)

FORMAT_ADDENDUM = """
A <format> tag says where the text goes. email: a spoken greeting on its own line, a blank line between paragraphs (at topic shifts and on a spoken "new paragraph"), a spoken sign-off on its own lines. chat: one concise message. document: full sentences and paragraphs. notes: bullet points ("- ") for enumerations. code: keep identifiers exactly. plain or no tag: as above.
A <tone> tag (casual, neutral, formal) changes punctuation only, never words. Never add a greeting, sign-off or name that was not spoken."""


def build(variant, meta):
    raw, spelling = meta["raw"], meta.get("spelling") or []
    fmt, tone = meta.get("tag_format"), meta.get("tag_tone")
    if variant == "speakoflow":
        return [{"role": "system", "content": bench.SPEAKOFLOW_SYSTEM}, {"role": "user", "content": raw}]
    if variant == "tagged":
        return contract.messages(raw, spelling, fmt, tone)
    if variant == "instructed-tags":
        msgs = [{"role": "system", "content": bench.SYSTEM + FORMAT_ADDENDUM}]
        for u, a in bench.EXAMPLES:
            msgs += [{"role": "user", "content": contract.user_message(u)}, {"role": "assistant", "content": a}]
        return msgs + [{"role": "user", "content": contract.user_message(raw, spelling, fmt, tone)}]
    raise SystemExit(f"unknown variant {variant}")


def call(port, messages, lora=None):
    body = {"messages": messages, "temperature": 0, "max_tokens": 512,
            "chat_template_kwargs": {"enable_thinking": False}}
    if lora is not None:
        body["lora"] = lora
    req = urllib.request.Request(f"http://127.0.0.1:{port}/v1/chat/completions", json.dumps(body).encode(),
                                 {"Content-Type": "application/json"})
    started = time.time()
    out = json.load(urllib.request.urlopen(req, timeout=180))
    return out["choices"][0]["message"]["content"].strip(), round((time.time() - started) * 1000)


# -- scoring -----------------------------------------------------------------

def tokens(text):
    text = re.sub(r"(?m)^\s*(\d+[.)]|[-*•])\s+", "", text)  # list markers are structure
    return [w for w in re.findall(r"[\w'&]+", text.lower())]


def dropped(expected, out):
    missing = Counter(tokens(expected)) - Counter(tokens(out))
    return bool(missing)


def structure(text):
    sig = []
    for line in text.strip().split("\n"):
        line = line.strip()
        sig.append("blank" if not line else "num" if re.match(r"\d+[.)]\s", line)
                   else "bullet" if re.match(r"[-*•]\s", line) else "text")
    return sig


GREETING = re.compile(r"^(hi|hello|hey|dear|good morning)\b", re.I)
SIGNOFF = re.compile(r"(thanks|kind regards|regards|best|cheers|many thanks|talk soon)[,.!]?\s*(\n\s*\w+\.?)?\s*$", re.I)


def format_ok(meta, out):
    raw, exp = meta["raw"], meta["expected"]
    if structure(out) != structure(exp):
        return False
    if GREETING.search(out.strip()) and not GREETING.search(raw.strip()):
        return False
    if SIGNOFF.search(out) and not re.search(r"\b(thanks|regards|best|cheers|talk soon)\b", raw, re.I):
        return False
    if meta["format"] == "code":
        idents = [w for w in re.findall(r"\S+", exp) if re.search(r"[a-z][A-Z]|_|/|\.\w", w.strip(".,?!"))]
        if any(w.strip(".,?!") not in out for w in idents):
            return False
    return True


def score(rows):
    """rows: [{"meta":..., "out":..., "ms":...}] -> metrics."""
    def rate(hits, total):
        return round(100 * hits / total, 1) if total else None
    change = [r for r in rows if r["meta"]["raw"] != r["meta"]["expected"]]
    same = [r for r in rows if r["meta"]["raw"] == r["meta"]["expected"]]
    exact = lambda r: r["out"].strip() == r["meta"]["expected"].strip()
    spelled = [(r, t) for r in rows for t in r["meta"].get("spoken_terms", [])
               if t in (r["meta"].get("spelling") or []) and t not in r["meta"]["raw"]]
    unspoken = [r for r in rows if r["meta"].get("unspoken_terms")]
    inserted = [r for r in unspoken if any(t.lower() in r["out"].lower() for t in r["meta"]["unspoken_terms"])]
    edit, restraint = rate(sum(map(exact, change)), len(change)), rate(sum(map(exact, same)), len(same))
    ms = [r["ms"] for r in rows if r.get("ms") is not None]
    by_format = {}
    for f in contract.FORMATS:
        sub = [r for r in rows if r["meta"]["format"] == f]
        if sub:
            by_format[f] = rate(sum(map(exact, sub)), len(sub))
    house = [r for r in rows if "house-style" in r["meta"].get("kinds", [])]
    # Format adherence counts only where the format has something to do.
    shaped = [r for r in rows if structure(r["meta"]["expected"]) != ["text"] or r["meta"]["format"] == "code"
              or r["meta"].get("greeting") or r["meta"].get("signoff")]
    return {
        "n": len(rows), "edit_accuracy": edit, "restraint": restraint,
        "overall": round((edit + restraint) / 2, 1) if edit is not None and restraint is not None else None,
        "dropped_words": rate(sum(dropped(r["meta"]["expected"], r["out"]) for r in rows), len(rows)),
        "spelling": rate(sum(t in r["out"] for r, t in spelled), len(spelled)),
        "unspoken_inserted": rate(len(inserted), len(unspoken)),
        "format": rate(sum(format_ok(r["meta"], r["out"]) for r in shaped), len(shaped)),
        "house_style": rate(sum(map(exact, house)), len(house)),
        "exact_by_format": by_format,
        "latency_median_ms": statistics.median(ms) if ms else None,
    }


def run_bench(port, variant, lora):
    passed = total = 0
    fails = []
    cases = [(n, r, m, mn) for n, r, m, mn in bench.CASES] + [(n, r, m, mn) for n, _, r, m, mn in bench.HELD_OUT]
    for name, raw, must, must_not in cases:
        meta = {"raw": raw, "spelling": bench.relevant_vocab(raw, bench.VOCAB), "tag_format": None, "tag_tone": None}
        out, _ = call(port, build(variant, meta), lora)
        bad = [p for p in must if not re.search(p, out, re.I | re.M)] + [p for p in must_not if re.search(p, out, re.I | re.M)]
        total += 1
        passed += not bad
        if bad:
            fails.append({"name": name, "raw": raw, "out": out})
    return {"passed": passed, "total": total, "fails": fails}


def table(runs):
    cols = ["edit_accuracy", "restraint", "overall", "dropped_words", "spelling", "unspoken_inserted", "format", "house_style"]
    print("| candidate | n | " + " | ".join(cols) + " | bench |")
    print("|---" * (len(cols) + 3) + "|")
    for name, run in runs:
        m = run["metrics"]
        b = run.get("bench")
        cells = ["–" if m.get(c) is None else f"{m[c]}" for c in cols]
        bench_cell = f"{b['passed']}/{b['total']}" if b else "–"
        print(f"| {name} | {m['n']} | " + " | ".join(cells) + f" | {bench_cell} |")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--port")
    ap.add_argument("--variant", choices=["speakoflow", "instructed-tags", "tagged"])
    ap.add_argument("--data")
    ap.add_argument("--lora", type=int, action="append", help="adapter id to apply at scale 1 (repeatable)")
    ap.add_argument("--name", help="label in the table")
    ap.add_argument("--json")
    ap.add_argument("--limit", type=int)
    ap.add_argument("--no-bench", action="store_true")
    ap.add_argument("--score", nargs="+")
    ap.add_argument("--baseline")
    args = ap.parse_args()

    if args.score:
        runs = []
        for path in args.score:
            run = json.loads(Path(path).read_text())
            run["metrics"] = score(run["rows"])
            runs.append((run.get("name", Path(path).stem), run))
        table(runs)
        return
    if args.baseline:
        metas = [json.loads(l)["meta"] for l in Path(args.baseline).read_text().splitlines() if l.strip()]
        runs = [("do nothing (raw text)", {"metrics": score([{"meta": m, "out": m["raw"]} for m in metas])}),
                ("oracle (reference)", {"metrics": score([{"meta": m, "out": m["expected"]} for m in metas])})]
        table(runs)
        return

    lora = [{"id": i, "scale": 1.0} for i in args.lora] if args.lora else ([] if args.variant == "tagged" else None)
    metas = [json.loads(l)["meta"] for l in Path(args.data).read_text().splitlines() if l.strip()]
    metas = metas[: args.limit] if args.limit else metas
    call(args.port, build(args.variant, metas[0]), lora)  # warm-up, untimed
    rows = []
    for i, meta in enumerate(metas):
        out, ms = call(args.port, build(args.variant, meta), lora)
        rows.append({"meta": meta, "out": out, "ms": ms})
        if (i + 1) % 50 == 0:
            print(f"{i + 1}/{len(metas)}", file=sys.stderr)
    run = {"name": args.name or args.variant, "variant": args.variant, "lora": lora, "rows": rows,
           "metrics": score(rows)}
    if not args.no_bench:
        run["bench"] = run_bench(args.port, args.variant, lora)
    if args.json:
        Path(args.json).write_text(json.dumps(run, indent=1, ensure_ascii=False))
    table([(run["name"], run)])


if __name__ == "__main__":
    main()
