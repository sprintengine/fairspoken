// POST /v1/models/download. Only ever re-downloads a model the host already
// has, which a correct host turns into a checksum pass ending in `ready`.

import { check, describe, eq, expectError, skip } from '../lib/assert.mjs';
import { DOWNLOAD_STAGES } from '../lib/schema.mjs';

const READY_TIMEOUT_MS = 600000;

/**
 * An installed model, preferring one no worker serves (so the check never
 * touches a model in use) and the largest (its validation takes longest,
 * which gives the 409 probe a chance).
 */
function pickInstalled(stats) {
  const installed = stats.models.filter((m) => m.installed);
  const idle = installed.filter((m) => m.assignedWorkers.length === 0).sort((a, b) => b.sizeBytes - a.sizeBytes);
  return idle[0] ?? installed[0];
}

export default [
  {
    name: 'download.unknown-model',
    description: 'an unknown model id answers 400',
    async run(ctx) {
      expectError(await ctx.host.post('/v1/models/download', { model: 'no-such-model' }), 400, 'POST /v1/models/download {"model":"no-such-model"}');
    },
  },
  {
    name: 'download.invalid-body',
    description: 'an unknown field, a missing model or a non-JSON body answers 400',
    async run(ctx) {
      const s = await ctx.stats();
      const id = s.models[0].id;
      expectError(await ctx.host.post('/v1/models/download', { model: id, force: true }), 400, `POST /v1/models/download {"model":"${id}","force":true}`);
      expectError(await ctx.host.post('/v1/models/download', {}), 400, 'POST /v1/models/download {}');
      expectError(await ctx.host.post('/v1/models/download', 'not json', { headers: { 'Content-Type': 'application/json' } }), 400, 'POST /v1/models/download with a non-JSON body');
    },
  },
  {
    name: 'download.installed',
    description: 'downloading an installed model answers 202 {model, status: "downloading"}, then model_download events end in "ready"',
    async run(ctx) {
      const s = await ctx.stats();
      const model = pickInstalled(s);
      if (!model) skip('no installed model to re-download (the suite never downloads a missing model)');
      if (s.modelDownload && !['ready', 'error'].includes(s.modelDownload.stage)) skip(`a download of ${s.modelDownload.model} is already running`);
      const sub = await ctx.subscribe();
      try {
        const from = sub.events.length;
        const res = await ctx.host.post('/v1/models/download', { model: model.id });
        check(res.status === 202, `POST /v1/models/download {"model":"${model.id}"}: expected 202, got ${describe(res)}`);
        eq(res.json, { model: model.id, status: 'downloading' }, '202 body');

        // Opportunistic 409: more requests while the first still runs.
        const startedAt = Date.now();
        const probes = await Promise.all([0, 1, 2].map(() => ctx.host.post('/v1/models/download', { model: model.id })));
        const conflict = probes.find((p) => p.status !== 202) ?? probes[0];
        ctx.shared.conflict = { status: conflict.status, text: conflict.text, json: conflict.json, model: model.id };

        // Every 202 started a download of its own; let them all finish.
        const finished = (e) => e.type === 'model_download' && e.data?.model === model.id && (e.data.stage === 'ready' || e.data.stage === 'error');
        let ready = { index: from - 1 };
        for (let i = 0; i < 1 + probes.filter((p) => p.status === 202).length; i += 1) {
          ready = await sub.waitFor(finished, READY_TIMEOUT_MS, ready.index + 1, `model_download ready for ${model.id}`);
          check(ready.data.stage === 'ready', `download of installed ${model.id} ended in stage ${ready.data.stage}: ${ready.dataText}`);
        }
        const events = sub.events.slice(from).filter((e) => e.type === 'model_download');
        for (const e of events) {
          check(DOWNLOAD_STAGES.includes(e.data.stage), `model_download stage ${JSON.stringify(e.data.stage)} is not one of ${DOWNLOAD_STAGES.join('/')}`);
          check(('error' in e.data) === (e.data.stage === 'error'), `model_download ${e.data.stage} event ${'error' in e.data ? 'has' : 'lacks'} an error field: ${e.dataText}`);
        }
        for (let i = 1; i < events.length; i += 1) {
          const a = events[i - 1].data;
          const b = events[i].data;
          check(!(a.model === b.model && a.stage === b.stage && a.percentage === b.percentage), `repeated identical model_download progress: ${events[i].dataText}`);
        }
        const after = await ctx.stats();
        check(after.modelDownload && after.modelDownload.model === model.id && after.modelDownload.stage === 'ready', `/v1/stats modelDownload after ready is ${JSON.stringify(after.modelDownload)}`);
        check(after.models.find((m) => m.id === model.id)?.installed === true, `${model.id} is no longer installed after the download`);
        ctx.note(`${model.id}: stages ${events.map((e) => `${e.data.stage}(${e.data.percentage})`).join(' → ')} in ${((Date.now() - startedAt) / 1000).toFixed(1)} s`);
      } finally {
        sub.close();
      }
    },
  },
  {
    name: 'download.conflict',
    description: 'a second download while one is running answers 409 (SKIP when the first finished too quickly)',
    async run(ctx) {
      const c = ctx.shared.conflict;
      if (!c) skip('needs download.installed to run first');
      if (c.status === 202) skip(`the first download of ${c.model} had already finished when the second request arrived (202)`);
      expectError({ status: c.status, text: c.text, json: c.json }, 409, `second POST /v1/models/download {"model":"${c.model}"}`);
    },
  },
];
