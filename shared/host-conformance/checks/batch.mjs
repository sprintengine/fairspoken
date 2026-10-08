// POST /v1/transcriptions (complete WAV uploads).

import { check, describe, eq, expectError } from '../lib/assert.mjs';
import { BACKENDS } from '../lib/schema.mjs';
import { concat, silence, tone, toStereo, upsample, wav16, wav8, wavFloat32, wordHitRate } from '../lib/audio.mjs';

const RESPONSE_KEYS = ['backend', 'durationSeconds', 'model', 'serverVersion', 'text'];

/** Checks `{text, durationSeconds, backend, model, serverVersion}` and the transcript. */
export async function checkTranscription(ctx, res, { seconds, label, speech }) {
  check(res.status === 200, `${label}: expected 200, got ${describe(res)}`);
  const body = res.json;
  check(body && typeof body === 'object', `${label}: body is not JSON`);
  eq(Object.keys(body).sort(), RESPONSE_KEYS, `${label}: response fields`);
  check(typeof body.text === 'string', `${label}: text should be a string`);
  check(typeof body.durationSeconds === 'number', `${label}: durationSeconds should be a number`);
  check(Math.abs(body.durationSeconds - seconds) <= Math.max(0.05, seconds * 0.01), `${label}: durationSeconds ${body.durationSeconds}, expected ≈ ${seconds.toFixed(3)}`);
  check(BACKENDS.includes(body.backend), `${label}: backend ${JSON.stringify(body.backend)} should be one of ${BACKENDS.join('/')}`);
  check(typeof body.serverVersion === 'string' && body.serverVersion.length > 0, `${label}: serverVersion should be a non-empty string`);
  const served = ctx.servedModels(await ctx.stats());
  check(served.includes(body.model), `${label}: model ${JSON.stringify(body.model)} is not a served model (${served.join(', ')})`);
  if (speech && speech.real && ctx.transcriptCheck) {
    const rate = wordHitRate(body.text, speech.words);
    check(rate >= 0.6, `${label}: transcript ${JSON.stringify(body.text)} has only ${Math.round(rate * 100)}% of the expected words (${speech.words.join(' ')})`);
  }
  return body;
}

export default [
  {
    name: 'batch.speech',
    description: 'a 16 kHz mono 16-bit WAV transcribes: response shape, durationSeconds, served model, transcript words',
    async run(ctx) {
      await ctx.requireModel();
      const speech = ctx.speech;
      const seconds = speech.samples.length / speech.sampleRate;
      const res = await ctx.batch(wav16(speech.samples, speech.sampleRate), { 'x-fairspoken-backend': 'parakeet', 'x-fairspoken-language': 'en' });
      const body = await checkTranscription(ctx, res, { seconds, label: 'POST /v1/transcriptions', speech });
      ctx.note(speech.real ? `transcript: ${JSON.stringify(body.text)}` : 'no `say`/`afconvert`: used a tone and checked the response shape only');
    },
  },
  {
    name: 'batch.48k-stereo',
    description: 'a 48 kHz stereo 16-bit WAV is accepted (downmixed and resampled) and transcribes',
    async run(ctx) {
      await ctx.requireModel();
      const speech = ctx.speech;
      const hi = upsample(speech.samples, 3);
      const res = await ctx.batch(wav16(toStereo(hi), 48000, 2));
      await checkTranscription(ctx, res, { seconds: hi.length / 48000, label: 'POST /v1/transcriptions (48 kHz stereo)', speech });
    },
  },
  {
    name: 'batch.not-16-bit',
    description: '8-bit and 32-bit float WAVs answer 400',
    async run(ctx) {
      const audio = tone(1);
      expectError(await ctx.batch(wav8(audio, 16000)), 400, 'POST /v1/transcriptions with an 8-bit WAV');
      expectError(await ctx.batch(wavFloat32(audio, 16000)), 400, 'POST /v1/transcriptions with a float32 WAV');
    },
  },
  {
    name: 'batch.garbage',
    description: 'a body that is not a WAV, or an empty body, answers 400',
    async run(ctx) {
      expectError(await ctx.batch(Buffer.from('definitely not a wav file, just some bytes '.repeat(40))), 400, 'POST /v1/transcriptions with a non-WAV body');
      expectError(await ctx.batch(Buffer.alloc(0)), 400, 'POST /v1/transcriptions with an empty body');
    },
  },
  {
    name: 'batch.backend-header',
    description: 'x-fairspoken-backend other than parakeet/whisper answers 400',
    async run(ctx) {
      expectError(await ctx.batch(wav16(tone(1), 16000), { 'x-fairspoken-backend': 'nonsense' }), 400, 'POST /v1/transcriptions with x-fairspoken-backend: nonsense');
    },
  },
  {
    name: 'batch.legacy-header-prefix',
    description: 'the pre-rename x-multivoice-* header names are still read (x-multivoice-backend: nonsense answers 400)',
    async run(ctx) {
      expectError(await ctx.batch(wav16(tone(1), 16000), { 'x-multivoice-backend': 'nonsense' }), 400, 'POST /v1/transcriptions with x-multivoice-backend: nonsense');
    },
  },
  {
    name: 'batch.model-header-ignored',
    description: 'x-fairspoken-model is accepted and ignored: the response model is the served one',
    async run(ctx) {
      await ctx.requireModel();
      const speech = ctx.speech;
      const res = await ctx.batch(wav16(speech.samples, speech.sampleRate), { 'x-fairspoken-model': 'no-such-model' });
      await checkTranscription(ctx, res, { seconds: speech.samples.length / speech.sampleRate, label: 'POST /v1/transcriptions with x-fairspoken-model: no-such-model', speech });
    },
  },
  {
    name: 'batch.vocabulary-hints',
    description: 'x-fairspoken-vocabulary-hints (percent-encoded JSON array) and x-fairspoken-backend: whisper are accepted',
    async run(ctx) {
      await ctx.requireModel();
      const speech = ctx.speech;
      const hints = encodeURIComponent(JSON.stringify(['Fairspoken', 'Parakeet', 'naïve café']));
      const res = await ctx.batch(wav16(speech.samples, speech.sampleRate), { 'x-fairspoken-vocabulary-hints': hints, 'x-fairspoken-backend': 'whisper', 'x-fairspoken-language': 'en' });
      await checkTranscription(ctx, res, { seconds: speech.samples.length / speech.sampleRate, label: 'POST /v1/transcriptions with vocabulary hints', speech });
    },
  },
  {
    name: 'batch.over-duration',
    description: 'a WAV longer than maxRecordingSeconds answers 413',
    async run(ctx) {
      await ctx.setConfig({ maxRecordingSeconds: 10 });
      try {
        const audio = concat(tone(1), silence(10));
        expectError(await ctx.batch(wav16(audio, 16000)), 413, 'POST /v1/transcriptions with 11 s of audio (maxRecordingSeconds 10)');
      } finally {
        await ctx.cleanup('restore maxRecordingSeconds', () => ctx.setConfig({ maxRecordingSeconds: ctx.originalConfig.maxRecordingSeconds }));
      }
      ctx.assertCleanedUp();
    },
  },
];
