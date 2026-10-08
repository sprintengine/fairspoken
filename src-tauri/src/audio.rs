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

/// One capture-callback's worth of input level, normalized to [0, 1].
/// Streamed to the UI so the recording meter reflects the real signal.
#[derive(Clone, Copy, Debug)]
pub struct LevelSample {
    pub peak: f32,
    pub rms: f32,
}

/// Invoked (from the audio thread) when the input stream reports an error,
/// e.g. the capture device disappearing mid-recording.
pub type StreamErrorCallback = Box<dyn Fn(String) + Send + Sync + 'static>;

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
        level_sink: Option<SyncSender<LevelSample>>,
        on_stream_error: Option<StreamErrorCallback>,
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
        let max_samples = sample_rate as usize * usize::from(max_recording_seconds.clamp(1, 600));
        let buffer = Arc::new(Mutex::new(Vec::with_capacity(
            sample_rate as usize
                * usize::from(DEFAULT_MAX_RECORDING_SECONDS.min(max_recording_seconds)),
        )));
        let dropped_stream_frames = Arc::new(AtomicU64::new(0));
        let capture_config = InputCaptureConfig {
            channels,
            input_gain,
            max_samples,
            buffer: &buffer,
            dropped_stream_frames: &dropped_stream_frames,
        };

        let stream = match supported_config.sample_format() {
            SampleFormat::F32 => build_input_stream::<f32>(
                &device,
                &config,
                capture_config,
                stream_sink,
                level_sink,
                on_stream_error,
            ),
            SampleFormat::F64 => build_input_stream::<f64>(
                &device,
                &config,
                capture_config,
                stream_sink,
                level_sink,
                on_stream_error,
            ),
            SampleFormat::I8 => build_input_stream::<i8>(
                &device,
                &config,
                capture_config,
                stream_sink,
                level_sink,
                on_stream_error,
            ),
            SampleFormat::I16 => build_input_stream::<i16>(
                &device,
                &config,
                capture_config,
                stream_sink,
                level_sink,
                on_stream_error,
            ),
            SampleFormat::I32 => build_input_stream::<i32>(
                &device,
                &config,
                capture_config,
                stream_sink,
                level_sink,
                on_stream_error,
            ),
            SampleFormat::U8 => build_input_stream::<u8>(
                &device,
                &config,
                capture_config,
                stream_sink,
                level_sink,
                on_stream_error,
            ),
            SampleFormat::U16 => build_input_stream::<u16>(
                &device,
                &config,
                capture_config,
                stream_sink,
                level_sink,
                on_stream_error,
            ),
            SampleFormat::U32 => build_input_stream::<u32>(
                &device,
                &config,
                capture_config,
                stream_sink,
                level_sink,
                on_stream_error,
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
        // The stream is gone, so nothing writes to the buffer any more: move
        // the samples out rather than copying minutes of audio.
        let pcm_i16 = std::mem::take(
            &mut *buffer
                .lock()
                .map_err(|_| "Recording buffer lock failed".to_string())?,
        );
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
    capture: InputCaptureConfig<'_>,
    stream_sink: Option<SyncSender<AudioFrame>>,
    level_sink: Option<SyncSender<LevelSample>>,
    on_stream_error: Option<StreamErrorCallback>,
) -> Result<Stream, String>
where
    T: ToI16Sample + cpal::SizedSample + Copy + Send + 'static,
{
    let buffer = Arc::clone(capture.buffer);
    let captured_samples = Arc::new(AtomicUsize::new(0));
    let dropped_stream_frames = Arc::clone(capture.dropped_stream_frames);
    let err_fn = move |err: cpal::StreamError| {
        eprintln!("Audio input stream error: {err}");
        if let Some(on_stream_error) = &on_stream_error {
            on_stream_error(err.to_string());
        }
    };
    let sink = stream_sink;
    let sample_rate = config.sample_rate;
    let channels = capture.channels;
    let input_gain = capture.input_gain;
    let max_samples = capture.max_samples;
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

                // Live level for the recording meter. Non-blocking: a full
                // channel just drops the sample — the meter is display-only.
                if let Some(level_sink) = &level_sink {
                    let _ = level_sink.try_send(chunk_level(&chunk));
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

#[derive(Clone, Copy)]
struct InputCaptureConfig<'a> {
    channels: usize,
    input_gain: f32,
    max_samples: usize,
    buffer: &'a Arc<Mutex<Vec<i16>>>,
    dropped_stream_frames: &'a Arc<AtomicU64>,
}

fn chunk_level(chunk: &[i16]) -> LevelSample {
    let mut peak = 0.0_f32;
    let mut sum_sq = 0.0_f64;
    for sample in chunk {
        let normalized = f32::from(*sample) / 32768.0;
        peak = peak.max(normalized.abs());
        sum_sq += f64::from(normalized * normalized);
    }
    LevelSample {
        peak,
        rms: (sum_sq / chunk.len().max(1) as f64).sqrt() as f32,
    }
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

/// The rate every transcription host and engine works at; audio streamed to
/// a host is brought down to it first.
pub const STREAM_SAMPLE_RATE: u32 = 16_000;
/// Low-pass corner: 92% of the 8 kHz output Nyquist frequency, so the
/// filter's transition band ends at about 8 kHz.
const DOWNSAMPLE_CUTOFF_HZ: f64 = 7_360.0;
/// Filter taps either side of each output sample, in output periods.
/// Thirty-two keeps the transition band about 1.25 kHz wide.
const DOWNSAMPLE_HALF_WIDTH: u64 = 32;
/// Kaiser window shape: about 80 dB of stopband attenuation.
const DOWNSAMPLE_KAISER_BETA: f64 = 8.0;
/// Most filter phases precomputed. An odd capture rate whose ratio to 16 kHz
/// needs more has each output's position rounded to 1/512 of an input
/// sample, far below anything audible.
const DOWNSAMPLE_MAX_PHASES: u64 = 512;

/// Streaming low-pass decimator from a capture rate to `STREAM_SAMPLE_RATE`:
/// a polyphase windowed-sinc (Kaiser) FIR, so speech energy above 8 kHz is
/// filtered out rather than folded back into the band the models hear.
/// Frames of any size go through `process`; `flush` emits the last few
/// outputs once the stream ends. Output sample `k` sits at input position
/// `k * input_rate / 16000`, so `ceil(n * 16000 / input_rate)` samples come
/// out for `n` in, however they were split into frames.
pub struct Downsampler {
    input_rate: u32,
    /// The 16 kHz / input-rate ratio in lowest terms, `up / down`.
    up: u64,
    down: u64,
    phases: u64,
    /// Taps either side of an output's position, in input samples.
    half: u64,
    /// `phases` rows of `2 * half` coefficients.
    table: Vec<f32>,
    /// Input samples from absolute index `base` on.
    history: Vec<f32>,
    base: u64,
    consumed: u64,
    produced: u64,
}

impl Downsampler {
    /// `None` when `input_rate` is already at or below 16 kHz: there is
    /// nothing to remove, and the host resamples up if it must.
    pub fn new(input_rate: u32) -> Option<Self> {
        if input_rate <= STREAM_SAMPLE_RATE {
            return None;
        }
        let common = gcd(u64::from(STREAM_SAMPLE_RATE), u64::from(input_rate));
        let up = u64::from(STREAM_SAMPLE_RATE) / common;
        let down = u64::from(input_rate) / common;
        let phases = up.min(DOWNSAMPLE_MAX_PHASES);
        let half = (DOWNSAMPLE_HALF_WIDTH * u64::from(input_rate))
            .div_ceil(u64::from(STREAM_SAMPLE_RATE));
        let taps = 2 * half as usize;
        // Cycles per input sample.
        let cutoff = DOWNSAMPLE_CUTOFF_HZ / f64::from(input_rate);
        let window_norm = bessel_i0(DOWNSAMPLE_KAISER_BETA);
        let mut table = Vec::with_capacity(phases as usize * taps);
        for phase in 0..phases {
            let fraction = phase as f64 / phases as f64;
            let row: Vec<f64> = (0..taps)
                .map(|tap| {
                    // Distance from the output's position to this tap's input.
                    let x = (tap as f64 + 1.0 - half as f64) - fraction;
                    let edge = (x / half as f64).clamp(-1.0, 1.0);
                    let window = bessel_i0(DOWNSAMPLE_KAISER_BETA * (1.0 - edge * edge).sqrt())
                        / window_norm;
                    2.0 * cutoff * sinc(2.0 * cutoff * x) * window
                })
                .collect();
            // Unity gain at DC for every phase.
            let sum: f64 = row.iter().sum();
            table.extend(row.iter().map(|c| (c / sum) as f32));
        }
        Some(Self {
            input_rate,
            up,
            down,
            phases,
            half,
            table,
            history: Vec::new(),
            base: 0,
            consumed: 0,
            produced: 0,
        })
    }

    pub fn input_rate(&self) -> u32 {
        self.input_rate
    }

    /// Takes the next input samples and returns the outputs they complete.
    pub fn process(&mut self, samples: &[i16]) -> Vec<i16> {
        self.history
            .extend(samples.iter().map(|sample| f32::from(*sample)));
        self.consumed += samples.len() as u64;
        let mut output =
            Vec::with_capacity((samples.len() as u64 * self.up / self.down) as usize + 1);
        // An output is ready once the input `half` samples past it is in.
        while self.next_position().0 + self.half < self.consumed {
            output.push(self.next_output());
        }
        self.trim_history();
        output
    }

    /// The outputs still owed once the input has ended, the missing future
    /// samples taken as silence.
    pub fn flush(&mut self) -> Vec<i16> {
        let mut output = Vec::new();
        while self.produced * self.down < self.consumed * self.up {
            output.push(self.next_output());
        }
        output
    }

    /// The input index at or before the next output's position, and the
    /// filter phase for the fraction past it.
    fn next_position(&self) -> (u64, u64) {
        let position = self.produced * self.down;
        let mut n0 = position / self.up;
        let remainder = position % self.up;
        let mut phase = if self.phases == self.up {
            remainder
        } else {
            (remainder * self.phases + self.up / 2) / self.up
        };
        if phase == self.phases {
            n0 += 1;
            phase = 0;
        }
        (n0, phase)
    }

    fn next_output(&mut self) -> i16 {
        let (n0, phase) = self.next_position();
        let taps = 2 * self.half as usize;
        let row = &self.table[phase as usize * taps..(phase as usize + 1) * taps];
        // The first tap's input index; before the stream or past its end the
        // input is silence.
        let first = n0 as i64 + 1 - self.half as i64;
        let mut sum = 0.0_f32;
        for (tap, coefficient) in row.iter().enumerate() {
            let index = first + tap as i64;
            if index < self.base as i64 || index >= self.consumed as i64 {
                continue;
            }
            sum += coefficient * self.history[(index as u64 - self.base) as usize];
        }
        self.produced += 1;
        sum.round()
            .clamp(f32::from(i16::MIN), f32::from(i16::MAX)) as i16
    }

    /// Drops input no future output reaches, in batches.
    fn trim_history(&mut self) {
        let needed_from = (self.next_position().0 + 1).saturating_sub(self.half);
        let stale = needed_from.saturating_sub(self.base) as usize;
        if stale >= 4_096 {
            self.history.drain(..stale);
            self.base += stale as u64;
        }
    }
}

fn gcd(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

fn sinc(x: f64) -> f64 {
    if x.abs() < 1e-12 {
        1.0
    } else {
        let px = std::f64::consts::PI * x;
        px.sin() / px
    }
}

/// The zeroth-order modified Bessel function of the first kind, by its
/// power series (the Kaiser window's building block).
fn bessel_i0(x: f64) -> f64 {
    let half = x / 2.0;
    let mut term = 1.0;
    let mut sum = 1.0;
    for k in 1..64 {
        term *= half / k as f64;
        let squared = term * term;
        sum += squared;
        if squared < sum * 1e-16 {
            break;
        }
    }
    sum
}

#[cfg(test)]
mod tests {
    use super::{append_mono_samples, Downsampler, Recording, ToI16Sample, STREAM_SAMPLE_RATE};

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

    fn tone(rate: u32, hz: f64, amplitude: f64, samples: usize) -> Vec<i16> {
        (0..samples)
            .map(|n| {
                (amplitude * (2.0 * std::f64::consts::PI * hz * n as f64 / f64::from(rate)).sin())
                    .round() as i16
            })
            .collect()
    }

    /// The whole input through the downsampler in capture-callback-sized
    /// frames, then flushed.
    fn downsample(rate: u32, input: &[i16], frame: usize) -> Vec<i16> {
        let mut downsampler = Downsampler::new(rate).expect("a rate above 16 kHz");
        let mut output = Vec::new();
        for chunk in input.chunks(frame) {
            output.extend(downsampler.process(chunk));
        }
        output.extend(downsampler.flush());
        output
    }

    fn rms(samples: &[i16]) -> f64 {
        (samples.iter().map(|s| f64::from(*s).powi(2)).sum::<f64>() / samples.len() as f64).sqrt()
    }

    /// The output away from the start and end, where the filter sees silence.
    fn middle(samples: &[i16]) -> &[i16] {
        &samples[samples.len() / 10..samples.len() * 9 / 10]
    }

    #[test]
    fn downsampling_keeps_speech_and_removes_what_would_alias() {
        for rate in [48_000, 44_100, 96_000] {
            let input = tone(rate, 1_000.0, 10_000.0, rate as usize);
            let kept = downsample(rate, &input, 480);
            let ratio = rms(middle(&kept)) / rms(middle(&input));
            assert!((0.98..1.02).contains(&ratio), "{rate} Hz: 1 kHz kept at {ratio}");

            // 10 kHz would fold to 6 kHz; 8.5 kHz to 7.5 kHz.
            for hz in [10_000.0, 8_500.0] {
                let input = tone(rate, hz, 10_000.0, rate as usize);
                let removed = downsample(rate, &input, 480);
                let ratio = rms(middle(&removed)) / rms(middle(&input));
                assert!(ratio < 0.001, "{rate} Hz: {hz} Hz left at {ratio}");
            }

            let dc = downsample(rate, &vec![10_000; rate as usize], 441);
            assert!(middle(&dc).iter().all(|s| (s - 10_000).abs() <= 1));
        }
    }

    #[test]
    fn downsampled_length_and_samples_do_not_depend_on_framing() {
        for (rate, samples) in [(48_000, 48_000), (44_100, 12_345), (22_050, 7), (48_000, 0)] {
            let input = tone(rate, 440.0, 8_000.0, samples);
            let whole = downsample(rate, &input, samples.max(1));
            let expected = (samples as u64 * u64::from(STREAM_SAMPLE_RATE)).div_ceil(u64::from(rate));
            assert_eq!(whole.len() as u64, expected, "{rate} Hz, {samples} samples");
            for frame in [1, 160, 441, 4_800] {
                assert_eq!(downsample(rate, &input, frame), whole, "{rate} Hz in {frame}s");
            }
        }
        // A long stream trims its history without changing the output.
        let long = tone(48_000, 300.0, 8_000.0, 48_000 * 5);
        assert_eq!(downsample(48_000, &long, 480), downsample(48_000, &long, long.len()));
    }

    #[test]
    fn only_rates_above_the_stream_rate_are_downsampled_odd_ones_included() {
        assert!(Downsampler::new(16_000).is_none());
        assert!(Downsampler::new(8_000).is_none());
        // An odd rate whose ratio needs more phases than are precomputed.
        let input = tone(44_056, 1_000.0, 10_000.0, 44_056);
        let output = downsample(44_056, &input, 480);
        assert_eq!(output.len(), 16_000);
        let ratio = rms(middle(&output)) / rms(middle(&input));
        assert!((0.98..1.02).contains(&ratio), "{ratio}");
    }
}
