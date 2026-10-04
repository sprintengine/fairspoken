// Dashboard, favicon, health and the /v1/stats snapshot.

import { check, describe, eq, excerpt } from '../lib/assert.mjs';
import { validateStats } from '../lib/schema.mjs';

export default [
  {
    name: 'root.dashboard',
    description: 'GET / serves the dashboard HTML without auth',
    async run(ctx) {
      const res = await ctx.host.get('/', { auth: 'none' });
      check(res.status === 200, `GET / without a token: expected 200, got ${describe(res)}`);
      check(/^text\/html\b/i.test(res.headers['content-type'] || ''), `GET / content-type is ${res.headers['content-type']}, expected text/html`);
      check(/<html[\s>]/i.test(res.text), 'GET / body is not an HTML document');
      check(res.text.includes('/v1/'), 'GET / body does not look like the host dashboard (no /v1/ API reference)');
    },
  },
  {
    name: 'root.favicon',
    description: 'GET /favicon.ico answers 204 without auth',
    async run(ctx) {
      const res = await ctx.host.get('/favicon.ico', { auth: 'none' });
      check(res.status === 204, `GET /favicon.ico without a token: expected 204, got ${describe(res)}`);
      check(res.body.length === 0, `GET /favicon.ico 204 has a body: ${excerpt(res.text)}`);
    },
  },
  {
    name: 'health',
    description: 'GET /v1/health answers {ok: true, mode: "standalone-host", backend, serverVersion}',
    async run(ctx) {
      const res = await ctx.host.get('/v1/health');
      check(res.status === 200, `GET /v1/health: ${describe(res)}`);
      check(/^application\/json\b/i.test(res.headers['content-type'] || ''), `content-type is ${res.headers['content-type']}, expected application/json`);
      const h = res.json;
      check(h && typeof h === 'object', `body is not JSON: ${excerpt(res.text)}`);
      eq(h.ok, true, 'ok');
      eq(h.mode, 'standalone-host', 'mode');
      check(typeof h.backend === 'string' && h.backend.length > 0, `backend should be a non-empty string, got ${JSON.stringify(h.backend)}`);
      check(typeof h.serverVersion === 'string' && h.serverVersion.length > 0, `serverVersion should be a non-empty string, got ${JSON.stringify(h.serverVersion)}`);
      ctx.note(`backend ${h.backend}, serverVersion ${h.serverVersion}`);
    },
  },
  {
    name: 'stats.shape',
    description: 'GET /v1/stats has every PROTOCOL.md field with the right JSON type, and consistent workers/models/model',
    async run(ctx) {
      const res = await ctx.host.get('/v1/stats');
      check(res.status === 200, `GET /v1/stats: ${describe(res)}`);
      check(/^application\/json\b/i.test(res.headers['content-type'] || ''), `content-type is ${res.headers['content-type']}, expected application/json`);
      const problems = validateStats(res.json);
      check(problems.length === 0, () => problems.join('; '));
      const s = res.json;
      const installed = s.models.filter((m) => m.installed).map((m) => m.id);
      ctx.note(`${s.workerCount} worker(s), queueCapacity ${s.queueCapacity}, model ${s.model}, installed: ${installed.join(', ') || 'none'}`);
    },
  },
];
