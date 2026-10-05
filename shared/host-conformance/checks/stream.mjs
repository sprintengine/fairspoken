// POST /v1/transcriptions/stream (chunked PCM frames).

import { check, describe, expectError, fail, is4xx, median } from '../lib/assert.mjs';
import { concat, encodeFrame, framesFor, silence, tone, upsample } from '../lib/audio.mjs';
import { sleep } from '../lib/http.mjs';
import { traceJob } from '../lib/context.mjs';
import { checkTranscription } from './batch.mjs';

/** Sends raw chunks then ends the body; returns the response. */
async function sendChunks(ctx, chunks, { headers = {}, intervalMs = 0 } = {}) {
  // Each rejected stream still queues a job; let earlier ones drain so a
  // small queue never turns the expected error into a 429.
  await ctx.waitForQuiet();
  const upload = ctx.host.openStream({ headers });
  await upload.writeAll(chunks, intervalMs);
  return upload.end();
}

function noResponse(res, label) {
  if (res.status === null) fail(`${label}: no HTTP response (${res.error?.code || res.error?.message})`);
}

export default [
  {
    name: 'stream.speech',
    description: 'frames sent over time (with zero-count frames between them) transcribe: same response as a batch upload',
    async run(ctx) {
      await ctx.requireModel();
      const speech = ctx.speech;
      const res = await ctx.streamAudio(speech.samples, speech.sampleRate, { frameMs: 100, intervalMs: 50, zeroFrames: true });
      noResponse(res, 'stream');
      const body = await checkTranscription(ctx, res, { seconds: speech.samples.length / speech.sampleRate, label: 'POST /v1/transcriptions/stream', speech });
      ctx.note(`release-to-text ${res.latencyMs.toFixed(0)} ms; transcript ${JSON.stringify(body.text)}`);
    },
  },
  {
    name: 'stream.latency',
    description: 'release-to-text latency (body end to response end) over three real-time streams; reports the median',
    async run(ctx) {
      await ctx.requireModel();
      const speech = ctx.speech;
      const latencies = [];
      for (let i = 0; i < 3; i += 1) {
        const res = await ctx.streamAudio(speech.samples, speech.sampleRate, { frameMs: 100, intervalMs: 100 });
        noResponse(res, `run ${i + 1}`);
        await checkTranscription(ctx, res, { seconds: speech.samples.length / speech.sampleRate, label: `stream run ${i + 1}`, speech });
        latencies.push(res.latencyMs);
      }
      ctx.shared.latencies = latencies;
      ctx.note(`release-to-text median ${median(latencies).toFixed(0)} ms (runs: ${latencies.map((l) => l.toFixed(0)).join(', ')} ms; ${(speech.samples.length / speech.sampleRate).toFixed(2)} s of audio at 100 ms per 100 ms)`);
    },
  },
  {
    name: 'stream.48k',
    description: 'a stream at 48000 Hz is accepted and transcribes',
    async run(ctx) {
      await ctx.requireModel();
      const speech = ctx.speech;
      const hi = upsample(speech.samples, 3);
      const res = await ctx.streamAudio(hi, 48000, { frameMs: 100, intervalMs: 20 });
      noResponse(res, 'stream at 48 kHz');
      await checkTranscription(ctx, res, { seconds: hi.length / 48000, label: 'POST /v1/transcriptions/stream at 48 kHz', speech });
    },
  },
  {
    name: 'stream.sample-rate-change',
    description: 'a sample-rate change mid-stream answers 413',
    async run(ctx) {
      const res = await sendChunks(ctx, [...framesFor(tone(0.3), 16000), ...framesFor(tone(0.3, 48000), 48000)], { intervalMs: 10 });
      noResponse(res, 'sample-rate change');
      expectError(res, 413, 'stream switching 16000 → 48000 Hz');
    },
  },
  {
    name: 'stream.bad-sample-rate',
    description: 'sample_rate 0 or above 192000 answers 4xx',
    async run(ctx) {
      for (const rate of [0, 192001, 0xffffffff]) {
        const res = await sendChunks(ctx, [encodeFrame(rate, tone(0.1))]);
        noResponse(res, `sample_rate ${rate}`);
        expectError(res, is4xx, `stream frame with sample_rate ${rate}`);
      }
      const late = await sendChunks(ctx, [...framesFor(tone(0.2), 16000), encodeFrame(0, tone(0.1))]);
      noResponse(late, 'sample_rate 0 after valid frames');
      expectError(late, is4xx, 'stream frame with sample_rate 0 after valid frames');
    },
  },
  {
    name: 'stream.frame-too-large',
    description: 'a frame announcing more than 192000 samples answers 4xx',
    async run(ctx) {
      const header = encodeFrame(16000, new Int16Array(0), 192001);
      const res = await sendChunks(ctx, [header, Buffer.alloc(16000)]);
      noResponse(res, 'oversized frame');
      expectError(res, is4xx, 'stream frame with sample_count 192001');
    },
  },
  {
    name: 'stream.empty',
    description: 'an empty body or only zero-count frames answers 400',
    async run(ctx) {
      const empty = await sendChunks(ctx, []);
      noResponse(empty, 'empty stream');
      expectError(empty, 400, 'stream with an empty body');
      const zeros = await sendChunks(ctx, Array.from({ length: 5 }, () => encodeFrame(16000, new Int16Array(0))), { intervalMs: 10 });
      noResponse(zeros, 'zero-count frames');
      expectError(zeros, 400, 'stream of only zero-count frames');
    },
  },
  {
    name: 'stream.truncated',
    description: 'a body that ends mid-frame (header or samples) answers 4xx',
    async run(ctx) {
      const good = framesFor(tone(0.3), 16000);
      const frame = encodeFrame(16000, tone(0.1));
      const midSamples = await sendChunks(ctx, [...good, frame.subarray(0, frame.length - 101)]);
      noResponse(midSamples, 'truncated samples');
      expectError(midSamples, is4xx, 'stream ending inside a frame\'s samples');
      const midHeader = await sendChunks(ctx, [...good, frame.subarray(0, 5)]);
      noResponse(midHeader, 'truncated header');
      expectError(midHeader, is4xx, 'stream ending inside a frame header');
    },
  },
  {
    name: 'stream.backend-header',
    description: 'x-fairspoken-backend other than parakeet/whisper answers 400',
    async run(ctx) {
      const res = await sendChunks(ctx, framesFor(tone(0.3), 16000), { headers: { 'x-fairspoken-backend': 'nonsense' } });
      noResponse(res, 'bad backend header');
      expectError(res, 400, 'stream with x-fairspoken-backend: nonsense');
    },
  },
  {
    name: 'stream.over-duration',
    description: 'streaming past maxRecordingSeconds answers 413',
    async run(ctx) {
      await ctx.setConfig({ maxRecordingSeconds: 10 });
      try {
        const res = await sendChunks(ctx, framesFor(concat(tone(1), silence(10)), 16000, { frameMs: 100 }), { intervalMs: 2 });
        noResponse(res, 'over-long stream');
        expectError(res, 413, 'stream of 11 s (maxRecordingSeconds 10)');
      } finally {
        await ctx.setConfig({ maxRecordingSeconds: ctx.originalConfig.maxRecordingSeconds });
      }
    },
  },
  {
    name: 'stream.aborted-upload',
    description: 'a stream whose connection drops ends its job with job_failed "Stream upload aborted", stream_finished audioSeconds 0, and the host keeps serving',
    async run(ctx) {
      const s = await ctx.stats();
      const modelReady = s.workers.every((w) => w.modelAvailable);
      if (modelReady) await ctx.waitForIdle(180000);
      const sub = await ctx.subscribe();
      try {
        const from = sub.events.length;
        const tag = ctx.tag();
        const upload = ctx.host.openStream({ headers: tag.headers });
        const frames = framesFor(tone(2), 16000);
        await upload.writeAll(frames.slice(0, 5), 50);
        const queued = await sub.waitFor((e) => e.type === 'job_queued' && (tag.client === null || e.data.client === tag.client), 10000, from, 'job_queued for the stream');
        const jobId = queued.data.jobId;
        if (modelReady) await sub.waitFor((e) => e.type === 'job_started' && e.data.jobId === jobId, 10000, from, `job_started for job ${jobId}`);
        await upload.writeAll(frames.slice(5, 8), 50);
        upload.abort();

        const trace = await traceJob(sub, { from, client: tag.client, source: 'stream', timeoutMs: 30000 });
        check(trace.jobQueued.data.jobId === jobId, 'correlated a different job');
        await sleep(500);
        const terminals = sub.events.slice(from).filter((e) => (e.type === 'job_completed' || e.type === 'job_failed') && e.data.jobId === jobId);
        check(terminals.length === 1, `job ${jobId} has ${terminals.length} terminal events`);
        check(trace.streamFinished.data.audioSeconds === 0, `stream_finished.audioSeconds is ${trace.streamFinished.data.audioSeconds}, expected 0 for an aborted upload`);
        if (modelReady) {
          check(trace.terminal.type === 'job_failed', `aborted stream's job ended with ${trace.terminal.type}: ${trace.terminal.dataText}`);
          check(trace.terminal.data.error === 'Stream upload aborted', `job_failed.error is ${JSON.stringify(trace.terminal.data.error)}, expected "Stream upload aborted"`);
          check(trace.terminal.data.worker === trace.jobStarted.data.worker, `job_failed.worker ${trace.terminal.data.worker} differs from job_started.worker ${trace.jobStarted.data.worker}`);
        } else {
          ctx.note(`served model not installed: only checked for exactly one terminal event (${trace.terminal.type})`);
        }
      } finally {
        sub.close();
      }
      const health = await ctx.host.get('/v1/health');
      check(health.status === 200, `GET /v1/health after the aborted upload: ${describe(health)}`);
      if (modelReady) {
        await ctx.waitForIdle(30000);
        const after = await ctx.shortStream();
        check(after.status === 200, `a stream after the aborted upload: ${describe(after)}`);
      } else {
        const after = await ctx.poke();
        check(after.status === 400, `an empty stream after the aborted upload: expected 400, got ${describe(after)}`);
      }
    },
  },
];
