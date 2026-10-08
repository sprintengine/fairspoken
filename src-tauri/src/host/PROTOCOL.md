# Transcription host protocol

The contract between Fairspoken clients, the host dashboard and any host
implementation (this Rust host, the Swift host). JSON is UTF-8 and camelCase.
Timestamps named `at` or `…Ms` with an epoch meaning are Unix epoch
milliseconds; other `…Ms` fields are durations.

Wire identifiers are the `x-fairspoken-*` headers, the
`application/vnd.fairspoken.pcm-stream` content type and the
`FAIRSPOKEN_HOST_*` environment variables. Clients send only these names.
Hosts must also accept the pre-rename `x-multivoice-*` headers and
`MULTIVOICE_HOST_*` variables when the new name is absent (the new name wins
when both are present).

## Authentication

When the host has a token (`FAIRSPOKEN_HOST_TOKEN`), every route except
`GET /` and `GET /favicon.ico` requires it:

- `Authorization: Bearer <token>` on any method, or
- `?token=<token>` on `GET` only (browsers' `EventSource` cannot set headers;
  mutating routes stay header-only so the token stays out of access logs).

A missing or wrong token answers `401 {"error":"unauthorized"}`. Errors
elsewhere are `{"error": "<message>"}` with a 4xx/5xx status.

`GET /v1/hello` and `POST /v1/pair` (below) never require the token.

## Discovery and pairing

A client finds hosts on the user's tailnet without the user typing an
address, and pairs with a password the host's operator chose instead of the
long token.

### Pairing password

The operator may set a pairing password (`FAIRSPOKEN_HOST_PAIRING_PASSWORD`,
or `pairingPassword` in the host config file, the env var winning). Rules:

- 6–128 characters. A six-digit number is allowed.
- Setting a password on a host with no token makes the host generate one
  (32 random bytes, base64url, no padding) and persist it, because pairing
  must hand the client a token. Set through `POST /v1/config`, the generated
  token is required from the next request on (a dashboard opened without a
  token pairs to get it).
- Stored in the host config file next to the token (same file, `0600` where
  the platform supports it). It is never returned by any route, logged, or
  sent in events. `/v1/stats` reports only `"pairingEnabled": bool` and
  `"pairingPasswordSource"`: `"env"` (`FAIRSPOKEN_HOST_PAIRING_PASSWORD`),
  `"saved"` (the config file) or `null` with pairing off.
- `POST /v1/config` accepts `pairingPassword: string | null` (`null` or `""`
  turns pairing off) with the same validation; the answer reports
  `pairingEnabled`, never the password. While the env var sets the password
  (`pairingPasswordSource: "env"`), any `pairingPassword` answers `409` and
  nothing in that update applies: the env var would win again at the next
  start.

### GET /v1/hello (no auth)

Identifies a Fairspoken host to a scanning client. Answers `200`:

```jsonc
{
  "service": "fairspoken-host",   // clients ignore any other value
  "protocol": 1,                  // bumps only on incompatible discovery/pairing changes
  "name": "Studio Mac",           // operator-set display name, else the machine's host name
  "serverVersion": "0.4.0",
  "auth": "password"              // none: no token | password: pair with POST /v1/pair | token: token set, no pairing password
}
```

It reveals nothing else (no models, clients or stats). Hosts answer it
cheaply; clients probe many addresses at once.

### POST /v1/pair (no auth)

Body (`deny_unknown_fields`, at most 4 KB, else `413`):
`{"password": "<string>", "clientName": "<string>"?}`. `clientName` is
informational, e.g. "Conal's MacBook"; the host trims it, strips control
characters and truncates it to 64 characters (never rejects it), and may show
it in its client list.

| Status | Body | When |
| --- | --- | --- |
| `200` | `{"token": "<token>", "name": "<host name>"}` | Password matches. `token` is the host token. |
| `200` | `{"token": null, "name": "<host name>"}` | The host has no token (`auth: "none"`); nothing to pair. |
| `401` | `{"error": "wrong password"}` | Mismatch. |
| `404` | `{"error": "pairing disabled"}` | Token set but no pairing password. |
| `429` | `{"error": "too many attempts", "retryAfterSeconds": n}` | Rate limit (below). Also sets `Retry-After`. |
| `400` | `{"error": "<message>"}` | Malformed body. |

- The comparison is constant-time.
- Rate limit: at most 5 failed attempts per client address per 10 minutes,
  and 20 failed attempts across all addresses per 10 minutes. Once exceeded,
  every attempt (right or wrong) from that scope answers `429` until the
  window has room again. Successes don't count. State is in memory only.
- Each attempt with a well-formed body emits a `pairing` event on
  `/v1/events`: `{at, client, clientName, ok}` (`clientName` cleaned as above
  or `null`; `ok` is true for either `200`, including `token: null`).
- The client address follows the same rule as `/v1/stats` `client`.

### Client discovery (informative)

Clients scan with the Tailscale CLI, which every desktop Tailscale install
ships: `tailscale status --json` (look on `PATH`, then
`/Applications/Tailscale.app/Contents/MacOS/Tailscale` on macOS and
`%ProgramFiles%\Tailscale\tailscale.exe` on Windows). For `Self` and every
peer with `Online: true` (skipping phones and TVs: `OS` `iOS`, `android`,
`tvOS`), the client probes in parallel (≤ 32 at once, 1.5 s timeout each):

1. `https://<DNSName without the trailing dot>/v1/hello`, for a host
   published with `tailscale serve` over HTTPS,
2. `http://<DNSName without the trailing dot>/v1/hello` (port 80), for one
   published with `tailscale serve` over plain HTTP, and
3. `http://<first IPv4 in TailscaleIPs>:48173/v1/hello`, for a host bound to
   `0.0.0.0` or its Tailscale address on the default port.

Clients also probe `http://127.0.0.1:<port>/v1/hello` for hosts on the same
computer, on the default port and (on macOS) on the port in Fairspoken
Server's `host-config.json`. These come first in the list and are probed even
when Tailscale isn't installed or running. When the same host (same name and
port) answers both on loopback and on this computer's Tailscale address, only
the loopback entry is listed, so a client on the host's own computer saves
`127.0.0.1`.

A host bound to one specific non-loopback address (its Tailscale IP) also
listens on `127.0.0.1` at the same port, best effort, so apps on its own
computer can reach it whichever address they saved.

A peer that answers with `service: "fairspoken-host"` is listed once, at the
first URL in that order that answered. Without the CLI, or for a
non-default port, the user can type a machine name or `name:port`, which the
client probes the same way (MagicDNS resolves bare machine names). A bare
name only works over HTTP unless the CLI expands it to the full `*.ts.net`
name, because `tailscale serve` certificates cover only that name. After a
successful pair, the client saves the URL and token into its existing
remote-host settings and runs its normal connection test.

## Endpoints

| Method | Path | Purpose |
| --- | --- | --- |
| GET | `/` | Dashboard HTML (no auth; it authenticates its own calls) |
| GET | `/favicon.ico` | `204`, no auth |
| GET | `/v1/hello` | Discovery identity (no auth, below) |
| POST | `/v1/pair` | Exchange the pairing password for the token (no auth, below) |
| GET | `/v1/health` | `{ok, mode: "standalone-host", backend, serverVersion}` |
| GET | `/v1/stats` | Full host snapshot (below) |
| GET | `/v1/events` | Live Server-Sent Events feed (below) |
| POST | `/v1/config` | Change live configuration |
| POST | `/v1/models/download` | Install a model on the host |
| POST | `/v1/transcriptions` | Transcribe a complete WAV upload |
| POST | `/v1/transcriptions/stream` | Transcribe PCM frames while the client speaks |
| GET | `/v1/update` | Self-update status (optional, below) |
| POST | `/v1/update/check`, `/v1/update/install`, `/v1/update/restart`, `/v1/update/settings` | Drive the self-updater (optional) |

### POST /v1/transcriptions

Body: a WAV file (mono PCM16). Answers
`{text, durationSeconds, backend, model, serverVersion}`. `backend` is the
engine that ran (`"parakeet"` or `"whisper"`) and `model` the model id the
serving worker holds — the host's choice, never the client's.

Request headers: `x-fairspoken-language` (default `en`),
`x-fairspoken-vocabulary-hints` (percent-encoded JSON string array),
`x-fairspoken-backend` (`parakeet`/`whisper`; anything else is `400`).
`x-fairspoken-model` and `x-fairspoken-client` are accepted and ignored.
`x-fairspoken-super-mode: 1` asks for super mode on this request and on
`/v1/transcriptions/stream` (see [Super mode](#super-mode-optional)).

Status: `413` over the size/duration limit (the duration is checked from the
WAV header before the audio is decoded), `400` for a sample rate outside
1–192000 Hz, `429` queue or stream capacity reached, or as many batch uploads
in flight as workers plus queue slots (checked before the body is read),
`500` worker failure.

### POST /v1/transcriptions/stream

`Content-Type: application/vnd.fairspoken.pcm-stream` (informational; the
host does not require it). Chunked request body of frames, sent as audio is
captured:

```
u32 LE sample_rate | u32 LE sample_count | sample_count × i16 LE samples
```

`sample_rate` must stay constant (1–192000), at most 192000 samples per frame;
a zero-count frame is a no-op. Closing the body ends the recording. The
response is the same JSON as `/v1/transcriptions`. A stream that sends no
frame for 30 s loses its job, which ends with `"Stream upload aborted"` so
the worker is free again (a phone that dropped off the network never holds
it).

### POST /v1/config

Body (every field optional; `deny_unknown_fields`):
`{maxActiveStreams 1–32, maxRecordingSeconds 10–600, useGpu, model | workerModels[], pairingPassword}`
(`pairingPassword` as in "Pairing password" above).
`model` sets every worker; `workerModels` must list exactly one id per
worker. All fields validate before any apply; the result persists to the host
config file. Answers `{maxActiveStreams, maxRecordingSeconds, useGpu, model,
workerModels, pairingEnabled}`; `409` for `pairingPassword` while
`FAIRSPOKEN_HOST_PAIRING_PASSWORD` is set. Worker count and queue capacity
are restart-only.

### POST /v1/models/download

Body `{"model": "<id>"}`. Answers `202 {model, status: "downloading"}`;
`409` if a download is already running. Progress appears as
`modelDownload` in `/v1/stats` and as `model_download` events.

### Self-update (optional): /v1/update…

Implemented by the standalone Rust host, which updates itself from the
`host-<channel>.json` feeds. A host updated some other way (the Swift host
ships inside a Sparkle-updated Mac app) answers `404` and the dashboard hides
its update controls. All routes need the token like any other.

`GET /v1/update`:

```jsonc
{
  "currentVersion": "0.2.0",
  "channel": "stable",            // stable | nightly
  "channelSource": "version",     // env (FAIRSPOKEN_HOST_UPDATE_CHANNEL) | saved | version
  "defaultChannel": "stable",     // derived from currentVersion
  "autoUpdate": false,
  "checksEnabled": true,          // false with FAIRSPOKEN_HOST_UPDATE_CHECKS=0
  "platform": "linux-x86_64",     // the feed's platform key, null if unpublished
  "state": "available",           // idle | checking | available | downloading | ready | restarting | error
  "available": {                  // null unless an update is offered
    "version": "0.3.0", "channel": "stable",
    "notes": "https://github.com/…/releases/tag/v0.3.0", "pubDate": "2026-10-05T12:00:00Z",
    "switchToStable": false       // true: nightly build on the stable channel, offered a lower stable
  },
  "progress": null,               // {downloadedBytes, totalBytes|null, percentage|null} while downloading
  "error": null,
  "lastCheckedMs": 1791200000000, // null before the first check
  "installedVersion": null,       // set in "ready"
  "restartMode": "service"        // service: exit 75 for launchd/systemd; reexec: re-executes itself
}
```

- `POST /v1/update/check` starts a check: `202` + status (`state:
  "checking"`); `409` while installing or when an update awaits a restart.
- `POST /v1/update/install` downloads, verifies (sha256 and minisign) and
  installs the offered update: `202` + status (`downloading`, then `ready` or
  `error`); `409` with nothing offered or while busy.
- `POST /v1/update/restart` restarts into the installed update once in-flight
  work has finished (at most 60 s): `202` + status (`restarting`); `409`
  unless `ready`. The connection drops; poll `GET /v1/health` until
  `serverVersion` changes.
- `POST /v1/update/settings` body `{channel?: "stable" | "nightly" | null,
  autoUpdate?: bool}` (`deny_unknown_fields`; `null` follows the running
  version). Persists to the host config file as `updateChannel` /
  `autoUpdate`, answers `200` + status, `400` for invalid values. A channel
  change starts a check.

A feed that does not exist yet (404: `host-stable.json` appears with the
first stable release, `host-nightly.json` with the first nightly) counts as
up to date, not as an error. The host binary is found in the archive by file
name (`transcription-host`, `transcription-host.exe` on Windows) at any depth.
The host checks ~30 s after start and every 6 h. A check only records the
offer; it installs on its own only with `autoUpdate`, and then restarts
when no stream, queued or running job remains.

### GET /v1/stats

```jsonc
{
  "serverVersion": "0.1.0", "bindAddr": "127.0.0.1:48173", "uptimeSeconds": 812,
  "activeSessions": 2, "activeStreams": 1, "queuedJobs": 0, "runningJobs": 1,
  "workerCount": 2, "queueCapacity": 8, "maxActiveStreams": 4,
  "maxRecordingSeconds": 600, "useGpu": true, "pairingEnabled": true,
  "pairingPasswordSource": "saved",          // env | saved | null (pairing off)
  "model": "parakeet-tdt-0.6b-v3",          // or "mixed" when workers differ
  "models": [                                // every model this build can serve
    { "id": "parakeet-tdt-0.6b-v3", "name": "Parakeet TDT 0.6B v3",
      "publisher": "NVIDIA", "sizeBytes": 2549805858, "installed": true,
      "assignedWorkers": [0, 1] }
  ],
  "modelDownload": null,                     // {model, stage, percentage, error}
  "rejectedJobs": 0, "failedJobs": 0, "totalTranscriptions": 41,
  "totalAudioSeconds": 512.4, "averageQueueMs": 3, "averageProcessingMs": 140,
  "workers": [{ "index": 0, "state": "transcribing",
    "assignedModel": "parakeet-tdt-0.6b-v3", "modelAvailable": true,
    "loadedModel": "parakeet-tdt-0.6b-v3", "completedJobs": 20, "lastError": null,
    "job": { "id": 43, "model": "…", "source": "stream", "client": "100.64.0.7",
             "audioSeconds": 0.0, "elapsedMs": 2140 } }],
  "queue":   [{ "id": 42, "model": "…", "source": "batch", "client": "…",
                "audioSeconds": 4.1, "waitingMs": 12 }],
  "streams": [{ "id": 12, "client": "100.64.0.7", "elapsedMs": 2200 }],
  "clients": [{ "address": "100.64.0.7", "requests": 9, "completed": 8,
                "rejected": 0, "failed": 1, "totalAudioSeconds": 61.0,
                "lastSeenMs": 1759600000000, "lastModel": "…" }],
  "recent":  [{ "id": 41, "completedAtMs": 1759600000000, "durationSeconds": 7.9,
                "backend": "parakeet", "model": "…", "source": "stream",
                "client": "…", "queueWaitMs": 2, "processingMs": 131 }]
}
```

Worker `state` is `idle`, `loading`, `transcribing` or `model-unavailable`.
`source` is `stream` or `batch`. `client` is the peer IP, or for loopback
requests the first `X-Forwarded-For` / `Tailscale-User-Login` value.
`models` sizes are on-disk bytes when installed, else the download size.
`queue[].id` and `workers[].job.id` are job ids and `streams[].id` stream ids,
matching the ids in `/v1/events`.

### GET /v1/events

A Server-Sent Events stream of host activity. Never carries transcript text
or audio.

Response: `200`, `Content-Type: text/event-stream`, `Cache-Control: no-cache`,
`Connection: close`, `X-Accel-Buffering: no`. HTTP/1.1 requests get
`Transfer-Encoding: chunked` with one chunk per frame; HTTP/1.0 gets a body
delimited by connection close. Every frame is flushed as it is produced.

Frame format (one JSON object on a single `data:` line):

```
event: <type>\n
data: <json>\n
\n
```

Idle heartbeat every 15 s: the comment frame `: ping\n\n`.

The first frame is always `snapshot`, whose data is exactly the `/v1/stats`
body. The snapshot and subscription are taken atomically, so applying the
events that follow to it yields the current state with no gap or overlap.

| Event | Data |
| --- | --- |
| `snapshot` | `/v1/stats` body |
| `stream_started` | `{at, streamId, client}` |
| `stream_finished` | `{at, streamId, client, audioSeconds}` (`0` if the upload failed) |
| `job_queued` | `{at, jobId, client, source, model, audioSeconds}` |
| `job_started` | `{at, jobId, worker, model, client, queueWaitMs}` |
| `job_completed` | `{at, jobId, worker, model, client, audioSeconds, processingMs}` |
| `job_failed` | `{at, jobId, worker, client, error}` |
| `worker_state` | `{at, worker, state, model}` |
| `model_download` | `{at, model, stage, percentage, error?}` |
| `pairing` | `{at, client, clientName, ok}` (see `POST /v1/pair`) |

Semantics:

- `client` is a string or `null`. `worker` is a worker index.
- `job_queued.model` is the configured model (or `"mixed"`); the serving
  model is known from `job_started`. A stream job is queued with
  `audioSeconds: 0` as soon as the stream opens, so the worker decodes while
  the client speaks; `job_completed.audioSeconds` is the real length.
  Stream ids and job ids are separate counters.
- Every `job_queued` is followed by exactly one terminal event:
  `job_completed` or `job_failed`. `job_failed.worker` is `null` when the job
  never reached a worker (queue full: `"Transcription queue is full"`); a
  stream whose upload broke off ends with `"Stream upload aborted"`.
- A job's terminal event precedes its worker's `worker_state` back to `idle`.
- `worker_state` is sent only on change. `state` uses the `/v1/stats` values;
  `model` is what the worker holds or is working towards (`null` when idle
  with nothing loaded).
- `model_download.stage` is `starting`, `downloading`, `validating`, `ready`
  or `error`; `error` is present only for `error`. Repeated identical
  progress is not re-sent. On `ready`, refetch `/v1/stats` for `models`.
- Unknown event types must be ignored (the set may grow).

Limits: at most 16 concurrent subscribers; the 17th gets
`503 {"error":"Too many event subscribers (limit 16)"}`. Each subscriber has
a 256-frame buffer; one that falls further behind is disconnected (never
blocking transcription) and should reconnect to resync from a new snapshot.
A vanished client is detected at the next failed write (at worst one
heartbeat later), which frees its slot.

## Super mode (optional)

Super mode runs two engines on the same dictation, Parakeet and Whisper,
and merges their transcripts: Parakeet's text stands except where Whisper
spells one of the request's vocabulary hints that Parakeet only sounds
like. A host that does not implement it ignores the header and never sends
the fields below; clients must treat their absence as `off`.

**Request.** `x-fairspoken-super-mode: 1` (also `true`; `0`/`false` or
absent means not asked) on `POST /v1/transcriptions` or
`POST /v1/transcriptions/stream`. The host decides once, when the request
arrives (for a stream: before its first frame is read), and never changes
the decision during the dictation.

**Response.** When, and only when, the request carried the header, the
response JSON gains:

| Field | Value |
| --- | --- |
| `superMode` | `"used"` both engines ran and were merged · `"shed"` skipped for capacity · `"unavailable"` the host lacks a loaded Parakeet or Whisper engine · `"off"` the operator turned super mode off |
| `secondaryModel` | Only with `"used"`: the model id of the Whisper side. |

With `"used"`, `backend` and `model` name the Parakeet side of the pair
(whichever worker served the job) and `secondaryModel` the Whisper side;
otherwise they name the serving worker's engine and model as usual.
Shedding is never an error: the dictation is transcribed by one engine
as usual and answers `200`.

**Decision rule.** The host grants super mode only when all hold:

1. the operator allows it (`superMode: "allow"`, the default);
2. some worker has a Parakeet engine loaded and some worker a Whisper
   engine loaded;
3. the job queue is empty;
4. free workers (idle, model loaded, not lent) ≥ 2 × the dictations that
   want super mode right now (this one plus those granted but not yet
   started), with at least one free worker of each engine.

Otherwise the answer is `"unavailable"` (2 fails) or `"shed"` (3 or 4
fail). A granted job is taken by any free worker, which then borrows an
idle worker serving the other engine for the length of the dictation; if
none is free by then the job runs on one engine and answers `"shed"`. A
lent worker takes no queued jobs. Lending does not appear in
`/v1/events` (the dictation is one job on its serving worker).

**Stats.** `/v1/stats` (and the events `snapshot`) gains:

```jsonc
"superMode": {
  "policy": "allow",       // allow | off
  "available": true,       // a Parakeet and a Whisper engine are loaded
  "used": 12,              // dictations merged from both engines
  "shed": 3,               // asked for but skipped for capacity
  "unavailable": 0,        // asked for while an engine was missing
  "lentWorkers": [1]       // workers lent to another worker's job right now
}
```

**Configuration.** Host config file key `superMode`: `"allow"` (default)
or `"off"`; environment `FAIRSPOKEN_HOST_SUPER_MODE` (`allow`/`off`, legacy
`MULTIVOICE_HOST_SUPER_MODE`) seeds it. Restart-only; it is not part of
`POST /v1/config`. To serve super mode, run at least two workers with both
engines, e.g. `FAIRSPOKEN_HOST_WORKERS=2
FAIRSPOKEN_HOST_MODEL=parakeet-tdt-0.6b-v3,base`.
