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

## Endpoints

| Method | Path | Purpose |
| --- | --- | --- |
| GET | `/` | Dashboard HTML (no auth; it authenticates its own calls) |
| GET | `/favicon.ico` | `204`, no auth |
| GET | `/v1/health` | `{ok, mode: "standalone-host", backend, serverVersion}` |
| GET | `/v1/stats` | Full host snapshot (below) |
| GET | `/v1/events` | Live Server-Sent Events feed (below) |
| POST | `/v1/config` | Change live configuration |
| POST | `/v1/models/download` | Install a model on the host |
| POST | `/v1/transcriptions` | Transcribe a complete WAV upload |
| POST | `/v1/transcriptions/stream` | Transcribe PCM frames while the client speaks |

### POST /v1/transcriptions

Body: a WAV file (mono PCM16). Answers
`{text, durationSeconds, backend, model, serverVersion}`. `backend` is the
engine that ran (`"parakeet"` or `"whisper"`) and `model` the model id the
serving worker holds — the host's choice, never the client's.

Request headers: `x-fairspoken-language` (default `en`),
`x-fairspoken-vocabulary-hints` (percent-encoded JSON string array),
`x-fairspoken-backend` (`parakeet`/`whisper`; anything else is `400`).
`x-fairspoken-model` and `x-fairspoken-client` are accepted and ignored.

Status: `413` over the size/duration limit, `429` queue or stream capacity
reached, `500` worker failure.

### POST /v1/transcriptions/stream

`Content-Type: application/vnd.fairspoken.pcm-stream` (informational; the
host does not require it). Chunked request body of frames, sent as audio is
captured:

```
u32 LE sample_rate | u32 LE sample_count | sample_count × i16 LE samples
```

`sample_rate` must stay constant (1–192000), at most 192000 samples per frame;
a zero-count frame is a no-op. Closing the body ends the recording. The
response is the same JSON as `/v1/transcriptions`.

### POST /v1/config

Body (every field optional; `deny_unknown_fields`):
`{maxActiveStreams 1–32, maxRecordingSeconds 10–600, useGpu, model | workerModels[]}`.
`model` sets every worker; `workerModels` must list exactly one id per
worker. All fields validate before any apply; the result persists to the host
config file. Answers `{maxActiveStreams, maxRecordingSeconds, useGpu, model,
workerModels}`. Worker count and queue capacity are restart-only.

### POST /v1/models/download

Body `{"model": "<id>"}`. Answers `202 {model, status: "downloading"}`;
`409` if a download is already running. Progress appears as
`modelDownload` in `/v1/stats` and as `model_download` events.

### GET /v1/stats

```jsonc
{
  "serverVersion": "0.1.0", "bindAddr": "127.0.0.1:48173", "uptimeSeconds": 812,
  "activeSessions": 2, "activeStreams": 1, "queuedJobs": 0, "runningJobs": 1,
  "workerCount": 2, "queueCapacity": 8, "maxActiveStreams": 4,
  "maxRecordingSeconds": 600, "useGpu": true,
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
