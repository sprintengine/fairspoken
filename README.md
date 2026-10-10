# Fairspoken

Fast, private, local-first desktop dictation. Press a shortcut, speak, and the
text lands in whatever app you're typing in.

Fairspoken transcribes **on your machine** by default with NVIDIA's Parakeet
TDT model: no account, no network, and no audio ever leaves your computer. On
Apple Silicon a typical dictation is transcribed in a few hundred milliseconds.

If you'd rather not spend local CPU/RAM, or want the extra features below,
there is an optional hosted service, **Fairspoken Cloud**, that you can sign in
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
- **Fairspoken Cloud (optional, paid)**: hosted transcription, AI polish
  (filler-word and self-correction cleanup with one-click "Undo AI edit"), and
  app-aware tone. Polish and context awareness are off unless you turn them on.

## Install

Pre-built releases are published on the
[Releases page](https://github.com/sprintengine/fairspoken/releases).
macOS is the primary platform. Each release carries:

- **macOS** (Apple Silicon, macOS 26): the native app,
  `Fairspoken-<version>-macos-arm64.dmg` (or `.zip`), and Fairspoken Server,
  `Fairspoken-Server-<version>-macos-arm64.dmg` (or `.zip`), for a Mac that
  transcribes for other devices. The desktop app built from this repository is
  not released for macOS.
- **Windows and Linux**: the desktop app (`-windows-x64-setup.exe`,
  `.AppImage`, `.deb`), produced by CI and less tested.
- **The standalone transcription host** for macOS (arm64 and x64), Windows and
  Linux (x64 and arm64), as `-transcription-host-<platform>` archives.

Installed Windows and Linux builds update themselves: stable builds follow
stable releases, and nightly builds (the prereleases) follow nightlies. Updates
download in the background and install when you choose **Restart to update** in
Settings → General → Updates. The macOS apps do not update themselves yet;
install each new release by hand. See [docs/releasing.md](docs/releasing.md).

On macOS, Fairspoken needs **Microphone** access, and **Accessibility** access
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
Models come from Hugging Face; where huggingface.co is blocked, point every
Fairspoken app at a Hugging Face mirror or a folder of model files with the
model source setting: see [Model sources](docs/model-sources.md).

Smaller Parakeet-only build (excludes the Whisper engine):

```bash
npm run tauri build -- --no-default-features
```

Useful environment variables:

| Variable | Purpose |
| --- | --- |
| `FAIRSPOKEN_MODEL_DIR` | Override the model cache directory |
| `FAIRSPOKEN_SETTINGS_PATH` | Override the settings file location |
| `FAIRSPOKEN_CLOUD_URL` | Fairspoken Cloud endpoint. Set at compile time to offer the Cloud location and cloud polish (a runtime value overrides it); builds without it offer only local and self-hosted transcription |

Variables from before the rename (`MULTIVOICE_*`, `MULTIVOICE_TAURI_*`) are
still accepted when the `FAIRSPOKEN_*` name is unset.

## Remote transcription host

Run the standalone host on the machine that should do the transcribing:

```bash
cargo run --manifest-path src-tauri/Cargo.toml --release --bin transcription-host
```

It listens on `127.0.0.1:48173` by default. To accept connections from other
machines, bind to all interfaces and require a token:

```bash
FAIRSPOKEN_HOST_ADDR=0.0.0.0:48173 FAIRSPOKEN_HOST_TOKEN=<choose-a-token> \
  cargo run --manifest-path src-tauri/Cargo.toml --release --bin transcription-host
```

Capacity settings (defaults shown):

```bash
FAIRSPOKEN_HOST_WORKERS=1
FAIRSPOKEN_HOST_QUEUE_CAPACITY=8
FAIRSPOKEN_HOST_MAX_ACTIVE_STREAMS=4
FAIRSPOKEN_HOST_MAX_RECORDING_SECONDS=600
```

The host downloads models from Hugging Face unless `--model-source`,
`FAIRSPOKEN_MODEL_SOURCE` or `modelSource` in its config file names a mirror
or a folder; see [Model sources](docs/model-sources.md#the-transcription-host).

In the app's settings, set **Transcription location** to **Remote host** and
enter the host URL and token. The client streams mono PCM while you record and
the host returns the final transcript.

Open `http://<host>:48173/?token=<token>` for the live dashboard: connected
clients, the queue, workers and every model the host can serve, with requests
animated as they flow through, plus configuration, model downloads and recent
jobs. Add `&demo=1` (or open `/?demo=1`) for simulated data. The dashboard reads
`GET /v1/stats` and the Server-Sent Events feed `GET /v1/events`; the full host
protocol is in [`src-tauri/src/host/PROTOCOL.md`](src-tauri/src/host/PROTOCOL.md).

### Running the host on a Tailscale network

Keep the host bound to loopback and let `tailscale serve` publish it to your
tailnet over HTTPS with a real `*.ts.net` certificate:

```bash
cargo build --manifest-path src-tauri/Cargo.toml --release --bin transcription-host
FAIRSPOKEN_HOST_TOKEN=<choose-a-token> src-tauri/target/release/transcription-host
tailscale serve --bg --https=443 http://127.0.0.1:48173
```

Open `https://<machine>.<tailnet>.ts.net/?token=<token>` to reach the
dashboard and download a model. On each client, set the host URL to
`https://<machine>.<tailnet>.ts.net` and enter the token.

#### Pairing with a password

Instead of copying the token to every client, give the host a pairing
password (6 to 128 characters; a six-digit number works):

```bash
FAIRSPOKEN_HOST_PAIRING_PASSWORD=<password> src-tauri/target/release/transcription-host
# or save it in the host config:
transcription-host --set-pairing-password      # reads it from standard input
transcription-host --clear-pairing-password
```

The dashboard's **Pairing** card sets or clears it too. A host without a
token generates one when you set a password. On each client, choose **My
host**, click **Find hosts on my tailnet** (or add the machine by name),
pick the host and enter the password; the app saves the URL and token and
tests the connection. Five wrong passwords from one address lock it out for
10 minutes. `FAIRSPOKEN_HOST_NAME` sets the name clients see (default: the
machine's name).

Plain `http://` also works for tailnet addresses (`100.64.0.0/10`, MagicDNS
names and `*.ts.net`) because tailnet traffic is already encrypted. For that,
bind the host with `FAIRSPOKEN_HOST_ADDR=0.0.0.0:48173` instead of using
`tailscale serve`.

To keep the host running across logins and reboots:

- **macOS:** copy
  `packaging/host/ie.fairspoken.transcription-host.plist` to
  `~/Library/LaunchAgents/`, fill in the binary path, token and log directory,
  `chmod 600` it, then run
  `launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/ie.fairspoken.transcription-host.plist`.
- **Linux:** copy the binary to `~/.local/bin/transcription-host`, put
  `FAIRSPOKEN_HOST_TOKEN=<token>` in `~/.config/fairspoken/host.env`,
  copy `packaging/host/fairspoken-transcription-host.service` to
  `~/.config/systemd/user/`, then run
  `systemctl --user enable --now fairspoken-transcription-host` (and
  `loginctl enable-linger $USER` to start it without logging in).

### Updating the host

A host installed from a release archive updates itself. About 30 seconds
after it starts, and every 6 hours, it reads the update feed for its channel
and records whether a newer build exists; it does not install anything on
its own unless you turn on **Install updates automatically**. The dashboard
shows an update button in its top bar and an **Updates** panel (version,
Stable/Nightly channel, last check, automatic install). From a shell:

```bash
transcription-host --check-update               # exit 0: up to date, 10: update available
transcription-host --update                     # asks before installing; --yes skips the question
transcription-host --update --channel nightly   # also saves the channel
transcription-host --set-update-channel stable
transcription-host --version
```

The channel defaults to the one the running build came from (nightly builds
follow nightlies). A saved choice overrides that, and
`FAIRSPOKEN_HOST_UPDATE_CHANNEL=stable|nightly` overrides both. A nightly
host switched to Stable is offered the latest stable release even though its
version number is lower. `FAIRSPOKEN_HOST_UPDATE_CHECKS=0` turns the
automatic checks off; `FAIRSPOKEN_HOST_UPDATE_FEED_URL` points them at a
mirror of the feeds.

Every update is checked against the sha256 in the feed and the release
signing key built into the host before anything changes. The new binary must
answer `--version` before it replaces the old one, the old one is kept next
to it as `transcription-host.previous`, and it is restored automatically if
the installed binary fails the same check. The host's folder must be writable
by the user it runs as. After installing, the host restarts once in-flight
dictations finish: under the packaged launchd agent or systemd unit it exits
with status 75 and the service manager starts the new version; otherwise it
restarts itself with the same arguments. An update installed with `--update`
takes effect the next time the host starts.

## Privacy

- **Local mode** (the default): audio and text stay on your computer.
- **Remote host**: audio goes only to the host you configure.
- **Fairspoken Cloud**: audio (for cloud transcription) and transcript text
  (for AI polish) are sent to the Fairspoken Cloud service for processing and
  are not stored.
- **Context awareness** (off by default) reads text near your cursor through
  the macOS Accessibility API, locally, to improve casing and vocabulary.
  Password fields are never read.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). Please report security issues
privately as described in [SECURITY.md](SECURITY.md).

## License

Fairspoken is released under the [MIT License](LICENSE). The speech models it
downloads are licensed separately. Parakeet TDT is © NVIDIA and licensed
CC BY 4.0. See [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).
