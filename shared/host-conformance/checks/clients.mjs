// Client attribution from forwarding headers on loopback requests.

import { check, eq } from '../lib/assert.mjs';

async function attributed(ctx, headers, expected) {
  await ctx.waitForQuiet();
  const sub = await ctx.subscribe();
  try {
    const from = sub.events.length;
    const upload = ctx.host.openStream({ headers });
    const res = await upload.end();
    check(res.status === 400, `empty stream: expected 400, got ${res.status}`);
    const started = await sub.waitFor((e) => e.type === 'stream_started', 10000, from, 'stream_started');
    eq(started.data.client, expected, `stream_started.client for ${JSON.stringify(headers)}`);
    const queued = await sub.waitFor((e) => e.type === 'job_queued', 10000, from, 'job_queued');
    eq(queued.data.client, expected, `job_queued.client for ${JSON.stringify(headers)}`);
    const stats = await ctx.stats();
    const entry = stats.clients.find((c) => c.address === expected);
    check(entry && entry.requests >= 1, `/v1/stats clients has no entry for ${expected}`);
  } finally {
    sub.close();
  }
}

export default [
  {
    name: 'clients.x-forwarded-for',
    description: 'a loopback request with X-Forwarded-For "100.64.0.7, 10.0.0.1" is attributed to 100.64.0.7',
    async run(ctx) {
      ctx.requireLoopback();
      await attributed(ctx, { 'X-Forwarded-For': '100.64.0.7, 10.0.0.1' }, '100.64.0.7');
    },
  },
  {
    name: 'clients.tailscale-user-login',
    description: 'a loopback request with Tailscale-User-Login (no X-Forwarded-For) is attributed to that login',
    async run(ctx) {
      ctx.requireLoopback();
      await attributed(ctx, { 'Tailscale-User-Login': 'alice@example.com' }, 'alice@example.com');
    },
  },
];
