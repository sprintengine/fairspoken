#!/usr/bin/env python3
"""Generate the recording start/stop interaction sounds.

Produces src/assets/sounds/recording-start.wav and recording-stop.wav:
a pair of mirrored two-tone earcons (rising perfect fourth D5->G5 for
start, falling G5->D5 for stop) with a soft glass-mallet timbre.

Requires: numpy. Run from the repo root:

    python3 scripts/generate_recording_sounds.py
"""

from __future__ import annotations

import struct
import wave
from pathlib import Path

import numpy as np

SAMPLE_RATE = 48_000
D5 = 587.3295
G5 = 783.9909

# Partials as (frequency ratio, relative amplitude). The slightly
# inharmonic top partial gives the tone its glassy shimmer.
PARTIALS = [(1.0, 1.0), (2.0, 0.22), (3.0, 0.085), (4.16, 0.03)]
UNISON_DETUNE_CENTS = 2.5
ATTACK_SECONDS = 0.005
NOTE_GAP_SECONDS = 0.115
TAIL_SECONDS = 0.75
PEAK_LEVEL = 0.32  # ~ -10 dBFS; an interaction sound should sit under speech


def synth_note(freq: float, amp: float, fast_tau: float, slow_tau: float) -> np.ndarray:
    """Render one stereo note (2, n) with a percussive dual-exponential decay."""
    n = int(TAIL_SECONDS * SAMPLE_RATE)
    t = np.arange(n) / SAMPLE_RATE

    envelope = 0.62 * np.exp(-t / fast_tau) + 0.38 * np.exp(-t / slow_tau)
    attack = int(ATTACK_SECONDS * SAMPLE_RATE)
    envelope[:attack] *= 0.5 - 0.5 * np.cos(np.pi * np.arange(attack) / attack)

    note = np.zeros((2, n))
    # Two detuned unison layers spread slightly left/right for width.
    for detune, weight in ((-UNISON_DETUNE_CENTS, (0.58, 0.42)), (UNISON_DETUNE_CENTS, (0.42, 0.58))):
        layer = np.zeros(n)
        base = freq * 2.0 ** (detune / 1200.0)
        for ratio, partial_amp in PARTIALS:
            partial_env = envelope * np.exp(-t * (ratio - 1.0) / (2.2 * slow_tau))
            layer += partial_amp * partial_env * np.sin(2.0 * np.pi * base * ratio * t)
        note[0] += weight[0] * layer
        note[1] += weight[1] * layer

    # A whisper of low-passed noise at the onset reads as a soft mallet contact.
    rng = np.random.default_rng(hash((round(freq), "mallet")) & 0xFFFF_FFFF)
    contact_len = int(0.009 * SAMPLE_RATE)
    contact = rng.standard_normal(contact_len)
    for _ in range(4):
        contact = np.convolve(contact, [0.25, 0.5, 0.25], mode="same")
    contact *= np.hanning(contact_len) * 0.05
    note[:, :contact_len] += contact

    return amp * note


def pan(note: np.ndarray, position: float) -> np.ndarray:
    """Constant-power pan; position in [-1, 1]."""
    angle = (position + 1.0) * np.pi / 4.0
    return np.vstack((note[0] * np.cos(angle) * np.sqrt(2), note[1] * np.sin(angle) * np.sqrt(2)))


def render_chime(first: float, second: float, mirror_pan: bool) -> np.ndarray:
    gap = int(NOTE_GAP_SECONDS * SAMPLE_RATE)
    total = gap + int(TAIL_SECONDS * SAMPLE_RATE)
    out = np.zeros((2, total))

    side = -1.0 if not mirror_pan else 1.0
    lead = pan(synth_note(first, 0.78, fast_tau=0.075, slow_tau=0.21), 0.22 * side)
    rest = pan(synth_note(second, 1.0, fast_tau=0.095, slow_tau=0.34), -0.22 * side)

    out[:, : lead.shape[1]] += lead
    out[:, gap : gap + rest.shape[1]] += rest

    # Trim the tail once it falls below -72 dBFS, then fade the last 30 ms.
    threshold = 10 ** (-72 / 20) * np.max(np.abs(out))
    keep = np.max(np.abs(out), axis=0) > threshold
    end = int(np.max(np.nonzero(keep))) + 1
    out = out[:, :end]
    fade = min(int(0.03 * SAMPLE_RATE), end)
    out[:, -fade:] *= np.linspace(1.0, 0.0, fade)

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

    start = render_chime(D5, G5, mirror_pan=False)
    stop = render_chime(G5, D5, mirror_pan=True)

    write_wav_16bit(out_dir / "recording-start.wav", start)
    write_wav_16bit(out_dir / "recording-stop.wav", stop)

    for name, audio in (("recording-start.wav", start), ("recording-stop.wav", stop)):
        duration = audio.shape[1] / SAMPLE_RATE
        peak_db = 20 * np.log10(np.max(np.abs(audio)))
        print(f"{name}: {duration:.2f}s, peak {peak_db:.1f} dBFS")


if __name__ == "__main__":
    main()
