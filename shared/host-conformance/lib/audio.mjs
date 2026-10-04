// Audio fixtures: WAV encode/decode, PCM stream frames, synthetic tones and
// speech rendered with macOS `say` when it is available.

import { spawnSync } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';

export const SPEECH_PHRASE = 'The quick brown fox jumps over the lazy dog.';
export const SPEECH_WORDS = ['quick', 'brown', 'fox', 'jumps', 'lazy', 'dog'];

/** `u32 LE sample_rate | u32 LE sample_count | sample_count × i16 LE`. */
export function encodeFrame(sampleRate, samples, countOverride) {
  const count = countOverride ?? samples.length;
  const buf = Buffer.alloc(8 + samples.length * 2);
  buf.writeUInt32LE(sampleRate >>> 0, 0);
  buf.writeUInt32LE(count >>> 0, 4);
  for (let i = 0; i < samples.length; i += 1) buf.writeInt16LE(samples[i], 8 + i * 2);
  return buf;
}

/** Splits PCM into frames of `frameMs`, optionally with zero-count frames between them. */
export function framesFor(samples, sampleRate, { frameMs = 100, zeroFrames = false } = {}) {
  const per = Math.max(1, Math.round((sampleRate * frameMs) / 1000));
  const out = [];
  for (let i = 0; i < samples.length; i += per) {
    out.push(encodeFrame(sampleRate, samples.subarray(i, Math.min(samples.length, i + per))));
    if (zeroFrames) out.push(encodeFrame(sampleRate, new Int16Array(0)));
  }
  return out;
}

/** Any PCM WAV container. `data` is the raw sample bytes. */
export function buildWav({ sampleRate, channels = 1, bitsPerSample = 16, format = 1, data }) {
  const blockAlign = channels * (bitsPerSample / 8);
  const header = Buffer.alloc(44);
  header.write('RIFF', 0, 'ascii');
  header.writeUInt32LE(36 + data.length, 4);
  header.write('WAVE', 8, 'ascii');
  header.write('fmt ', 12, 'ascii');
  header.writeUInt32LE(16, 16);
  header.writeUInt16LE(format, 20);
  header.writeUInt16LE(channels, 22);
  header.writeUInt32LE(sampleRate, 24);
  header.writeUInt32LE(sampleRate * blockAlign, 28);
  header.writeUInt16LE(blockAlign, 32);
  header.writeUInt16LE(bitsPerSample, 34);
  header.write('data', 36, 'ascii');
  header.writeUInt32LE(data.length, 40);
  return Buffer.concat([header, data]);
}

export function pcm16Bytes(samples) {
  const buf = Buffer.alloc(samples.length * 2);
  for (let i = 0; i < samples.length; i += 1) buf.writeInt16LE(samples[i], i * 2);
  return buf;
}

/** Mono (or interleaved multi-channel) 16-bit PCM WAV. */
export function wav16(samples, sampleRate, channels = 1) {
  return buildWav({ sampleRate, channels, bitsPerSample: 16, data: pcm16Bytes(samples) });
}

/** Unsigned 8-bit PCM WAV of the same audio. */
export function wav8(samples, sampleRate) {
  const data = Buffer.alloc(samples.length);
  for (let i = 0; i < samples.length; i += 1) data[i] = Math.max(0, Math.min(255, Math.round(samples[i] / 256) + 128));
  return buildWav({ sampleRate, bitsPerSample: 8, data });
}

/** 32-bit IEEE float WAV (format tag 3) of the same audio. */
export function wavFloat32(samples, sampleRate) {
  const data = Buffer.alloc(samples.length * 4);
  for (let i = 0; i < samples.length; i += 1) data.writeFloatLE(samples[i] / 32768, i * 4);
  return buildWav({ sampleRate, bitsPerSample: 32, format: 3, data });
}

/** Reads a 16-bit PCM WAV (any chunk layout) into mono Int16Array. */
export function parseWav16(buf) {
  if (buf.toString('ascii', 0, 4) !== 'RIFF' || buf.toString('ascii', 8, 12) !== 'WAVE') throw new Error('not a WAV file');
  let pos = 12;
  let fmt;
  let data;
  while (pos + 8 <= buf.length) {
    const id = buf.toString('ascii', pos, pos + 4);
    const size = buf.readUInt32LE(pos + 4);
    const body = buf.subarray(pos + 8, pos + 8 + size);
    if (id === 'fmt ') {
      fmt = { format: body.readUInt16LE(0), channels: body.readUInt16LE(2), sampleRate: body.readUInt32LE(4), bits: body.readUInt16LE(14) };
    } else if (id === 'data') {
      data = body;
    }
    pos += 8 + size + (size % 2);
  }
  if (!fmt || !data) throw new Error('WAV is missing fmt or data');
  if (fmt.bits !== 16) throw new Error(`expected 16-bit WAV, got ${fmt.bits}-bit`);
  const frames = Math.floor(data.length / (2 * fmt.channels));
  const out = new Int16Array(frames);
  for (let i = 0; i < frames; i += 1) {
    let sum = 0;
    for (let c = 0; c < fmt.channels; c += 1) sum += data.readInt16LE((i * fmt.channels + c) * 2);
    out[i] = Math.trunc(sum / fmt.channels);
  }
  return { samples: out, sampleRate: fmt.sampleRate };
}

export function silence(seconds, sampleRate = 16000) {
  return new Int16Array(Math.round(seconds * sampleRate));
}

/** A 440 Hz tone with a little vibrato and fade in/out. */
export function tone(seconds, sampleRate = 16000, freq = 440) {
  const n = Math.round(seconds * sampleRate);
  const out = new Int16Array(n);
  const fade = Math.min(n / 2, sampleRate * 0.02);
  for (let i = 0; i < n; i += 1) {
    const t = i / sampleRate;
    const env = Math.min(1, i / fade, (n - i) / fade);
    out[i] = Math.round(8000 * env * Math.sin(2 * Math.PI * (freq + 20 * Math.sin(2 * Math.PI * 3 * t)) * t));
  }
  return out;
}

export function concat(...parts) {
  const out = new Int16Array(parts.reduce((n, p) => n + p.length, 0));
  let pos = 0;
  for (const p of parts) {
    out.set(p, pos);
    pos += p.length;
  }
  return out;
}

/** Linear-interpolation upsample by an integer factor. */
export function upsample(samples, factor) {
  const out = new Int16Array(samples.length * factor);
  for (let i = 0; i < samples.length; i += 1) {
    const a = samples[i];
    const b = i + 1 < samples.length ? samples[i + 1] : a;
    for (let k = 0; k < factor; k += 1) out[i * factor + k] = Math.round(a + ((b - a) * k) / factor);
  }
  return out;
}

/** Interleaves a mono signal into two identical channels. */
export function toStereo(samples) {
  const out = new Int16Array(samples.length * 2);
  for (let i = 0; i < samples.length; i += 1) {
    out[2 * i] = samples[i];
    out[2 * i + 1] = samples[i];
  }
  return out;
}

function haveTool(name) {
  return spawnSync('/usr/bin/which', [name], { encoding: 'utf8' }).status === 0;
}

/**
 * 16 kHz mono speech of SPEECH_PHRASE padded with silence. Falls back to a
 * synthetic tone (`real: false`) when `say`/`afconvert` are unavailable.
 */
export function makeSpeech() {
  const pad = silence(0.3);
  if (process.platform === 'darwin' && haveTool('say') && haveTool('afconvert')) {
    const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'host-conformance-'));
    try {
      const aiff = path.join(dir, 'speech.aiff');
      const wav = path.join(dir, 'speech.wav');
      const said = spawnSync('say', ['-o', aiff, SPEECH_PHRASE], { encoding: 'utf8' });
      if (said.status === 0) {
        const conv = spawnSync('afconvert', ['-f', 'WAVE', '-d', 'LEI16@16000', '-c', '1', aiff, wav], { encoding: 'utf8' });
        if (conv.status === 0) {
          const parsed = parseWav16(fs.readFileSync(wav));
          if (parsed.sampleRate === 16000 && parsed.samples.length > 8000) {
            return { samples: concat(pad, parsed.samples, pad), sampleRate: 16000, real: true, phrase: SPEECH_PHRASE, words: SPEECH_WORDS };
          }
        }
      }
    } finally {
      fs.rmSync(dir, { recursive: true, force: true });
    }
  }
  return { samples: concat(pad, tone(2.4), pad), sampleRate: 16000, real: false, phrase: null, words: [] };
}

/** Lowercased words with punctuation stripped. */
export function words(text) {
  return String(text)
    .toLowerCase()
    .replace(/[^\p{L}\p{N}\s']/gu, ' ')
    .split(/\s+/)
    .filter(Boolean);
}

/** Fraction of `expected` words present in `text`. */
export function wordHitRate(text, expected) {
  if (!expected.length) return 1;
  const got = new Set(words(text));
  return expected.filter((w) => got.has(w)).length / expected.length;
}
