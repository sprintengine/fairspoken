// HTTP helpers for talking to a transcription host: plain requests, the
// chunked PCM stream upload, the Server-Sent Events reader and a raw socket
// reader for exchanges node:http hides (HTTP/1.0, Expect, keep-alive).

import http from 'node:http';
import net from 'node:net';

export const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

export const STREAM_CONTENT_TYPE = 'application/vnd.fairspoken.pcm-stream';

/** Every SSE connection this process holds open (see `openSubscriberCount`). */
const openSubscribers = new Set();
export const openSubscriberCount = () => openSubscribers.size;
/** Stream uploads and raw connections still open, for `closeOpenConnections`. */
const openUploads = new Set();
const openRaw = new Set();

/** Destroys every open subscriber, stream upload and raw connection (after a check timed out). */
export function closeOpenConnections() {
  const n = openSubscribers.size + openUploads.size + openRaw.size;
  for (const sub of [...openSubscribers]) sub.close();
  for (const up of [...openUploads]) up.abort();
  for (const conn of [...openRaw]) conn.close();
  return n;
}

function tryJson(text) {
  try {
    return JSON.parse(text);
  } catch {
    return undefined;
  }
}

export class Host {
  constructor(baseUrl, token) {
    const url = new URL(baseUrl);
    if (url.protocol !== 'http:') throw new Error(`Only http:// URLs are supported (got ${baseUrl})`);
    this.url = url;
    this.hostname = url.hostname.replace(/^\[|\]$/g, '');
    this.port = Number(url.port || 80);
    this.prefix = url.pathname.replace(/\/+$/, '');
    this.token = token || null;
    this.aborted = null;
  }

  /**
   * Refuses every later request, upload, subscription and raw connection on
   * this Host with `reason`, so a check that outlived its timeout stops at its
   * next call instead of running alongside the rest of the run.
   */
  abort(reason) {
    this.aborted = new Error(reason);
  }

  refuseIfAborted() {
    if (this.aborted) throw this.aborted;
  }

  /** True when the host is reached over loopback, so the suite's requests
   *  arrive from loopback and forwarding headers are trusted. */
  get isLoopback() {
    const h = this.hostname.toLowerCase();
    return h === 'localhost' || h === '::1' || /^127\./.test(h);
  }

  get hostHeader() {
    return this.url.host;
  }

  /**
   * auth: 'bearer' (default; nothing when the suite has no token), 'none',
   * 'query', { bearer: '<value>' } or { query: '<value>' }.
   */
  resolve(path, auth = 'bearer', headers = {}) {
    const out = { ...headers };
    let fullPath = this.prefix + path;
    const addQuery = (value) => {
      fullPath += (fullPath.includes('?') ? '&' : '?') + 'token=' + encodeURIComponent(value);
    };
    if (auth === 'bearer') {
      if (this.token) out.Authorization = `Bearer ${this.token}`;
    } else if (auth === 'query') {
      if (this.token) addQuery(this.token);
    } else if (auth && typeof auth === 'object') {
      if ('bearer' in auth) out.Authorization = `Bearer ${auth.bearer}`;
      if ('query' in auth) addQuery(auth.query);
    }
    return { path: fullPath, headers: out };
  }

  /** One request on a fresh connection. Resolves with the full response. */
  request(method, path, { headers = {}, body, auth = 'bearer', timeoutMs = 30000 } = {}) {
    if (this.aborted) return Promise.reject(this.aborted);
    const resolved = this.resolve(path, auth, headers);
    return new Promise((resolve, reject) => {
      const payload = body === undefined ? undefined : Buffer.isBuffer(body) ? body : Buffer.from(typeof body === 'string' ? body : JSON.stringify(body));
      const reqHeaders = { ...resolved.headers };
      if (payload !== undefined) {
        reqHeaders['Content-Length'] = payload.length;
        if (body !== undefined && !Buffer.isBuffer(body) && typeof body !== 'string' && !Object.keys(reqHeaders).some((k) => k.toLowerCase() === 'content-type')) {
          reqHeaders['Content-Type'] = 'application/json';
        }
      } else if (method !== 'GET' && method !== 'HEAD') {
        reqHeaders['Content-Length'] = 0;
      }
      const req = http.request(
        { host: this.hostname, port: this.port, method, path: resolved.path, headers: reqHeaders, agent: false },
        (res) => {
          const chunks = [];
          res.on('data', (c) => chunks.push(c));
          res.on('end', () => {
            clearTimeout(timer);
            const buf = Buffer.concat(chunks);
            const text = buf.toString('utf8');
            resolve({ status: res.statusCode, headers: res.headers, body: buf, text, json: tryJson(text), httpVersion: res.httpVersion });
          });
          res.on('error', (err) => {
            clearTimeout(timer);
            reject(err);
          });
        },
      );
      const timer = setTimeout(() => req.destroy(new Error(`${method} ${path} timed out after ${timeoutMs} ms`)), timeoutMs);
      req.on('error', (err) => {
        clearTimeout(timer);
        reject(err);
      });
      if (payload !== undefined) req.write(payload);
      req.end();
    });
  }

  get(path, opts) {
    return this.request('GET', path, opts);
  }

  post(path, body, opts = {}) {
    return this.request('POST', path, { ...opts, body });
  }

  /** Starts a chunked upload to `/v1/transcriptions/stream`. */
  openStream({ headers = {}, auth = 'bearer', path = '/v1/transcriptions/stream', timeoutMs } = {}) {
    this.refuseIfAborted();
    return new StreamUpload(this, { headers, auth, path, timeoutMs });
  }

  /** Opens `/v1/events`. Resolves once the response head arrives. */
  subscribe({ auth = 'bearer', headers = {}, path = '/v1/events' } = {}) {
    if (this.aborted) return Promise.reject(this.aborted);
    const resolved = this.resolve(path, auth, headers);
    return new Promise((resolve, reject) => {
      const req = http.request(
        { host: this.hostname, port: this.port, method: 'GET', path: resolved.path, headers: resolved.headers, agent: false },
        (res) => resolve(new Subscriber(req, res)),
      );
      const timer = setTimeout(() => req.destroy(new Error('GET /v1/events: no response head within 10 s')), 10000);
      req.on('response', () => clearTimeout(timer));
      req.on('error', (err) => {
        clearTimeout(timer);
        reject(err);
      });
      req.end();
    });
  }

  /** A raw TCP connection with a buffered reader. */
  connectRaw() {
    if (this.aborted) return Promise.reject(this.aborted);
    return new Promise((resolve, reject) => {
      const socket = net.connect({ host: this.hostname, port: this.port }, () => resolve(new RawConnection(socket)));
      socket.once('error', reject);
    });
  }
}

/**
 * A `/v1/transcriptions/stream` upload with the body written over time.
 * `done` always resolves (never rejects): `{status, headers, text, json,
 * endedAt, doneAt, error}` where `status` is null when no response arrived.
 * An upload with no write and no complete answer for `timeoutMs` is destroyed
 * and resolves with `status: null` and the timeout as `error`.
 */
export class StreamUpload {
  constructor(host, { headers, auth, path, timeoutMs = 120000 }) {
    const resolved = host.resolve(path, auth, { 'Content-Type': STREAM_CONTENT_TYPE, ...headers });
    this.path = path;
    this.timeoutMs = timeoutMs;
    this.timer = null;
    this.responded = false;
    this.closed = false;
    this.endedAt = null;
    this.bytesWritten = 0;
    this.req = http.request({
      host: host.hostname,
      port: host.port,
      method: 'POST',
      path: resolved.path,
      headers: { ...resolved.headers, 'Transfer-Encoding': 'chunked' },
      agent: false,
    });
    openUploads.add(this);
    this.done = new Promise((resolve) => {
      this.settle = (result) => {
        clearTimeout(this.timer);
        openUploads.delete(this);
        resolve(result);
      };
      this.req.on('response', (res) => {
        this.responded = true;
        const chunks = [];
        res.on('data', (c) => chunks.push(c));
        res.on('end', () => {
          const text = Buffer.concat(chunks).toString('utf8');
          this.closed = true;
          this.settle({ status: res.statusCode, headers: res.headers, text, json: tryJson(text), endedAt: this.endedAt, doneAt: performance.now(), error: null });
        });
        res.on('error', (err) => this.settle({ status: res.statusCode, headers: res.headers, text: '', json: undefined, endedAt: this.endedAt, doneAt: performance.now(), error: err }));
      });
      this.req.on('error', (err) => {
        this.closed = true;
        if (!this.responded) this.settle({ status: null, headers: {}, text: '', json: undefined, endedAt: this.endedAt, doneAt: performance.now(), error: err });
      });
    });
    this.arm();
    this.req.flushHeaders();
  }

  /** (Re)starts the inactivity timeout. */
  arm() {
    clearTimeout(this.timer);
    this.timer = setTimeout(() => {
      const err = new Error(`POST ${this.path}: no answer within ${this.timeoutMs} ms of the last write`);
      this.closed = true;
      this.req.destroy(err);
      this.settle({ status: null, headers: {}, text: '', json: undefined, endedAt: this.endedAt, doneAt: performance.now(), error: err });
    }, this.timeoutMs);
    this.timer.unref();
  }

  /** Writes one chunk unless the host already answered or the socket closed. */
  write(buf) {
    if (this.responded || this.closed || this.req.destroyed || this.endedAt !== null) return false;
    if (buf.length === 0) return true; // a zero-length chunk would end the body
    this.bytesWritten += buf.length;
    this.req.write(buf);
    this.arm();
    return true;
  }

  /** Writes chunks with `intervalMs` between them. Stops early if the host answers. */
  async writeAll(chunks, intervalMs = 0) {
    let i = 0;
    for (const chunk of chunks) {
      if (!this.write(chunk)) return i;
      i += 1;
      if (intervalMs > 0) await sleep(intervalMs);
      else if (i % 16 === 0) await sleep(0);
    }
    return i;
  }

  /** Ends the body (the terminating zero chunk) and records when. */
  end() {
    if (this.endedAt === null) {
      this.endedAt = performance.now();
      if (!this.closed) this.arm();
    }
    if (!this.req.destroyed) this.req.end();
    return this.done;
  }

  /** Kills the connection mid-upload. */
  abort() {
    this.closed = true;
    this.req.destroy();
    if (!this.responded) this.settle({ status: null, headers: {}, text: '', json: undefined, endedAt: this.endedAt, doneAt: performance.now(), error: new Error('upload aborted') });
  }
}

/**
 * A connected `/v1/events` response. Keeps every frame (raw text and parsed)
 * so checks can assert on order and format.
 */
export class Subscriber {
  constructor(req, res) {
    this.req = req;
    this.res = res;
    this.status = res.statusCode;
    this.headers = res.headers;
    this.httpVersion = res.httpVersion;
    this.frames = []; // {raw, kind: 'event'|'comment'|'invalid', type, data, at}
    this.events = []; // the 'event' frames
    this.buffer = '';
    this.body = '';
    this.ended = false;
    this.waiters = new Set();
    this.isStream = this.status === 200;
    if (this.isStream) openSubscribers.add(this);
    res.setEncoding('utf8');
    res.on('data', (text) => {
      if (!this.isStream) {
        this.body += text;
        return;
      }
      this.buffer += text;
      let cut;
      while ((cut = this.buffer.indexOf('\n\n')) !== -1) {
        const raw = this.buffer.slice(0, cut + 2);
        this.buffer = this.buffer.slice(cut + 2);
        this.addFrame(raw);
      }
    });
    const finish = () => {
      this.ended = true;
      openSubscribers.delete(this);
      this.notify();
    };
    res.on('end', finish);
    res.on('close', finish);
    res.on('error', finish);
    req.on('error', finish);
    this.json = undefined;
    if (!this.isStream) {
      this.bodyDone = new Promise((resolve) => res.on('end', () => {
        this.json = tryJson(this.body);
        resolve();
      }));
    }
  }

  addFrame(raw) {
    const at = performance.now();
    const m = /^event: ([^\n]*)\ndata: ([^\n]*)\n\n$/.exec(raw);
    let frame;
    if (m) {
      frame = { raw, kind: 'event', type: m[1], data: tryJson(m[2]), dataText: m[2], at, index: this.events.length };
      this.events.push(frame);
    } else if (raw.startsWith(':')) {
      frame = { raw, kind: 'comment', at };
    } else {
      frame = { raw, kind: 'invalid', at };
    }
    this.frames.push(frame);
    this.notify();
  }

  notify() {
    for (const waiter of this.waiters) waiter();
  }

  /** First event at or after `from` matching `pred`, or undefined. */
  find(pred, from = 0) {
    for (let i = from; i < this.events.length; i += 1) {
      if (pred(this.events[i])) return this.events[i];
    }
    return undefined;
  }

  /** Waits for an event matching `pred` (searching from index `from`). */
  waitFor(pred, timeoutMs, from = 0, label = 'event') {
    const found = this.find(pred, from);
    if (found) return Promise.resolve(found);
    return new Promise((resolve, reject) => {
      const waiter = () => {
        const hit = this.find(pred, from);
        if (hit) {
          cleanup();
          resolve(hit);
        } else if (this.ended) {
          cleanup();
          reject(new Error(`event stream ended while waiting for ${label}`));
        }
      };
      const timer = setTimeout(() => {
        cleanup();
        reject(new Error(`timed out after ${timeoutMs} ms waiting for ${label}`));
      }, timeoutMs);
      const cleanup = () => {
        clearTimeout(timer);
        this.waiters.delete(waiter);
      };
      this.waiters.add(waiter);
    });
  }

  /** Waits for any frame (event or comment) matching `pred`. */
  waitForFrame(pred, timeoutMs, label = 'frame') {
    const hit = this.frames.find(pred);
    if (hit) return Promise.resolve(hit);
    return new Promise((resolve, reject) => {
      const waiter = () => {
        const found = this.frames.find(pred);
        if (found) {
          cleanup();
          resolve(found);
        } else if (this.ended) {
          cleanup();
          reject(new Error(`event stream ended while waiting for ${label}`));
        }
      };
      const timer = setTimeout(() => {
        cleanup();
        reject(new Error(`timed out after ${timeoutMs} ms waiting for ${label}`));
      }, timeoutMs);
      const cleanup = () => {
        clearTimeout(timer);
        this.waiters.delete(waiter);
      };
      this.waiters.add(waiter);
    });
  }

  snapshot(timeoutMs = 10000) {
    return this.waitFor((e) => e.type === 'snapshot', timeoutMs, 0, 'snapshot frame');
  }

  close() {
    openSubscribers.delete(this);
    this.ended = true;
    this.req.destroy();
    this.res.destroy();
    this.notify();
  }
}

/** A plain TCP connection with helpers to read HTTP responses off it. */
export class RawConnection {
  constructor(socket) {
    this.socket = socket;
    this.buffer = Buffer.alloc(0);
    this.ended = false;
    this.waiters = new Set();
    openRaw.add(this);
    socket.on('data', (chunk) => {
      this.buffer = Buffer.concat([this.buffer, chunk]);
      this.notify();
    });
    const finish = () => {
      this.ended = true;
      openRaw.delete(this);
      this.notify();
    };
    socket.on('end', finish);
    socket.on('close', finish);
    socket.on('error', finish);
  }

  notify() {
    for (const waiter of this.waiters) waiter();
  }

  write(data) {
    this.socket.write(data);
  }

  /** Waits until `parse(buffer, ended)` returns a value other than undefined. */
  waitFor(parse, timeoutMs, label = 'data') {
    const now = parse(this.buffer, this.ended);
    if (now !== undefined) return Promise.resolve(now);
    return new Promise((resolve, reject) => {
      const waiter = () => {
        let value;
        try {
          value = parse(this.buffer, this.ended);
        } catch (err) {
          cleanup();
          reject(err);
          return;
        }
        if (value !== undefined) {
          cleanup();
          resolve(value);
        } else if (this.ended) {
          cleanup();
          reject(new Error(`connection closed while waiting for ${label}`));
        }
      };
      const timer = setTimeout(() => {
        cleanup();
        reject(new Error(`timed out after ${timeoutMs} ms waiting for ${label}`));
      }, timeoutMs);
      const cleanup = () => {
        clearTimeout(timer);
        this.waiters.delete(waiter);
      };
      this.waiters.add(waiter);
    });
  }

  /** Consumes `n` bytes from the front of the buffer. */
  consume(n) {
    this.buffer = this.buffer.subarray(n);
  }

  /**
   * Reads one complete HTTP response (Content-Length, chunked or
   * close-delimited body) and consumes it. Interim 1xx responses are
   * returned as their own responses.
   */
  async readResponse(timeoutMs = 10000) {
    const result = await this.waitFor((buf, ended) => parseResponse(buf, ended), timeoutMs, 'HTTP response');
    this.consume(result.length);
    return result.response;
  }

  close() {
    openRaw.delete(this);
    this.socket.destroy();
  }
}

/** Splits a response head off `buf`. Returns undefined until it is complete. */
export function parseHead(buf) {
  const end = buf.indexOf('\r\n\r\n');
  if (end === -1) return undefined;
  const lines = buf.subarray(0, end).toString('latin1').split('\r\n');
  const statusLine = lines.shift();
  const m = /^HTTP\/(\d\.\d) (\d{3})/.exec(statusLine);
  if (!m) throw new Error(`malformed status line: ${JSON.stringify(statusLine)}`);
  const headers = {};
  for (const line of lines) {
    const i = line.indexOf(':');
    if (i === -1) continue;
    const name = line.slice(0, i).trim().toLowerCase();
    const value = line.slice(i + 1).trim();
    headers[name] = name in headers ? `${headers[name]}, ${value}` : value;
  }
  return { statusLine, version: m[1], status: Number(m[2]), headers, headLength: end + 4 };
}

function parseResponse(buf, ended) {
  const head = parseHead(buf);
  if (!head) return undefined;
  const rest = buf.subarray(head.headLength);
  const make = (body, used) => {
    const text = body.toString('utf8');
    return { length: head.headLength + used, response: { ...head, body, text, json: tryJson(text) } };
  };
  if (head.status >= 100 && head.status < 200) return make(Buffer.alloc(0), 0);
  if ((head.headers['transfer-encoding'] || '').toLowerCase().includes('chunked')) {
    const parts = [];
    let pos = 0;
    for (;;) {
      const lineEnd = rest.indexOf('\r\n', pos);
      if (lineEnd === -1) return undefined;
      const size = parseInt(rest.subarray(pos, lineEnd).toString('latin1'), 16);
      if (Number.isNaN(size)) throw new Error('malformed chunk size');
      if (size === 0) {
        const trailerEnd = rest.indexOf('\r\n', lineEnd + 2);
        if (trailerEnd === -1) return undefined;
        return make(Buffer.concat(parts), trailerEnd + 2);
      }
      if (rest.length < lineEnd + 2 + size + 2) return undefined;
      parts.push(rest.subarray(lineEnd + 2, lineEnd + 2 + size));
      pos = lineEnd + 2 + size + 2;
    }
  }
  if ('content-length' in head.headers) {
    const n = Number(head.headers['content-length']);
    if (rest.length < n) return undefined;
    return make(rest.subarray(0, n), n);
  }
  if (ended) return make(rest, rest.length);
  return undefined;
}
