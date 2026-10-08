// GET /v1/events: headers, framing, snapshot, heartbeat, subscriber limit
// and the event sequence of real jobs.

import { check, eq, excerpt, fail, skip } from '../lib/assert.mjs';
import { openSubscriberCount, parseHead, sleep } from '../lib/http.mjs';
import { wav16 } from '../lib/audio.mjs';
import { validateEvent, validateStats } from '../lib/schema.mjs';
import { compareWithStats, reduce } from '../lib/state.mjs';
import { assertOrder, terminalProblems, traceJob } from '../lib/context.mjs';

const LIMIT = 16;
const LIMIT_BODY = { error: 'Too many event subscribers (limit 16)' };
const near = (a, b) => Math.abs(a - b) <= Math.max(0.05, b * 0.01);

/** Pokes the host so connections closed earlier are noticed and their slots freed. */
async function reclaimSlots(ctx) {
  for (let i = 0; i < 2; i += 1) {
    await ctx.poke();
    await sleep(250);
  }
}

export default [
  {
    name: 'events.headers',
    description: '200 with content-type text/event-stream, cache-control no-cache, connection close, x-accel-buffering no, chunked over HTTP/1.1',
    async run(ctx) {
      const sub = await ctx.subscribe();
      try {
        const h = sub.headers;
        check(/^text\/event-stream\b/i.test(h['content-type'] || ''), `content-type is ${JSON.stringify(h['content-type'])}`);
        check((h['cache-control'] || '').toLowerCase() === 'no-cache', `cache-control is ${JSON.stringify(h['cache-control'])}`);
        check((h.connection || '').toLowerCase() === 'close', `connection is ${JSON.stringify(h.connection)}`);
        check((h['x-accel-buffering'] || '').toLowerCase() === 'no', `x-accel-buffering is ${JSON.stringify(h['x-accel-buffering'])}`);
        check((h['transfer-encoding'] || '').toLowerCase() === 'chunked', `transfer-encoding is ${JSON.stringify(h['transfer-encoding'])}, expected chunked for HTTP/1.1`);
        check(!('content-length' in h), 'event stream has a content-length');
      } finally {
        sub.close();
      }
    },
  },
  {
    name: 'events.snapshot',
    description: 'the first frame is event: snapshot whose data has the /v1/stats shape',
    async run(ctx) {
      const sub = await ctx.subscribe();
      try {
        const first = sub.frames[0];
        check(first && first.kind === 'event' && first.type === 'snapshot', `first frame is ${excerpt(first?.raw)}`);
        check(first.data !== undefined, `snapshot data is not JSON: ${excerpt(first.dataText)}`);
        const problems = validateStats(first.data, 'snapshot');
        check(problems.length === 0, () => problems.join('; '));
        const stats = await ctx.stats();
        eq(Object.keys(first.data).sort(), Object.keys(stats).sort(), 'snapshot fields vs /v1/stats fields');
      } finally {
        sub.close();
      }
    },
  },
  {
    name: 'events.frame-format',
    description: 'every frame is "event: <type>\\ndata: <single-line JSON>\\n\\n" (or a ": " comment) and every payload has exactly the PROTOCOL.md fields',
    async run(ctx) {
      const sub = await ctx.subscribe();
      try {
        const from = sub.events.length;
        await ctx.poke();
        await sub.waitFor((e) => e.type === 'job_failed' || e.type === 'job_completed', 20000, from, 'a terminal event for the empty stream');
        await sub.waitFor((e) => e.type === 'stream_finished', 5000, from, 'stream_finished for the empty stream');
        await sleep(300);
        const invalid = sub.frames.filter((f) => f.kind === 'invalid');
        check(invalid.length === 0, () => `malformed frames: ${invalid.map((f) => JSON.stringify(f.raw)).join(', ')}`);
        const problems = [];
        for (const e of sub.events) {
          if (e.data === undefined) problems.push(`${e.type} data is not JSON: ${excerpt(e.dataText)}`);
          else problems.push(...validateEvent(e.type, e.data));
        }
        check(problems.length === 0, () => problems.join('; '));
        const types = sub.events.slice(from).map((e) => e.type);
        for (const want of ['stream_started', 'job_queued', 'stream_finished']) check(types.includes(want), `no ${want} for a rejected empty stream (got ${types.join(', ')})`);
      } finally {
        sub.close();
      }
    },
  },
  {
    name: 'events.chunk-per-frame',
    description: 'over HTTP/1.1 each frame is its own chunk, and frames are flushed as they happen (snapshot and a new event each within 2 s)',
    async run(ctx) {
      const conn = await ctx.host.connectRaw();
      let poked;
      try {
        const auth = ctx.token ? `Authorization: Bearer ${ctx.token}\r\n` : '';
        conn.write(`GET ${ctx.host.prefix}/v1/events HTTP/1.1\r\nHost: ${ctx.host.hostHeader}\r\n${auth}\r\n`);
        const head = await conn.waitFor((buf) => parseHead(buf), 10000, 'response head');
        check(head.status === 200, `GET /v1/events: status ${head.status}`);
        check((head.headers['transfer-encoding'] || '').toLowerCase().includes('chunked'), `transfer-encoding is ${head.headers['transfer-encoding']}`);
        conn.consume(head.headLength);
        const nextChunk = (label) =>
          conn.waitFor((buf) => {
            const lineEnd = buf.indexOf('\r\n');
            if (lineEnd === -1) return undefined;
            const size = parseInt(buf.subarray(0, lineEnd).toString('latin1'), 16);
            if (Number.isNaN(size)) throw new Error(`malformed chunk size line ${JSON.stringify(buf.subarray(0, lineEnd).toString('latin1'))}`);
            if (buf.length < lineEnd + 2 + size + 2) return undefined;
            const text = buf.subarray(lineEnd + 2, lineEnd + 2 + size).toString('utf8');
            conn.consume(lineEnd + 2 + size + 2);
            return text;
          }, 2000, label);
        const first = await nextChunk('the snapshot chunk');
        check(/^event: snapshot\ndata: [^\n]*\n\n$/.test(first), `first chunk is not exactly the snapshot frame: ${JSON.stringify(first.slice(0, 60))}… (${first.length} bytes)`);
        poked = ctx.poke();
        const chunks = [];
        for (let i = 0; i < 3; i += 1) {
          const chunk = await nextChunk(`event chunk ${i + 1} after a request that produces events`);
          chunks.push(chunk);
          check(/^event: [a-z_]+\ndata: [^\n]*\n\n$/.test(chunk) || chunk === ': ping\n\n', `chunk is not exactly one frame: ${JSON.stringify(chunk.slice(0, 120))}`);
        }
        check(chunks[0].startsWith('event: stream_started\n'), `first event chunk after a stream request is ${JSON.stringify(chunks[0].slice(0, 40))}, expected stream_started`);
      } finally {
        conn.close();
        // The poke's own request must not outlive the check (it always resolves).
        if (poked) await poked;
      }
    },
  },
  {
    name: 'events.heartbeat',
    description: 'an idle feed sends the comment frame ": ping\\n\\n" within ~16 s (--slow)',
    async run(ctx) {
      if (!ctx.slow) skip('waits up to 17 s; run with --slow');
      const sub = await ctx.subscribe();
      try {
        const t0 = performance.now();
        const ping = await sub.waitForFrame((f) => f.kind === 'comment', 17000, 'a heartbeat comment');
        const elapsed = (ping.at - t0) / 1000;
        check(ping.raw === ': ping\n\n', `heartbeat frame is ${JSON.stringify(ping.raw)}, expected ": ping\\n\\n"`);
        ctx.note(`first heartbeat after ${elapsed.toFixed(1)} s`);
      } finally {
        sub.close();
      }
    },
  },
  {
    name: 'events.http10',
    description: 'an HTTP/1.0 request gets a close-delimited (not chunked) body that starts with the snapshot frame',
    async run(ctx) {
      const conn = await ctx.host.connectRaw();
      try {
        const auth = ctx.token ? `Authorization: Bearer ${ctx.token}\r\n` : '';
        conn.write(`GET ${ctx.host.prefix}/v1/events HTTP/1.0\r\nHost: ${ctx.host.hostHeader}\r\n${auth}\r\n`);
        const head = await conn.waitFor((buf) => parseHead(buf), 10000, 'response head');
        check(head.status === 200, `HTTP/1.0 GET /v1/events: status ${head.status}`);
        check(!(head.headers['transfer-encoding'] || '').toLowerCase().includes('chunked'), `HTTP/1.0 response uses transfer-encoding ${head.headers['transfer-encoding']}`);
        check(!('content-length' in head.headers), 'HTTP/1.0 event stream has a content-length');
        check(/^text\/event-stream\b/i.test(head.headers['content-type'] || ''), `content-type is ${head.headers['content-type']}`);
        const body = await conn.waitFor((buf) => {
          const text = buf.subarray(head.headLength).toString('utf8');
          return text.includes('\n\n') ? text : undefined;
        }, 10000, 'the first frame');
        check(body.startsWith('event: snapshot\ndata: {'), `HTTP/1.0 body starts with ${JSON.stringify(body.slice(0, 40))}`);
        const frame = body.slice(0, body.indexOf('\n\n'));
        const data = JSON.parse(frame.slice('event: snapshot\ndata: '.length));
        const problems = validateStats(data, 'snapshot');
        check(problems.length === 0, () => problems.join('; '));
      } finally {
        conn.close();
      }
    },
  },
  {
    name: 'events.subscriber-limit',
    description: `${LIMIT} concurrent subscribers are accepted, the next answers 503 {"error":"Too many event subscribers (limit 16)"}, and closing one frees a slot`,
    async run(ctx) {
      await reclaimSlots(ctx);
      const mineAlready = openSubscriberCount();
      const free = LIMIT - mineAlready;
      const subs = [];
      try {
        let rejected = null;
        for (let i = 0; i < free + 1; i += 1) {
          const sub = await ctx.host.subscribe();
          if (sub.status === 200) {
            subs.push(sub);
            continue;
          }
          await sub.bodyDone;
          rejected = sub;
          break;
        }
        check(rejected, `${subs.length + mineAlready} subscribers were accepted; the limit is ${LIMIT}`);
        check(subs.length === free, `only ${subs.length + mineAlready} subscribers were accepted before a ${rejected.status} (expected ${LIMIT}); is another client (a dashboard tab) subscribed?`);
        check(rejected.status === 503, `subscriber ${LIMIT + 1}: expected 503, got ${rejected.status} ${excerpt(rejected.body)}`);
        eq(rejected.json, LIMIT_BODY, `subscriber ${LIMIT + 1} body`);

        subs.pop().close();
        const t0 = Date.now();
        let reopened = null;
        while (!reopened && Date.now() - t0 < 10000) {
          await ctx.poke();
          await sleep(200);
          const sub = await ctx.host.subscribe();
          if (sub.status === 200) reopened = sub;
          else {
            await sub.bodyDone;
            sub.close();
          }
        }
        if (!reopened) fail('closing a subscriber did not free its slot within 10 s (after events were published)');
        subs.push(reopened);
        ctx.note(`slot freed ${((Date.now() - t0) / 1000).toFixed(1)} s after closing a subscriber`);
      } finally {
        for (const sub of subs) sub.close();
        await reclaimSlots(ctx);
      }
    },
  },
  {
    name: 'events.stream-order',
    description: 'a stream transcription emits stream_started → job_queued → job_started → worker_state busy → stream_finished → job_completed → worker_state idle, and the events reproduce /v1/stats',
    async run(ctx) {
      await ctx.requireModel();
      const sub = await ctx.subscribe();
      try {
        const snapshot = sub.events[0].data;
        const from = 1;
        const tag = ctx.tag();
        const speech = ctx.speech;
        const seconds = speech.samples.length / speech.sampleRate;
        const res = await ctx.streamAudio(speech.samples, speech.sampleRate, { headers: tag.headers, intervalMs: 25 });
        check(res.status === 200, `stream: ${res.status} ${excerpt(res.text)}`);
        const trace = await traceJob(sub, { from, client: tag.client, source: 'stream' });
        const { streamStarted, jobQueued, jobStarted, streamFinished, terminal } = trace;
        check(jobStarted, 'no job_started for the job');
        const worker = jobStarted.data.worker;
        const busy = sub.find((e) => e.type === 'worker_state' && e.data.worker === worker && (e.data.state === 'transcribing' || e.data.state === 'loading'), jobStarted.index);
        const idle = await sub.waitFor((e) => e.type === 'worker_state' && e.data.worker === worker && e.data.state === 'idle', 10000, terminal.index, `worker_state idle for worker ${worker} after the terminal event`);
        assertOrder(sub.events, [
          [streamStarted, 'stream_started'],
          [jobQueued, 'job_queued'],
          [jobStarted, 'job_started'],
          [busy, `worker_state transcribing/loading for worker ${worker}`],
          [streamFinished, 'stream_finished'],
          [terminal, 'job_completed'],
          [idle, `worker_state idle for worker ${worker}`],
        ]);
        check(terminal.type === 'job_completed', `job ended with ${terminal.type}: ${terminal.dataText}`);
        eq(jobQueued.data.audioSeconds, 0, 'job_queued.audioSeconds for a stream');
        eq(jobQueued.data.model, snapshot.model, 'job_queued.model (the configured model or "mixed")');
        check(near(streamFinished.data.audioSeconds, seconds), `stream_finished.audioSeconds ${streamFinished.data.audioSeconds}, expected ≈ ${seconds.toFixed(3)}`);
        check(near(terminal.data.audioSeconds, seconds), `job_completed.audioSeconds ${terminal.data.audioSeconds}, expected ≈ ${seconds.toFixed(3)}`);
        eq(terminal.data.worker, worker, 'job_completed.worker');
        eq(terminal.data.model, jobStarted.data.model, 'job_completed.model vs job_started.model');
        eq(terminal.data.model, res.json.model, 'job_completed.model vs the response model');
        eq(busy.data.model, jobStarted.data.model, 'worker_state.model while busy');
        if (tag.client) {
          for (const e of [streamStarted, jobQueued, jobStarted, streamFinished, terminal]) eq(e.data.client, tag.client, `${e.type}.client`);
        }
        eq(streamFinished.data.streamId, streamStarted.data.streamId, 'stream_finished.streamId');
        check(terminalProblems(sub.events.slice(from)).length === 0, () => terminalProblems(sub.events.slice(from)).join('; '));
        if (res.json.text.trim()) check(!sub.frames.some((f) => f.raw.includes(res.json.text.trim())), 'the transcript text appears in the event feed');

        await ctx.waitForIdle(30000);
        await sleep(200);
        const stats = await ctx.stats();
        const problems = compareWithStats(reduce(snapshot, sub.events.slice(from)), stats, { clients: tag.client ? [tag.client] : [] });
        check(problems.length === 0, () => `snapshot + events disagree with /v1/stats: ${problems.join('; ')}`);
        eq(stats.totalTranscriptions, snapshot.totalTranscriptions + 1, 'totalTranscriptions after one stream');
        if (tag.client) {
          const before = snapshot.clients.find((c) => c.address === tag.client);
          const after = stats.clients.find((c) => c.address === tag.client);
          check(after, `/v1/stats clients has no entry for ${tag.client}`);
          eq([after.requests, after.completed], [(before?.requests ?? 0) + 1, (before?.completed ?? 0) + 1], `clients[${tag.client}] [requests, completed]`);
          eq(after.lastModel, res.json.model, `clients[${tag.client}].lastModel`);
        }
        const recent = stats.recent.find((r) => r.source === 'stream' && r.model === res.json.model && near(r.durationSeconds, seconds) && (tag.client === null || r.client === tag.client));
        check(recent, `/v1/stats recent[] has no entry for the stream (source stream, model ${res.json.model}, durationSeconds ≈ ${seconds.toFixed(3)})`);
      } finally {
        sub.close();
      }
    },
  },
  {
    name: 'events.batch-job',
    description: 'a batch upload emits job_queued (source batch, audioSeconds = duration) → job_started → job_completed → worker_state idle, and no stream events',
    async run(ctx) {
      await ctx.requireModel();
      const sub = await ctx.subscribe();
      try {
        const from = 1;
        const tag = ctx.tag();
        const speech = ctx.speech;
        const seconds = speech.samples.length / speech.sampleRate;
        const res = await ctx.batch(wav16(speech.samples, speech.sampleRate), tag.headers);
        check(res.status === 200, `batch: ${res.status} ${excerpt(res.text)}`);
        const { jobQueued, jobStarted, terminal } = await traceJob(sub, { from, client: tag.client, source: 'batch' });
        check(jobStarted, 'no job_started for the batch job');
        check(near(jobQueued.data.audioSeconds, seconds), `job_queued.audioSeconds ${jobQueued.data.audioSeconds}, expected ≈ ${seconds.toFixed(3)}`);
        check(terminal.type === 'job_completed', `batch job ended with ${terminal.type}: ${terminal.dataText}`);
        check(near(terminal.data.audioSeconds, seconds), `job_completed.audioSeconds ${terminal.data.audioSeconds}, expected ≈ ${seconds.toFixed(3)}`);
        const idle = await sub.waitFor((e) => e.type === 'worker_state' && e.data.worker === jobStarted.data.worker && e.data.state === 'idle', 10000, terminal.index, 'worker_state idle after the batch job');
        assertOrder(sub.events, [[jobQueued, 'job_queued'], [jobStarted, 'job_started'], [terminal, 'job_completed'], [idle, 'worker_state idle']]);
        if (tag.client) check(!sub.find((e) => e.type.startsWith('stream_') && e.data?.client === tag.client, from), 'a batch upload produced stream events');
      } finally {
        sub.close();
      }
    },
  },
];
