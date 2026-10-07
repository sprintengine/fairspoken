#!/usr/bin/env python3
"""Rebuild SpeakoFlow Mini as Hugging Face safetensors, so it can be trained further.

SpeakoFlow publishes no LoRA, no merged safetensors and no training recipe:
only GGUF files. Its card says it is Qwen/Qwen3.5-0.8B plus a rank-16 LoRA,
merged before quantisation. Converting the base model with the same llama.cpp
converter and comparing tensor by tensor shows that the BF16 GGUF differs from
the base only in the projections a LoRA touches (full-attention q/k/v/o and
the MLP gate/up/down) and, at rounding level, in `ssm_a`, which the converter
computes as -exp(A_log). The projections are stored without any permutation,
so the merged weights can be copied back into the base checkpoint verbatim.

    python3 -I speakoflow_to_hf.py <Qwen3.5-0.8B HF dir> <SpeakoFlow BF16 .gguf> <out dir>
        [--check <base converted to BF16 .gguf>]

With --check, every tensor is compared against the base model's own BF16
conversion first and the script refuses unless the differences are exactly
the expected LoRA targets. Re-converting the output with convert_hf_to_gguf.py
must reproduce the SpeakoFlow GGUF bit for bit apart from `ssm_a` rounding
(export.py --verify-against does that check).

Needs: numpy, safetensors, gguf (llama.cpp's gguf-py), torch for bfloat16.
"""
import argparse
import json
import re
import shutil
from pathlib import Path

import numpy as np
import torch
from gguf import GGUFReader
from safetensors.torch import load_file, save_file

# GGUF name suffix -> HF module path inside a decoder layer.
LORA_TARGETS = {
    "attn_q.weight": "self_attn.q_proj.weight",
    "attn_k.weight": "self_attn.k_proj.weight",
    "attn_v.weight": "self_attn.v_proj.weight",
    "attn_output.weight": "self_attn.o_proj.weight",
    "ffn_gate.weight": "mlp.gate_proj.weight",
    "ffn_up.weight": "mlp.up_proj.weight",
    "ffn_down.weight": "mlp.down_proj.weight",
}
# Recomputed by the converter in float32, so it differs at rounding level.
ROUNDING_ONLY = {"ssm_a"}


def tensor_bf16(t) -> torch.Tensor:
    if t.tensor_type.name != "BF16":
        raise SystemExit(f"{t.name}: expected BF16, got {t.tensor_type.name}")
    # gguf-py hands BF16 back as raw bytes; GGUF lists dimensions innermost
    # first, so (in, out) there is (out, in) here.
    raw = np.ascontiguousarray(t.data).view(np.uint16).reshape([int(d) for d in reversed(t.shape)])
    return torch.from_numpy(raw.copy()).view(torch.bfloat16)


def main():
    p = argparse.ArgumentParser()
    p.add_argument("base_hf")
    p.add_argument("speakoflow_gguf")
    p.add_argument("out")
    p.add_argument("--check", help="the base model converted to BF16 GGUF by the same converter")
    args = p.parse_args()

    tuned = {t.name: t for t in GGUFReader(args.speakoflow_gguf).tensors}
    if args.check:
        base = {t.name: t for t in GGUFReader(args.check).tensors}
        if set(base) != set(tuned):
            raise SystemExit("tensor names differ from the base conversion; not a Qwen3.5-0.8B fine-tune")
        unexpected = []
        for name, t in tuned.items():
            if np.array_equal(np.asarray(t.data), np.asarray(base[name].data)):
                continue
            suffix = name.split(".", 2)[2] if name.startswith("blk.") else name
            if suffix not in LORA_TARGETS and suffix not in ROUNDING_ONLY:
                unexpected.append(name)
        if unexpected:
            raise SystemExit(f"unexpected differences, refusing: {unexpected}")
        print("check: differences are limited to LoRA targets and ssm_a rounding")

    src = Path(args.base_hf)
    out = Path(args.out)
    out.mkdir(parents=True, exist_ok=True)
    shards = sorted(src.glob("*.safetensors"))
    replaced = 0
    for shard in shards:
        weights = load_file(str(shard))
        for name in list(weights):
            m = re.fullmatch(r"model\.language_model\.layers\.(\d+)\.(.+)", name)
            if not m:
                continue
            layer, module = m.groups()
            gguf_suffix = next((k for k, v in LORA_TARGETS.items() if v == module), None)
            if gguf_suffix is None:
                continue
            t = tuned[f"blk.{layer}.{gguf_suffix}"]
            merged = tensor_bf16(t)
            if tuple(merged.shape) != tuple(weights[name].shape):
                raise SystemExit(f"{name}: shape {tuple(merged.shape)} != {tuple(weights[name].shape)}")
            weights[name] = merged.contiguous()
            replaced += 1
        save_file(weights, str(out / shard.name), metadata={"format": "pt"})
    for f in src.iterdir():
        if f.suffix != ".safetensors" and f.is_file():
            shutil.copy2(f, out / f.name)
    (out / "SPEAKOFLOW_PROVENANCE.json").write_text(json.dumps({
        "base": str(src),
        "merged_from": Path(args.speakoflow_gguf).name,
        "replaced_tensors": replaced,
        "note": "SpeakoFlow Mini merged LoRA weights copied into the Qwen3.5-0.8B checkpoint (Apache-2.0).",
    }, indent=1))
    print(f"wrote {out} ({replaced} tensors replaced)")


if __name__ == "__main__":
    main()
