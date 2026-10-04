// Whole-run audit of one /v1/events subscriber held open from the first
// check to the last: framing, payload shapes, terminal events, no
// transcript text, and snapshot + events == the final /v1/stats.

import { check, skip } from '../lib/assert.mjs';
import { sleep } from '../lib/http.mjs';
import { validateEvent } from '../lib/schema.mjs';
import { compareWithStats, reduce } from '../lib/state.mjs';
import { terminalProblems } from '../lib/context.mjs';

export default [
  {
    name: 'events.audit',
    description: 'over the whole run: every frame well-formed, every payload has exactly its PROTOCOL.md fields, every job_queued has exactly one terminal event, no transcript text in any event, and the snapshot plus all events matches the final /v1/stats',
    async run(ctx) {
      const sub = ctx.audit;
      if (!sub) skip('the audit subscriber was not opened');
      check(!sub.ended, 'the host closed the long-lived event stream during the run (it should only drop a subscriber that falls 256 frames behind)');
      await ctx.waitForIdle(60000).catch(() => {});
      await sleep(500);
      const stats = await ctx.stats();
      await sleep(300);
      const problems = [];
      const invalid = sub.frames.filter((f) => f.kind === 'invalid');
      if (invalid.length) problems.push(`${invalid.length} malformed frame(s), e.g. ${JSON.stringify(invalid[0].raw.slice(0, 120))}`);
      for (const e of sub.events) {
        if (e.data === undefined) problems.push(`${e.type} data is not JSON`);
        else problems.push(...validateEvent(e.type, e.data));
        if (e.data && typeof e.data === 'object' && 'text' in e.data) problems.push(`${e.type} carries a text field`);
      }
      // worker_state is sent only on change.
      const lastState = new Map();
      for (const e of sub.events.slice(1)) {
        if (e.type !== 'worker_state' || !e.data) continue;
        const key = `${e.data.state}|${e.data.model}`;
        if (lastState.get(e.data.worker) === key) problems.push(`worker_state repeated unchanged for worker ${e.data.worker}: ${e.dataText}`);
        lastState.set(e.data.worker, key);
      }
      const snapshot = sub.events[0].data;
      const knownJobs = new Set([...snapshot.queue.map((j) => j.id), ...snapshot.workers.filter((w) => w.job).map((w) => w.job.id)]);
      problems.push(...terminalProblems(sub.events.slice(1), { knownJobs }));
      const texts = [...new Set(ctx.transcripts.map((t) => t.trim()).filter((t) => t.length >= 8))];
      for (const text of texts) {
        if (sub.frames.some((f) => f.raw.includes(text) || f.raw.includes(JSON.stringify(text).slice(1, -1)))) problems.push(`transcript ${JSON.stringify(text)} appears in the event feed`);
      }
      problems.push(...compareWithStats(reduce(snapshot, sub.events.slice(1)), stats).map((p) => `final state: ${p}`));
      const unique = [...new Set(problems)];
      check(unique.length === 0, () => unique.slice(0, 12).join('; ') + (unique.length > 12 ? `; … ${unique.length - 12} more` : ''));
      const counts = {};
      for (const e of sub.events.slice(1)) counts[e.type] = (counts[e.type] ?? 0) + 1;
      ctx.note(`${sub.events.length - 1} events, ${sub.frames.filter((f) => f.kind === 'comment').length} heartbeat(s): ${Object.entries(counts).map(([k, v]) => `${k} ${v}`).join(', ')}`);
    },
  },
];
