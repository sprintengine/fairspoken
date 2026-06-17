# Multivoice Tauri

Standalone Tauri/Rust rebuild of Multivoice desktop dictation.

This app is intentionally separate from the Electron app. The goal is a lighter cross-platform package with a Tauri WebView UI and native Rust services for audio capture, model management, local speech-to-text transcription, settings, and clipboard writes.

The default (and only, in a default build) engine is NVIDIA **Parakeet TDT 0.6B v3**, run in-process via `parakeet-rs` (ONNX Runtime). The previous whisper.cpp engine is now opt-in behind the `whisper` Cargo feature.

## Current State

- Tauri v2 vanilla TypeScript scaffold is created.
- The starter demo has been replaced with a compact Multivoice recorder shell.
- Native CPAL capture is wired for the default microphone.
- Recording has too-short and silence guards before transcription.
- Parakeet TDT is the default local engine, loaded from its ONNX model directory via `parakeet-rs`.
- whisper.cpp inference remains available as an opt-in engine: build with `--features whisper` (and `cuda`/`vulkan` for non-Mac GPU backends; macOS uses Metal + flash attention automatically).
- Model preparation downloads the Parakeet ONNX files (or, for whisper builds, ggml models from the whisper.cpp Hugging Face repository) into the model cache.
- Settings persist to the OS app data directory.
- Clipboard writes are verified after transcription.
- Remote transcription can stream microphone audio to a standalone Rust host.
- Windows release build succeeds and produces MSI and NSIS installers.

## Development

Install Rust and the Tauri prerequisites for your OS before running desktop commands.
A `--features whisper` build additionally needs `libclang` and `cmake` available (whisper.cpp is compiled from source); the default Parakeet build does not.

```bash
npm install
npm run build
npm run tauri dev
```

The model cache defaults to:

- Windows: `%LOCALAPPDATA%/Multivoice Tauri/models`
- Linux/macOS-style shells: `$HOME/.local/share/multivoice-tauri/models`

For isolated tests or development sessions, override paths with:

- `MULTIVOICE_TAURI_MODEL_DIR`
- `MULTIVOICE_TAURI_SETTINGS_PATH`

Useful verification commands:

```bash
npm run build
cargo test
cargo test --features whisper downloads_tiny_model -- --ignored
npm run tauri build
```

## Remote Transcription Host

Start the standalone host from `src-tauri`:

```bash
cargo run --bin transcription-host
```

By default it listens on `127.0.0.1:48173`. Override the bind address or require a bearer token with:

```bash
MULTIVOICE_HOST_ADDR=0.0.0.0:48173 MULTIVOICE_HOST_TOKEN=secret cargo run --bin transcription-host
```

The host accepts multiple active client streams and queues completed recordings
for one or more transcription workers. Defaults are conservative for a
single-purpose local server:

```bash
MULTIVOICE_HOST_WORKERS=1
MULTIVOICE_HOST_QUEUE_CAPACITY=8
MULTIVOICE_HOST_MAX_ACTIVE_STREAMS=4
MULTIVOICE_HOST_MAX_RECORDING_SECONDS=120
```

Streaming clients keep the transcription request open until the queued job
finishes and the host returns one final JSON transcript. Connection setup still
uses the configured remote timeout, but queued streams are not cut off by a
fixed read timeout while waiting for a worker.

In the desktop settings, set `Transcription` location to `Remote host` and enter the host URL. The client streams mono PCM frames while recording; the host transcribes with its operator-configured model and returns the final transcript for clipboard copy.

For a dedicated Mac mini setup and benchmark checklist, see
`docs/mac-mini-transcription-host.md`.

## Plan

See `docs/implementation-plan.md`.
