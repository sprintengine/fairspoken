#!/usr/bin/env python3
"""Train on my own dictations: turn a Fairspoken training-data export into polish examples.

    python3 -I import_dictations.py <export.zip | export dir | manifest.jsonl> --out mine.jsonl
        [--vocabulary terms.txt] [--no-change-share 0.4] [--keep-rewrites]

then add it to a training run (it is never put in the held-out test split):

    python3 -I generate_data.py --out <data dir> --extra-train mine.jsonl

The export is the opt-in one described in docs/training-data.md (format
version 1). Each manifest row has `raw_text` (the speech model's transcript),
`polished_text`, `final_text` (what was inserted, after corrections and
snippets) and `edited_text` (the dictation as the user left it, when the edit
watcher saw a change). The example is raw -> edited, falling back to final:
what the user actually wanted on screen. `polish.jsonl` (prompt/completion)
is read too when there is no manifest.

Filters, because a polish model must never learn to add words:
- terminal dictations are skipped (polish never runs there);
- a target that adds many words the speaker never said is a rewrite or a
  snippet expansion, not a cleanup, and is skipped unless --keep-rewrites.

The app category becomes the <format> tag the app would have sent
(messaging -> chat, email, docs -> document, code, other -> plain). Terms from
--vocabulary (one per line, e.g. the app's dictionary) that appear in the
target go in the <spelling> tag. --no-change-share tops the set up with
already-clean pairs (the target as its own input), so the model keeps its
restraint; 0.4 follows the simplewords recipe.

These files are personal data. Keep them, and everything trained from them,
on the device; never commit or upload them.
"""
import argparse
import io
import json
import random
import re
import sys
import zipfile
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import contract  # noqa: E402

CATEGORY_FORMAT = {"messaging": "chat", "email": "email", "docs": "document", "code": "code", "other": "plain"}


def words(text):
    return [w for w in re.split(r"[^\w']+", text.lower()) if w]


def added_share(raw, target):
    """The share of the target's content words that were never spoken."""
    spoken = set(words(raw))
    content = [w for w in words(target) if len(w) > 3]
    if not content:
        return 0.0
    novel = [w for w in content if w not in spoken and not any(w in s or s in w for s in spoken if len(s) > 3)]
    return len(novel) / len(content)


def read_rows(source):
    path = Path(source)
    if path.suffix == ".zip":
        with zipfile.ZipFile(path) as z:
            names = z.namelist()
            name = "manifest.jsonl" if "manifest.jsonl" in names else "polish.jsonl"
            text = io.TextIOWrapper(z.open(name), encoding="utf-8").read()
    elif path.is_dir():
        name = "manifest.jsonl" if (path / "manifest.jsonl").exists() else "polish.jsonl"
        text = (path / name).read_text(encoding="utf-8")
    else:
        text = path.read_text(encoding="utf-8")
    for line in text.splitlines():
        if line.strip():
            yield json.loads(line)


def to_pair(row):
    """(raw, target, app category) from a manifest or polish.jsonl row."""
    if "raw_text" in row:
        target = row.get("edited_text") or row.get("final_text")
        return row.get("raw_text"), target, row.get("app_category") or "other"
    return row.get("prompt"), row.get("completion"), "other"


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("source")
    ap.add_argument("--out", required=True)
    ap.add_argument("--vocabulary", help="one dictionary term per line")
    ap.add_argument("--no-change-share", type=float, default=0.4)
    ap.add_argument("--max-added", type=float, default=0.3,
                    help="skip targets where more than this share of content words was never spoken")
    ap.add_argument("--keep-rewrites", action="store_true")
    ap.add_argument("--seed", type=int, default=7)
    args = ap.parse_args()
    vocab = [t.strip() for t in Path(args.vocabulary).read_text().splitlines() if t.strip()] if args.vocabulary else []

    out, skipped = [], {"terminal": 0, "empty": 0, "rewrite": 0}
    for row in read_rows(args.source):
        raw, target, category = to_pair(row)
        if category == "terminal":
            skipped["terminal"] += 1
            continue
        if not raw or not raw.strip() or not target or not target.strip():
            skipped["empty"] += 1
            continue
        raw, target = raw.strip(), target.strip()
        if not args.keep_rewrites and added_share(raw, target) > args.max_added:
            skipped["rewrite"] += 1
            continue
        fmt = CATEGORY_FORMAT.get(category, "plain")
        spelling = [t for t in vocab if t in target]
        out.append({"messages": contract.messages(raw, spelling, fmt, None, target),
                    "meta": {"raw": raw, "expected": target, "format": fmt, "tone": "neutral",
                             "spelling": spelling, "source": "my-dictations", "id": row.get("id")}})

    rng = random.Random(args.seed)
    unchanged = sum(r["meta"]["raw"] == r["meta"]["expected"] for r in out)
    want = int(args.no_change_share * len(out) / max(1e-9, 1 - args.no_change_share)) if args.no_change_share < 1 else 0
    changed = [r for r in out if r["meta"]["raw"] != r["meta"]["expected"]]
    rng.shuffle(changed)
    added = 0
    for r in changed:
        if unchanged + added >= want:
            break
        m = r["meta"]
        if "\n" in m["expected"]:
            continue  # speech never arrives with line breaks
        out.append({"messages": contract.messages(m["expected"], m["spelling"], m["format"], None, m["expected"]),
                    "meta": {**m, "raw": m["expected"], "source": "my-dictations:no-change"}})
        added += 1

    with open(args.out, "w", encoding="utf-8") as f:
        for r in out:
            f.write(json.dumps(r, ensure_ascii=False) + "\n")
    print(json.dumps({"examples": len(out), "no_change": unchanged + added, "added_no_change": added, "skipped": skipped}))


if __name__ == "__main__":
    main()
