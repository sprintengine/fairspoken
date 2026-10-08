// Discovery and pairing (PROTOCOL.md "Discovery and pairing"): GET /v1/hello
// and POST /v1/pair without the token, the pairing password in
// POST /v1/config, the rate limit, `pairing` events, and the password never
// coming back from any route.

import { randomBytes } from 'node:crypto';
import { check, describe, eq, excerpt, expectError, fail, skip } from '../lib/assert.mjs';
import { validateEvent } from '../lib/schema.mjs';

const HELLO_KEYS = ['service', 'protocol', 'name', 'serverVersion', 'auth'];
const AUTH_MODES = ['none', 'password', 'token'];
const PER_ADDRESS_LIMIT = 5;
const WINDOW_SECONDS = 600;

/** A client address of its own, so per-address limits from earlier runs (or checks) don't interfere. */
function freshClient() {
  const [a, b, c] = randomBytes(3);
  return `10.${a}.${b}.${(c % 254) + 1}`;
}

/** A password no host is configured with. */
const wrongPassword = () => `wrong-${randomBytes(9).toString('base64url')}`;

async function hello(ctx) {
  const res = await ctx.host.get('/v1/hello', { auth: 'none' });
  check(res.status === 200, `GET /v1/hello without a token: ${describe(res)}`);
  check(res.json && typeof res.json === 'object' && !Array.isArray(res.json), `GET /v1/hello body is not a JSON object: ${excerpt(res.text)}`);
  return res.json;
}

/** POST /v1/pair from `client` (a forwarding header, trusted on loopback). */
function pair(ctx, body, client) {
  const headers = client && ctx.host.isLoopback ? { 'X-Forwarded-For': client } : {};
  return ctx.host.post('/v1/pair', body, { auth: 'none', headers });
}

async function requirePairing(ctx) {
  const h = await hello(ctx);
  if (h.auth !== 'password') skip(`needs a host with a pairing password (this host's auth is "${h.auth}")`);
  return h;
}

function requirePassword(ctx) {
  if (!ctx.pairingPassword) skip('run with --pairing-password against a host that has one');
}

function noteSecret(ctx, password) {
  if (!ctx.pairingSecrets.includes(password)) ctx.pairingSecrets.push(password);
}

/** A 200 pairing answer; a 429 for a right password names the likely cause. */
function expectPaired(res, label) {
  if (res.status === 429) {
    const wait = res.json && res.json.retryAfterSeconds;
    fail(`${label}: 429 — the host's overall pairing limit (20 failures per 10 min) is used up, probably by earlier runs; restart the host or wait ${wait ?? '?'} s`);
  }
  check(res.status === 200, `${label}: expected 200, got ${describe(res)}`);
  check(res.json && typeof res.json === 'object', `${label}: body is not JSON: ${excerpt(res.text)}`);
  eq(Object.keys(res.json).sort(), ['name', 'token'], `${label} fields`);
  return res.json;
}

/** Every answer the operator's clients can see, as text, for the never-revealed check. */
async function visibleAnswers(ctx) {
  const answers = [];
  for (const path of ['/v1/stats', '/v1/health', '/v1/update']) answers.push([`GET ${path}`, (await ctx.host.get(path)).text]);
  answers.push(['GET /v1/hello', (await ctx.host.get('/v1/hello', { auth: 'none' })).text]);
  answers.push(['POST /v1/config {}', (await ctx.postConfig({})).text]);
  return answers;
}

function expectNotRevealed(answers, password, label) {
  for (const [route, text] of answers) {
    check(!text.includes(password), `${label}: ${route} contains the pairing password`);
  }
}

export default [
  {
    name: 'hello.shape',
    description: 'GET /v1/hello answers without a token (and with a wrong one) with exactly {service, protocol, name, serverVersion, auth}',
    async run(ctx) {
      const h = await hello(ctx);
      eq(Object.keys(h).sort(), [...HELLO_KEYS].sort(), 'hello fields');
      eq(h.service, 'fairspoken-host', 'service');
      check(Number.isInteger(h.protocol) && h.protocol >= 1, `protocol should be a positive integer, got ${JSON.stringify(h.protocol)}`);
      check(typeof h.name === 'string' && h.name.trim().length > 0, `name should be a non-empty string, got ${JSON.stringify(h.name)}`);
      check(AUTH_MODES.includes(h.auth), `auth should be one of ${AUTH_MODES.join('/')}, got ${JSON.stringify(h.auth)}`);
      const health = (await ctx.host.get('/v1/health')).json;
      eq(h.serverVersion, health.serverVersion, 'serverVersion vs /v1/health');
      if (ctx.token) check(h.auth !== 'none', `auth is "none" but the host requires a token`);
      else eq(h.auth, 'none', 'auth on a host without a token');
      if (ctx.pairingPassword) eq(h.auth, 'password', 'auth on a host with a pairing password');
      if (ctx.token) {
        const wrong = await ctx.host.get('/v1/hello', { auth: { bearer: `wrong-${ctx.token}` } });
        check(wrong.status === 200, `GET /v1/hello with a wrong Bearer token: expected 200, got ${describe(wrong)}`);
      }
      ctx.note(`name ${JSON.stringify(h.name)}, auth ${h.auth}, protocol ${h.protocol}`);
    },
  },
  {
    name: 'stats.pairing-enabled',
    description: '/v1/stats pairingEnabled is a boolean that matches /v1/hello auth "password"',
    async run(ctx) {
      const s = await ctx.stats();
      check(typeof s.pairingEnabled === 'boolean', `pairingEnabled should be a boolean, got ${JSON.stringify(s.pairingEnabled)}`);
      const h = await hello(ctx);
      eq(s.pairingEnabled, h.auth === 'password', 'pairingEnabled vs hello auth === "password"');
    },
  },
  {
    name: 'pair.malformed',
    description: 'POST /v1/pair answers 400 {"error"} for invalid JSON, a missing or non-string password and unknown fields',
    async run(ctx) {
      for (const [label, body] of [
        ['invalid JSON', '{"password":'],
        ['no password', { clientName: 'conformance' }],
        ['a number password', { password: 123456 }],
        ['an unknown field', { password: wrongPassword(), token: 'x' }],
      ]) {
        const res = await ctx.host.post('/v1/pair', body, { auth: 'none', headers: { 'Content-Type': 'application/json', ...(ctx.host.isLoopback ? { 'X-Forwarded-For': freshClient() } : {}) } });
        expectError(res, 400, `POST /v1/pair with ${label}`);
      }
    },
  },
  {
    name: 'pair.without-password',
    description: 'with no pairing password: 404 {"error":"pairing disabled"} on a host with a token, 200 {"token":null,name} on a host without one',
    async run(ctx) {
      const h = await hello(ctx);
      if (h.auth === 'password') skip('the host has a pairing password (config.pairing-password covers the disabled case)');
      const res = await pair(ctx, { password: wrongPassword(), clientName: 'conformance' }, freshClient());
      if (h.auth === 'token') {
        expectError(res, 404, 'POST /v1/pair on a host without a pairing password');
        eq(res.json, { error: 'pairing disabled' }, 'body');
      } else {
        eq(expectPaired(res, 'POST /v1/pair on a host without a token'), { token: null, name: h.name }, 'body');
      }
    },
  },
  {
    name: 'pair.correct-password',
    description: 'the pairing password answers 200 {token, name}; the token is the host token and authenticates',
    async run(ctx) {
      requirePassword(ctx);
      const h = await requirePairing(ctx);
      noteSecret(ctx, ctx.pairingPassword);
      const body = expectPaired(await pair(ctx, { password: ctx.pairingPassword, clientName: 'Conformance suite' }, freshClient()), 'POST /v1/pair with the right password');
      check(typeof body.token === 'string' && body.token.length > 0, `token should be a non-empty string, got ${JSON.stringify(body.token)}`);
      eq(body.name, h.name, 'name vs /v1/hello');
      if (ctx.token) eq(body.token, ctx.token, 'token vs --token');
      const health = await ctx.host.get('/v1/health', { auth: { bearer: body.token } });
      check(health.status === 200, `GET /v1/health with the paired token: ${describe(health)}`);
    },
  },
  {
    name: 'pair.wrong-password',
    description: 'a wrong pairing password answers 401 {"error":"wrong password"}',
    async run(ctx) {
      await requirePairing(ctx);
      const res = await pair(ctx, { password: wrongPassword() }, freshClient());
      expectError(res, 401, 'POST /v1/pair with a wrong password');
      eq(res.json, { error: 'wrong password' }, 'body');
    },
  },
  {
    name: 'pair.rate-limit',
    description: 'after 5 wrong passwords from one address, every attempt from it (right or wrong) answers 429 with Retry-After and retryAfterSeconds; other addresses still pair',
    async run(ctx) {
      ctx.requireLoopback();
      await requirePairing(ctx);
      const client = freshClient();
      for (let i = 1; i <= PER_ADDRESS_LIMIT; i += 1) {
        const res = await pair(ctx, { password: wrongPassword() }, client);
        check(res.status === 401, `wrong password #${i} from ${client}: expected 401, got ${describe(res)}${res.status === 429 ? ' (the overall limit may be used up by earlier runs; restart the host)' : ''}`);
      }
      const limited = await pair(ctx, { password: wrongPassword() }, client);
      expectError(limited, 429, `wrong password #${PER_ADDRESS_LIMIT + 1} from ${client}`);
      eq(Object.keys(limited.json).sort(), ['error', 'retryAfterSeconds'], '429 body fields');
      eq(limited.json.error, 'too many attempts', '429 error');
      const seconds = limited.json.retryAfterSeconds;
      check(Number.isInteger(seconds) && seconds >= 1 && seconds <= WINDOW_SECONDS, `retryAfterSeconds should be an integer in 1..${WINDOW_SECONDS}, got ${JSON.stringify(seconds)}`);
      const header = limited.headers['retry-after'];
      check(header !== undefined && /^\d+$/.test(header), `Retry-After should be a number of seconds, got ${JSON.stringify(header)}`);
      check(Math.abs(Number(header) - seconds) <= 1, `Retry-After (${header}) disagrees with retryAfterSeconds (${seconds})`);
      if (ctx.pairingPassword) {
        const right = await pair(ctx, { password: ctx.pairingPassword }, client);
        expectError(right, 429, `the right password from the rate-limited ${client}`);
        expectPaired(await pair(ctx, { password: ctx.pairingPassword }, freshClient()), 'the right password from another address');
      } else {
        ctx.note('without --pairing-password: right-password-while-limited not checked');
      }
    },
  },
  {
    name: 'pair.events',
    description: 'each attempt emits a pairing event {at, client, clientName, ok} with no password in it',
    async run(ctx) {
      ctx.requireLoopback();
      await requirePairing(ctx);
      const sub = await ctx.subscribe();
      try {
        const from = sub.events.length;
        const failing = freshClient();
        const guess = wrongPassword();
        const res = await pair(ctx, { password: guess, clientName: '  Conformance laptop  ' }, failing);
        check(res.status === 401, `wrong password: expected 401, got ${describe(res)}`);
        const refused = await sub.waitFor((e) => e.type === 'pairing' && e.data?.client === failing, 10000, from, `pairing event for ${failing}`);
        const problems = validateEvent('pairing', refused.data);
        check(problems.length === 0, () => problems.join('; '));
        eq(refused.data.ok, false, 'ok for a wrong password');
        eq(refused.data.clientName, 'Conformance laptop', 'clientName (trimmed)');
        check(!refused.raw.includes(guess), 'the pairing event carries the password that was tried');
        if (ctx.pairingPassword) {
          const passing = freshClient();
          expectPaired(await pair(ctx, { password: ctx.pairingPassword }, passing), 'right password');
          const paired = await sub.waitFor((e) => e.type === 'pairing' && e.data?.client === passing, 10000, from, `pairing event for ${passing}`);
          eq(paired.data.ok, true, 'ok for the right password');
          eq(paired.data.clientName, null, 'clientName when none was sent');
          check(!paired.raw.includes(ctx.pairingPassword), 'the pairing event carries the pairing password');
        }
      } finally {
        sub.close();
      }
    },
  },
  {
    name: 'config.pairing-password',
    description: 'POST /v1/config pairingPassword: 6–128 characters or 400 with nothing changed; a string turns pairing on, null and "" turn it off; answers report pairingEnabled only',
    async run(ctx) {
      ctx.requireToken();
      const before = await ctx.stats();
      if (before.pairingEnabled && !ctx.pairingPassword) skip('the host has a pairing password; pass it with --pairing-password so the check can restore it');
      const original = before.pairingEnabled ? ctx.pairingPassword : null;
      const password = `conf-${randomBytes(6).toString('hex')}`;
      noteSecret(ctx, password);
      try {
        for (const [label, value] of [
          ['5 characters', '12345'],
          ['129 characters', 'x'.repeat(129)],
          ['a number', 123456],
          ['an array', [password]],
        ]) {
          const res = await ctx.postConfig({ pairingPassword: value });
          expectError(res, 400, `POST /v1/config pairingPassword ${label}`);
          check(!res.text.includes(password), `the 400 for ${label} echoes the password`);
          eq((await ctx.stats()).pairingEnabled, before.pairingEnabled, `pairingEnabled after the rejected ${label}`);
        }
        const withStreams = await ctx.postConfig({ maxActiveStreams: 0, pairingPassword: password });
        expectError(withStreams, 400, 'a valid pairingPassword with maxActiveStreams 0');
        eq((await ctx.stats()).pairingEnabled, before.pairingEnabled, 'pairingEnabled after a rejected update with a valid password');

        const set = await ctx.setConfig({ pairingPassword: password });
        eq(set.pairingEnabled, true, 'pairingEnabled in the answer after setting a password');
        check(!JSON.stringify(set).includes(password), 'the config answer contains the password');
        eq((await ctx.stats()).pairingEnabled, true, '/v1/stats pairingEnabled after setting a password');
        eq((await hello(ctx)).auth, 'password', 'hello auth after setting a password');
        expectNotRevealed(await visibleAnswers(ctx), password, 'after setting a password');
        const paired = expectPaired(await pair(ctx, { password }, freshClient()), 'POST /v1/pair with the new password');
        eq(paired.token, ctx.token, 'token from pairing vs --token');

        const off = await ctx.setConfig({ pairingPassword: null });
        eq(off.pairingEnabled, false, 'pairingEnabled after pairingPassword null');
        eq((await hello(ctx)).auth, 'token', 'hello auth with pairing off');
        const disabled = await pair(ctx, { password }, freshClient());
        expectError(disabled, 404, 'POST /v1/pair with pairing off');
        eq(disabled.json, { error: 'pairing disabled' }, '404 body');

        await ctx.setConfig({ pairingPassword: password });
        eq((await ctx.setConfig({ pairingPassword: '' })).pairingEnabled, false, 'pairingEnabled after pairingPassword ""');
        eq((await ctx.setConfig({ pairingPassword: '123456' })).pairingEnabled, true, 'a six-digit number is a valid password');
        noteSecret(ctx, '123456');
      } finally {
        if (await ctx.cleanup('restore pairingPassword', () => ctx.setConfig({ pairingPassword: original }))) ctx.pairingDirty = false;
      }
      ctx.assertCleanedUp();
      eq((await ctx.stats()).pairingEnabled, before.pairingEnabled, 'pairingEnabled after restoring');
    },
  },
  {
    name: 'pair.password-not-revealed',
    description: 'the pairing password appears in no /v1/stats, /v1/health, /v1/update, /v1/hello or POST /v1/config answer',
    async run(ctx) {
      requirePassword(ctx);
      await requirePairing(ctx);
      noteSecret(ctx, ctx.pairingPassword);
      expectNotRevealed(await visibleAnswers(ctx), ctx.pairingPassword, 'with --pairing-password');
    },
  },
];
