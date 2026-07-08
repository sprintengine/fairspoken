use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::SampleFormat;
use std::io::Cursor;
use std::sync::mpsc::{self, Sender, SyncSender};
use std::sync::OnceLock;
use std::thread;
use std::time::Duration;

// The same WAV assets the webview used to play; embedding them keeps playback
// available before any window exists and independent of the asset pipeline.
const RECORDING_START_WAV: &[u8] = include_bytes!("../../src/assets/sounds/recording-start.wav");
const RECORDING_STOP_WAV: &[u8] = include_bytes!("../../src/assets/sounds/recording-stop.wav");

#[derive(Clone, Copy, Debug)]
pub enum InteractionSound {
    RecordingStart,
    RecordingStop,
}

/// Queue an interaction sound for playback and return immediately.
///
/// Playback happens on a dedicated worker thread through the default output
/// device, so it does not depend on the webview's autoplay policy or document
/// visibility — the structural cause of the old intermittent clicks. Failures
/// are logged and never propagate: recording must not be gated on the click.
pub fn play_interaction_sound(sound: InteractionSound) {
    let sender = SOUND_TX.get_or_init(spawn_sound_worker);
    let _ = sender.send(sound);
}

static SOUND_TX: OnceLock<Sender<InteractionSound>> = OnceLock::new();

struct Clip {
    mono: Vec<f32>,
    sample_rate: u32,
}

fn spawn_sound_worker() -> Sender<InteractionSound> {
    let (tx, rx) = mpsc::channel::<InteractionSound>();
    let spawned = thread::Builder::new()
        .name("interaction-sounds".to_string())
        .spawn(move || {
            let start = decode_wav_clip(RECORDING_START_WAV);
            let stop = decode_wav_clip(RECORDING_STOP_WAV);
            for sound in rx {
                let clip = match sound {
                    InteractionSound::RecordingStart => &start,
                    InteractionSound::RecordingStop => &stop,
                };
                match clip {
                    Ok(clip) => {
                        if let Err(err) = play_clip(clip) {
                            eprintln!("Interaction sound playback failed: {err}");
                        }
                    }
                    Err(err) => eprintln!("Interaction sound is unavailable: {err}"),
                }
            }
        });
    if spawned.is_err() {
        eprintln!("Failed to start the interaction sound worker");
    }
    tx
}

fn decode_wav_clip(bytes: &'static [u8]) -> Result<Clip, String> {
    let mut reader = hound::WavReader::new(Cursor::new(bytes))
        .map_err(|err| format!("Failed to decode interaction sound: {err}"))?;
    let spec = reader.spec();
    let channels = usize::from(spec.channels.max(1));
    let interleaved: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => reader
            .samples::<f32>()
            .collect::<Result<_, _>>()
            .map_err(|err| format!("Failed to read interaction sound samples: {err}"))?,
        hound::SampleFormat::Int => {
            let scale = (1_i64 << u32::from(spec.bits_per_sample.max(1) - 1)) as f32;
            reader
                .samples::<i32>()
                .map(|sample| sample.map(|sample| sample as f32 / scale))
                .collect::<Result<_, _>>()
                .map_err(|err| format!("Failed to read interaction sound samples: {err}"))?
        }
    };

    let mono = interleaved
        .chunks(channels)
        .map(|frame| frame.iter().sum::<f32>() / frame.len() as f32)
        .collect();
    Ok(Clip {
        mono,
        sample_rate: spec.sample_rate,
    })
}

/// Opens the default output device, plays the clip once, and tears the stream
/// down again. Clips are ~160ms, so per-play stream setup is inaudible in
/// practice and avoids holding an output device open while the app idles.
fn play_clip(clip: &Clip) -> Result<(), String> {
    let host = cpal::default_host();
    let device = host
        .default_output_device()
        .ok_or_else(|| "No default output device is available".to_string())?;
    let supported = device
        .default_output_config()
        .map_err(|err| format!("Default output config is unavailable: {err}"))?;
    let output_rate = supported.sample_rate();
    let channels = usize::from(supported.channels()).max(1);
    let config: cpal::StreamConfig = supported.clone().into();
    let samples = resample_mono(&clip.mono, clip.sample_rate, output_rate);
    if samples.is_empty() {
        return Ok(());
    }

    let clip_ms = samples.len() as u64 * 1000 / u64::from(output_rate.max(1)) + 250;
    let (done_tx, done_rx) = mpsc::sync_channel::<()>(1);
    let mut position = 0_usize;
    let err_fn = |err| eprintln!("Interaction sound stream error: {err}");

    let stream = match supported.sample_format() {
        SampleFormat::F32 => device
            .build_output_stream(
                &config,
                move |data: &mut [f32], _| {
                    fill_output(data, channels, &samples, &mut position, &done_tx, |value| {
                        value
                    });
                },
                err_fn,
                None,
            )
            .map_err(|err| format!("Failed to build interaction sound stream: {err}"))?,
        SampleFormat::I16 => device
            .build_output_stream(
                &config,
                move |data: &mut [i16], _| {
                    fill_output(data, channels, &samples, &mut position, &done_tx, |value| {
                        (value.clamp(-1.0, 1.0) * 32767.0) as i16
                    });
                },
                err_fn,
                None,
            )
            .map_err(|err| format!("Failed to build interaction sound stream: {err}"))?,
        SampleFormat::U16 => device
            .build_output_stream(
                &config,
                move |data: &mut [u16], _| {
                    fill_output(data, channels, &samples, &mut position, &done_tx, |value| {
                        ((value.clamp(-1.0, 1.0) * 0.5 + 0.5) * f32::from(u16::MAX)) as u16
                    });
                },
                err_fn,
                None,
            )
            .map_err(|err| format!("Failed to build interaction sound stream: {err}"))?,
        other => return Err(format!("Unsupported output sample format: {other:?}")),
    };

    stream
        .play()
        .map_err(|err| format!("Failed to play interaction sound: {err}"))?;

    // Wait for the clip to finish (or a generous timeout) before dropping the
    // stream, otherwise the click is cut off.
    let _ = done_rx.recv_timeout(Duration::from_millis(clip_ms));
    Ok(())
}

fn fill_output<T: Copy>(
    data: &mut [T],
    channels: usize,
    samples: &[f32],
    position: &mut usize,
    done_tx: &SyncSender<()>,
    convert: impl Fn(f32) -> T,
) {
    for frame in data.chunks_mut(channels) {
        let value = samples.get(*position).copied().unwrap_or(0.0);
        *position += 1;
        for slot in frame.iter_mut() {
            *slot = convert(value);
        }
    }
    if *position >= samples.len() {
        let _ = done_tx.try_send(());
    }
}

fn resample_mono(samples: &[f32], source_rate: u32, target_rate: u32) -> Vec<f32> {
    if source_rate == target_rate || samples.is_empty() {
        return samples.to_vec();
    }

    let output_len =
        (samples.len() as u64 * u64::from(target_rate) / u64::from(source_rate.max(1))) as usize;
    let ratio = f64::from(source_rate) / f64::from(target_rate);
    let mut output = Vec::with_capacity(output_len);
    for index in 0..output_len {
        let source_pos = index as f64 * ratio;
        let lower = (source_pos.floor() as usize).min(samples.len() - 1);
        let upper = (lower + 1).min(samples.len() - 1);
        let weight = (source_pos - lower as f64) as f32;
        output.push(samples[lower] + (samples[upper] - samples[lower]) * weight);
    }
    output
}

#[cfg(test)]
mod tests {
    use super::{decode_wav_clip, resample_mono, RECORDING_START_WAV, RECORDING_STOP_WAV};

    #[test]
    fn bundled_interaction_sounds_decode_to_mono_clips() {
        for bytes in [RECORDING_START_WAV, RECORDING_STOP_WAV] {
            let clip = decode_wav_clip(bytes).expect("bundled clip decodes");
            assert!(!clip.mono.is_empty());
            assert!(clip.sample_rate > 0);
            assert!(clip.mono.iter().all(|sample| sample.abs() <= 1.0));
        }
    }

    #[test]
    fn resample_mono_scales_length_by_rate_ratio() {
        let samples = vec![0.5_f32; 48_000];
        assert_eq!(resample_mono(&samples, 48_000, 16_000).len(), 16_000);
        assert_eq!(resample_mono(&samples, 48_000, 48_000).len(), 48_000);
        assert!(resample_mono(&[], 48_000, 16_000).is_empty());
    }
}
