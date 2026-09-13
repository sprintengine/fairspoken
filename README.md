# MultiVoice

Fast, private, local-first desktop dictation. Press a shortcut, speak, and the
text lands in whatever app you're typing in.

MultiVoice transcribes **on your machine** by default with NVIDIA's Parakeet
TDT model: no account, no network, and no audio ever leaves your computer. On
Apple Silicon a typical dictation is transcribed in a few hundred milliseconds.

If you'd rather not spend local CPU/RAM, or want the extra features below,
there is an optional hosted service, **MultiVoice Cloud**, that you can sign in
to and pay for with credits. It is off by default and the app is fully usable
without it.

## Features

- **Local transcription** with Parakeet TDT 0.6B v3 (ONNX Runtime, in-process).
  The Models screen groups Parakeet and Whisper variants, with v3 recommended
  by default and English-only Parakeet v2 also available. Search Hugging Face
  for other speech models; results indicate which have a supported local format.
  Whisper (whisper.cpp) is also included; choose its variant in Models.
- **Insert anywhere**: text goes straight into the focused app (Accessibility
  insert → ⌘V → AppleScript fallback), with your clipboard as a backup.
- **Dictionary and snippets**: custom corrections and trigger phrases
  ("my email" → your address), applied after transcription.
- **History and notes**: recent transcripts, copy-again, and a notes view.
- **Remote host**: run the bundled `transcription-host` on another machine
  (e.g. a Mac mini) and stream audio to it over your own network.
- **MultiVoice Cloud (optional, paid)**: hosted transcription, AI polish
  (filler-word and self-correction cleanup with one-click "Undo AI edit"), and
  app-aware tone. Polish and context awareness are off unless you turn them on.

## Install

Pre-built releases are published on the
[Releases page](https://github.com/sprintengine/multivoice-tauri/releases).
macOS is the primary platform; Windows and Linux builds are produced by CI and
are less tested.

On macOS, MultiVoice needs **Microphone** access, and **Accessibility** access
to insert text into other apps.

## Build from source

Prerequisites: Node 22+, a stable Rust toolchain, `cmake`, `libclang`, and the
[Tauri v2 prerequisites](https://v2.tauri.app/start/prerequisites/) for your OS.

```bash
npm install
npm run tauri dev      # run in development
npm run tauri build    # produce an installer in src-tauri/target/release/bundle
```

The first launch downloads the Parakeet model (~2.5 GB) into the model cache.

Smaller Parakeet-only build (excludes the Whisper engine):

```bash
npm run tauri build -- --no-default-features
```

Useful environment variables:

| Variable | Purpose |
| --- | --- |
| `MULTIVOICE_TAURI_MODEL_DIR` | Override the model cache directory |
| `MULTIVOICE_TAURI_SETTINGS_PATH` | Override the settings file location |
| `MULTIVOICE_CLOUD_URL` | Point cloud mode at a different endpoint (compile time or runtime) |

## Remote transcription host

Run the standalone host on the machine that should do the transcribing:

```bash
cargo run --manifest-path src-tauri/Cargo.toml --release --bin transcription-host
```

It listens on `127.0.0.1:48173` by default. To accept connections from other
machines, bind to all interfaces and require a token:

```bash
MULTIVOICE_HOST_ADDR=0.0.0.0:48173 MULTIVOICE_HOST_TOKEN=<choose-a-token> \
  cargo run --manifest-path src-tauri/Cargo.toml --release --bin transcription-host
```

Capacity settings (defaults shown):

```bash
MULTIVOICE_HOST_WORKERS=1
MULTIVOICE_HOST_QUEUE_CAPACITY=8
MULTIVOICE_HOST_MAX_ACTIVE_STREAMS=4
MULTIVOICE_HOST_MAX_RECORDING_SECONDS=120
```

In the app's settings, set **Transcription location** to **Remote host** and
enter the host URL and token. The client streams mono PCM while you record and
the host returns the final transcript.

## Privacy

- **Local mode** (the default): audio and text stay on your computer.
- **Remote host**: audio goes only to the host you configure.
- **MultiVoice Cloud**: audio (for cloud transcription) and transcript text
  (for AI polish) are sent to the MultiVoice Cloud service for processing and
  are not stored.
- **Context awareness** (off by default) reads text near your cursor through
  the macOS Accessibility API, locally, to improve casing and vocabulary.
  Password fields are never read.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). Please report security issues
privately as described in [SECURITY.md](SECURITY.md).

## License

MultiVoice is released under the [MIT License](LICENSE). The speech models it
downloads are licensed separately. Parakeet TDT is © NVIDIA and licensed
CC BY 4.0. See [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).
