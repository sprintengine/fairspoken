#!/usr/bin/env python3
"""Merge a PEFT LoRA adapter into a Hugging Face checkpoint: W' = W + (alpha / r) B A.

    python3 -I merge_adapter.py <base HF dir> <peft adapter dir> <out dir>

This replaces `mlx_lm.fuse` for export. fuse saves MLX's sanitised weights
(norms shifted by +1, conv1d axes moved, `language_model.model.` names), which
convert_hf_to_gguf.py then misreads for Qwen3.5, and `--export-gguf` does not
support Qwen at all. Merging in the original checkpoint's own layout keeps
every untouched tensor byte-identical, so the converter sees exactly what it
saw for the base model. The sum is done in float32 and stored back in the
base tensor's dtype (BF16).
"""
import argparse
import json
import shutil
from pathlib import Path

import torch
from safetensors.torch import load_file, save_file


def merge(base, adapter, out):
    base, adapter, out = Path(base), Path(adapter), Path(out)
    cfg = json.loads((adapter / "adapter_config.json").read_text())
    scaling = float(cfg["lora_alpha"]) / float(cfg["r"])
    lora = load_file(str(adapter / "adapter_model.safetensors"))
    pairs = {}
    for key, value in lora.items():
        name = key.removeprefix("base_model.model.")
        if name.endswith(".lora_A.weight"):
            pairs.setdefault(name[: -len(".lora_A.weight")] + ".weight", {})["A"] = value
        elif name.endswith(".lora_B.weight"):
            pairs.setdefault(name[: -len(".lora_B.weight")] + ".weight", {})["B"] = value
        else:
            raise SystemExit(f"unexpected adapter tensor {key!r}")
    out.mkdir(parents=True, exist_ok=True)
    merged = 0
    for shard in sorted(base.glob("*.safetensors")):
        weights = load_file(str(shard))
        for name in list(weights):
            if name not in pairs:
                continue
            pair = pairs.pop(name)
            if "A" not in pair or "B" not in pair:
                raise SystemExit(f"{name}: adapter has only one of lora_A/lora_B")
            w = weights[name]
            delta = scaling * (pair["B"].float() @ pair["A"].float())
            if tuple(delta.shape) != tuple(w.shape):
                raise SystemExit(f"{name}: delta {tuple(delta.shape)} != weight {tuple(w.shape)}")
            weights[name] = (w.float() + delta).to(w.dtype).contiguous()
            merged += 1
        save_file(weights, str(out / shard.name), metadata={"format": "pt"})
    if pairs:
        raise SystemExit(f"adapter targets missing from the base checkpoint: {sorted(pairs)[:5]}")
    for f in base.iterdir():
        if f.is_file() and f.suffix != ".safetensors":
            shutil.copy2(f, out / f.name)
    return merged


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("base")
    ap.add_argument("adapter")
    ap.add_argument("out")
    args = ap.parse_args()
    n = merge(args.base, args.adapter, args.out)
    print(f"wrote {args.out} ({n} tensors merged)")


if __name__ == "__main__":
    main()
