#!/usr/bin/env node
// Conformance suite for the Fairspoken transcription host protocol
// (src-tauri/src/host/PROTOCOL.md). Runs against any host URL.
//
//   node shared/host-conformance/run.mjs --url http://127.0.0.1:48173 [--token T]
//        [--only a,b] [--skip a,b] [--slow] [--json] [--no-transcript-check] [--list]

import { parseArgs } from 'node:util';
import { CheckFailure, SkipError } from './lib/assert.mjs';
import { Context } from './lib/context.mjs';
import basic from './checks/basic.mjs';
import auth from './checks/auth.mjs';
import httpChecks from './checks/http.mjs';
import config from './checks/config.mjs';
import events from './checks/events.mjs';
import clients from './checks/clients.mjs';
import batch from './checks/batch.mjs';
import stream from './checks/stream.mjs';
import capacity from './checks/capacity.mjs';
import download from './checks/download.mjs';
import update from './checks/update.mjs';
import audit from './checks/audit.mjs';

const byName = (list, ...names) => names.map((n) => list.find((c) => c.name === n));
const [eventsHeaders, eventsSnapshot, eventsFormat, eventsChunks, eventsHeartbeat, eventsHttp10, eventsLimit, eventsStreamOrder, eventsBatch] = byName(
  events,
  'events.headers',
  'events.snapshot',
  'events.frame-format',
  'events.chunk-per-frame',
  'events.heartbeat',
  'events.http10',
  'events.subscriber-limit',
  'events.stream-order',
  'events.batch-job',
);

/** Run order: cheap protocol checks first, transcription later, the audit last. */
const CHECKS = [
  ...basic,
  ...auth,
  ...httpChecks,
  ...config,
  eventsHeaders,
  eventsSnapshot,
  eventsFormat,
  eventsChunks,
  eventsHttp10,
  eventsHeartbeat,
  eventsLimit,
  ...clients,
  ...batch,
  ...stream,
  eventsStreamOrder,
  eventsBatch,
  ...capacity,
  ...download,
  ...update,
  ...audit,
];

const CHECK_TIMEOUT_MS = 15 * 60 * 1000;

function usage() {
  return `usage: node run.mjs --url http://HOST:PORT [--token T] [--only name,group] [--skip name,group]
                   [--slow] [--json] [--no-transcript-check] [--list]`;
}

function selected(name, list) {
  return list.some((sel) => name === sel || name.startsWith(`${sel}.`));
}

function splitList(value) {
  return (value || '')
    .split(',')
    .map((s) => s.trim())
    .filter(Boolean);
}

async function main() {
  let args;
  try {
    args = parseArgs({
      options: {
        url: { type: 'string' },
        token: { type: 'string' },
        only: { type: 'string' },
        skip: { type: 'string' },
        slow: { type: 'boolean', default: false },
        json: { type: 'boolean', default: false },
        'no-transcript-check': { type: 'boolean', default: false },
        list: { type: 'boolean', default: false },
        help: { type: 'boolean', short: 'h', default: false },
      },
    }).values;
  } catch (err) {
    console.error(`${err.message}\n${usage()}`);
    return 2;
  }
  if (args.help) {
    console.log(usage());
    return 0;
  }
  if (args.list) {
    for (const c of CHECKS) console.log(`${c.name.padEnd(34)} ${c.description}`);
    return 0;
  }
  if (!args.url) {
    console.error(usage());
    return 2;
  }

  const only = splitList(args.only);
  const skipList = splitList(args.skip);
  for (const sel of [...only, ...skipList]) {
    if (!CHECKS.some((c) => selected(c.name, [sel]))) {
      console.error(`unknown check or group: ${sel} (see --list)`);
      return 2;
    }
  }
  const plan = CHECKS.filter((c) => (!only.length || selected(c.name, only)) && !selected(c.name, skipList));

  const ctx = new Context({ url: args.url, token: args.token, slow: args.slow, transcriptCheck: !args['no-transcript-check'] });
  const out = (line) => {
    if (!args.json) console.log(line);
  };

  try {
    const health = await ctx.host.get('/v1/health');
    if (health.status === 401) {
      console.error(ctx.token ? 'the host rejected --token (401 on /v1/health)' : 'the host requires a token: pass --token');
      return 2;
    }
  } catch (err) {
    console.error(`cannot reach ${args.url}: ${err.code || err.message}`);
    return 2;
  }
  try {
    await ctx.captureConfig();
  } catch (err) {
    console.error(`GET /v1/stats failed: ${err.message}`);
    return 2;
  }

  out(`host ${args.url}  token ${ctx.token ? 'yes' : 'no'}  speech ${process.platform === 'darwin' ? 'say' : 'synthetic'}  checks ${plan.length}`);
  const results = [];
  let interrupted = false;
  const onSignal = async () => {
    if (interrupted) process.exit(130);
    interrupted = true;
    console.error('\ninterrupted: restoring host config');
    try {
      await ctx.restoreConfig();
    } finally {
      process.exit(130);
    }
  };
  process.on('SIGINT', onSignal);
  process.on('SIGTERM', onSignal);

  try {
    if (plan.some((c) => c.name === 'events.audit')) {
      try {
        ctx.audit = await ctx.subscribe();
      } catch (err) {
        out(`could not open the audit subscriber: ${err.message}`);
      }
    }
    for (const c of plan) {
      ctx.notes = [];
      const started = performance.now();
      let status;
      let message = '';
      let timer;
      try {
        await Promise.race([
          c.run(ctx),
          new Promise((_, reject) => {
            timer = setTimeout(() => reject(new CheckFailure(`check did not finish within ${CHECK_TIMEOUT_MS / 60000} min`)), CHECK_TIMEOUT_MS);
          }),
        ]);
        status = 'PASS';
      } catch (err) {
        if (err instanceof SkipError) {
          status = 'SKIP';
          message = err.message;
        } else {
          status = 'FAIL';
          message = err instanceof CheckFailure ? err.message : `${err.name}: ${err.message}`;
        }
      } finally {
        clearTimeout(timer);
      }
      const ms = performance.now() - started;
      const result = { name: c.name, status, ms: Math.round(ms), message, notes: [...ctx.notes] };
      results.push(result);
      const detail = [message, ...result.notes].filter(Boolean).join(' | ');
      out(`${status}  ${c.name.padEnd(34)} ${(ms / 1000).toFixed(1).padStart(6)} s${detail ? `  ${detail}` : ''}`);
    }
  } finally {
    if (ctx.audit) ctx.audit.close();
    try {
      const restored = await ctx.restoreConfig();
      if (restored && restored.status !== 200) out(`WARNING: restoring the host config failed: ${restored.status} ${restored.text}`);
      else if (restored) out('host config restored');
    } catch (err) {
      out(`WARNING: restoring the host config failed: ${err.message}`);
    }
  }

  const count = (s) => results.filter((r) => r.status === s).length;
  const summary = { pass: count('PASS'), fail: count('FAIL'), skip: count('SKIP') };
  if (args.json) {
    console.log(JSON.stringify({ url: args.url, results, summary, latenciesMs: ctx.shared.latencies ?? null }, null, 2));
  } else {
    out(`\n${summary.pass} passed, ${summary.fail} failed, ${summary.skip} skipped`);
    for (const r of results.filter((x) => x.status === 'FAIL')) out(`  FAIL ${r.name}: ${r.message}`);
  }
  return summary.fail ? 1 : 0;
}

main().then(
  (code) => process.exit(code),
  (err) => {
    console.error(err);
    process.exit(2);
  },
);
