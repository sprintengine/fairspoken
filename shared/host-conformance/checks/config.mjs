// POST /v1/config: echo, validation (all-or-nothing) and applying a change.

import { check, describe, eq, expectError } from '../lib/assert.mjs';

const ECHO_KEYS = ['maxActiveStreams', 'maxRecordingSeconds', 'useGpu', 'model', 'workerModels'];

function configOf(stats) {
  return {
    maxActiveStreams: stats.maxActiveStreams,
    maxRecordingSeconds: stats.maxRecordingSeconds,
    useGpu: stats.useGpu,
    model: stats.model,
    workerModels: stats.workers.map((w) => w.assignedModel),
  };
}

/** Rejected with 400 and the live config (as /v1/stats shows it) unchanged. */
async function expectRejected(ctx, body, label) {
  const before = configOf(await ctx.stats());
  const res = await ctx.postConfig(body, typeof body === 'string' ? { headers: { 'Content-Type': 'application/json' } } : undefined);
  expectError(res, 400, label);
  eq(configOf(await ctx.stats()), before, `${label}: configuration after the rejected update`);
}

/** A valid value different from the current one. */
const otherStreams = (n) => (n === 4 ? 5 : 4);
const otherSeconds = (n) => (n === 300 ? 301 : 300);

export default [
  {
    name: 'config.echo',
    description: 'POST /v1/config {} answers 200 with exactly {maxActiveStreams, maxRecordingSeconds, useGpu, model, workerModels} matching /v1/stats',
    async run(ctx) {
      const res = await ctx.postConfig({});
      check(res.status === 200, `POST /v1/config {}: ${describe(res)}`);
      check(res.json && typeof res.json === 'object', `body is not JSON: ${res.text}`);
      eq(Object.keys(res.json).sort(), [...ECHO_KEYS].sort(), 'echo fields');
      eq(res.json, configOf(await ctx.stats()), 'echo vs /v1/stats');
    },
  },
  {
    name: 'config.out-of-range',
    description: 'maxActiveStreams 0/33 and maxRecordingSeconds 9/601 answer 400 and change nothing',
    async run(ctx) {
      for (const body of [{ maxActiveStreams: 0 }, { maxActiveStreams: 33 }, { maxRecordingSeconds: 9 }, { maxRecordingSeconds: 601 }]) {
        await expectRejected(ctx, body, `POST /v1/config ${JSON.stringify(body)}`);
      }
    },
  },
  {
    name: 'config.validate-before-apply',
    description: 'a valid field sent with an invalid one is not applied',
    async run(ctx) {
      const s = await ctx.stats();
      const wrongLength = Array(s.workerCount + 1).fill(s.workers[0].assignedModel);
      const valid = { maxActiveStreams: otherStreams(s.maxActiveStreams), maxRecordingSeconds: otherSeconds(s.maxRecordingSeconds), useGpu: !s.useGpu };
      for (const body of [
        { ...valid, maxActiveStreams: 0 },
        { ...valid, maxRecordingSeconds: 601 },
        { ...valid, model: 'no-such-model' },
        { ...valid, workerModels: wrongLength },
        { ...valid, bogusField: 1 },
      ]) {
        await expectRejected(ctx, body, `POST /v1/config ${JSON.stringify(body)}`);
      }
    },
  },
  {
    name: 'config.unknown-field',
    description: 'an unknown field answers 400',
    async run(ctx) {
      await expectRejected(ctx, { bogusField: true }, 'POST /v1/config {"bogusField":true}');
      await expectRejected(ctx, { workerCount: 2 }, 'POST /v1/config {"workerCount":2} (restart-only, not a config field)');
    },
  },
  {
    name: 'config.model-and-worker-models',
    description: 'model and workerModels together answer 400',
    async run(ctx) {
      const s = await ctx.stats();
      const ids = s.workers.map((w) => w.assignedModel);
      await expectRejected(ctx, { model: ids[0], workerModels: ids }, 'POST /v1/config with model and workerModels');
    },
  },
  {
    name: 'config.worker-models-length',
    description: 'workerModels with the wrong number of entries answers 400',
    async run(ctx) {
      const s = await ctx.stats();
      const id = s.workers[0].assignedModel;
      await expectRejected(ctx, { workerModels: Array(s.workerCount + 1).fill(id) }, `POST /v1/config workerModels of length ${s.workerCount + 1}`);
      await expectRejected(ctx, { workerModels: [] }, 'POST /v1/config workerModels []');
    },
  },
  {
    name: 'config.unsupported-model',
    description: 'an unknown model id answers 400 (model and workerModels)',
    async run(ctx) {
      const s = await ctx.stats();
      await expectRejected(ctx, { model: 'no-such-model' }, 'POST /v1/config {"model":"no-such-model"}');
      await expectRejected(ctx, { workerModels: Array(s.workerCount).fill('no-such-model') }, 'POST /v1/config workerModels ["no-such-model", …]');
    },
  },
  {
    name: 'config.invalid-body',
    description: 'a non-JSON body or a wrongly typed field answers 400',
    async run(ctx) {
      await expectRejected(ctx, 'this is not json', 'POST /v1/config with a non-JSON body');
      await expectRejected(ctx, '', 'POST /v1/config with an empty body');
      await expectRejected(ctx, { maxActiveStreams: '4' }, 'POST /v1/config {"maxActiveStreams":"4"}');
      await expectRejected(ctx, { useGpu: 'yes' }, 'POST /v1/config {"useGpu":"yes"}');
      await expectRejected(ctx, [], 'POST /v1/config []');
    },
  },
  {
    name: 'config.apply',
    description: 'a valid update answers the new config, shows in /v1/stats, and can be reverted',
    async run(ctx) {
      const before = await ctx.stats();
      const ids = before.workers.map((w) => w.assignedModel);
      const update = { maxActiveStreams: otherStreams(before.maxActiveStreams), maxRecordingSeconds: otherSeconds(before.maxRecordingSeconds), useGpu: before.useGpu, workerModels: ids };
      const echo = await ctx.setConfig(update);
      const expected = { ...configOf(before), maxActiveStreams: update.maxActiveStreams, maxRecordingSeconds: update.maxRecordingSeconds };
      eq(echo, expected, 'echo after the update');
      eq(configOf(await ctx.stats()), expected, '/v1/stats after the update');
      if (ids.every((id) => id === ids[0])) {
        // `model` sets every worker; with a uniform host this changes nothing.
        const single = await ctx.setConfig({ model: ids[0] });
        eq([single.model, single.workerModels], [ids[0], ids], 'echo after {"model": <current model>}');
      }
      const restored = await ctx.setConfig({ maxActiveStreams: before.maxActiveStreams, maxRecordingSeconds: before.maxRecordingSeconds, useGpu: before.useGpu, workerModels: ids });
      eq(restored, configOf(before), 'echo after reverting');
      eq(configOf(await ctx.stats()), configOf(before), '/v1/stats after reverting');
    },
  },
];
