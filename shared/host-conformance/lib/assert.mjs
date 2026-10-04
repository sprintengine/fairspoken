// Assertions that throw with readable messages, plus the skip signal.

export class SkipError extends Error {}
export class CheckFailure extends Error {}

export function skip(reason) {
  throw new SkipError(reason);
}

export function fail(message) {
  throw new CheckFailure(message);
}

export function check(condition, message) {
  if (!condition) fail(typeof message === 'function' ? message() : message);
}

export function excerpt(value, max = 200) {
  const text = typeof value === 'string' ? value : JSON.stringify(value);
  if (text === undefined) return String(value);
  return text.length > max ? `${text.slice(0, max)}…` : text;
}

export function describe(res) {
  return `${res.status} ${excerpt(res.text ?? '')}`;
}

/** JSON with object keys sorted, so comparisons ignore key order. */
export function canonical(value) {
  return JSON.stringify(value, (_, v) => (v && typeof v === 'object' && !Array.isArray(v) ? Object.fromEntries(Object.keys(v).sort().map((k) => [k, v[k]])) : v));
}

export function eq(actual, expected, label) {
  const a = canonical(actual);
  const b = canonical(expected);
  check(a === b, `${label}: expected ${excerpt(b)}, got ${excerpt(a)}`);
}

/** `{error: <non-empty string>}` with the given status (number or predicate). */
export function expectError(res, status, label) {
  const statusOk = typeof status === 'function' ? status(res.status) : res.status === status;
  const want = typeof status === 'function' ? (status === is4xx ? 'a 4xx other than 401/404/405/429' : 'an error status') : String(status);
  check(statusOk, `${label}: expected ${want}, got ${describe(res)}`);
  check(
    res.json && typeof res.json === 'object' && !Array.isArray(res.json) && typeof res.json.error === 'string' && res.json.error.length > 0,
    `${label}: expected a JSON body {"error": "<message>"}, got ${excerpt(res.text ?? '')}`,
  );
  return res.json.error;
}

/** A 4xx rejection of the request itself: not auth, routing or capacity (401/404/405/429). */
export const is4xx = (s) => s >= 400 && s < 500 && ![401, 404, 405, 429].includes(s);

/** Polls `fn` until it returns a truthy value or `timeoutMs` passes. */
export async function poll(fn, timeoutMs, label, intervalMs = 100) {
  const deadline = Date.now() + timeoutMs;
  let last;
  for (;;) {
    last = await fn();
    if (last) return last;
    if (Date.now() >= deadline) fail(`timed out after ${timeoutMs} ms waiting for ${label}`);
    await new Promise((r) => setTimeout(r, intervalMs));
  }
}

export function median(values) {
  const sorted = [...values].sort((a, b) => a - b);
  const mid = Math.floor(sorted.length / 2);
  return sorted.length % 2 ? sorted[mid] : (sorted[mid - 1] + sorted[mid]) / 2;
}
