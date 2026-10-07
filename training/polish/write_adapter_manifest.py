#!/usr/bin/env python3
"""Write the manifest the app needs before it will load a polish LoRA adapter.

    python3 -I write_adapter_manifest.py <adapter.gguf> --base <base model.gguf> --pack <pack id> [--scale 1.0]

Writes <adapter>.json next to the adapter. The app (local_models.rs,
`validate_adapter`) loads an adapter only when this manifest names the exact
base model file it is serving, by SHA-256, and the adapter file still matches
its own recorded hash; anything else and polish runs without the adapter.
An adapter trained on another base would otherwise load without complaint
(same architecture, same tensor shapes) and quietly change the output.
"""
import argparse
import hashlib
import json
from pathlib import Path


def sha256(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for block in iter(lambda: f.read(1 << 20), b""):
            h.update(block)
    return h.hexdigest()


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("adapter")
    ap.add_argument("--base", required=True)
    ap.add_argument("--pack", required=True)
    ap.add_argument("--scale", type=float, default=1.0)
    args = ap.parse_args()
    adapter = Path(args.adapter)
    manifest = {
        "format": "fairspoken-polish-adapter",
        "version": 1,
        "pack": args.pack,
        "base_model_sha256": sha256(args.base),
        "adapter_sha256": sha256(adapter),
        "scale": args.scale,
    }
    out = adapter.with_suffix(".json")
    out.write_text(json.dumps(manifest, indent=1) + "\n")
    print(f"wrote {out}")


if __name__ == "__main__":
    main()
