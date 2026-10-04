# Contributing to Fairspoken

Thanks for your interest in improving Fairspoken. Bug reports, fixes, and
well-scoped features are all welcome.

## Before you start

- For anything larger than a small fix, open an issue first so we can agree on
  the approach before you spend time on it.
- Security problems: please **do not** open a public issue — see `SECURITY.md`.
- By participating you agree to follow the [Code of Conduct](CODE_OF_CONDUCT.md).

## Development setup

Prerequisites: Node 22+, a stable Rust toolchain, and the
[Tauri v2 prerequisites](https://v2.tauri.app/start/prerequisites/) for your OS.
A `--features whisper` build also needs `cmake` and `libclang`.

```bash
npm install
npm run tauri dev
```

The first run downloads the Parakeet model (~2.5 GB) into the model cache. To
keep development data separate from your everyday install, point the app at
throwaway locations:

```bash
MULTIVOICE_TAURI_MODEL_DIR=/tmp/mv-models \
MULTIVOICE_TAURI_SETTINGS_PATH=/tmp/mv-settings.json \
npm run tauri dev
```

## Before you open a pull request

Run the same checks CI runs:

```bash
npm run build                                               # type-check + bundle the frontend
cargo test --manifest-path src-tauri/Cargo.toml             # backend tests
cargo build --manifest-path src-tauri/Cargo.toml --features whisper   # optional engine still compiles
cargo deny --manifest-path src-tauri/Cargo.toml check licenses        # if you added a dependency
```

Guidelines:

- Keep pull requests focused: one change per PR, with a description of what
  changed and how you verified it (screenshots for UI changes).
- Add or update tests for behavior changes in the Rust backend.
- Local transcription must keep working with no network and no account. Cloud
  features are optional and must stay off by default.
- New dependencies need a license compatible with MIT (CI enforces the allow
  list in `src-tauri/deny.toml`).

## License

By contributing, you agree that your contributions are licensed under the
project's [MIT License](LICENSE).
