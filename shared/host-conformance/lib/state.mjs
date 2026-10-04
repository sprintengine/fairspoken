// Applies /v1/events frames to a /v1/stats snapshot, the way a dashboard
// keeps its view current, so the result can be compared with a fresh
// /v1/stats.

export function reduce(snapshot, events) {
  const state = {
    totalTranscriptions: snapshot.totalTranscriptions,
    totalAudioSeconds: snapshot.totalAudioSeconds,
    queue: new Set(snapshot.queue.map((j) => j.id)),
    streams: new Set(snapshot.streams.map((s) => s.id)),
    running: new Map(snapshot.workers.filter((w) => w.job).map((w) => [w.index, w.job.id])),
    workerStates: snapshot.workers.map((w) => w.state),
    clientCompleted: new Map(snapshot.clients.map((c) => [c.address, c.completed])),
  };
  for (const { type, data } of events) {
    if (!data) continue;
    switch (type) {
      case 'stream_started':
        state.streams.add(data.streamId);
        break;
      case 'stream_finished':
        state.streams.delete(data.streamId);
        break;
      case 'job_queued':
        state.queue.add(data.jobId);
        break;
      case 'job_started':
        state.queue.delete(data.jobId);
        state.running.set(data.worker, data.jobId);
        break;
      case 'job_completed':
        state.totalTranscriptions += 1;
        state.totalAudioSeconds += data.audioSeconds;
        state.running.delete(data.worker);
        if (data.client !== null) state.clientCompleted.set(data.client, (state.clientCompleted.get(data.client) ?? 0) + 1);
        break;
      case 'job_failed':
        state.queue.delete(data.jobId);
        if (data.worker !== null) state.running.delete(data.worker);
        break;
      case 'worker_state':
        state.workerStates[data.worker] = data.state;
        break;
      default:
        break;
    }
  }
  return state;
}

/** Differences between a reduced state and a fresh /v1/stats. */
export function compareWithStats(state, stats, { clients = [] } = {}) {
  const problems = [];
  if (state.totalTranscriptions !== stats.totalTranscriptions) {
    problems.push(`totalTranscriptions: events give ${state.totalTranscriptions}, /v1/stats says ${stats.totalTranscriptions}`);
  }
  if (Math.abs(state.totalAudioSeconds - stats.totalAudioSeconds) > 0.05 + 1e-4 * stats.totalAudioSeconds) {
    problems.push(`totalAudioSeconds: events give ${state.totalAudioSeconds.toFixed(3)}, /v1/stats says ${stats.totalAudioSeconds}`);
  }
  const sorted = (s) => JSON.stringify([...s].sort((a, b) => a - b));
  if (sorted(state.queue) !== sorted(stats.queue.map((j) => j.id))) {
    problems.push(`queue: events give ${sorted(state.queue)}, /v1/stats says ${sorted(stats.queue.map((j) => j.id))}`);
  }
  if (sorted(state.streams) !== sorted(stats.streams.map((s) => s.id))) {
    problems.push(`streams: events give ${sorted(state.streams)}, /v1/stats says ${sorted(stats.streams.map((s) => s.id))}`);
  }
  stats.workers.forEach((w, i) => {
    if (state.workerStates[i] !== w.state) problems.push(`workers[${i}].state: events give ${state.workerStates[i]}, /v1/stats says ${w.state}`);
    const runningId = state.running.get(i) ?? null;
    const actual = w.job ? w.job.id : null;
    if (runningId !== actual) problems.push(`workers[${i}].job: events give ${runningId}, /v1/stats says ${actual}`);
  });
  for (const address of clients) {
    const fromEvents = state.clientCompleted.get(address) ?? 0;
    const entry = stats.clients.find((c) => c.address === address);
    const actual = entry ? entry.completed : 0;
    if (fromEvents !== actual) problems.push(`clients[${address}].completed: events give ${fromEvents}, /v1/stats says ${actual}`);
  }
  return problems;
}
