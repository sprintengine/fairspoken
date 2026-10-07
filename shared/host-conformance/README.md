# Host conformance suite

Checks a running transcription host against the protocol in
`src-tauri/src/host/PROTOCOL.md`. It talks plain HTTP to any URL, so the same
suite runs against the Rust host and the Swift host.

Requirements: Node 22 or newer. No npm dependencies. On macOS the suite
renders real speech with `say` and `afconvert`; elsewhere it falls back to a
synthetic tone and only checks the shape of transcription responses.

## Usage

```sh
node shared/host-conformance/run.mjs --url http://127.0.0.1:48970 --token secret
```

| Option | Meaning |
| --- | --- |
| `--url URL` | Host base URL (required). |
| `--token T` | The host's token. Without it the `auth.*` checks and `config.pairing-password` are skipped. |
| `--pairing-password P` | The host's pairing password. Without it `pair.correct-password` and `pair.password-not-revealed` are skipped, and `pair.rate-limit` / `pair.events` check only wrong passwords. |
| `--only a,b` | Run only these checks or groups (`stream` matches `stream.*`). |
| `--skip a,b` | Leave out these checks or groups. |
| `--slow` | Include checks that wait on timers (the 15 s heartbeat). |
| `--json` | Print results as JSON instead of one line per check. |
| `--no-transcript-check` | Don't compare transcripts with the spoken words (for stub engines). |
| `--list` | List every check with a one-line description. |

Each check prints `PASS`, `FAIL` (with the reason) or `SKIP` (with what it
needs). Exit status: `0` all passed or skipped, `1` at least one failure,
`2` the host could not be reached or rejected the token.

The suite changes host configuration while it runs (`POST /v1/config`
persists to the host's config file). It records the configuration at the
start and posts it back at the end, also on Ctrl-C. Run it against a test
host, not one serving users: it fills the event-subscriber limit and the
transcription queue on purpose.

## Starting a host for a run

Rust (release binary, throwaway config file, stops the host on exit):

```sh
shared/host-conformance/start-rust-host.sh 48970 secret
# with a pairing password, for the pair.* checks:
FAIRSPOKEN_HOST_PAIRING_PASSWORD=conformance-pw \
  shared/host-conformance/start-rust-host.sh 48970 secret
node shared/host-conformance/run.mjs --url http://127.0.0.1:48970 --token secret --pairing-password conformance-pw
# or with a small queue so capacity.queue-full can run:
FAIRSPOKEN_HOST_WORKERS=1 FAIRSPOKEN_HOST_QUEUE_CAPACITY=2 \
  shared/host-conformance/start-rust-host.sh 48970 secret
```

The one-liner it wraps:

```sh
FAIRSPOKEN_HOST_ADDR=127.0.0.1:48970 FAIRSPOKEN_HOST_TOKEN=secret \
FAIRSPOKEN_HOST_CONFIG_PATH=$(mktemp -d)/host-config.json \
FAIRSPOKEN_HOST_WORKERS=1 FAIRSPOKEN_HOST_QUEUE_CAPACITY=2 \
  src-tauri/target/release/transcription-host
```

Build the binary with `cd src-tauri && cargo build --release --bin transcription-host`.
The Rust host reads models from `~/.local/share/fairspoken/models`
(override with `FAIRSPOKEN_MODEL_DIR`).

Swift: `Fairspoken Server.app/Contents/MacOS/Fairspoken Server --headless`
honours the same `FAIRSPOKEN_HOST_*` variables.

Host variables: `FAIRSPOKEN_HOST_ADDR`, `FAIRSPOKEN_HOST_TOKEN`,
`FAIRSPOKEN_HOST_CONFIG_PATH`, `FAIRSPOKEN_HOST_WORKERS`,
`FAIRSPOKEN_HOST_QUEUE_CAPACITY`, `FAIRSPOKEN_HOST_MODEL`,
`FAIRSPOKEN_HOST_MAX_ACTIVE_STREAMS`, `FAIRSPOKEN_HOST_MAX_RECORDING_SECONDS`,
`FAIRSPOKEN_HOST_USE_GPU`, `FAIRSPOKEN_HOST_PAIRING_PASSWORD`,
`FAIRSPOKEN_HOST_NAME`. A persisted config file overrides the env values
for the live settings, so use a fresh config path for each run. Both hosts
still accept the former `MULTIVOICE_HOST_*` names when the new ones are unset.

## What the groups cover

| Group | Covers |
| --- | --- |
| `root`, `health`, `stats` | `GET /` dashboard and `/favicon.ico` without auth; `/v1/health` body; every `/v1/stats` field and type, worker/model consistency (`model` is the uniform worker model or `mixed`, `assignedWorkers` matches `workers[].assignedModel`, counts match the arrays). |
| `auth`, `routes` | 401 `{"error":"unauthorized"}` on every route without, with a wrong, or with a near-miss token; Bearer on every method; `?token=` on GET only; 404 `{"error":"not_found"}` for unknown routes. |
| `http` | `Expect: 100-continue` POST; two requests on one keep-alive connection. |
| `hello`, `pair` | `GET /v1/hello` without a token (exact fields, `auth` consistent with the token and `/v1/stats` `pairingEnabled`); `POST /v1/pair`: 400 for malformed bodies, 404 `pairing disabled` / `token: null` without a password, the right password's token, 401 `wrong password`, 429 with `Retry-After` and `retryAfterSeconds` after 5 failures from one address (also for the right password), `pairing` events, and the password in no answer or event. `config.pairing-password` sets, validates and clears it through `POST /v1/config` and restores it. |
| `config` | Echo of `{}`; 400 for out-of-range values, unknown fields, `model` with `workerModels`, wrong `workerModels` length, unknown model ids and invalid bodies, each with the configuration unchanged afterwards (also when a valid field rides along); a valid change shows in `/v1/stats` and reverts. |
| `events` | `/v1/events` headers, snapshot first frame, exact frame format and payload fields, one chunk per frame, HTTP/1.0 close-delimited body, heartbeat (`--slow`), the 16-subscriber limit and slot reuse, the event sequence of a stream and a batch job, snapshot plus events equal to `/v1/stats`. |
| `clients` | Attribution from `X-Forwarded-For` and `Tailscale-User-Login` on loopback requests. |
| `batch` | WAV transcription response, 48 kHz stereo input, 400 for non-16-bit WAV, garbage and bad `x-fairspoken-backend` (and the legacy `x-multivoice-backend`), `x-fairspoken-model` ignored, vocabulary hints, 413 over `maxRecordingSeconds`. |
| `stream` | Streamed transcription with zero-count frames, release-to-text latency (median of three real-time runs), 48 kHz frames, and the error statuses: rate change 413, bad rate / oversized frame / truncated body 4xx, empty 400, over-duration 413, aborted upload (job ends with `Stream upload aborted`, host keeps serving). |
| `capacity` | 429 past `maxActiveStreams`; 429 plus `job_failed` (worker `null`, `Transcription queue is full`) when every worker is busy and the queue is full, for a stream and a batch upload. |
| `download` | 400 for unknown models and bad bodies; re-requesting an installed model answers 202 and ends in a `ready` event; 409 for a concurrent request when the first is still running. |
| `update` | Optional self-updater (`/v1/update…`; skipped when the host answers 404): 401 on every update route without a token or with `?token=` on POST, the status shape and its consistency with `/v1/health`, 4xx for bad settings with nothing changed, 409 for a restart with nothing installed. Never checks, installs or restarts. |
| `events.audit` | One subscriber held for the whole run: every frame well formed, every job has exactly one terminal event, `worker_state` only on change, no transcript text, and the snapshot plus every event matches the final `/v1/stats`. |

## Checks that need a model

Transcription checks need every worker's assigned model installed and skip
otherwise: `batch.speech`, `batch.48k-stereo`, `batch.model-header-ignored`,
`batch.vocabulary-hints`, `stream.speech`, `stream.latency`, `stream.48k`,
`events.stream-order`, `events.batch-job`, `capacity.queue-full`.
`stream.aborted-upload` runs either way but only checks the exact failure
event with a model. `download.installed` needs some installed model (it
prefers one no worker serves) and never downloads a missing one.

`capacity.queue-full` also needs a queue small enough to fill:
`queueCapacity` at most 4. Start the host with
`FAIRSPOKEN_HOST_QUEUE_CAPACITY=2` to run it.

Everything else (auth, config, pairing, events framing, error statuses) runs
without a model.

## Pairing rate limit

`pair.rate-limit` sends 5 wrong passwords from one forwarded address (it
needs a loopback URL), and a run spends about 7 of the host's 20 allowed
failures per 10 minutes overall. Run the suite at most twice per 10 minutes
against one host process, or restart the host between runs (the limit lives
in memory); otherwise the right-password checks answer 429.
