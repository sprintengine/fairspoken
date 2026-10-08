/* ── Voice-reactive level meter ─────────────────────────────────
   The pill's five wave bars are driven by real capture levels streamed from the
   backend as `audio-level` events; they sit flat through silence instead of
   pulsing on a loop. */

const waveBars = Array.from(document.querySelectorAll<HTMLElement>(".wave span"));
// Gate matches the backend's per-window speech RMS threshold, so the bars
// consider "voice" exactly what the transcriber's gap detection does.
const METER_GATE_RMS = 0.004;
// Conversational speech sits well below the old 0.055 ceiling, so the bars
// spent their time near the floor and barely moved. This is a normal speaking
// voice at full scale; louder speech simply clamps.
const METER_FULL_RMS = 0.026;
const METER_FLOOR_SCALE = 0.12;
const METER_BAR_WEIGHTS = [0.62, 0.95, 1, 0.78, 0.88];
const reducedMotionQuery = window.matchMedia("(prefers-reduced-motion: reduce)");

let meterTarget = 0;
let meterDisplay = 0;
let meterRafId: number | null = null;
// The meter keeps animating while this holds (the pill is recording).
let keepRunning: () => boolean = () => false;

function meterLevelFromRms(rms: number): number {
  const normalized = Math.min(1, Math.max(0, (rms - METER_GATE_RMS) / (METER_FULL_RMS - METER_GATE_RMS)));
  // Square root for a perceptual response: quiet speech still visibly moves.
  return Math.sqrt(normalized);
}

export function startMeter(isRecording: () => boolean): void {
  keepRunning = isRecording;
  meterTarget = 0;
  meterDisplay = 0;
  if (meterRafId === null) meterFrame();
}

export function stopMeter(): void {
  meterTarget = 0;
  if (meterRafId !== null) {
    cancelAnimationFrame(meterRafId);
    meterRafId = null;
  }
  resetMeterBars();
}

function meterFrame(): void {
  meterRafId = requestAnimationFrame((time) => {
    const attack = 0.45;
    const decay = reducedMotionQuery.matches ? 0.08 : 0.18;
    meterDisplay += (meterTarget - meterDisplay) * (meterTarget > meterDisplay ? attack : decay);
    // A slight per-bar shimmer while there is signal keeps the meter reading
    // as a waveform; with reduced motion the bars track level only.
    const shimmer = !reducedMotionQuery.matches && meterDisplay > 0.03;
    waveBars.forEach((bar, index) => {
      const wobble = shimmer ? 0.89 + 0.11 * Math.sin(time / 90 + index * 1.7) : 1;
      const scale = METER_FLOOR_SCALE + meterDisplay * METER_BAR_WEIGHTS[index] * wobble * (1 - METER_FLOOR_SCALE);
      bar.style.transform = `scaleY(${Math.min(1, Math.max(METER_FLOOR_SCALE, scale)).toFixed(3)})`;
    });
    if (keepRunning() || meterDisplay > 0.02) {
      meterFrame();
    } else {
      meterRafId = null;
      resetMeterBars();
    }
  });
}

export function resetMeterBars(): void {
  for (const bar of waveBars) {
    bar.style.transform = `scaleY(${METER_FLOOR_SCALE})`;
  }
}

/** Feed one `audio-level` reading; true when it carries voice. */
export function setMeterLevel(rms: number): boolean {
  meterTarget = meterLevelFromRms(rms);
  return meterTarget > 0;
}
