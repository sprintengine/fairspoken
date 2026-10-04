// Mock transcription host for the dashboard screenshot.
//
//   node docs/images/src/host-mock.mjs [port]
//
// Serves the REAL dashboard (src-tauri/src/host/dashboard.html, unmodified)
// at "/" and a realistic GET /v1/stats for a GP-practice morning clinic. The
// JSON matches StatsSnapshot in src-tauri/src/host/mod.rs field for field.
// Times are anchored to SCENARIO_NOW; render.mjs freezes the browser clock to
// the same instant so "last seen" / elapsed tickers read sensibly.

import { createServer } from "node:http";
import { readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const DASHBOARD = resolve(here, "../../../src-tauri/src/host/dashboard.html");

// Monday 5 October 2026, 10:24:30 in London (BST, UTC+1).
export const SCENARIO_NOW = Date.UTC(2026, 9, 5, 9, 24, 30);
export const SCENARIO_TZ = "Europe/London";

const V3 = "parakeet-tdt-0.6b-v3";
const V2 = "parakeet-tdt-0.6b-v2";
const TURBO = "large-v3-turbo";
const LARGE = "large-v3";

const PATEL = "Dr Patel — MacBook Pro";
const RECEPTION = "Reception — iPad";
const ROOM2 = "Consult Room 2 — iMac";
const NURSE = "Nurse — Android";
const OKAFOR = "Dr Okafor — iPhone";
const DESK = "Front desk — Windows PC";
const MANAGER = "Practice manager — Linux";

export function stats(now = SCENARIO_NOW) {
  const ago = (s) => now - s * 1000;
  // [secondsAgo, client, model, audioSeconds, queueMs, processingMs, source]
  const recent = [
    [6, NURSE, V3, 11.8, 0, 212, "stream"],
    [19, OKAFOR, TURBO, 27.4, 140, 611, "batch"],
    [33, ROOM2, LARGE, 46.2, 0, 1380, "stream"],
    [51, PATEL, V3, 18.9, 0, 236, "stream"],
    [74, RECEPTION, V2, 6.3, 0, 148, "stream"],
    [96, DESK, V3, 9.7, 310, 197, "batch"],
    [118, PATEL, V3, 32.5, 0, 318, "stream"],
    [141, OKAFOR, TURBO, 14.1, 0, 402, "stream"],
    [176, ROOM2, LARGE, 54.6, 0, 1520, "stream"],
    [203, NURSE, V3, 8.2, 0, 181, "stream"],
  ].map(([s, client, model, duration, q, p, source], i) => ({
    id: 418 - i,
    completedAtMs: ago(s),
    durationSeconds: duration,
    // NOTE: the shipped host always reports BACKEND_ID ("parakeet") here,
    // even for Whisper workers; the mock shows the engine that actually ran.
    backend: model.startsWith("parakeet") ? "parakeet" : "whisper",
    model,
    source,
    client,
    queueWaitMs: q,
    processingMs: p,
  }));

  return {
    serverVersion: "0.1.0",
    bindAddr: "127.0.0.1:48173",
    uptimeSeconds: 3 * 3600 + 54 * 60 + 12,
    activeSessions: 2,
    activeStreams: 2,
    queuedJobs: 2,
    runningJobs: 3,
    workerCount: 4,
    queueCapacity: 8,
    maxActiveStreams: 6,
    maxRecordingSeconds: 600,
    useGpu: true,
    model: "mixed",
    modelDownload: null,
    rejectedJobs: 0,
    failedJobs: 0,
    totalTranscriptions: 418,
    totalAudioSeconds: 2 * 3600 + 11 * 60 + 37,
    averageQueueMs: 46,
    averageProcessingMs: 284,
    workers: [
      { index: 0, state: "transcribing", assignedModel: V3, modelAvailable: true, loadedModel: V3, completedJobs: 188, lastError: null,
        job: { model: V3, source: "stream", client: PATEL, audioSeconds: 9.4, elapsedMs: 9400 } },
      { index: 1, state: "transcribing", assignedModel: V2, modelAvailable: true, loadedModel: V2, completedJobs: 74, lastError: null,
        job: { model: V2, source: "stream", client: RECEPTION, audioSeconds: 3.1, elapsedMs: 3100 } },
      { index: 2, state: "idle", assignedModel: TURBO, modelAvailable: true, loadedModel: TURBO, completedJobs: 83, lastError: null, job: null },
      { index: 3, state: "transcribing", assignedModel: LARGE, modelAvailable: true, loadedModel: LARGE, completedJobs: 67, lastError: null,
        job: { model: LARGE, source: "batch", client: ROOM2, audioSeconds: 52.8, elapsedMs: 1200 } },
    ],
    queue: [
      { id: 422, model: V3, source: "batch", client: DESK, audioSeconds: 7.6, waitingMs: 400 },
      { id: 423, model: LARGE, source: "batch", client: OKAFOR, audioSeconds: 21.3, waitingMs: 1100 },
    ],
    streams: [
      { client: PATEL, elapsedMs: 9400 },
      { client: RECEPTION, elapsedMs: 3100 },
    ],
    clients: [
      { address: PATEL, requests: 96, completed: 95, rejected: 0, failed: 0, totalAudioSeconds: 2210.4, lastSeenMs: ago(1), lastModel: V3 },
      { address: ROOM2, requests: 67, completed: 66, rejected: 0, failed: 0, totalAudioSeconds: 2604.9, lastSeenMs: ago(2), lastModel: LARGE },
      { address: RECEPTION, requests: 74, completed: 73, rejected: 0, failed: 0, totalAudioSeconds: 402.6, lastSeenMs: ago(1), lastModel: V2 },
      { address: OKAFOR, requests: 83, completed: 82, rejected: 0, failed: 0, totalAudioSeconds: 1288.1, lastSeenMs: ago(1), lastModel: TURBO },
      { address: NURSE, requests: 58, completed: 58, rejected: 0, failed: 0, totalAudioSeconds: 611.7, lastSeenMs: ago(6), lastModel: V3 },
      { address: DESK, requests: 38, completed: 37, rejected: 0, failed: 0, totalAudioSeconds: 379.2, lastSeenMs: ago(1), lastModel: V3 },
      { address: MANAGER, requests: 7, completed: 7, rejected: 0, failed: 0, totalAudioSeconds: 141.8, lastSeenMs: ago(14 * 60), lastModel: V3 },
    ],
    recent,
  };
}

export function startMockHost(port = 0) {
  const server = createServer((req, res) => {
    const path = new URL(req.url, "http://x").pathname;
    if (path === "/") {
      res.writeHead(200, { "content-type": "text/html; charset=utf-8" });
      return res.end(readFileSync(DASHBOARD));
    }
    if (path === "/v1/stats") {
      res.writeHead(200, { "content-type": "application/json", "cache-control": "no-store" });
      return res.end(JSON.stringify(stats()));
    }
    res.writeHead(path === "/favicon.ico" ? 204 : 404).end();
  });
  return new Promise((ok) => server.listen(port, "127.0.0.1", () => ok(server)));
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  const server = await startMockHost(Number(process.argv[2] || 48199));
  console.log(`mock host: http://127.0.0.1:${server.address().port}/`);
}
