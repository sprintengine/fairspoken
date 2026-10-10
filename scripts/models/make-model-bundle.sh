#!/usr/bin/env bash
# Builds the .zip a Fairspoken app installs a model from: "Download from
# link…" on the Models screen, or --model-link / FAIRSPOKEN_MODEL_LINKS /
# modelLinks for a transcription host. Run it on a machine that can reach
# huggingface.co, then put the zip on your own server or share.
#
#   scripts/models/make-model-bundle.sh <model-id> <desktop|mac> <out.zip>
#
#   desktop  the Windows/Linux/macOS desktop app and its transcription-host
#            (ONNX, whisper.cpp and GGUF files)
#   mac      the native macOS apps, Fairspoken and Fairspoken Server
#            (Core ML bundles)
#
# The zip holds exactly the files that app needs for that model, at its root;
# Core ML .mlmodelc bundles stay folders. See docs/model-sources.md.
#
# Needs bash, curl, and zip or python3 (plus python3 or node for the mac
# bundles' file lists). HF_TOKEN, when set, is sent as a bearer token.
set -euo pipefail

HUB="https://huggingface.co"

usage() {
  cat >&2 <<'EOF'
Usage: make-model-bundle.sh <model-id> <desktop|mac> <out.zip>

desktop model ids:
  parakeet-tdt-0.6b-v3  parakeet-tdt-0.6b-v2  parakeet-ultra
  tiny  base  small  medium  large-v2  large-v3  large-v3-turbo   (Whisper)
  speakoflow-mini  qwen3.5-0.8b  qwen3.5-2b  qwen3.5-4b            (text polish)
mac model ids:
  parakeet-tdt-0.6b-v3  parakeet-tdt-0.6b-v2  parakeet-ultra
EOF
  exit 2
}

[ "$#" -eq 3 ] || usage
model="$1"
app="$2"
out="$3"

# What to fetch: repo and revision, a folder inside the repo (or ""), plain
# files, and (mac) .mlmodelc bundles, which are folders of files.
repo="" rev="main" subdir="" files=() bundles=()
case "$app:$model" in
  desktop:parakeet-tdt-0.6b-v3)
    repo="istupakov/parakeet-tdt-0.6b-v3-onnx"
    files=(encoder-model.onnx encoder-model.onnx.data decoder_joint-model.onnx vocab.txt) ;;
  desktop:parakeet-tdt-0.6b-v2)
    repo="istupakov/parakeet-tdt-0.6b-v2-onnx" rev="0bbb45a3365852604aef28b538a8f066f4ccaa85"
    files=(encoder-model.onnx encoder-model.onnx.data decoder_joint-model.onnx vocab.txt) ;;
  desktop:parakeet-ultra)
    repo="altunenes/parakeet-rs" rev="4d2a8bc71f5c896ec40faa59732e6716295edaf2" subdir="parakeet-ultra"
    files=(encoder-model.onnx encoder-model.onnx.data decoder_joint-model.onnx vocab.txt) ;;
  desktop:tiny|desktop:base|desktop:small|desktop:medium|desktop:large-v2|desktop:large-v3|desktop:large-v3-turbo)
    repo="ggerganov/whisper.cpp"
    files=("ggml-$model.bin") ;;
  desktop:speakoflow-mini)
    repo="SpeakoFlow/speakoflow-mini" rev="835431771f72820251fe6c6b4b07f12b000e2647"
    files=(SpeakoFlow-Mini-0.8B-Q8_0.gguf) ;;
  desktop:qwen3.5-0.8b)
    repo="ggml-org/Qwen3.5-0.8B-GGUF" rev="8fea620810c4afa23dd6443f999a48574c1611a3"
    files=(Qwen3.5-0.8B-Q4_0.gguf) ;;
  desktop:qwen3.5-2b)
    repo="lmstudio-community/Qwen3.5-2B-GGUF" rev="bb84e11355a036e28f080c7793fa6d22b7c4e344"
    files=(Qwen3.5-2B-Q4_K_M.gguf) ;;
  desktop:qwen3.5-4b)
    repo="unsloth/Qwen3.5-4B-GGUF" rev="e87f176479d0855a907a41277aca2f8ee7a09523"
    files=(Qwen3.5-4B-Q4_K_M.gguf) ;;
  mac:parakeet-tdt-0.6b-v3)
    repo="FluidInference/parakeet-tdt-0.6b-v3-coreml"
    bundles=(Preprocessor.mlmodelc Encoder.mlmodelc Decoder.mlmodelc JointDecisionv3.mlmodelc)
    files=(parakeet_vocab.json) ;;
  mac:parakeet-ultra)
    repo="FluidInference/parakeet-ultra-coreml"
    bundles=(Preprocessor.mlmodelc Encoder.mlmodelc Decoder.mlmodelc JointDecisionv3.mlmodelc)
    files=(parakeet_vocab.json) ;;
  mac:parakeet-tdt-0.6b-v2)
    repo="FluidInference/parakeet-tdt-0.6b-v2-coreml"
    bundles=(Preprocessor.mlmodelc Encoder.mlmodelc Decoder.mlmodelc JointDecision.mlmodelc)
    files=(parakeet_vocab.json) ;;
  desktop:*|mac:*)
    echo "Unknown model id for the $app app: $model" >&2
    usage ;;
  *)
    echo "The app must be desktop or mac, not $app" >&2
    usage ;;
esac

command -v curl >/dev/null || { echo "This needs curl." >&2; exit 1; }
if command -v python3 >/dev/null; then json=python3
elif command -v node >/dev/null; then json=node
else json=""
fi
if [ "${#bundles[@]}" -gt 0 ] && [ -z "$json" ]; then
  echo "The mac bundles need python3 or node to read Hugging Face's file lists." >&2
  exit 1
fi
if ! command -v zip >/dev/null && ! command -v python3 >/dev/null; then
  echo "This needs zip or python3 to write the zip." >&2
  exit 1
fi

# The output path, absolute (the zip is written from inside the staging folder).
out_dir="$(dirname -- "$out")"
mkdir -p -- "$out_dir"
out="$(cd -- "$out_dir" && pwd)/$(basename -- "$out")"
case "$out" in *.zip) ;; *) echo "The output must end in .zip: $out" >&2; exit 2 ;; esac

work="$(mktemp -d "${TMPDIR:-/tmp}/fairspoken-bundle.XXXXXX")"
trap 'rm -rf -- "$work"' EXIT
stage="$work/bundle"
mkdir -p -- "$stage"

auth=()
if [ -n "${HF_TOKEN:-}" ]; then auth=(-H "Authorization: Bearer $HF_TOKEN"); fi

# fetch <path in repo> <destination> [expected size]
fetch() {
  local path="$1" dest="$2" size="${3:-}"
  mkdir -p -- "$(dirname -- "$dest")"
  echo "  $path" >&2
  curl --fail --location --retry 3 --retry-delay 2 --show-error --progress-bar \
    ${auth[@]+"${auth[@]}"} -o "$dest" "$HUB/$repo/resolve/$rev/$path" </dev/null
  if [ -n "$size" ]; then
    local got
    got="$(wc -c <"$dest" | tr -d ' ')"
    if [ "$got" != "$size" ]; then
      echo "$path: got $got bytes, expected $size" >&2
      exit 1
    fi
  fi
}

# Every file under <folder> in the repo, as "path<TAB>size" lines, from the
# Hub's tree API (what FluidAudio itself reads).
list_files() {
  local listing="$work/listing.json"
  curl --fail --location --retry 3 --retry-delay 2 --silent --show-error \
    ${auth[@]+"${auth[@]}"} -o "$listing" "$HUB/api/models/$repo/tree/$rev/$1?recursive=true"
  if [ "$json" = python3 ]; then
    python3 -I -c '
import json, sys
for entry in json.load(open(sys.argv[1])):
    if entry.get("type") == "file":
        print(entry["path"] + "\t" + str(entry.get("size", "")))
' "$listing"
  else
    # shellcheck disable=SC2016 # JavaScript, not shell.
    node -e '
for (const entry of JSON.parse(require("fs").readFileSync(process.argv[1], "utf8")))
  if (entry.type === "file") console.log(`${entry.path}\t${entry.size ?? ""}`);
' "$listing"
  fi
}

echo "Downloading $model for the $app app from $HUB/$repo" >&2
prefix="${subdir:+$subdir/}"
entries=()
for name in ${files[@]+"${files[@]}"}; do
  fetch "$prefix$name" "$stage/$name"
  entries+=("$name")
done
for bundle in ${bundles[@]+"${bundles[@]}"}; do
  list_files "$bundle" >"$work/files.tsv"
  count=0
  while IFS="$(printf '\t')" read -r path size; do
    [ -n "$path" ] || continue
    case "$path" in "$bundle"/*) ;; *) echo "Unexpected path in $bundle: $path" >&2; exit 1 ;; esac
    case "$path" in */../*|../*|*/..) echo "Unsafe path in $bundle: $path" >&2; exit 1 ;; esac
    fetch "$path" "$stage/$path" "$size"
    count=$((count + 1))
  done <"$work/files.tsv"
  if [ "$count" -eq 0 ]; then
    echo "$repo has no files in $bundle" >&2
    exit 1
  fi
  entries+=("$bundle")
done

rm -f -- "$out"
echo "Writing $out" >&2
if command -v zip >/dev/null; then
  # Stored, not compressed: model weights don't shrink, and storing is fast.
  (cd -- "$stage" && zip -X -r -0 -q "$out" -- "${entries[@]}")
else
  (cd -- "$stage" && python3 -I -c '
import os, sys, zipfile
with zipfile.ZipFile(sys.argv[1], "w", zipfile.ZIP_STORED, allowZip64=True) as z:
    for entry in sys.argv[2:]:
        if os.path.isdir(entry):
            for folder, _, names in os.walk(entry):
                for name in sorted(names):
                    z.write(os.path.join(folder, name))
        else:
            z.write(entry)
' "$out" "${entries[@]}")
fi

bytes="$(wc -c <"$out" | tr -d ' ')"
echo "Done: $out ($bytes bytes). It holds:" >&2
for entry in "${entries[@]}"; do echo "  $entry" >&2; done
