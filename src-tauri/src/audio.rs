use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, Stream, StreamConfig};
use std::sync::mpsc::{SyncSender, TrySendError};
use std::sync::{Arc, Mutex};

const DEFAULT_MAX_RECORDING_SECONDS: u16 = 120;

#[derive(Default)]
pub struct AudioService {
    is_recording: bool,
    stream: Option<Stream>,
    buffer: Option<Arc<Mutex<Vec<i16>>>>,
    sample_rate: u32,
}

#[derive(Clone, Debug)]
pub struct Recording {
    pub pcm_i16: Vec<i16>,
    pub sample_rate: u32,
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
        stream_sink: Option<SyncSender<AudioFrame>>,
    ) -> Result<(), String> {
        if self.is_recording {
            return Err("Recording already in progress".to_string());
        }

        let host = cpal::default_host();
        let device = host
            .default_input_device()
            .ok_or_else(|| "No default input device is available".to_string())?;
        let supported_config = device
            .default_input_config()
            .map_err(|err| format!("Failed to read default input config: {err}"))?;
        let sample_rate = supported_config.sample_rate();
        let channels = supported_config.channels() as usize;
        let config: StreamConfig = supported_config.clone().into();
        let max_samples = sample_rate as usize * usize::from(max_recording_seconds.max(1).min(300));
        let buffer = Arc::new(Mutex::new(Vec::with_capacity(
            sample_rate as usize
                * usize::from(DEFAULT_MAX_RECORDING_SECONDS.min(max_recording_seconds)),
        )));

        let stream = match supported_config.sample_format() {
            SampleFormat::F32 => build_input_stream::<f32>(
                &device,
                &config,
                channels,
                max_samples,
                &buffer,
                stream_sink,
            ),
            SampleFormat::F64 => build_input_stream::<f64>(
                &device,
                &config,
                channels,
                max_samples,
                &buffer,
                stream_sink,
            ),
            SampleFormat::I8 => build_input_stream::<i8>(
                &device,
                &config,
                channels,
                max_samples,
                &buffer,
                stream_sink,
            ),
            SampleFormat::I16 => build_input_stream::<i16>(
                &device,
                &config,
                channels,
                max_samples,
                &buffer,
                stream_sink,
            ),
            SampleFormat::I32 => build_input_stream::<i32>(
                &device,
                &config,
                channels,
                max_samples,
                &buffer,
                stream_sink,
            ),
            SampleFormat::U8 => build_input_stream::<u8>(
                &device,
                &config,
                channels,
                max_samples,
                &buffer,
                stream_sink,
            ),
            SampleFormat::U16 => build_input_stream::<u16>(
                &device,
                &config,
                channels,
                max_samples,
                &buffer,
                stream_sink,
            ),
            SampleFormat::U32 => build_input_stream::<u32>(
                &device,
                &config,
                channels,
                max_samples,
                &buffer,
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

        if pcm_i16.is_empty() {
            return Err("No audio samples were captured".to_string());
        }

        Ok(Recording {
            pcm_i16,
            sample_rate: self.sample_rate,
        })
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
    max_samples: usize,
    buffer: &Arc<Mutex<Vec<i16>>>,
    stream_sink: Option<SyncSender<AudioFrame>>,
) -> Result<Stream, String>
where
    T: ToI16Sample + cpal::SizedSample + Copy + Send + 'static,
{
    let buffer = Arc::clone(buffer);
    let err_fn = |err| eprintln!("Audio input stream error: {err}");
    let sink = stream_sink;
    let sample_rate = config.sample_rate;

    device
        .build_input_stream(
            config,
            move |data: &[T], _| {
                if let Ok(mut target) = buffer.lock() {
                    let start = target.len();
                    append_mono_samples(data, channels, max_samples, &mut target);
                    if let Some(sink) = &sink {
                        if target.len() > start {
                            let chunk = AudioFrame {
                                pcm_i16: target[start..].to_vec(),
                                sample_rate,
                            };
                            match sink.try_send(chunk) {
                                Ok(()) | Err(TrySendError::Full(_)) => {}
                                Err(TrySendError::Disconnected(_)) => {}
                            }
                        }
                    }
                }
            },
            err_fn,
            None,
        )
        .map_err(|err| format!("Failed to build audio input stream: {err}"))
}

fn append_mono_samples<T>(data: &[T], channels: usize, max_samples: usize, target: &mut Vec<i16>)
where
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
        target.push((sum / frame.len() as i32) as i16);
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
        append_mono_samples(&[100_i16, 300_i16, -100_i16, 100_i16], 2, 8, &mut target);

        assert_eq!(target, vec![200, 0]);
    }

    #[test]
    fn respects_max_sample_limit() {
        let mut target = vec![1_i16];
        append_mono_samples(&[2_i16, 3_i16, 4_i16], 1, 2, &mut target);

        assert_eq!(target, vec![1, 2]);
    }

    #[test]
    fn reports_recording_stats() {
        let recording = Recording {
            pcm_i16: vec![0, 16_384, -16_384, 0],
            sample_rate: 4,
        };
        let stats = recording.stats();

        assert_eq!(stats.duration_seconds, 1.0);
        assert!((stats.peak - 0.5).abs() < 0.001);
        assert!((stats.rms - 0.353).abs() < 0.001);
    }
}
