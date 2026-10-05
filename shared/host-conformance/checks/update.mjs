// Self-update routes (PROTOCOL.md "Self-update (optional)"). Optional: a host
// updated some other way answers 404 and these checks skip. Nothing here
// starts a check, an install or a restart.

import { check, describe, eq, excerpt, expectError, is4xx, skip } from '../lib/assert.mjs';

const STATES = ['idle', 'checking', 'available', 'downloading', 'ready', 'restarting', 'error'];
const CHANNELS = ['stable', 'nightly'];

async function status(ctx) {
  const res = await ctx.host.get('/v1/update');
  if (res.status === 404) skip('host has no self-updater (GET /v1/update is 404)');
  check(res.status === 200, `GET /v1/update: ${describe(res)}`);
  check(res.json && typeof res.json === 'object' && !Array.isArray(res.json), `GET /v1/update body is not a JSON object: ${excerpt(res.text)}`);
  return res.json;
}

function validateStatus(s) {
  const problems = [];
  const type = (key, ok, want) => { if (!ok(s[key])) problems.push(`${key} should be ${want}, got ${JSON.stringify(s[key])}`); };
  const str = (v) => typeof v === 'string' && v.length > 0;
  const nullable = (f) => (v) => v === null || f(v);
  type('currentVersion', str, 'a non-empty string');
  type('channel', (v) => CHANNELS.includes(v), 'stable or nightly');
  type('channelSource', (v) => ['env', 'saved', 'version'].includes(v), 'env, saved or version');
  type('defaultChannel', (v) => CHANNELS.includes(v), 'stable or nightly');
  type('autoUpdate', (v) => typeof v === 'boolean', 'a boolean');
  type('checksEnabled', (v) => typeof v === 'boolean', 'a boolean');
  type('platform', nullable(str), 'a string or null');
  type('state', (v) => STATES.includes(v), STATES.join('|'));
  type('error', nullable((v) => typeof v === 'string'), 'a string or null');
  type('lastCheckedMs', nullable(Number.isInteger), 'an integer or null');
  type('installedVersion', nullable(str), 'a string or null');
  type('restartMode', (v) => ['service', 'reexec'].includes(v), 'service or reexec');
  type('progress', nullable((v) => typeof v === 'object' && Number.isInteger(v.downloadedBytes)), 'null or {downloadedBytes, …}');
  type('available', nullable((v) => typeof v === 'object' && str(v.version) && CHANNELS.includes(v.channel) && typeof v.switchToStable === 'boolean'), 'null or {version, channel, switchToStable, …}');
  const derived = /-nightly\./.test(s.currentVersion || '') ? 'nightly' : 'stable';
  if (s.defaultChannel !== derived) problems.push(`defaultChannel ${s.defaultChannel} does not match currentVersion ${s.currentVersion}`);
  if (s.channelSource === 'version' && s.channel !== s.defaultChannel) problems.push('channelSource is version but channel differs from defaultChannel');
  if ((s.state === 'available') !== (s.available !== null && s.state === 'available')) problems.push('state available without an available update');
  return problems;
}

export default [
  {
    name: 'update.auth',
    description: 'every /v1/update route answers 401 without a token',
    async run(ctx) {
      ctx.requireToken();
      const get = await ctx.host.get('/v1/update', { auth: 'none' });
      check(get.status === 401, `GET /v1/update without a token: expected 401, got ${describe(get)}`);
      for (const path of ['/v1/update/check', '/v1/update/install', '/v1/update/restart', '/v1/update/settings']) {
        const res = await ctx.host.post(path, {}, { auth: 'none' });
        check(res.status === 401, `POST ${path} without a token: expected 401, got ${describe(res)}`);
        eq(res.json, { error: 'unauthorized' }, `POST ${path} body`);
        const query = await ctx.host.post(path, {}, { auth: 'query' });
        check(query.status === 401, `POST ${path}?token=<token> without a header: expected 401, got ${describe(query)}`);
      }
    },
  },
  {
    name: 'update.status',
    description: 'GET /v1/update reports version, channel and updater state with the documented types',
    async run(ctx) {
      const s = await status(ctx);
      const problems = validateStatus(s);
      check(problems.length === 0, () => `GET /v1/update: ${problems.join('; ')}`);
      const health = (await ctx.host.get('/v1/health')).json;
      eq(s.currentVersion, health.serverVersion, 'currentVersion vs /v1/health serverVersion');
      ctx.note(`${s.currentVersion} on ${s.channel} (${s.channelSource}), state ${s.state}, restart ${s.restartMode}`);
    },
  },
  {
    name: 'update.settings-validation',
    description: 'POST /v1/update/settings rejects bad channels, wrong types and unknown fields without changing anything',
    async run(ctx) {
      const before = await status(ctx);
      const bad = [
        [{ channel: 'beta' }, 'unknown channel'],
        [{ channel: 3 }, 'non-string channel'],
        [{ autoUpdate: 'yes' }, 'non-boolean autoUpdate'],
        [{ autoUpdate: !before.autoUpdate, chanel: 'stable' }, 'unknown field alongside a valid one'],
      ];
      for (const [body, label] of bad) {
        expectError(await ctx.host.post('/v1/update/settings', body), is4xx, `POST /v1/update/settings ${label}`);
      }
      expectError(await ctx.host.post('/v1/update/settings', '{not json'), is4xx, 'POST /v1/update/settings with an invalid body');
      const after = await status(ctx);
      eq({ channel: after.channel, channelSource: after.channelSource, autoUpdate: after.autoUpdate },
        { channel: before.channel, channelSource: before.channelSource, autoUpdate: before.autoUpdate },
        'update settings after rejected changes');
    },
  },
  {
    name: 'update.idle-actions',
    description: 'restart answers 409 when no installed update is waiting',
    async run(ctx) {
      const s = await status(ctx);
      if (s.state === 'ready' || s.state === 'restarting') skip(`host is ${s.state}; not touching it`);
      const res = await ctx.host.post('/v1/update/restart', {});
      expectError(res, 409, 'POST /v1/update/restart with nothing installed');
    },
  },
];
