// Token authentication (PROTOCOL.md "Authentication").

import { check, describe, eq, expectError } from '../lib/assert.mjs';

const UNAUTHORIZED = { error: 'unauthorized' };

/** Every authenticated route, with a request that is cheap whatever the outcome. */
const ROUTES = [
  ['GET', '/v1/health'],
  ['GET', '/v1/stats'],
  ['GET', '/v1/events'],
  ['POST', '/v1/config'],
  ['POST', '/v1/models/download'],
  ['POST', '/v1/transcriptions'],
  ['POST', '/v1/transcriptions/stream'],
  ['GET', '/v1/does-not-exist'],
];

async function call(ctx, method, path, auth) {
  if (path === '/v1/events') {
    const sub = await ctx.host.subscribe({ auth });
    if (sub.status === 200) {
      sub.close();
      return { status: 200, text: '(event stream)', json: undefined };
    }
    await sub.bodyDone;
    return { status: sub.status, text: sub.body, json: sub.json };
  }
  return ctx.host.request(method, path, { auth, body: method === 'POST' ? '' : undefined });
}

function expectUnauthorized(res, label) {
  check(res.status === 401, `${label}: expected 401, got ${describe(res)}`);
  eq(res.json, UNAUTHORIZED, `${label} body`);
}

function nearMisses(token) {
  const last = token.slice(-1);
  const swapped = token.slice(0, -1) + (last === 'x' ? 'y' : 'x');
  return [
    ['last character changed', swapped],
    ['one character longer', `${token}x`],
    ['one character shorter', token.slice(0, -1)],
    ['empty', ''],
  ];
}

export default [
  {
    name: 'auth.missing-token',
    description: 'every route except GET / and /favicon.ico answers 401 {"error":"unauthorized"} without a token',
    async run(ctx) {
      ctx.requireToken();
      for (const [method, path] of ROUTES) expectUnauthorized(await call(ctx, method, path, 'none'), `${method} ${path} without a token`);
    },
  },
  {
    name: 'auth.wrong-token',
    description: 'a wrong Bearer token or ?token= answers 401 on every route',
    async run(ctx) {
      ctx.requireToken();
      for (const [method, path] of ROUTES) {
        expectUnauthorized(await call(ctx, method, path, { bearer: `wrong-${ctx.token}` }), `${method} ${path} with a wrong Bearer token`);
        if (method === 'GET') expectUnauthorized(await call(ctx, method, path, { query: `wrong-${ctx.token}` }), `${method} ${path} with a wrong ?token=`);
      }
    },
  },
  {
    name: 'auth.near-miss-token',
    description: 'tokens differing only in the last character or in length answer 401 (header and query)',
    async run(ctx) {
      ctx.requireToken();
      for (const [label, value] of nearMisses(ctx.token)) {
        expectUnauthorized(await ctx.host.get('/v1/health', { auth: { bearer: value } }), `GET /v1/health with Bearer token ${label}`);
        expectUnauthorized(await ctx.host.get('/v1/stats', { auth: { query: value } }), `GET /v1/stats with ?token= ${label}`);
        expectUnauthorized(await ctx.host.post('/v1/config', {}, { auth: { bearer: value } }), `POST /v1/config with Bearer token ${label}`);
      }
    },
  },
  {
    name: 'auth.bearer-all-methods',
    description: 'Authorization: Bearer is accepted on every route, GET and POST',
    async run(ctx) {
      ctx.requireToken();
      for (const [method, path] of ROUTES) {
        const res = await call(ctx, method, path, 'bearer');
        check(res.status !== 401, `${method} ${path} with the right Bearer token answered 401`);
      }
      check((await ctx.host.get('/v1/health')).status === 200, 'GET /v1/health with Bearer is not 200');
      check((await ctx.postConfig({})).status === 200, 'POST /v1/config {} with Bearer is not 200');
    },
  },
  {
    name: 'auth.query-token-get',
    description: '?token= authenticates GET /v1/health, /v1/stats and /v1/events',
    async run(ctx) {
      ctx.requireToken();
      for (const path of ['/v1/health', '/v1/stats', '/v1/events']) {
        const res = await call(ctx, 'GET', path, 'query');
        check(res.status === 200, `GET ${path}?token=<token>: expected 200, got ${describe(res)}`);
      }
    },
  },
  {
    name: 'auth.query-token-post-rejected',
    description: '?token= does not authenticate POST routes (header only)',
    async run(ctx) {
      ctx.requireToken();
      expectUnauthorized(await ctx.host.post('/v1/config', {}, { auth: 'query' }), 'POST /v1/config?token=<token> without a header');
      expectUnauthorized(await ctx.host.post('/v1/models/download', { model: 'x' }, { auth: 'query' }), 'POST /v1/models/download?token=<token> without a header');
      expectUnauthorized(await ctx.host.post('/v1/transcriptions', '', { auth: 'query' }), 'POST /v1/transcriptions?token=<token> without a header');
      expectUnauthorized(await ctx.host.post('/v1/transcriptions/stream', '', { auth: 'query' }), 'POST /v1/transcriptions/stream?token=<token> without a header');
    },
  },
  {
    name: 'routes.not-found',
    description: 'an unknown route answers 404 {"error":"not_found"} when authenticated, 401 when not',
    async run(ctx) {
      for (const [method, path] of [['GET', '/v1/nope'], ['POST', '/v1/nope'], ['GET', '/v2/stats']]) {
        const res = await ctx.host.request(method, path);
        expectError(res, 404, `${method} ${path}`);
        eq(res.json, { error: 'not_found' }, `${method} ${path} body`);
      }
      if (ctx.token) expectUnauthorized(await ctx.host.get('/v1/nope', { auth: 'none' }), 'GET /v1/nope without a token');
    },
  },
];
