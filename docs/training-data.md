# Training data capture and export

Fairspoken can keep your dictations so you can later fine-tune a speech
model or train a polish model on them. This is off by default. Turn it on in
**Settings → General → Training data → Keep my dictations to improve models**.

Kept dictations never leave the device unless you export them yourself. They
may contain sensitive or patient information: do not turn this on for
clinical dictation unless your practice's data protection arrangements allow
it (see "Privacy and regulation" in the model comparison spec).

## What is kept

For each dictation:

| Field | Meaning |
|---|---|
| audio | 16 kHz mono 16-bit PCM WAV of the whole recording |
| raw text | the speech model's transcript, before polish and corrections |
| polished text | the polish model's output, when polish ran and changed anything |
| final text | what was inserted, after your corrections and snippets |
| edited text | the dictation as you left it, when the edit watcher (macOS) saw you change it |
| speech model | the model id (`parakeet-tdt-0.6b-v3`, `large-v3-turbo`…), or `remote-host` / `cloud` |
| app category | `messaging`, `email`, `docs`, `code`, `terminal` or `other`; never the app's name or window contents |
| timestamp, duration | epoch seconds, and the audio length in seconds |

Retention is 30 days (default), 90 days, or until you delete it. **Delete all**
removes every kept dictation.

## On-device layout

Under the app data directory (`~/.local/share/fairspoken/training-data` on
macOS and Linux, `%LOCALAPPDATA%\Fairspoken\training-data` on Windows;
`FAIRSPOKEN_TRAINING_DATA_DIR` overrides it):

```text
training-data/
  index.jsonl          one entry per line (camelCase fields, as in meta.json)
  <id>/audio.wav
  <id>/meta.json
```

Treat this layout as internal; use the export for anything else.

## Export format (version 1)

**Export…** writes `fairspoken-training-data-<epoch seconds>.zip` to your
Downloads folder and shows it in the file manager:

```text
manifest.jsonl
polish.jsonl
README.txt
audio/<id>.wav
```

### manifest.jsonl

One dictation per line, in the NeMo manifest style, with paths relative to the
zip's root:

```json
{"audio_filepath": "audio/6f1c….wav", "duration": 4.2, "text": "Ask Claude Code to rebase the branch.",
 "id": "6f1c…", "created_at": 1791331200, "speech_model": "parakeet-tdt-0.6b-v3", "app_category": "code",
 "raw_text": "ask cloud code to rebase the branch", "polished_text": "Ask cloud code to rebase the branch.",
 "final_text": "Ask cloud code to rebase the branch.", "edited_text": "Ask Claude Code to rebase the branch."}
```

- `text` is `edited_text` when present, otherwise `final_text`. It is the best
  available reference, but it is not a verbatim transcript: polish removes
  fillers and false starts, and snippets expand. For ASR fine-tuning, prefer
  rows with `edited_text`, or compare `text` against `raw_text` and drop rows
  that differ by more than a few words.
- `polished_text` and `edited_text` are `null` when absent.
- For NeMo, rewrite `audio_filepath` to absolute paths after unzipping (for
  example with `jq`), then pass the file as `train_ds.manifest_filepath`.

### polish.jsonl

Prompt/completion pairs for polish LoRA training, in MLX-LM's `completions`
format:

```json
{"id": "6f1c…", "prompt": "ask cloud code to rebase the branch", "completion": "Ask Claude Code to rebase the branch."}
```

`prompt` is the speech model's text and `completion` is the same target as the
manifest's `text`. Wrap `prompt` in the polish system prompt you train with,
and hold back unchanged pairs (around 40%) so the model learns restraint.

### Compatibility

Fields may be added in later versions; readers should ignore unknown fields.
A change that removes or renames a field will bump the version in
`README.txt`.
