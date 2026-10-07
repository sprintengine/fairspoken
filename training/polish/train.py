#!/usr/bin/env python3
"""mlx_lm.lora with a memory ceiling, for training on a shared 16 GB Mac.

    python3 -I train.py --config configs/base.yaml [--max-memory-gb 6] [mlx_lm.lora flags...]

MLX's own limits are soft: `set_memory_limit` makes it wait for work to
finish and free buffers before allocating past the limit, and
`set_cache_limit` bounds the buffer cache it keeps for reuse. Neither stops an
allocation that is genuinely needed, so a watchdog also samples MLX's active
plus cached memory and the process's resident size every second and aborts
the run (exit code 3) once either passes the ceiling. Checkpoints are saved
every `save_every` iterations, so an aborted run resumes with
`--resume-adapter-file <adapter_path>/adapters.safetensors`.

The peak is written to <adapter_path>/peak_memory.json when training ends.

Expected peak with the shipped configs (batch 1, 512 tokens, 8 LoRA layers,
gradient checkpointing): about 3-5 GB, an estimate from the model size
(1.5 GB of BF16 weights) and the logits of a 248k-entry vocabulary at 512
tokens, not a measurement. Train when the machine is otherwise idle, or on a
bigger Mac; batch 4 at 1,024 tokens reached 14 GB and was stopped.
"""
import argparse
import json
import os
import subprocess
import sys
import threading
import time
from pathlib import Path

import mlx.core as mx

GB = 1 << 30


def resident_bytes():
    try:
        out = subprocess.run(["ps", "-o", "rss=", "-p", str(os.getpid())],
                             capture_output=True, text=True, timeout=5).stdout
        return int(out.strip() or 0) * 1024
    except (OSError, ValueError, subprocess.SubprocessError):
        return 0


def watchdog(limit, peak):
    while True:
        mlx_bytes = mx.get_active_memory() + mx.get_cache_memory()
        rss = resident_bytes()
        peak["mlx"] = max(peak["mlx"], mlx_bytes)
        peak["rss"] = max(peak["rss"], rss)
        if max(mlx_bytes, rss) > limit:
            print(f"\ntrain.py: memory {max(mlx_bytes, rss) / GB:.1f} GB passed the "
                  f"{limit / GB:.1f} GB ceiling; stopping. Lower max_seq_length or "
                  "num_layers, or raise --max-memory-gb on a bigger machine.", file=sys.stderr, flush=True)
            os._exit(3)
        time.sleep(1)


def main():
    ap = argparse.ArgumentParser(add_help=False)
    ap.add_argument("--max-memory-gb", type=float, default=6.0)
    ap.add_argument("--cache-limit-gb", type=float, default=1.0)
    ours, rest = ap.parse_known_args()
    limit = int(ours.max_memory_gb * GB)
    # Leave headroom under the ceiling for what MLX does not count (Python,
    # the tokenizer, the dataset).
    mx.set_memory_limit(int(limit * 0.85))
    mx.set_cache_limit(int(ours.cache_limit_gb * GB))
    peak = {"mlx": 0, "rss": 0}
    threading.Thread(target=watchdog, args=(limit, peak), daemon=True).start()

    from mlx_lm import lora

    sys.argv = ["mlx_lm.lora", *rest]
    started = time.time()
    try:
        lora.main()
    finally:
        adapter_path = None
        if "--adapter-path" in rest:
            adapter_path = rest[rest.index("--adapter-path") + 1]
        elif "--config" in rest:
            import yaml
            cfg = yaml.safe_load(Path(rest[rest.index("--config") + 1]).read_text())
            adapter_path = cfg.get("adapter_path")
        report = {"seconds": round(time.time() - started), "peak_mlx_gb": round(mx.get_peak_memory() / GB, 2),
                  "peak_sampled_mlx_gb": round(peak["mlx"] / GB, 2), "peak_rss_gb": round(peak["rss"] / GB, 2),
                  "ceiling_gb": ours.max_memory_gb}
        print(json.dumps(report))
        if adapter_path and Path(adapter_path).is_dir():
            (Path(adapter_path) / "peak_memory.json").write_text(json.dumps(report, indent=1))


if __name__ == "__main__":
    main()
