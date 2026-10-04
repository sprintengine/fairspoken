// HTTP plumbing clients rely on: Expect: 100-continue and keep-alive.

import { check, eq, excerpt } from '../lib/assert.mjs';
import { parseHead, sleep } from '../lib/http.mjs';

export default [
  {
    name: 'http.expect-continue',
    description: 'a POST with Expect: 100-continue succeeds the way curl sends it (body after a 100 or after 1 s)',
    async run(ctx) {
      const conn = await ctx.host.connectRaw();
      try {
        const body = '{}';
        const auth = ctx.token ? `Authorization: Bearer ${ctx.token}\r\n` : '';
        conn.write(`POST ${ctx.host.prefix}/v1/config HTTP/1.1\r\nHost: ${ctx.host.hostHeader}\r\n${auth}Content-Type: application/json\r\nContent-Length: ${body.length}\r\nExpect: 100-continue\r\nConnection: close\r\n\r\n`);
        ctx.configDirty = true;
        let interim = null;
        try {
          interim = await conn.waitFor((buf) => {
            const head = parseHead(buf);
            return head && head.status === 100 ? head : head ? null : undefined;
          }, 1000, '100 Continue');
        } catch {
          interim = undefined; // curl sends the body anyway after 1 s
        }
        if (interim) conn.consume(interim.headLength);
        conn.write(body);
        const res = await conn.readResponse(10000);
        check(res.status === 200, `POST /v1/config with Expect: 100-continue: ${res.status} ${excerpt(res.text)}`);
        check(res.json && 'maxActiveStreams' in res.json, `unexpected body ${excerpt(res.text)}`);
        ctx.note(interim ? 'host sent 100 Continue' : 'host sent no 100 Continue (body sent after 1 s)');
      } finally {
        conn.close();
      }
    },
  },
  {
    name: 'http.keep-alive',
    description: 'two requests on one HTTP/1.1 connection both succeed',
    async run(ctx) {
      const conn = await ctx.host.connectRaw();
      try {
        const auth = ctx.token ? `Authorization: Bearer ${ctx.token}\r\n` : '';
        const req = (path) => `GET ${ctx.host.prefix}${path} HTTP/1.1\r\nHost: ${ctx.host.hostHeader}\r\n${auth}\r\n`;
        conn.write(req('/v1/health'));
        const first = await conn.readResponse(10000);
        check(first.status === 200, `first request: ${first.status} ${excerpt(first.text)}`);
        check((first.headers.connection || '').toLowerCase() !== 'close', 'first response closes the connection (Connection: close)');
        await sleep(50);
        check(!conn.ended, 'the host closed the connection after the first response');
        conn.write(req('/v1/stats'));
        const second = await conn.readResponse(10000);
        check(second.status === 200, `second request on the same connection: ${second.status} ${excerpt(second.text)}`);
        eq(typeof second.json?.workerCount, 'number', 'second response is /v1/stats');
      } finally {
        conn.close();
      }
    },
  },
];
