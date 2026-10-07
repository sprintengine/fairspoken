// Shared state and helpers handed to every check.

import { Host, sleep } from './http.mjs';
import { check, describe, fail, poll, skip } from './assert.mjs';
import { framesFor, makeSpeech } from './audio.mjs';

export class Context {
  constructor({ url, token, pairingPassword, slow, transcriptCheck }) {
    this.host = new Host(url, token);
    this.token = token || null;
    this.pairingPassword = pairingPassword || null;
    this.pairingSecrets = []; // every pairing password the run used, for the leak checks
    this.slow = !!slow;
    this.transcriptCheck = transcriptCheck !== false;
    this.notes = [];
    this.shared = {};
    this.transcripts = []; // every transcript the host returned, for the leak audit
    this.originalConfig = null;
    this.configDirty = false;
    this.nextTag = 1;
    this._speech = null;
  }

  note(message) {
    this.notes.push(message);
  }

  async stats() {
    const res = await this.host.get('/v1/stats');
    check(res.status === 200 && res.json, () => `GET /v1/stats: ${describe(res)}`);
    return res.json;
  }

  /** Remembers the live configuration so it can be restored at the end. */
  async captureConfig() {
    const s = await this.stats();
    this.initialStats = s;
    this.originalConfig = {
      maxActiveStreams: s.maxActiveStreams,
      maxRecordingSeconds: s.maxRecordingSeconds,
      useGpu: s.useGpu,
      workerModels: s.workers.map((w) => w.assignedModel),
    };
  }

  /** POST /v1/config, marking the configuration as changed. */
  async postConfig(body, opts) {
    this.configDirty = true;
    return this.host.post('/v1/config', body, opts);
  }

  async setConfig(body) {
    const res = await this.postConfig(body);
    check(res.status === 200, () => `POST /v1/config ${JSON.stringify(body)}: ${describe(res)}`);
    return res.json;
  }

  async restoreConfig() {
    if (!this.configDirty || !this.originalConfig) return null;
    const res = await this.host.post('/v1/config', this.originalConfig);
    if (res.status === 200) this.configDirty = false;
    return res;
  }

  /** Served models are the workers' assigned models. */
  servedModels(stats = this.initialStats) {
    return [...new Set(stats.workers.map((w) => w.assignedModel))];
  }

  /** Skips unless every worker's assigned model is installed, then waits for the workers to settle. */
  async requireModel() {
    const s = await this.stats();
    const missing = s.workers.filter((w) => !w.modelAvailable).map((w) => `worker ${w.index}: ${w.assignedModel}`);
    if (missing.length) skip(`needs the served model installed (${missing.join(', ')} not installed)`);
    await this.waitForIdle(180000);
  }

  /** Waits until nothing is streaming, queued or running and every worker is idle with its model loaded. */
  async waitForIdle(timeoutMs = 60000) {
    return poll(async () => {
      const s = await this.stats();
      const ready = s.workers.every((w) => w.job === null && (w.state === 'idle' || w.state === 'model-unavailable') && (!w.modelAvailable || w.loadedModel === w.assignedModel));
      return ready && s.activeStreams === 0 && s.queuedJobs === 0 && s.runningJobs === 0 ? s : null;
    }, timeoutMs, 'the host to be idle (workers idle with their model loaded, no streams or jobs)', 200);
  }

  /** Waits until no stream, queued job or running job is left (model or not). */
  async waitForQuiet(timeoutMs = 60000) {
    return poll(async () => {
      const s = await this.stats();
      return s.activeStreams === 0 && s.queuedJobs === 0 && s.runningJobs === 0 ? s : null;
    }, timeoutMs, 'the host to finish its streams and jobs', 100);
  }

  requireToken() {
    if (!this.token) skip('run with --token against a host that has a token');
  }

  requireLoopback() {
    if (!this.host.isLoopback) skip('forwarding headers are only trusted from loopback; run against 127.0.0.1');
  }

  /**
   * A forwarding header naming a unique client so a check can pick its own
   * events out of the feed. Empty (and `client: null`) when the host is not on
   * loopback; checks then match the first event after they started.
   */
  tag() {
    if (!this.host.isLoopback) return { headers: {}, client: null };
    const n = this.nextTag++;
    const client = `100.64.${200 + Math.floor(n / 250)}.${(n % 250) + 1}`;
    return { headers: { 'X-Forwarded-For': client }, client };
  }

  get speech() {
    if (!this._speech) this._speech = makeSpeech();
    return this._speech;
  }

  /** Sends a stream that the host rejects (empty body): cheap way to produce events. */
  async poke() {
    const upload = this.host.openStream();
    return upload.end();
  }

  /** Opens /v1/events, retrying a 503 while slots held by closed connections are reclaimed. */
  async subscribe(opts) {
    const deadline = Date.now() + 20000;
    for (;;) {
      const sub = await this.host.subscribe(opts);
      if (sub.status !== 503 || Date.now() > deadline) {
        check(sub.status === 200, () => `GET /v1/events answered ${sub.status}`);
        await sub.snapshot();
        return sub;
      }
      await sub.bodyDone;
      sub.close();
      await this.poke();
      await sleep(250);
    }
  }

  /**
   * Streams `samples` as frames of `frameMs`, `intervalMs` apart, then ends
   * the body. Returns the response plus the release-to-text latency.
   */
  async streamAudio(samples, sampleRate, { headers = {}, frameMs = 100, intervalMs = 25, zeroFrames = false } = {}) {
    const upload = this.host.openStream({ headers });
    await upload.writeAll(framesFor(samples, sampleRate, { frameMs, zeroFrames }), intervalMs);
    const res = await upload.end();
    if (res.status === 200 && res.json && typeof res.json.text === 'string') this.transcripts.push(res.json.text);
    return { ...res, latencyMs: res.endedAt !== null ? res.doneAt - res.endedAt : null };
  }

  async batch(wav, headers = {}) {
    const res = await this.host.post('/v1/transcriptions', wav, { headers: { 'Content-Type': 'audio/wav', ...headers }, timeoutMs: 120000 });
    if (res.status === 200 && res.json && typeof res.json.text === 'string') this.transcripts.push(res.json.text);
    return res;
  }

  /** A speech stream sent faster than real time, for checks that only need a stream to succeed. */
  async shortStream(headers = {}) {
    return this.streamAudio(this.speech.samples, this.speech.sampleRate, { headers, intervalMs: 0 });
  }
}

/**
 * Finds one request's activity in an event feed: the stream (for stream
 * uploads), its job and the job's terminal event.
 */
export async function traceJob(sub, { from = 0, client = null, source, timeoutMs = 60000, needTerminal = true }) {
  const mine = (e) => client === null || e.data?.client === client;
  const trace = {};
  if (source === 'stream') {
    trace.streamStarted = await sub.waitFor((e) => e.type === 'stream_started' && mine(e), 10000, from, 'stream_started');
  }
  trace.jobQueued = await sub.waitFor(
    (e) => e.type === 'job_queued' && mine(e) && e.data.source === source,
    10000,
    trace.streamStarted ? trace.streamStarted.index : from,
    `job_queued (source ${source})`,
  );
  const jobId = trace.jobQueued.data.jobId;
  if (needTerminal) {
    trace.terminal = await sub.waitFor((e) => (e.type === 'job_completed' || e.type === 'job_failed') && e.data.jobId === jobId, timeoutMs, from, `terminal event for job ${jobId}`);
  }
  trace.jobStarted = sub.find((e) => e.type === 'job_started' && e.data.jobId === jobId, from);
  if (trace.streamStarted) {
    const streamId = trace.streamStarted.data.streamId;
    trace.streamFinished = await sub.waitFor((e) => e.type === 'stream_finished' && e.data.streamId === streamId, 10000, from, `stream_finished for stream ${streamId}`);
  }
  return trace;
}

/** Every job_queued in `events` must have exactly one terminal event, and no terminal event may repeat. */
export function terminalProblems(events, { knownJobs = new Set() } = {}) {
  const problems = [];
  const queued = new Set();
  const terminals = new Map();
  for (const e of events) {
    if (!e.data) continue;
    if (e.type === 'job_queued') queued.add(e.data.jobId);
    if (e.type === 'job_completed' || e.type === 'job_failed') {
      const id = e.data.jobId;
      terminals.set(id, (terminals.get(id) ?? 0) + 1);
      if (!queued.has(id) && !knownJobs.has(id)) problems.push(`${e.type} for job ${id} that was never queued`);
    }
  }
  for (const id of queued) {
    const n = terminals.get(id) ?? 0;
    if (n !== 1) problems.push(`job ${id} has ${n} terminal events (expected exactly 1)`);
  }
  for (const [id, n] of terminals) {
    if (n > 1 && !queued.has(id)) problems.push(`job ${id} has ${n} terminal events`);
  }
  return problems;
}

export function assertOrder(events, labels) {
  for (let i = 1; i < labels.length; i += 1) {
    const [a, an] = labels[i - 1];
    const [b, bn] = labels[i];
    if (!a || !b) fail(`missing ${!a ? an : bn}`);
    if (!(a.index < b.index)) fail(`${an} (#${a.index}) should come before ${bn} (#${b.index})`);
  }
}
