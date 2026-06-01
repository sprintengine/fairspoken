use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, Stream, StreamConfig, SupportedStreamConfig};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{SyncSender, TrySendError};
use std::sync::{Arc, Mutex};

const DEFAULT_MAX_RECORDING_SECONDS: u16 = 120;

#[derive(Default)]
pub struct AudioService {
    is_recording: bool,
    stream: Option<Stream>,
    buffer: Option<Arc<Mutex<Vec<i16>>>>,
    dropped_stream_frames: Option<Arc<AtomicU64>>,
    sample_rate: u32,
}

#[derive(Clone, Debug)]
pub struct Recording {
    pub pcm_i16: Vec<i16>,
    pub sample_rate: u32,
    pub dropped_stream_frames: u64,
}

#[derive(Clone, Debug)]
pub struct AudioFrame {
    pub pcm_i16: Vec<i16>,
    pub sample_rate: u32,
}

#[derive(Clone, Copy, Debug)]
pub struct AudioStats {
    pub duration_seconds: f32,
    pub peak: f32,
    pub rms: f32,
    pub dropped_stream_frames: u64,
}

impl Recording {
    pub fn stats(&self) -> AudioStats {
        let mut sum_sq = 0.0_f64;
        let mut peak = 0.0_f32;

        for sample in &self.pcm_i16 {
            let normalized = f32::from(*sample) / 32768.0;
            let abs = normalized.abs();
            peak = peak.max(abs);
            sum_sq += f64::from(normalized * normalized);
        }

        let len = self.pcm_i16.len().max(1);
        AudioStats {
            duration_seconds: self.pcm_i16.len() as f32 / self.sample_rate.max(1) as f32,
            peak,
            rms: (sum_sq / len as f64).sqrt() as f32,
            dropped_stream_frames: self.dropped_stream_frames,
        }
    }
}

impl AudioService {
    pub fn is_recording(&self) -> bool {
        self.is_recording
    }

    pub fn start(
        &mut self,
        max_recording_seconds: u16,
        input_gain: u8,
        stream_sink: Option<SyncSender<AudioFrame>>,
    ) -> Result<(), String> {
        if self.is_recording {
            return Err("Recording already in progress".to_string());
        }

        let host = cpal::default_host();
        let (device, supported_config) = input_device_and_config(&host)?;
        let sample_rate = supported_config.sample_rate();
        let channels = supported_config.channels() as usize;
        let config: StreamConfig = supported_config.clone().into();
        let input_gain = input_gain.clamp(1, 6) as f32;
        let max_samples = sample_rate as usize * usize::from(max_recording_seconds.max(1).min(300));
        let buffer = Arc::new(Mutex::new(Vec::with_capacity(
            sample_rate as usize
                * usize::from(DEFAULT_MAX_RECORDING_SECONDS.min(max_recording_seconds)),
        )));
        let dropped_stream_frames = Arc::new(AtomicU64::new(0));

        let stream = match supported_config.sample_format() {
            SampleFormat::F32 => build_input_stream::<f32>(
                &device,
                &config,
                channels,
                input_gain,
                max_samples,
                &buffer,
                &dropped_stream_frames,
                stream_sink,
            ),
            SampleFormat::F64 => build_input_stream::<f64>(
                &device,
                &config,
                channels,
                input_gain,
                max_samples,
                &buffer,
                &dropped_stream_frames,
                stream_sink,
            ),
            SampleFormat::I8 => build_input_stream::<i8>(
                &device,
                &config,
                channels,
                input_gain,
                max_samples,
                &buffer,
                &dropped_stream_frames,
                stream_sink,
            ),
            SampleFormat::I16 => build_input_stream::<i16>(
                &device,
                &config,
                channels,
                input_gain,
                max_samples,
                &buffer,
                &dropped_stream_frames,
                stream_sink,
            ),
            SampleFormat::I32 => build_input_stream::<i32>(
                &device,
                &config,
                channels,
                input_gain,
                max_samples,
                &buffer,
                &dropped_stream_frames,
                stream_sink,
            ),
            SampleFormat::U8 => build_input_stream::<u8>(
                &device,
                &config,
                channels,
                input_gain,
                max_samples,
                &buffer,
                &dropped_stream_frames,
                stream_sink,
            ),
            SampleFormat::U16 => build_input_stream::<u16>(
                &device,
                &config,
                channels,
                input_gain,
                max_samples,
                &buffer,
                &dropped_stream_frames,
                stream_sink,
            ),
            SampleFormat::U32 => build_input_stream::<u32>(
                &device,
                &config,
                channels,
                input_gain,
                max_samples,
                &buffer,
                &dropped_stream_frames,
                stream_sink,
            ),
            other => Err(format!("Unsupported input sample format: {other:?}")),
        }?;

        stream
            .play()
            .map_err(|err| format!("Failed to start audio stream: {err}"))?;

        self.is_recording = true;
        self.sample_rate = sample_rate;
        self.buffer = Some(buffer);
        self.dropped_stream_frames = Some(dropped_stream_frames);
        self.stream = Some(stream);
        Ok(())
    }

    pub fn stop(&mut self) -> Result<Recording, String> {
        if !self.is_recording {
            return Err("No recording in progress".to_string());
        }

        self.stream.take();
        self.is_recording = false;
        let buffer = self
            .buffer
            .take()
            .ok_or_else(|| "Recording buffer is unavailable".to_string())?;
        let pcm_i16 = buffer
            .lock()
            .map_err(|_| "Recording buffer lock failed".to_string())?
            .clone();
        let dropped_stream_frames = self
            .dropped_stream_frames
            .take()
            .map(|counter| counter.load(Ordering::Relaxed))
            .unwrap_or(0);

        if pcm_i16.is_empty() {
            return Err("No audio samples were captured".to_string());
        }

        Ok(Recording {
            pcm_i16,
            sample_rate: self.sample_rate,
            dropped_stream_frames,
        })
    }
}

fn input_device_and_config(
    host: &cpal::Host,
) -> Result<(cpal::Device, SupportedStreamConfig), String> {
    let default_device = host.default_input_device();
    let mut default_error = None;

    if let Some(device) = default_device {
        match device.default_input_config() {
            Ok(config) => return Ok((device, config)),
            Err(err) => {
                let name = input_device_name(&device);
                default_error = Some(format!(
                    "Default input device{} is unavailable: {}{}",
                    name.as_deref()
                        .map(|name| format!(" '{name}'"))
                        .unwrap_or_default(),
                    err,
                    macos_audio_hint(&err.to_string())
                ));
            }
        }
    }

    let mut fallback_errors = Vec::new();
    let input_devices = host
        .input_devices()
        .map_err(|err| format!("Failed to enumerate input devices: {err}"))?;

    for device in input_devices {
        match device.default_input_config() {
            Ok(config) => {
                if let Some(err) = default_error {
                    eprintln!(
                        "{err}; using fallback input device{}",
                        input_device_label(&device)
                    );
                }
                return Ok((device, config));
            }
            Err(err) => {
                fallback_errors.push(format!(
                    "{}: {err}",
                    input_device_name(&device)
                        .unwrap_or_else(|| "Unnamed input device".to_string())
                ));
            }
        }
    }

    let mut message =
        default_error.unwrap_or_else(|| "No default input device is available".to_string());
    if !fallback_errors.is_empty() {
        message.push_str("; checked input devices: ");
        message.push_str(&fallback_errors.join("; "));
    }
    Err(message)
}

fn input_device_name(device: &cpal::Device) -> Option<String> {
    device
        .description()
        .ok()
        .map(|description| description.name().to_string())
        .filter(|name| !name.trim().is_empty())
}

fn input_device_label(device: &cpal::Device) -> String {
    input_device_name(device)
        .map(|name| format!(" '{name}'"))
        .unwrap_or_default()
}

fn macos_audio_hint(error: &str) -> &'static str {
    if cfg!(target_os = "macos") && (error.contains("560947818") || error.contains("!obj")) {
        " (CoreAudio !obj: macOS could not resolve the selected input device; check microphone permission and the system default input device)"
    } else {
        ""
    }
}

trait ToI16Sample {
    fn to_i16_sample(self) -> i16;
}

impl ToI16Sample for f32 {
    fn to_i16_sample(self) -> i16 {
        let clamped = self.clamp(-1.0, 1.0);
        if clamped < 0.0 {
            (clamped * 32768.0) as i16
        } else {
            (clamped * 32767.0) as i16
        }
    }
}

impl ToI16Sample for f64 {
    fn to_i16_sample(self) -> i16 {
        (self as f32).to_i16_sample()
    }
}

impl ToI16Sample for i8 {
    fn to_i16_sample(self) -> i16 {
        i16::from(self) << 8
    }
}

impl ToI16Sample for i16 {
    fn to_i16_sample(self) -> i16 {
        self
    }
}

impl ToI16Sample for i32 {
    fn to_i16_sample(self) -> i16 {
        (self >> 16) as i16
    }
}

impl ToI16Sample for u8 {
    fn to_i16_sample(self) -> i16 {
        (i16::from(self) - 128) << 8
    }
}

impl ToI16Sample for u16 {
    fn to_i16_sample(self) -> i16 {
        (i32::from(self) - 32768) as i16
    }
}

impl ToI16Sample for u32 {
    fn to_i16_sample(self) -> i16 {
        ((i64::from(self) - 2_147_483_648) >> 16) as i16
    }
}

fn build_input_stream<T>(
    device: &cpal::Device,
    config: &StreamConfig,
    channels: usize,
    input_gain: f32,
    max_samples: usize,
    buffer: &Arc<Mutex<Vec<i16>>>,
    dropped_stream_frames: &Arc<AtomicU64>,
    stream_sink: Option<SyncSender<AudioFrame>>,
) -> Result<Stream, String>
where
    T: ToI16Sample + cpal::SizedSample + Copy + Send + 'static,
{
    let buffer = Arc::clone(buffer);
    let captured_samples = Arc::new(AtomicUsize::new(0));
    let dropped_stream_frames = Arc::clone(dropped_stream_frames);
    let err_fn = |err| eprintln!("Audio input stream error: {err}");
    let sink = stream_sink;
    let sample_rate = config.sample_rate;
    let mut chunk = Vec::new();

    device
        .build_input_stream(
            config,
            move |data: &[T], _| {
                let captured = captured_samples.load(Ordering::Relaxed);
                if captured >= max_samples {
                    return;
                }

                chunk.clear();
                append_mono_samples(
                    data,
                    channels,
                    input_gain,
                    max_samples - captured,
                    &mut chunk,
                );
                if chunk.is_empty() {
                    return;
                }

                if let Ok(mut target) = buffer.lock() {
                    target.extend_from_slice(&chunk);
                    captured_samples.fetch_add(chunk.len(), Ordering::Relaxed);
                } else {
                    return;
                }

                if let Some(sink) = &sink {
                    let frame = AudioFrame {
                        pcm_i16: chunk.clone(),
                        sample_rate,
                    };
                    match sink.try_send(frame) {
                        Ok(()) => {}
                        Err(TrySendError::Full(_)) => {
                            dropped_stream_frames.fetch_add(1, Ordering::Relaxed);
                        }
                        Err(TrySendError::Disconnected(_)) => {}
                    }
                }
            },
            err_fn,
            None,
        )
        .map_err(|err| format!("Failed to build audio input stream: {err}"))
}

fn append_mono_samples<T>(
    data: &[T],
    channels: usize,
    input_gain: f32,
    max_samples: usize,
    target: &mut Vec<i16>,
) where
    T: ToI16Sample + Copy,
{
    if target.len() >= max_samples {
        return;
    }

    let channels = channels.max(1);
    for frame in data.chunks(channels) {
        if target.len() >= max_samples {
            break;
        }

        let sum: i32 = frame
            .iter()
            .map(|sample| i32::from(sample.to_i16_sample()))
            .sum();
        let mono = sum as f32 / frame.len() as f32;
        target.push((mono * input_gain).clamp(i16::MIN as f32, i16::MAX as f32) as i16);
    }
}

#[cfg(test)]
mod tests {
    use super::{append_mono_samples, Recording, ToI16Sample};

    #[test]
    fn converts_float_samples_to_i16() {
        assert_eq!((-1.0_f32).to_i16_sample(), i16::MIN);
        assert_eq!(1.0_f32.to_i16_sample(), i16::MAX);
        assert_eq!(0.0_f32.to_i16_sample(), 0);
    }

    #[test]
    fn averages_interleaved_channels_to_mono() {
        let mut target = Vec::new();
        append_mono_samples(
            &[100_i16, 300_i16, -100_i16, 100_i16],
            2,
            1.0,
            8,
            &mut target,
        );

        assert_eq!(target, vec![200, 0]);
    }

    #[test]
    fn respects_max_sample_limit() {
        let mut target = vec![1_i16];
        append_mono_samples(&[2_i16, 3_i16, 4_i16], 1, 1.0, 2, &mut target);

        assert_eq!(target, vec![1, 2]);
    }

    #[test]
    fn applies_input_gain_to_mono_samples() {
        let mut target = Vec::new();
        append_mono_samples(&[1000_i16, -1000_i16], 1, 3.0, 8, &mut target);

        assert_eq!(target, vec![3000, -3000]);
    }

    #[test]
    fn reports_recording_stats() {
        let recording = Recording {
            pcm_i16: vec![0, 16_384, -16_384, 0],
            sample_rate: 4,
            dropped_stream_frames: 2,
        };
        let stats = recording.stats();

        assert_eq!(stats.duration_seconds, 1.0);
        assert!((stats.peak - 0.5).abs() < 0.001);
        assert!((stats.rms - 0.353).abs() < 0.001);
        assert_eq!(stats.dropped_stream_frames, 2);
    }
}
