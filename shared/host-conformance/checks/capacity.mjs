// 429s: the active-stream limit and a full transcription queue.

import { check, eq, expectError, poll, skip } from '../lib/assert.mjs';
import { framesFor, tone, wav16 } from '../lib/audio.mjs';
import { sleep } from '../lib/http.mjs';

const MAX_QUEUE_FOR_SATURATION = 4;

/** Ends held streams with speech (silence alone may be answered with an error). */
async function finish(ctx, uploads) {
  const speech = framesFor(ctx.speech.samples, ctx.speech.sampleRate);
  return Promise.all(
    uploads.map(async (up) => {
      await up.writeAll(speech, 0);
      return up.end();
    }),
  );
}

export default [
  {
    name: 'capacity.streams',
    description: 'with maxActiveStreams 1 and one stream open, a second stream answers 429',
    async run(ctx) {
      await ctx.waitForQuiet();
      await ctx.setConfig({ maxActiveStreams: 1 });
      const held = ctx.host.openStream();
      try {
        held.write(framesFor(tone(0.1), 16000)[0]);
        await poll(async () => (await ctx.stats()).activeStreams === 1, 10000, 'the held stream to show in activeStreams');
        const second = ctx.host.openStream();
        const res = await second.end();
        check(res.status !== null, `second stream: no HTTP response (${res.error?.message})`);
        expectError(res, 429, 'second stream while maxActiveStreams (1) streams are open');
      } finally {
        const [done] = await finish(ctx, [held]);
        await ctx.setConfig({ maxActiveStreams: ctx.originalConfig.maxActiveStreams });
        check(done.status !== null, `held stream got no response after it ended (${done.error?.message})`);
      }
    },
  },
  {
    name: 'capacity.queue-full',
    description: 'with every worker busy and the queue full, a stream and a batch upload answer 429 and emit job_failed (worker null) "Transcription queue is full"',
    async run(ctx) {
      const s = await ctx.stats();
      const workers = s.workerCount;
      const queue = s.queueCapacity;
      if (queue > MAX_QUEUE_FOR_SATURATION || workers + queue + 2 > 32) {
        skip(`queueCapacity ${queue} with ${workers} worker(s) is too large to saturate; restart the host with FAIRSPOKEN_HOST_QUEUE_CAPACITY=2 (and FAIRSPOKEN_HOST_WORKERS=1) to enable this check`);
      }
      await ctx.requireModel();
      const sub = await ctx.subscribe();
      const held = [];
      try {
        await ctx.setConfig({ maxActiveStreams: workers + queue + 2 });
        for (let i = 0; i < workers + queue; i += 1) {
          const up = ctx.host.openStream({ headers: ctx.tag().headers });
          held.push(up);
          up.write(framesFor(tone(0.1), 16000)[0]);
          await poll(async () => {
            const now = await ctx.stats();
            return now.runningJobs + now.queuedJobs === i + 1 && now.runningJobs === Math.min(i + 1, workers);
          }, 15000, `${i + 1} held stream job(s) to be running or queued`);
        }
        await sleep(200);
        const full = await ctx.stats();
        check(full.runningJobs === workers && full.queuedJobs === queue, `expected ${workers} running and ${queue} queued, got ${full.runningJobs} running and ${full.queuedJobs} queued`);
        // /v1/stats ids are the /v1/events ids.
        const ids = (type, key) => new Set(sub.events.filter((e) => e.type === type).map((e) => e.data[key]));
        const startedJobs = ids('job_started', 'jobId');
        const queuedJobs = [...ids('job_queued', 'jobId')].filter((id) => !startedJobs.has(id));
        eq(full.queue.map((j) => j.id).sort((a, b) => a - b), queuedJobs.sort((a, b) => a - b), 'stats.queue[].id vs queued job ids from /v1/events');
        eq(full.workers.map((w) => w.job?.id).sort((a, b) => a - b), [...startedJobs].sort((a, b) => a - b), 'stats.workers[].job.id vs job_started ids');
        eq(full.streams.map((st) => st.id).sort((a, b) => a - b), [...ids('stream_started', 'streamId')].sort((a, b) => a - b), 'stats.streams[].id vs stream_started ids');
        check(full.queue.every((j) => j.source === 'stream' && j.audioSeconds === 0), `queued stream jobs should show source stream and audioSeconds 0: ${JSON.stringify(full.queue)}`);

        const from = sub.events.length;
        const streamTag = ctx.tag();
        const rejected = ctx.host.openStream({ headers: streamTag.headers });
        await rejected.writeAll(framesFor(tone(0.3), 16000), 10);
        const res = await rejected.end();
        check(res.status !== null, `stream with the queue full: no HTTP response (${res.error?.message})`);
        expectError(res, 429, 'stream while every worker is busy and the queue is full');
        const mine = (client) => (e) => client === null || e.data?.client === client;
        const queuedEv = await sub.waitFor((e) => e.type === 'job_queued' && e.data.source === 'stream' && mine(streamTag.client)(e), 10000, from, 'job_queued for the rejected stream');
        const failed = await sub.waitFor((e) => (e.type === 'job_failed' || e.type === 'job_completed') && e.data.jobId === queuedEv.data.jobId, 10000, from, 'terminal event for the rejected stream');
        check(failed.type === 'job_failed', `rejected stream's job ended with ${failed.type}`);
        check(failed.data.worker === null, `job_failed.worker is ${failed.data.worker}, expected null (never reached a worker)`);
        check(failed.data.error === 'Transcription queue is full', `job_failed.error is ${JSON.stringify(failed.data.error)}, expected "Transcription queue is full"`);
        const streamStarted = sub.find((e) => e.type === 'stream_started' && mine(streamTag.client)(e), from);
        check(streamStarted, 'no stream_started for the rejected stream');
        const finished = await sub.waitFor((e) => e.type === 'stream_finished' && e.data.streamId === streamStarted.data.streamId, 10000, from, 'stream_finished for the rejected stream');
        check(finished.data.audioSeconds === 0, `stream_finished.audioSeconds is ${finished.data.audioSeconds}, expected 0`);

        const batchFrom = sub.events.length;
        const batchTag = ctx.tag();
        const batch = await ctx.batch(wav16(tone(1), 16000), batchTag.headers);
        expectError(batch, 429, 'batch upload while every worker is busy and the queue is full');
        const bq = await sub.waitFor((e) => e.type === 'job_queued' && e.data.source === 'batch' && mine(batchTag.client)(e), 10000, batchFrom, 'job_queued for the rejected batch upload');
        const bf = await sub.waitFor((e) => (e.type === 'job_failed' || e.type === 'job_completed') && e.data.jobId === bq.data.jobId, 10000, batchFrom, 'terminal event for the rejected batch upload');
        check(bf.type === 'job_failed' && bf.data.worker === null && bf.data.error === 'Transcription queue is full', `rejected batch job ended with ${bf.type} ${bf.dataText}`);
        await sleep(300);
        for (const id of [queuedEv.data.jobId, bq.data.jobId]) {
          check(!sub.find((e) => e.type === 'job_started' && e.data.jobId === id, from), `rejected job ${id} was started anyway`);
        }
      } finally {
        const results = await finish(ctx, held);
        sub.close();
        await ctx.setConfig({ maxActiveStreams: ctx.originalConfig.maxActiveStreams });
        const bad = results.filter((r) => r.status !== 200);
        check(bad.length === 0, () => `held streams did not all finish with 200: ${bad.map((r) => r.status ?? r.error?.message).join(', ')}`);
      }
    },
  },
];
