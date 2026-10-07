#!/usr/bin/env python3
"""Rewrite an mlx-lm LoRA adapter in PEFT layout, for llama.cpp and for merging.

    python3 -I mlx_to_peft.py <mlx adapter dir> <out dir> [--base-name <base model dir or id>]

mlx-lm saves `adapters.safetensors` with `<module>.lora_a` (in, r) and
`<module>.lora_b` (r, out) under MLX module names
(`language_model.model.layers.N...`) and applies `scale * x A B`. PEFT, which
convert_lora_to_gguf.py reads, stores `base_model.model.<HF name>.lora_A.weight`
(r, in) and `.lora_B.weight` (out, r) and applies `lora_alpha / r`. So A and B
are transposed, names are mapped back to the Hugging Face checkpoint's
(`model.language_model.layers.N...`), and lora_alpha = scale * r.

Only plain linear projections are accepted (the configs restrict LoRA to them);
anything else stops the conversion rather than produce an adapter llama.cpp
would apply to the wrong tensor.
"""
import argparse
import json
from pathlib import Path

import numpy as np
from safetensors.numpy import load_file, save_file

ALLOWED = ("self_attn.q_proj", "self_attn.k_proj", "self_attn.v_proj", "self_attn.o_proj",
           "mlp.gate_proj", "mlp.up_proj", "mlp.down_proj")


def hf_module(mlx_name):
    """`language_model.model.layers.3.mlp.up_proj` -> `model.language_model.layers.3.mlp.up_proj`."""
    if mlx_name.startswith("language_model.model."):
        return "model.language_model." + mlx_name[len("language_model.model."):]
    if mlx_name.startswith("model."):
        return mlx_name
    raise SystemExit(f"unexpected adapter tensor {mlx_name!r}")


def convert(src, out, base_name=None):
    src, out = Path(src), Path(out)
    cfg = json.loads((src / "adapter_config.json").read_text())
    lp = cfg["lora_parameters"]
    rank, scale = int(lp["rank"]), float(lp["scale"])
    weights = load_file(str(src / "adapters.safetensors"))
    peft, modules = {}, set()
    for name, value in weights.items():
        module, kind = name.rsplit(".", 1)
        if kind not in ("lora_a", "lora_b"):
            raise SystemExit(f"unexpected adapter tensor {name!r}")
        if not module.endswith(ALLOWED):
            raise SystemExit(f"{module}: only {ALLOWED} can be exported safely")
        hf = hf_module(module)
        modules.add(hf.rsplit(".", 1)[-1])
        key = f"base_model.model.{hf}.{'lora_A' if kind == 'lora_a' else 'lora_B'}.weight"
        peft[key] = np.ascontiguousarray(value.T).astype(np.float32)
        expected = rank if kind == "lora_a" else None
        if expected is not None and peft[key].shape[0] != rank:
            raise SystemExit(f"{name}: rank {peft[key].shape[0]} != {rank}")
    out.mkdir(parents=True, exist_ok=True)
    save_file(peft, str(out / "adapter_model.safetensors"))
    (out / "adapter_config.json").write_text(json.dumps({
        "peft_type": "LORA", "task_type": "CAUSAL_LM", "r": rank, "lora_alpha": scale * rank,
        "lora_dropout": float(lp.get("dropout", 0.0)), "bias": "none", "fan_in_fan_out": False,
        "target_modules": sorted(modules), "base_model_name_or_path": base_name or cfg.get("model"),
    }, indent=1))
    return len(peft)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("src")
    ap.add_argument("out")
    ap.add_argument("--base-name")
    args = ap.parse_args()
    n = convert(args.src, args.out, args.base_name)
    print(f"wrote {args.out} ({n} tensors)")


if __name__ == "__main__":
    main()
