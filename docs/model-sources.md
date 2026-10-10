# Model sources

Fairspoken downloads its speech models (and, in the desktop app, its local
polish models) from Hugging Face. If your organisation blocks huggingface.co,
host the models on your own network and point Fairspoken at your copy with
the **model source** setting.

It works the same way in every Fairspoken app:

| App | Where to set it |
| --- | --- |
| Desktop app (Windows, Linux, macOS; Tauri) | Settings → Transcription → Advanced → **Model source**, or `modelSource` in `settings.json` |
| Transcription host (`transcription-host`) | `--model-source`, `FAIRSPOKEN_MODEL_SOURCE`, or `modelSource` in `host-config.json` (see [below](#the-transcription-host)) |
| Fairspoken for Mac and Fairspoken Server (Swift) | Their Settings, or `modelSource` in their settings and host config files |

**Test** next to the field in the desktop app checks that the source can
serve the first file of the selected model, without saving anything or
downloading the whole model, and shows either where it found the file or
exactly what failed.

## The three kinds of value

| Value | Meaning |
| --- | --- |
| *(empty)* | Hugging Face, `https://huggingface.co`. The default. |
| `http://…` or `https://…` | The base URL of a **Hugging Face–compatible mirror**. |
| Anything else: an absolute path, `~/…`, or a `file://` URL | A **local or network folder** holding the model files. |

Leading and trailing spaces are ignored, as are trailing slashes. The value
can be at most 2048 characters. These are refused with a message saying why:

- URLs with any other scheme (`ftp://`, `smb://`, `s3://`, …). Mount a network
  share and use its path instead (`/Volumes/models`, `\\server\share\models`).
- URLs with a user name or password in them (`https://user:pass@…`).
- Relative paths (`models`, `./models`, `..\models`).

### A mirror URL

Each file is fetched from

```
{base}/{repo}/resolve/{revision}/{file}
```

exactly as from Hugging Face, with `https://huggingface.co` replaced by your
base URL. For example, with the base `https://mirror.example/hf`, Parakeet
TDT 0.6B v3's encoder comes from

```
https://mirror.example/hf/istupakov/parakeet-tdt-0.6b-v3-onnx/resolve/main/encoder-model.onnx
```

Any server that answers that layout works: an Artifactory or Nexus Hugging
Face remote repository, [hf-mirror](https://hf-mirror.com), Olah, or a plain
static file server whose folders are laid out the same way (one folder per
revision under `resolve/`, so a pinned revision is a folder named after the
commit).

The desktop app also asks the mirror for `{base}/api/models/{repo}` when you
press **Refresh** on the Models screen, to show download counts for the polish
models; a mirror without that API only means no counts are shown.

**The Swift apps need more from a mirror.** Fairspoken for Mac and Fairspoken
Server list a model's files before downloading it, through the Hub's
file-listing API:

```
{base}/api/models/{repo}/tree/{revision}
```

Artifactory, Nexus and hf-mirror serve it; a plain static file server does
not. For the Swift apps, use a folder (below) instead of a static server.

### A folder

The folder holds each repository as `{folder}/{owner}/{repo-name}/`, with the
repository's files inside it, in the layout

```
hf download owner/repo-name --local-dir {folder}/owner/repo-name
```

produces. The revision is not part of the folder layout, so download the
revision listed below. Files are **copied** (never linked) into the app's
usual model directory and get the same size and checksum checks as a download
where the app has them, so a wrong or partial copy is refused rather than
loaded. The folder can be on a network share; it only needs to be readable
while models are being installed.

A missing file is reported with the folder it was looked for in, e.g.
`encoder-model.onnx not found in /Volumes/models/istupakov/parakeet-tdt-0.6b-v3-onnx`.

## The models to mirror

To fill a folder, run these on a machine that can reach Hugging Face (with the
[`hf` CLI](https://huggingface.co/docs/huggingface_hub/guides/cli)), then copy
`$FOLDER` to your share. Fetch only the models your users choose. A mirror
that proxies Hugging Face (Artifactory, Nexus, hf-mirror) needs no
preparation.

### Desktop app and transcription host (Rust)

Speech models:

| Model (setting id) | Repository | Revision | Files |
| --- | --- | --- | --- |
| Parakeet TDT 0.6B v3 (`parakeet-tdt-0.6b-v3`, the default) | `istupakov/parakeet-tdt-0.6b-v3-onnx` | `main` | `encoder-model.onnx`, `encoder-model.onnx.data`, `decoder_joint-model.onnx`, `vocab.txt` |
| Parakeet TDT 0.6B v2, English (`parakeet-tdt-0.6b-v2`) | `istupakov/parakeet-tdt-0.6b-v2-onnx` | `0bbb45a3365852604aef28b538a8f066f4ccaa85` | the same four files |
| Parakeet Ultra (`parakeet-ultra`) | `altunenes/parakeet-rs` | `4d2a8bc71f5c896ec40faa59732e6716295edaf2` | the same four files, in its `parakeet-ultra/` folder |
| Whisper (`tiny`, `base`, `small`, `medium`, `large-v2`, `large-v3`, `large-v3-turbo`) | `ggerganov/whisper.cpp` | `main` | `ggml-{id}.bin`, e.g. `ggml-large-v3-turbo.bin` |

```bash
FOLDER=/srv/fairspoken-models
FILES="encoder-model.onnx encoder-model.onnx.data decoder_joint-model.onnx vocab.txt"

# Parakeet TDT 0.6B v3 (the default)
hf download istupakov/parakeet-tdt-0.6b-v3-onnx $FILES \
  --local-dir "$FOLDER/istupakov/parakeet-tdt-0.6b-v3-onnx"

# Parakeet TDT 0.6B v2 (English)
hf download istupakov/parakeet-tdt-0.6b-v2-onnx $FILES \
  --revision 0bbb45a3365852604aef28b538a8f066f4ccaa85 \
  --local-dir "$FOLDER/istupakov/parakeet-tdt-0.6b-v2-onnx"

# Parakeet Ultra
hf download altunenes/parakeet-rs \
  parakeet-ultra/encoder-model.onnx parakeet-ultra/encoder-model.onnx.data \
  parakeet-ultra/decoder_joint-model.onnx parakeet-ultra/vocab.txt \
  --revision 4d2a8bc71f5c896ec40faa59732e6716295edaf2 \
  --local-dir "$FOLDER/altunenes/parakeet-rs"

# Whisper: one file per model; repeat for each one you use
hf download ggerganov/whisper.cpp ggml-large-v3-turbo.bin \
  --local-dir "$FOLDER/ggerganov/whisper.cpp"
```

Local polish models (desktop app only; one `.gguf` file each):

| Model | Repository | Revision | File |
| --- | --- | --- | --- |
| SpeakoFlow Mini | `SpeakoFlow/speakoflow-mini` | `835431771f72820251fe6c6b4b07f12b000e2647` | `SpeakoFlow-Mini-0.8B-Q8_0.gguf` |
| Qwen3.5 0.8B | `ggml-org/Qwen3.5-0.8B-GGUF` | `8fea620810c4afa23dd6443f999a48574c1611a3` | `Qwen3.5-0.8B-Q4_0.gguf` |
| Qwen3.5 2B | `lmstudio-community/Qwen3.5-2B-GGUF` | `bb84e11355a036e28f080c7793fa6d22b7c4e344` | `Qwen3.5-2B-Q4_K_M.gguf` |
| Qwen3.5 4B | `unsloth/Qwen3.5-4B-GGUF` | `e87f176479d0855a907a41277aca2f8ee7a09523` | `Qwen3.5-4B-Q4_K_M.gguf` |

```bash
hf download SpeakoFlow/speakoflow-mini SpeakoFlow-Mini-0.8B-Q8_0.gguf \
  --revision 835431771f72820251fe6c6b4b07f12b000e2647 \
  --local-dir "$FOLDER/SpeakoFlow/speakoflow-mini"
```

Local polish also needs the llama.cpp runtime, which the desktop app
downloads from the llama.cpp GitHub releases
(`https://github.com/ggml-org/llama.cpp/releases/download/b10930/…`), not
from the model source. Allow that address, or leave local polish off.

When Fairspoken updates a model, its repository or revision in this table
changes too; refresh your folder from the new table after upgrading.

### Fairspoken for Mac and Fairspoken Server (Swift)

The Swift apps run Core ML conversions of the same Parakeet models, published
by FluidInference, at revision `main`, laid out the same way:

| Model | Repository |
| --- | --- |
| Parakeet TDT 0.6B v3 | `FluidInference/parakeet-tdt-0.6b-v3-coreml` |
| Parakeet Ultra | `FluidInference/parakeet-ultra-coreml` |
| Parakeet TDT 0.6B v2 | `FluidInference/parakeet-tdt-0.6b-v2-coreml` |

```bash
hf download FluidInference/parakeet-tdt-0.6b-v3-coreml \
  --local-dir "$FOLDER/FluidInference/parakeet-tdt-0.6b-v3-coreml"
```

One folder can serve every app: the Rust and Swift repositories have
different names, so they sit side by side.

## The transcription host

The standalone `transcription-host` takes the model source from, highest
first:

1. `--model-source <url|folder>` on the command line. `--model-source=""`
   forces Hugging Face over the two below.
2. The `FAIRSPOKEN_MODEL_SOURCE` environment variable (an empty value counts
   as unset).
3. `modelSource` in the host config file (`host-config.json`; the path is in
   `FAIRSPOKEN_HOST_CONFIG_PATH`, or the host's config directory).
4. Hugging Face.

```bash
transcription-host --model-source https://artifactory.example.org/api/huggingfaceml/hf
FAIRSPOKEN_MODEL_SOURCE=/srv/fairspoken-models transcription-host
```

```json
{
  "maxActiveStreams": 4,
  "maxRecordingSeconds": 600,
  "useGpu": true,
  "workerModels": ["parakeet-tdt-0.6b-v3"],
  "modelSource": "https://mirror.example/hf"
}
```

The host prints the source it uses at startup (`Models download from …`) and
refuses to start when the value is invalid, naming where it came from. It
applies to model downloads started from the dashboard. Fairspoken Server
reads the same `modelSource` key from its config file.

## Searching Hugging Face

With a model source set, the desktop app's Models screen searches only the
models on this device and no longer queries the Hugging Face Hub, which your
users are assumed not to reach.
