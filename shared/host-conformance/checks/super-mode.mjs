// Super mode (PROTOCOL.md "Super mode (optional)"): x-fairspoken-super-mode,
// the response's superMode/secondaryModel and /v1/stats superMode. Every
// check skips on a host without super mode (no stats.superMode), and the
// transcription ones skip unless the host has a Parakeet and a Whisper
// engine loaded.

import { check, eq, poll, skip } from '../lib/assert.mjs';
import { framesFor, tone, wav16, wordHitRate } from '../lib/audio.mjs';

const ASK = { 'x-fairspoken-super-mode': '1' };

async function superStats(ctx) {
  const s = await ctx.stats();
  if (!s.superMode) skip('the host does not report superMode in /v1/stats (no super mode support)');
  return s;
}

async function requireBothEngines(ctx) {
  const s = await superStats(ctx);
  if (s.superMode.policy !== 'allow') skip(`super mode is turned off on this host (policy ${s.superMode.policy})`);
  if (!s.superMode.available) {
    skip('needs a Parakeet and a Whisper worker with their models loaded, e.g. FAIRSPOKEN_HOST_WORKERS=2 FAIRSPOKEN_HOST_MODEL=parakeet-tdt-0.6b-v3,base');
  }
  await ctx.requireModel();
  return ctx.stats();
}

function speechWav(ctx) {
  return wav16(ctx.speech.samples, ctx.speech.sampleRate);
}

function checkSpeech(ctx, body, label) {
  if (ctx.speech.real && ctx.transcriptCheck) {
    const rate = wordHitRate(body.text, ctx.speech.words);
    check(rate >= 0.6, `${label}: transcript ${JSON.stringify(body.text)} has only ${Math.round(rate * 100)}% of the expected words`);
  }
}

export default [
  {
    name: 'super-mode.stats',
    description: '/v1/stats superMode has policy, available, used, shed, unavailable and lentWorkers',
    async run(ctx) {
      const s = await superStats(ctx);
      const m = s.superMode;
      check(['allow', 'off'].includes(m.policy), `superMode.policy ${JSON.stringify(m.policy)} should be allow or off`);
      check(typeof m.available === 'boolean', 'superMode.available should be a boolean');
      for (const key of ['used', 'shed', 'unavailable']) {
        check(Number.isInteger(m[key]) && m[key] >= 0, `superMode.${key} should be a non-negative integer, got ${JSON.stringify(m[key])}`);
      }
      check(Array.isArray(m.lentWorkers) && m.lentWorkers.every((w) => Number.isInteger(w) && w >= 0 && w < s.workerCount), `superMode.lentWorkers should list worker indexes: ${JSON.stringify(m.lentWorkers)}`);
    },
  },
  {
    name: 'super-mode.not-asked',
    description: 'a request without x-fairspoken-super-mode gets no superMode or secondaryModel field',
    async run(ctx) {
      await superStats(ctx);
      await ctx.requireModel();
      const res = await ctx.batch(speechWav(ctx));
      check(res.status === 200, `batch: expected 200, got ${res.status}`);
      check(!('superMode' in res.json) && !('secondaryModel' in res.json), `unexpected super mode fields: ${JSON.stringify(res.json)}`);
    },
  },
  {
    name: 'super-mode.unavailable',
    description: 'on a host without both engines loaded, asking answers 200 with superMode "unavailable" (or "off" when turned off) and counts it',
    async run(ctx) {
      const s = await superStats(ctx);
      if (s.superMode.available && s.superMode.policy === 'allow') skip('the host has both engines; see super-mode.used');
      await ctx.requireModel();
      const res = await ctx.batch(speechWav(ctx), ASK);
      check(res.status === 200, `batch asking for super mode: expected 200, got ${res.status}`);
      const expected = s.superMode.policy === 'off' ? 'off' : 'unavailable';
      eq(res.json.superMode, expected, 'superMode');
      check(!('secondaryModel' in res.json), 'secondaryModel should be absent unless used');
      if (expected === 'unavailable') {
        const after = await ctx.stats();
        check(after.superMode.unavailable === s.superMode.unavailable + 1, `superMode.unavailable went ${s.superMode.unavailable} → ${after.superMode.unavailable}`);
      }
    },
  },
  {
    name: 'super-mode.used',
    description: 'with both engines idle, a stream and a batch upload asking for super mode answer superMode "used" with a secondaryModel, and stats count them',
    async run(ctx) {
      const before = await requireBothEngines(ctx);
      const served = ctx.servedModels(before);
      const stream = await ctx.streamAudio(ctx.speech.samples, ctx.speech.sampleRate, { headers: ASK, intervalMs: 25 });
      check(stream.status === 200, `stream: expected 200, got ${stream.status} ${stream.text ?? ''}`);
      eq(stream.json.superMode, 'used', 'stream superMode');
      check(served.includes(stream.json.secondaryModel), `secondaryModel ${JSON.stringify(stream.json.secondaryModel)} is not a served model (${served.join(', ')})`);
      check(stream.json.backend === 'parakeet' && stream.json.model !== stream.json.secondaryModel, `with super mode used, backend/model should name the Parakeet side: ${JSON.stringify(stream.json)}`);
      checkSpeech(ctx, stream.json, 'stream');
      ctx.note(`super mode release-to-text ${stream.latencyMs} ms`);
      await ctx.waitForIdle();
      const batch = await ctx.batch(speechWav(ctx), ASK);
      check(batch.status === 200, `batch: expected 200, got ${batch.status}`);
      eq(batch.json.superMode, 'used', 'batch superMode');
      eq(batch.json.backend, 'parakeet', 'batch backend with super mode used');
      checkSpeech(ctx, batch.json, 'batch');
      const after = await ctx.stats();
      check(after.superMode.used === before.superMode.used + 2, `superMode.used went ${before.superMode.used} → ${after.superMode.used}`);
      eq(after.superMode.lentWorkers, [], 'lentWorkers after the jobs finished');
    },
  },
  {
    name: 'super-mode.shed',
    description: 'with a worker busy (fewer than two free workers), asking for super mode is shed: 200, superMode "shed", one engine, counted',
    async run(ctx) {
      const before = await requireBothEngines(ctx);
      if (before.workerCount > 3) skip(`${before.workerCount} workers: this check holds one stream and needs at most 3 workers to leave fewer than two free`);
      await ctx.setConfig({ maxActiveStreams: Math.max(before.maxActiveStreams, 2) });
      // A held stream keeps one worker busy; the remaining free workers are
      // fewer than the two a super mode dictation needs.
      const held = ctx.host.openStream();
      let heldDone;
      try {
        held.write(framesFor(tone(0.1), 16000)[0]);
        await poll(async () => (await ctx.stats()).runningJobs === 1, 15000, 'the held stream to occupy a worker');
        const res = await ctx.batch(speechWav(ctx), ASK);
        check(res.status === 200, `batch while a worker is busy: expected 200, got ${res.status}`);
        eq(res.json.superMode, 'shed', 'superMode');
        check(!('secondaryModel' in res.json), 'secondaryModel should be absent when shed');
        checkSpeech(ctx, res.json, 'shed batch');
      } finally {
        heldDone = await ctx.cleanup('end the held stream', async () => {
          await held.writeAll(framesFor(ctx.speech.samples, ctx.speech.sampleRate), 0);
          return held.end();
        });
        await ctx.cleanup('restore maxActiveStreams', () => ctx.setConfig({ maxActiveStreams: ctx.originalConfig.maxActiveStreams }));
      }
      ctx.assertCleanedUp();
      check(heldDone.status === 200, `held stream ended with ${heldDone.status}${heldDone.error ? ` (${heldDone.error.message})` : ''}`);
      const after = await ctx.stats();
      check(after.superMode.shed === before.superMode.shed + 1, `superMode.shed went ${before.superMode.shed} → ${after.superMode.shed}`);
    },
  },
];
