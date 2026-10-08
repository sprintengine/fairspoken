# Polish model training

Retrains Fairspoken's local polish model so it understands the
`<spelling>`, `<format>` and `<tone>` tags, and builds small per-pack LoRA
adapters (the first is `ie-general-practice`). Nothing here runs inside the
app; the app only loads the files it produces.

## What a LoRA is

A LoRA ("low-rank adaptation") is a small add-on file, a few megabytes, that
adjusts how a base model behaves without changing the base model itself.
Training one means showing the model thousands of **examples** of an input
and the output we want (a raw transcript and its cleaned version) and
nudging a small set of extra weights until it produces those outputs. It
learns *how to behave*: which edits to make, which to leave alone, how an
email is laid out, that "500 milligrams" is written "500 mg" in a GP letter.

It is not trained from word lists. Give it a list of drug names and nothing
happens: there is no input→output behaviour in a list to learn. A term only
gets learned as a side effect of appearing in many examples, and then only
unreliably, and a model that has "learned" a term is more likely to write it
when it was not said, which is exactly what polish must never do.

So vocabulary keeps its own path: the pack and dictionary terms go through
retrieval (only the terms that sound like something in the transcript), into
the `<spelling>` tag, where the model has been trained to use a spelling only
for a word that was actually spoken. LoRA teaches the behaviour around it:
restraint, formats, house style, and with your own data, your habits. See
`docs/polish-lora.md` for the longer answer.

## The contract

The model's user turn is built by `contract.py` (and, identically, by
`tagged_user_message` in `src-tauri/src/local_models.rs`; both are tested
against `fixtures/contract.json`):

```text
<spelling>term, term</spelling>
<format>email|chat|document|notes|code|plain</format>
<tone>casual|neutral|formal</tone>
<transcript>…</transcript>

Output only the cleaned transcript.
```

Every tag is optional; an omitted tag means no terms, plain, neutral.

## Run it

One command, on an Apple Silicon Mac with ~6 GB of memory free:

```bash
training/polish/run.sh all
```

or step by step: `setup fetch base-model data train-base export-base
train-pack export-pack load-check eval`. Artefacts go to `$FAIRSPOKEN_TRAIN`
(default `/Volumes/workdrive/fairspoken-train`), never the repo. Python 3.12+
(`PYTHON=python3.14 run.sh setup` if the default is older).

| step | what it does | time / memory (expected) |
|---|---|---|
| setup | two venvs: training (mlx-lm) and conversion (llama.cpp's converters need transformers 4.x) | minutes |
| fetch | Qwen3.5-0.8B, SpeakoFlow Mini BF16 and Q8_0, llama.cpp b10930 (the app's runtime) and its source; all pinned by revision and SHA-256 | ~4 GB download |
| base-model | rebuilds SpeakoFlow Mini as safetensors (`speakoflow_to_hf.py --check`) | 1 min, 2.6 GB (measured) |
| data | `generate_data.py`: base 6,000 train / 200 valid / 400 held-out test; GP 1,500 / 100 / 200 | seconds |
| train-base | `train.py --config configs/base.yaml` | ~1-2 h on an M-series Mac (estimate), 3-5 GB, capped at 6 GB |
| export-base | adapter → PEFT → merge in HF layout → `convert_hf_to_gguf.py` → `llama-quantize Q8_0` | minutes, ~3 GB |
| train-pack | `configs/pack-ie-general-practice.yaml` on the merged model | ~20-30 min (estimate), same cap |
| export-pack | `convert_lora_to_gguf.py` → `ie-general-practice.lora.gguf` + manifest | seconds |
| load-check | starts the app's llama-server with `--lora … --lora-init-without-apply`, fails unless `/lora-adapters` lists the adapter at scale 0 and a polish with it differs from one without | 1-2 GB |
| eval | SpeakoFlow Mini (its own raw prompt), any `FAIRSPOKEN_EVAL_INSTRUCTED` GGUFs with the instructed prompt plus tags, and the retrained model with and without the adapter | ~10 min |

Train when the machine is otherwise idle. `train.py` sets MLX's memory and
cache limits and aborts above `FAIRSPOKEN_TRAIN_MAX_GB` (default 6), so a run
cannot take the whole machine; the shipped configs (batch 1, 512 tokens,
8 LoRA layers, gradient checkpointing) are sized for that. An earlier
batch 4, 1,024-token configuration reached 14 GB on a 16 GB Mac and was
stopped. For a faster run, use a bigger Mac and raise `batch_size` and
`FAIRSPOKEN_TRAIN_MAX_GB` together, or train the same data with PEFT on a
cloud GPU (the export step reads a PEFT adapter directly).

### Train on my own dictations

The opt-in training-data export (Settings → General → Training data, format
in `docs/training-data.md`) can be added to the training set:

```bash
FAIRSPOKEN_MY_DICTATIONS=~/Downloads/fairspoken-training-data-<n>.zip training/polish/run.sh data train-base export-base eval
```

`import_dictations.py` turns each dictation into raw → edited (falling back
to what was inserted), skips terminal dictations and snippet expansions,
tags the format from the app category, and tops up no-change pairs to 40%.
The result is personal data: keep it and every model trained from it on your
device.

### Use the result in the app

Settings → Text polish → Developer → **Use a local polish model file**: point
it at `export/fairspoken-polish-0.8b-Q8_0.gguf` with the prompt **Tagged**.
For the adapter, add `export/ie-general-practice.lora.gguf` under the
developer adapter list (the manifest next to it must name that exact model
file by SHA-256, or the app polishes without it). Nothing is published.

## Files

| file | purpose |
|---|---|
| `contract.py` | the tagged user turn and the tag-trained system prompt |
| `generate_data.py` | deterministic synthetic data; `--pack <pack.json>` adds a vocabulary pack's terms and passages |
| `import_dictations.py` | the "train on my own dictations" importer |
| `speakoflow_to_hf.py` | SpeakoFlow Mini GGUF → safetensors, verified tensor by tensor |
| `train.py`, `configs/` | mlx-lm LoRA with a memory ceiling |
| `mlx_to_peft.py`, `merge_adapter.py` | adapter layout conversion and merge, avoiding `mlx_lm.fuse` |
| `write_adapter_manifest.py` | the manifest the app checks before loading an adapter |
| `eval.py` | held-out metrics and the polish bench's regex suite |
| `test_pipeline.py` | cheap checks of the above (`python3 -I test_pipeline.py`) |

## How SpeakoFlow Mini was built, and why we start from it

SpeakoFlow publishes GGUF files only (BF16 to Q4_K_M), an Apache-2.0 licence,
its system prompt and 105 example cases (`SpeakoFlow/dictation-cleanup-examples`,
CC-BY-4.0). The LoRA, merged safetensors, training data and recipe are not
public; the card says Qwen3.5-0.8B plus a rank-16 LoRA, merged, then
quantised. Converting the base model with llama.cpp b10930 and comparing
tensor by tensor shows the BF16 GGUF differs only in the full-attention
q/k/v/o and MLP projections (96 tensors), which the converter stores
verbatim, plus 1-ULP float32 rounding in `ssm_a`. `speakoflow_to_hf.py`
copies those 96 tensors back into the Qwen checkpoint; re-converting the
result reproduces the published GGUF bit for bit apart from that rounding.
So the retrain starts from SpeakoFlow's cleanup skill, not from the base
model's 4.9% edit accuracy.

## Known pitfalls handled

- `mlx_lm.fuse --export-gguf` does not support Qwen; `fuse` output is MLX's
  sanitised layout (norms +1, moved conv axes), which the GGUF converter
  misreads. `merge_adapter.py` merges in the original layout instead.
- Qwen3.5's linear-attention projections are reordered and fused on GGUF
  export, which is where LoRA conversion breaks. The configs train only the
  projections stored verbatim, as SpeakoFlow did.
- The training and serving chat templates both render
  `<think>\n\n</think>\n\n` before the answer with thinking disabled, so the
  model trains on what llama-server sends.
