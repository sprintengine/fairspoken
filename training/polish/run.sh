#!/usr/bin/env bash
# Retrain the local polish model for the tagged contract, and build a per-pack
# LoRA adapter, end to end:
#
#   training/polish/run.sh all            # everything below, in order
#   training/polish/run.sh <step> ...     # one or more steps
#
# Steps: setup fetch base-model data train-base export-base train-pack export-pack load-check eval
#
# Everything large lives in $FAIRSPOKEN_TRAIN (default
# /Volumes/workdrive/fairspoken-train), never in the repo. Each download goes
# into its own directory and is checked against a pinned SHA-256.
#
# Memory: train-base and train-pack run train.py with a 6 GB ceiling
# (FAIRSPOKEN_TRAIN_MAX_GB); expect roughly 3-5 GB at the shipped configs.
# Conversion peaks at about 2.6 GB and llama-server at 1-2 GB. Run it when the
# machine is otherwise idle, or on a bigger Mac.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$HERE/../.." && pwd)"
W="${FAIRSPOKEN_TRAIN:-/Volumes/workdrive/fairspoken-train}"
TRAIN_VENV="${FAIRSPOKEN_TRAIN_VENV:-$W/venv-train}"
CONVERT_VENV="${FAIRSPOKEN_CONVERT_VENV:-$W/venv-convert}"
PY="${PYTHON:-python3}"
MAX_GB="${FAIRSPOKEN_TRAIN_MAX_GB:-6}"
LLAMA_TAG=b10930
LLAMA="$W/llama.cpp/llama.cpp-$LLAMA_TAG"
RUNTIME="$W/runtime/llama-$LLAMA_TAG"
PORT="${FAIRSPOKEN_EVAL_PORT:-48931}"

QWEN_REPO=Qwen/Qwen3.5-0.8B
QWEN_REV=2fc06364715b967f1860aea9cf38778875588b17
QWEN_SHA=04b1c301231dd422b8860db31311ab2721511346a32cb1e079c4c4e5f1fe4696
SF_REV=835431771f72820251fe6c6b4b07f12b000e2647
SF_BF16_SHA=0b62e20d2a411d2a08880422ca604cb312c51772e9f9a18b504b94e9695abcdb
SF_Q8_SHA=696769bb6911f51bc231b112926e934cf7bfc760e6cdfa24212907bc5ad41fc9
# The runtime the app downloads (local_models.rs runtime_asset).
RUNTIME_ASSET=llama-$LLAMA_TAG-bin-macos-arm64.tar.gz
RUNTIME_SHA=0f3f18c106841b11fab65b039b2b7e05d83c74e3063b4de631b13c13ea4efc19
# llama.cpp's source at the same tag (GitHub's tag archive), for the converters.
LLAMA_SRC_SHA=d05ac4c8e2bf1ea5f7271d9d49d9186b65b7deafcb6e44d1fd9447eaa1c7b191
# Qwen3.5-0.8B's side files at QWEN_REV (checked against the Hub's blob ids).
QWEN_FILES="
config.json b90b86f35c8e6925ef74ee04d0e758f0a845c83a42089ad82bbaa948de9b4204
chat_template.jinja 273d8e0e683b885071fb17e08d71e5f2a5ddfb5309756181681de4f5a1822d80
merges.txt a9d356d7bdf1ef4949e3e748e95b8e10ad9d4e2e838eddc38a0a7b6b94d1db8d
model.safetensors.index.json d8a08838a613b025eb7952ed9db11696213e57e76a375661ef5c12f9dd5dcf4e
tokenizer.json 5f9e4d4901a92b997e463c1f46055088b6cca5ca61a6522d1b9f64c4bb81cb42
tokenizer_config.json 49e2b6e395f959f077f1e992b338919c0d4a9732fc6e613995e06557f843500c
vocab.json ce99b4cb2983d118806ce0a8b777a35b093e2000a503ebde25853284c9dfa003
LICENSE bbedc3fda3305820b977265f01b8619d87570a6739de3a5582c3464840f1e57a
preprocessor_config.json 27225450ac9c6529872ee1924fcb0962ff5634834f817040f444118116f4e516
video_preprocessor_config.json 7768af27c1fafa9cc9011c1dc20067e03f8915e03b63504550e11d5066986d13
"

tpy() { "$TRAIN_VENV/bin/python" -I "$@"; }
cpy() { "$CONVERT_VENV/bin/python" -I "$@"; }
# llama.cpp's converters import their sibling `conversion` package, so they
# cannot run isolated (-I); -s -E still ignores user site and environment.
convert_hf() { "$CONVERT_VENV/bin/python" -s -E "$LLAMA/convert_hf_to_gguf.py" "$@"; }
convert_lora() { "$CONVERT_VENV/bin/python" -s -E "$LLAMA/convert_lora_to_gguf.py" "$@"; }

fetch_one() { # url dest sha256
  local url=$1 dest=$2 sha=$3
  mkdir -p "$(dirname "$dest")"
  if [ ! -f "$dest" ] || ! echo "$sha  $dest" | shasum -a 256 -c - >/dev/null 2>&1; then
    curl -fL --retry 3 -o "$dest.partial" "$url"
    echo "$sha  $dest.partial" | shasum -a 256 -c -
    mv "$dest.partial" "$dest"
  fi
}

step_setup() {
  [ -x "$TRAIN_VENV/bin/python" ] || "$PY" -m venv "$TRAIN_VENV"
  "$TRAIN_VENV/bin/pip" install -q -r "$HERE/requirements.txt"
  [ -x "$CONVERT_VENV/bin/python" ] || "$PY" -m venv "$CONVERT_VENV"
  "$CONVERT_VENV/bin/pip" install -q -r "$HERE/requirements-convert.txt"
}

step_fetch() {
  local d="$W/downloads"
  fetch_one "https://huggingface.co/SpeakoFlow/speakoflow-mini/resolve/$SF_REV/SpeakoFlow-Mini-0.8B-BF16.gguf" \
    "$d/speakoflow-bf16/SpeakoFlow-Mini-0.8B-BF16.gguf" "$SF_BF16_SHA"
  fetch_one "https://huggingface.co/SpeakoFlow/speakoflow-mini/resolve/$SF_REV/SpeakoFlow-Mini-0.8B-Q8_0.gguf" \
    "$d/speakoflow-q8/SpeakoFlow-Mini-0.8B-Q8_0.gguf" "$SF_Q8_SHA"
  fetch_one "https://huggingface.co/$QWEN_REPO/resolve/$QWEN_REV/model.safetensors-00001-of-00001.safetensors" \
    "$d/qwen3.5-0.8b-hf/model.safetensors-00001-of-00001.safetensors" "$QWEN_SHA"
  local f sha
  while read -r f sha; do
    [ -n "$f" ] || continue
    fetch_one "https://huggingface.co/$QWEN_REPO/resolve/$QWEN_REV/$f" "$d/qwen3.5-0.8b-hf/$f" "$sha" </dev/null
  done <<<"$QWEN_FILES"
  fetch_one "https://github.com/ggml-org/llama.cpp/releases/download/$LLAMA_TAG/$RUNTIME_ASSET" \
    "$d/llama-$LLAMA_TAG-bin/$RUNTIME_ASSET" "$RUNTIME_SHA"
  mkdir -p "$W/runtime" "$W/llama.cpp"
  fetch_one "https://github.com/ggml-org/llama.cpp/archive/refs/tags/$LLAMA_TAG.tar.gz" \
    "$d/llama.cpp-src/$LLAMA_TAG.tar.gz" "$LLAMA_SRC_SHA"
  [ -d "$RUNTIME" ] || tar -xzf "$d/llama-$LLAMA_TAG-bin/$RUNTIME_ASSET" -C "$W/runtime"
  [ -d "$LLAMA" ] || tar -xzf "$d/llama.cpp-src/$LLAMA_TAG.tar.gz" -C "$W/llama.cpp"
  "$CONVERT_VENV/bin/pip" install -q "$LLAMA/gguf-py"
}

step_base_model() {
  # The best starting point: SpeakoFlow Mini's merged weights, rebuilt as
  # safetensors and checked against the base model's own conversion.
  mkdir -p "$W/work" "$W/models"
  [ -f "$W/work/qwen3.5-0.8b-base-bf16.gguf" ] || convert_hf "$W/downloads/qwen3.5-0.8b-hf" --outtype bf16 \
    --outfile "$W/work/qwen3.5-0.8b-base-bf16.gguf"
  cpy "$HERE/speakoflow_to_hf.py" "$W/downloads/qwen3.5-0.8b-hf" \
    "$W/downloads/speakoflow-bf16/SpeakoFlow-Mini-0.8B-BF16.gguf" "$W/models/speakoflow-mini-hf" \
    --check "$W/work/qwen3.5-0.8b-base-bf16.gguf"
}

step_data() {
  local extra=()
  # Train on my own dictations: FAIRSPOKEN_MY_DICTATIONS=<export zip> adds them.
  if [ -n "${FAIRSPOKEN_MY_DICTATIONS:-}" ]; then
    tpy "$HERE/import_dictations.py" "$FAIRSPOKEN_MY_DICTATIONS" --out "$W/data/my-dictations.jsonl"
    extra=(--extra-train "$W/data/my-dictations.jsonl")
  fi
  tpy "$HERE/generate_data.py" --out "$W/data/base" ${extra[@]+"${extra[@]}"}
  tpy "$HERE/generate_data.py" --profile ie-general-practice --train 1500 --valid 100 --test 200 \
    --out "$W/data/ie-general-practice"
}

step_train_base() {
  (cd "$W" && tpy "$HERE/train.py" --max-memory-gb "$MAX_GB" --config "$HERE/configs/base.yaml")
}

step_export_base() {
  cpy "$HERE/mlx_to_peft.py" "$W/adapters/base" "$W/adapters/base-peft" --base-name "$W/models/speakoflow-mini-hf"
  cpy "$HERE/merge_adapter.py" "$W/models/speakoflow-mini-hf" "$W/adapters/base-peft" "$W/models/fairspoken-polish-hf"
  mkdir -p "$W/export"
  convert_hf "$W/models/fairspoken-polish-hf" --outtype bf16 --outfile "$W/export/fairspoken-polish-0.8b-bf16.gguf"
  "$RUNTIME/llama-quantize" "$W/export/fairspoken-polish-0.8b-bf16.gguf" "$W/export/fairspoken-polish-0.8b-Q8_0.gguf" Q8_0
  shasum -a 256 "$W/export/fairspoken-polish-0.8b-Q8_0.gguf"
}

step_train_pack() {
  (cd "$W" && tpy "$HERE/train.py" --max-memory-gb "$MAX_GB" --config "$HERE/configs/pack-ie-general-practice.yaml")
}

step_export_pack() {
  cpy "$HERE/mlx_to_peft.py" "$W/adapters/ie-general-practice" "$W/adapters/ie-general-practice-peft" \
    --base-name "$W/models/fairspoken-polish-hf"
  convert_lora "$W/adapters/ie-general-practice-peft" --base "$W/models/fairspoken-polish-hf" --outtype f16 \
    --outfile "$W/export/ie-general-practice.lora.gguf"
  cpy "$HERE/write_adapter_manifest.py" "$W/export/ie-general-practice.lora.gguf" \
    --base "$W/export/fairspoken-polish-0.8b-Q8_0.gguf" --pack ie-general-practice
}

SERVER=
serve() { # model [extra llama-server args...]; the app's own flags (local_models.rs)
  local model=$1; shift
  local log="$W/work/server-$(basename "$model").log"
  if curl -sf "http://127.0.0.1:$PORT/health" >/dev/null 2>&1; then
    echo "a server already answers on port $PORT; stop it or set FAIRSPOKEN_EVAL_PORT" >&2
    return 1
  fi
  mkdir -p "$W/work"
  # exec, so $SERVER is llama-server itself and stop() reaches it.
  (cd "$RUNTIME" && exec ./llama-server --model "$model" --host 127.0.0.1 --port "$PORT" --ctx-size 8192 --parallel 1 \
    --reasoning off --spec-type ngram-simple --spec-ngram-simple-size-n 2 --spec-ngram-simple-size-m 24 \
    --no-webui "$@" >"$log" 2>&1) &
  SERVER=$!
  trap stop EXIT
  for _ in $(seq 1 300); do
    kill -0 "$SERVER" 2>/dev/null || { echo "llama-server exited; see $log" >&2; return 1; }
    curl -sf "http://127.0.0.1:$PORT/health" >/dev/null && return 0
    sleep 0.3
  done
  echo "llama-server did not start; see $log" >&2
  return 1
}
stop() {
  [ -n "$SERVER" ] || return 0
  kill "$SERVER" 2>/dev/null || true
  wait "$SERVER" 2>/dev/null || true
  SERVER=
}

step_load_check() {
  # The merged model and the pack adapter load in the runtime the app uses,
  # the adapter starts disabled, and a request can switch it on.
  serve "$W/export/fairspoken-polish-0.8b-Q8_0.gguf" --lora "$W/export/ie-general-practice.lora.gguf" --lora-init-without-apply
  local url="http://127.0.0.1:$PORT" adapters off on
  local body='{"messages":[{"role":"user","content":"<format>notes</format>\n<transcript>start ramipril 5 milligrams once daily</transcript>\n\nOutput only the cleaned transcript."}],"temperature":0,"chat_template_kwargs":{"enable_thinking":false}'
  adapters=$(curl -sf "$url/lora-adapters")
  off=$(curl -sf "$url/v1/chat/completions" -H 'Content-Type: application/json' -d "$body,\"lora\":[]}")
  on=$(curl -sf "$url/v1/chat/completions" -H 'Content-Type: application/json' -d "$body,\"lora\":[{\"id\":0,\"scale\":1.0}]}")
  stop
  echo "$adapters"
  tpy - "$adapters" "$off" "$on" <<'EOF'
import json, sys
adapters, off, on = (json.loads(a) for a in sys.argv[1:4])
assert len(adapters) == 1 and adapters[0]["id"] == 0 and adapters[0]["path"].endswith("ie-general-practice.lora.gguf"), \
    f"expected the one ie-general-practice adapter, got {adapters}"
assert adapters[0]["scale"] == 0, f"the adapter should load disabled (scale 0), got {adapters[0]['scale']}"
text = lambda r: r["choices"][0]["message"]["content"].strip()
print(f"without the adapter: {text(off)!r}\nwith the adapter:    {text(on)!r}")
assert text(off) != text(on), "switching the adapter on changed nothing"
EOF
}

step_eval() {
  local out="$W/eval"
  rm -rf "$out"  # --score reads every JSON here; never mix in an earlier run's
  mkdir -p "$out"
  tpy "$HERE/eval.py" --baseline "$W/data/base/test.jsonl" | tee "$out/baseline.md"
  serve "$W/downloads/speakoflow-q8/SpeakoFlow-Mini-0.8B-Q8_0.gguf"
  tpy "$HERE/eval.py" --port "$PORT" --variant speakoflow --name "SpeakoFlow Mini Q8_0 (raw prompt)" \
    --data "$W/data/base/test.jsonl" --json "$out/speakoflow.json"
  stop
  for gguf in ${FAIRSPOKEN_EVAL_INSTRUCTED:-}; do  # e.g. the app's Qwen3.5 0.8B / 2B GGUFs
    serve "$gguf"
    tpy "$HERE/eval.py" --port "$PORT" --variant instructed-tags --name "$(basename "$gguf") instructed+tags" \
      --data "$W/data/base/test.jsonl" --json "$out/instructed-$(basename "$gguf").json"
    stop
  done
  serve "$W/export/fairspoken-polish-0.8b-Q8_0.gguf" --lora "$W/export/ie-general-practice.lora.gguf" --lora-init-without-apply
  tpy "$HERE/eval.py" --port "$PORT" --variant tagged --name "Fairspoken polish 0.8B Q8_0 (tagged)" \
    --data "$W/data/base/test.jsonl" --json "$out/tagged.json"
  tpy "$HERE/eval.py" --port "$PORT" --variant tagged --no-bench --name "tagged, GP test, no adapter" \
    --data "$W/data/ie-general-practice/test.jsonl" --json "$out/gp-base.json"
  tpy "$HERE/eval.py" --port "$PORT" --variant tagged --no-bench --lora 0 --name "tagged + ie-general-practice adapter, GP test" \
    --data "$W/data/ie-general-practice/test.jsonl" --json "$out/gp-adapter.json"
  tpy "$HERE/eval.py" --port "$PORT" --variant tagged --no-bench --lora 0 --name "tagged + adapter, general test" \
    --data "$W/data/base/test.jsonl" --json "$out/general-adapter.json"
  stop
  tpy "$HERE/eval.py" --score "$out"/*.json | tee "$out/table.md"
}

steps=("$@")
[ ${#steps[@]} -gt 0 ] || { sed -n '2,19p' "$0"; exit 1; }
[ "${steps[0]}" = all ] && steps=(setup fetch base-model data train-base export-base train-pack export-pack load-check eval)
for s in "${steps[@]}"; do
  echo "== $s"
  "step_${s//-/_}"
done
