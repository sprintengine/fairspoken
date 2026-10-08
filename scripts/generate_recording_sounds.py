#!/usr/bin/env python3
"""Generate the recording start/stop interaction sounds.

Produces src/assets/sounds/recording-start.wav and recording-stop.wav:
a pair of quiet, muted ticks -- a soft fingertip-on-wood tap rather than
a musical chime. Start sits slightly higher than stop so the pair still
reads as on/off, but neither sounds like a note.

Requires: numpy. Run from the repo root:

    python3 scripts/generate_recording_sounds.py
"""

from __future__ import annotations

import struct
import wave
import zlib
from pathlib import Path

import numpy as np

SAMPLE_RATE = 48_000
START_BODY_HZ = 440.0
STOP_BODY_HZ = 310.0

CLICK_SECONDS = 0.16
PEAK_LEVEL = 0.10  # ~ -20 dBFS; a tick should be felt more than heard


def lowpass(signal: np.ndarray, passes: int) -> np.ndarray:
    """Cheap repeated [1 2 1]/4 smoothing; each pass darkens the noise."""
    for _ in range(passes):
        signal = np.convolve(signal, [0.25, 0.5, 0.25], mode="same")
    return signal


def synth_tick(body_freq: float, seed: str) -> np.ndarray:
    """Render one stereo tick (2, n): a muted tap with a low damped body."""
    n = int(CLICK_SECONDS * SAMPLE_RATE)
    t = np.arange(n) / SAMPLE_RATE

    # Contact: a few milliseconds of heavily low-passed noise. This is the
    # "click" itself -- dull and woody, not sharp.
    # Seed from a CRC of a stable string rather than hash(): Python salts
    # str hashes per process (PYTHONHASHSEED), so hash() would give a
    # different click on every run and make the output non-reproducible.
    rng = np.random.default_rng(zlib.crc32(f"{round(body_freq)}:{seed}".encode()))
    contact_len = int(0.007 * SAMPLE_RATE)
    contact = lowpass(rng.standard_normal(contact_len), passes=8)
    contact *= np.hanning(contact_len)
    contact /= np.max(np.abs(contact))

    # Body: a quickly damped low resonance that gives the tap a muted
    # "thock" instead of a bare noise burst. The half-frequency layer adds
    # a touch of warmth; both die out in well under 100 ms.
    body = np.sin(2.0 * np.pi * body_freq * t) * np.exp(-t / 0.030)
    body += 0.45 * np.sin(2.0 * np.pi * body_freq * 0.5 * t) * np.exp(-t / 0.045)
    attack = int(0.002 * SAMPLE_RATE)
    body[:attack] *= np.linspace(0.0, 1.0, attack)

    mono = 0.50 * body
    mono[:contact_len] += 0.60 * contact

    fade = int(0.02 * SAMPLE_RATE)
    mono[-fade:] *= np.linspace(1.0, 0.0, fade)

    out = np.vstack((mono, mono))
    return out * (PEAK_LEVEL / np.max(np.abs(out)))


def write_wav_16bit(path: Path, audio: np.ndarray) -> None:
    rng = np.random.default_rng(0xC0FFEE)
    dither = (rng.random(audio.shape) - rng.random(audio.shape)) / 32768.0  # TPDF
    samples = np.clip(audio + dither, -1.0, 1.0)
    pcm = np.round(samples * 32767.0).astype("<i2").T.reshape(-1)

    with wave.open(str(path), "wb") as wav:
        wav.setnchannels(2)
        wav.setsampwidth(2)
        wav.setframerate(SAMPLE_RATE)
        wav.writeframes(struct.pack(f"<{pcm.size}h", *pcm))


def main() -> None:
    out_dir = Path(__file__).resolve().parent.parent / "src" / "assets" / "sounds"
    out_dir.mkdir(parents=True, exist_ok=True)

    start = synth_tick(START_BODY_HZ, "start")
    stop = synth_tick(STOP_BODY_HZ, "stop")

    write_wav_16bit(out_dir / "recording-start.wav", start)
    write_wav_16bit(out_dir / "recording-stop.wav", stop)

    for name, audio in (("recording-start.wav", start), ("recording-stop.wav", stop)):
        duration = audio.shape[1] / SAMPLE_RATE
        peak_db = 20 * np.log10(np.max(np.abs(audio)))
        print(f"{name}: {duration:.2f}s, peak {peak_db:.1f} dBFS")


if __name__ == "__main__":
    main()
