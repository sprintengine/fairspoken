// Shape validators for /v1/stats and /v1/events payloads, written from
// PROTOCOL.md. Each returns a list of problems (empty when valid).

export const WORKER_STATES = ['idle', 'loading', 'transcribing', 'model-unavailable'];
export const SOURCES = ['stream', 'batch'];
export const DOWNLOAD_STAGES = ['starting', 'downloading', 'validating', 'ready', 'error'];
export const BACKENDS = ['parakeet', 'whisper'];

const t = {
  string: (v) => (typeof v === 'string' ? null : 'a string'),
  nestring: (v) => (typeof v === 'string' && v.length > 0 ? null : 'a non-empty string'),
  stringOrNull: (v) => (v === null || typeof v === 'string' ? null : 'a string or null'),
  bool: (v) => (typeof v === 'boolean' ? null : 'a boolean'),
  uint: (v) => (Number.isInteger(v) && v >= 0 ? null : 'a non-negative integer'),
  uintOrNull: (v) => (v === null || (Number.isInteger(v) && v >= 0) ? null : 'a non-negative integer or null'),
  number: (v) => (typeof v === 'number' && Number.isFinite(v) && v >= 0 ? null : 'a non-negative number'),
  epochMs: (v) => (Number.isInteger(v) && v > 1.5e12 && v < Date.now() + 86400000 ? null : 'epoch milliseconds (integer)'),
  range: (lo, hi) => (v) => (Number.isInteger(v) && v >= lo && v <= hi ? null : `an integer in ${lo}..${hi}`),
  oneOf: (values) => (v) => (values.includes(v) ? null : `one of ${values.join('/')}`),
  array: (item) => (v, path, problems) => {
    if (!Array.isArray(v)) return 'an array';
    v.forEach((entry, i) => item(entry, `${path}[${i}]`, problems));
    return null;
  },
  object: (spec, opts) => (v, path, problems) => {
    if (!v || typeof v !== 'object' || Array.isArray(v)) return 'an object';
    shape(v, spec, path, problems, opts);
    return null;
  },
  nullOr: (inner) => (v, path, problems) => (v === null ? null : inner(v, path, problems)),
};

/** Checks `obj` against `spec` ({field: validator}); `exact` also flags extra fields. */
export function shape(obj, spec, path, problems, { exact = false } = {}) {
  for (const [key, validate] of Object.entries(spec)) {
    if (!(key in obj)) {
      problems.push(`${path}.${key} is missing`);
      continue;
    }
    const want = validate(obj[key], `${path}.${key}`, problems);
    if (want) problems.push(`${path}.${key} should be ${want}, got ${JSON.stringify(obj[key])}`);
  }
  if (exact) {
    for (const key of Object.keys(obj)) {
      if (!(key in spec)) problems.push(`${path} has unexpected field ${key}`);
    }
  }
  return problems;
}

const itemOf = (spec) => (v, path, problems) => {
  const want = t.object(spec)(v, path, problems);
  if (want) problems.push(`${path} should be ${want}`);
};

const MODEL = { id: t.nestring, name: t.nestring, publisher: t.string, sizeBytes: t.uint, installed: t.bool, assignedWorkers: t.array((v, p, pr) => { const w = t.uint(v); if (w) pr.push(`${p} should be ${w}`); }) };
const DOWNLOAD = { model: t.nestring, stage: t.oneOf(DOWNLOAD_STAGES), percentage: t.range(0, 100), error: t.stringOrNull };
const JOB = { id: t.uint, model: t.nestring, source: t.oneOf(SOURCES), client: t.stringOrNull, audioSeconds: t.number, elapsedMs: t.uint };
const WORKER = {
  index: t.uint,
  state: t.oneOf(WORKER_STATES),
  assignedModel: t.nestring,
  modelAvailable: t.bool,
  loadedModel: t.stringOrNull,
  completedJobs: t.uint,
  lastError: t.stringOrNull,
  job: t.nullOr(t.object(JOB)),
};
const QUEUED = { id: t.uint, model: t.nestring, source: t.oneOf(SOURCES), client: t.stringOrNull, audioSeconds: t.number, waitingMs: t.uint };
const STREAM = { id: t.uint, client: t.stringOrNull, elapsedMs: t.uint };
const CLIENT = { address: t.nestring, requests: t.uint, completed: t.uint, rejected: t.uint, failed: t.uint, totalAudioSeconds: t.number, lastSeenMs: t.epochMs, lastModel: t.stringOrNull };
const RECENT = { id: t.uint, completedAtMs: t.epochMs, durationSeconds: t.number, backend: t.nestring, model: t.nestring, source: t.oneOf(SOURCES), client: t.stringOrNull, queueWaitMs: t.uint, processingMs: t.uint };

export const STATS_SPEC = {
  serverVersion: t.nestring,
  bindAddr: t.string,
  uptimeSeconds: t.uint,
  activeSessions: t.uint,
  activeStreams: t.uint,
  queuedJobs: t.uint,
  runningJobs: t.uint,
  workerCount: t.range(1, 1024),
  queueCapacity: t.range(1, 1 << 20),
  maxActiveStreams: t.range(1, 32),
  maxRecordingSeconds: t.range(10, 600),
  useGpu: t.bool,
  pairingEnabled: t.bool,
  model: t.nestring,
  models: t.array(itemOf(MODEL)),
  modelDownload: t.nullOr(t.object(DOWNLOAD)),
  rejectedJobs: t.uint,
  failedJobs: t.uint,
  totalTranscriptions: t.uint,
  totalAudioSeconds: t.number,
  averageQueueMs: t.uint,
  averageProcessingMs: t.uint,
  workers: t.array(itemOf(WORKER)),
  queue: t.array(itemOf(QUEUED)),
  streams: t.array(itemOf(STREAM)),
  clients: t.array(itemOf(CLIENT)),
  recent: t.array(itemOf(RECENT)),
};

/** Field types plus the cross-field rules PROTOCOL.md implies. */
export function validateStats(stats, path = 'stats') {
  const problems = [];
  if (!stats || typeof stats !== 'object' || Array.isArray(stats)) return [`${path} is not a JSON object`];
  shape(stats, STATS_SPEC, path, problems);
  if (problems.length) return problems;

  const ids = stats.models.map((m) => m.id);
  if (new Set(ids).size !== ids.length) problems.push(`${path}.models has duplicate ids`);
  if (stats.workers.length !== stats.workerCount) problems.push(`${path}.workers has ${stats.workers.length} entries but workerCount is ${stats.workerCount}`);
  stats.workers.forEach((w, i) => {
    if (w.index !== i) problems.push(`${path}.workers[${i}].index is ${w.index}`);
    if (!ids.includes(w.assignedModel)) problems.push(`${path}.workers[${i}].assignedModel ${w.assignedModel} is not in models[]`);
    const model = stats.models.find((m) => m.id === w.assignedModel);
    if (model && model.installed !== w.modelAvailable) problems.push(`${path}.workers[${i}].modelAvailable (${w.modelAvailable}) disagrees with models[${w.assignedModel}].installed (${model.installed})`);
  });
  const assigned = stats.workers.map((w) => w.assignedModel);
  const uniform = assigned.every((m) => m === assigned[0]);
  const expectedModel = uniform ? assigned[0] : 'mixed';
  if (stats.model !== expectedModel) problems.push(`${path}.model is ${JSON.stringify(stats.model)}, expected ${JSON.stringify(expectedModel)} from workers[].assignedModel`);
  for (const m of stats.models) {
    const expected = assigned.flatMap((id, i) => (id === m.id ? [i] : []));
    if (JSON.stringify([...m.assignedWorkers].sort((a, b) => a - b)) !== JSON.stringify(expected)) {
      problems.push(`${path}.models[${m.id}].assignedWorkers is ${JSON.stringify(m.assignedWorkers)}, expected ${JSON.stringify(expected)}`);
    }
  }
  if (stats.activeStreams !== stats.streams.length) problems.push(`${path}.activeStreams (${stats.activeStreams}) != streams.length (${stats.streams.length})`);
  if (stats.queuedJobs !== stats.queue.length) problems.push(`${path}.queuedJobs (${stats.queuedJobs}) != queue.length (${stats.queue.length})`);
  const running = stats.workers.filter((w) => w.job !== null).length;
  if (stats.runningJobs !== running) problems.push(`${path}.runningJobs (${stats.runningJobs}) != workers with a job (${running})`);
  return problems;
}

const EVENT_COMMON = { at: t.epochMs };
export const EVENT_SPECS = {
  stream_started: { ...EVENT_COMMON, streamId: t.uint, client: t.stringOrNull },
  stream_finished: { ...EVENT_COMMON, streamId: t.uint, client: t.stringOrNull, audioSeconds: t.number },
  job_queued: { ...EVENT_COMMON, jobId: t.uint, client: t.stringOrNull, source: t.oneOf(SOURCES), model: t.nestring, audioSeconds: t.number },
  job_started: { ...EVENT_COMMON, jobId: t.uint, worker: t.uint, model: t.nestring, client: t.stringOrNull, queueWaitMs: t.uint },
  job_completed: { ...EVENT_COMMON, jobId: t.uint, worker: t.uint, model: t.nestring, client: t.stringOrNull, audioSeconds: t.number, processingMs: t.uint },
  job_failed: { ...EVENT_COMMON, jobId: t.uint, worker: t.uintOrNull, client: t.stringOrNull, error: t.nestring },
  worker_state: { ...EVENT_COMMON, worker: t.uint, state: t.oneOf(WORKER_STATES), model: t.stringOrNull },
  model_download: { ...EVENT_COMMON, model: t.nestring, stage: t.oneOf(DOWNLOAD_STAGES), percentage: t.range(0, 100) },
  pairing: { ...EVENT_COMMON, client: t.stringOrNull, clientName: t.stringOrNull, ok: t.bool },
};

/** Exactly the fields PROTOCOL.md lists for the event type (unknown types pass). */
export function validateEvent(type, data) {
  const problems = [];
  const path = `${type}`;
  if (type === 'snapshot') return validateStats(data, 'snapshot');
  const spec = EVENT_SPECS[type];
  if (!spec) return problems;
  if (!data || typeof data !== 'object' || Array.isArray(data)) return [`${path} data is not a JSON object`];
  if (type === 'model_download') {
    shape(data, data.stage === 'error' ? { ...spec, error: t.nestring } : spec, path, problems, { exact: true });
    return problems;
  }
  shape(data, spec, path, problems, { exact: true });
  return problems;
}

export const TERMINAL = new Set(['job_completed', 'job_failed']);
