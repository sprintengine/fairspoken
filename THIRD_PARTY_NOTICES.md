# Third-party notices

Fairspoken's source code is MIT-licensed (see `LICENSE`). The speech models it
downloads at runtime, and some of the native libraries it links, are the work
of others and are distributed under their own licenses. No model weights are
stored in this repository; they are downloaded on first use into the local
model cache.

## Speech models

### NVIDIA Parakeet TDT 0.6B v3 (default engine)

- Original model: [nvidia/parakeet-tdt-0.6b-v3](https://huggingface.co/nvidia/parakeet-tdt-0.6b-v3),
  © NVIDIA Corporation.
- License: [Creative Commons Attribution 4.0 International (CC BY 4.0)](https://creativecommons.org/licenses/by/4.0/).
- Fairspoken downloads the ONNX export published at
  [istupakov/parakeet-tdt-0.6b-v3-onnx](https://huggingface.co/istupakov/parakeet-tdt-0.6b-v3-onnx)
  (a format conversion of the NVIDIA weights; no changes to the model itself).

### NVIDIA Parakeet TDT 0.6B v2 (English alternative)

- Original model: [nvidia/parakeet-tdt-0.6b-v2](https://huggingface.co/nvidia/parakeet-tdt-0.6b-v2),
  © NVIDIA Corporation, licensed CC BY 4.0.
- The app downloads the unmodified ONNX conversion from
  [istupakov/parakeet-tdt-0.6b-v2-onnx](https://huggingface.co/istupakov/parakeet-tdt-0.6b-v2-onnx)
  at revision `0bbb45a3365852604aef28b538a8f066f4ccaa85`, with pinned file hashes.

### Moondream Parakeet Ultra (optional)

- Original model: [moondream/parakeet-ultra](https://huggingface.co/moondream/parakeet-ultra),
  © Moondream, a post-trained version of
  [nvidia/parakeet-tdt-0.6b-v3](https://huggingface.co/nvidia/parakeet-tdt-0.6b-v3) © NVIDIA Corporation.
- License: [Creative Commons Attribution 4.0 International (CC BY 4.0)](https://creativecommons.org/licenses/by/4.0/).
- The app downloads the ONNX conversion (no changes to the weights; Moondream's
  voice-activity head is not included) from the `parakeet-ultra` folder of
  [altunenes/parakeet-rs](https://huggingface.co/altunenes/parakeet-rs/tree/main/parakeet-ultra)
  at revision `4d2a8bc71f5c896ec40faa59732e6716295edaf2`, with pinned file hashes.

### OpenAI Whisper via whisper.cpp (included by default)

- Models: OpenAI Whisper, converted to ggml format and published at
  [ggerganov/whisper.cpp](https://huggingface.co/ggerganov/whisper.cpp).
- License: MIT (Whisper model weights and whisper.cpp).

## Local polish (cleanup) models

Optional; downloaded on demand from Hugging Face at a pinned revision and
SHA-256, and run by a private llama.cpp `llama-server` (MIT, downloaded from
the [ggml-org/llama.cpp](https://github.com/ggml-org/llama.cpp) release `b10930`).

- **Qwen3.5 0.8B / 2B / 4B** — © Alibaba Cloud (Qwen), Apache License 2.0;
  GGUF conversions from `ggml-org`, `lmstudio-community` and `unsloth`.
- **SpeakoFlow Mini** — [SpeakoFlow/speakoflow-mini](https://huggingface.co/SpeakoFlow/speakoflow-mini),
  a fine-tune of Qwen/Qwen3.5-0.8B by Abhishek Barali, Apache License 2.0.
  The app downloads `SpeakoFlow-Mini-0.8B-Q8_0.gguf` at revision
  `835431771f72820251fe6c6b4b07f12b000e2647` and sends it the system prompt
  published on its model card.

## Runtimes and libraries

- **ONNX Runtime** (downloaded by the `ort` crate at build time) — MIT,
  © Microsoft Corporation.
- **whisper.cpp** (compiled in only with `--features whisper`) — MIT.
- **Tauri** — MIT OR Apache-2.0.
- **Inter** typeface (`@fontsource/inter`) — SIL Open Font License 1.1.

The full set of Rust dependency licenses is checked in CI with
[`cargo-deny`](https://github.com/EmbarkStudios/cargo-deny) against the allow
list in `src-tauri/deny.toml`. Run `cargo deny --manifest-path src-tauri/Cargo.toml check licenses`
to reproduce it locally.
