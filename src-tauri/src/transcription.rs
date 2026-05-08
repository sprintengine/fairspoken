use crate::audio::{AudioFrame, Recording};
use crate::models::{ModelService, SherpaModel, WhisperModel};
use crate::settings::{Settings, TranscriptionBackend};
use sherpa_onnx::{OnlineRecognizer, OnlineRecognizerConfig, OnlineStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::thread::{self, JoinHandle};
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

#[derive(Default)]
pub struct TranscriptionService {
    active: Option<ActiveTranscriptionBackend>,
    session: Option<ActiveTranscriptionSession>,
}

enum ActiveTranscriptionBackend {
    Whisper(WhisperTranscriber),
    Sherpa(SherpaBackend),
}

struct SherpaBackend {
    model: SherpaModel,
}

enum ActiveTranscriptionSession {
    Whisper,
    Sherpa(SherpaSessionHandle),
}

struct SherpaSessionHandle {
    audio_tx: SyncSender<AudioFrame>,
    control_tx: mpsc::Sender<SherpaControl>,
    result_rx: Receiver<Result<String, String>>,
    worker: Option<JoinHandle<()>>,
}

enum SherpaControl {
    Finish,
    Cancel,
}

struct WhisperTranscriber {
    active_model: Option<WhisperModel>,
    context: Option<WhisperContext>,
}

impl TranscriptionService {
    pub fn start_session(
        &mut self,
        settings: &Settings,
        models: &ModelService,
    ) -> Result<Option<SyncSender<AudioFrame>>, String> {
        if self.session.is_some() {
            return Err("Transcription session already in progress".to_string());
        }

        self.ensure_backend(settings)?;
        match settings.transcription_backend {
            TranscriptionBackend::Whisper => {
                if let Some(ActiveTranscriptionBackend::Whisper(transcriber)) = self.active.as_mut()
                {
                    let model_path = models.path_for(settings.model);
                    transcriber.ensure_context(settings.model, &model_path)?;
                }
                self.session = Some(ActiveTranscriptionSession::Whisper);
                Ok(None)
            }
            TranscriptionBackend::SherpaStreaming => {
                let paths = models.sherpa_paths(settings.sherpa_model);
                let status = models.transcription_status(
                    TranscriptionBackend::SherpaStreaming,
                    settings.model,
                    settings.sherpa_model,
                );
                if !status.cached {
                    return Err(status.message);
                }

                let handle = SherpaSessionHandle::start(settings.sherpa_model, paths)?;
                let audio_tx = handle.audio_tx.clone();
                self.session = Some(ActiveTranscriptionSession::Sherpa(handle));
                Ok(Some(audio_tx))
            }
        }
    }

    pub fn finish_session(
        &mut self,
        recording: &Recording,
        settings: &Settings,
        models: &ModelService,
    ) -> Result<String, String> {
        match self
            .session
            .take()
            .ok_or_else(|| "No transcription session is active".to_string())?
        {
            ActiveTranscriptionSession::Whisper => {
                let model_path = models.path_for(settings.model);
                let Some(ActiveTranscriptionBackend::Whisper(transcriber)) = self.active.as_mut()
                else {
                    return Err("Whisper backend is unavailable".to_string());
                };
                transcriber.transcribe(recording, settings, &model_path)
            }
            ActiveTranscriptionSession::Sherpa(handle) => handle.finish(),
        }
    }

    pub fn cancel_session(&mut self) {
        if let Some(session) = self.session.take() {
            if let ActiveTranscriptionSession::Sherpa(handle) = session {
                handle.cancel();
            }
        }
    }

    pub fn unload(&mut self) {
        self.cancel_session();
        self.active.take();
    }

    fn ensure_backend(&mut self, settings: &Settings) -> Result<(), String> {
        let reusable = matches!(
            (&self.active, settings.transcription_backend),
            (
                Some(ActiveTranscriptionBackend::Whisper(_)),
                TranscriptionBackend::Whisper
            )
        ) || matches!(
            (&self.active, settings.transcription_backend),
            (Some(ActiveTranscriptionBackend::Sherpa(backend)), TranscriptionBackend::SherpaStreaming)
                if backend.model == settings.sherpa_model
        );

        if reusable {
            return Ok(());
        }

        self.active.take();
        self.active = Some(match settings.transcription_backend {
            TranscriptionBackend::Whisper => {
                ActiveTranscriptionBackend::Whisper(WhisperTranscriber::default())
            }
            TranscriptionBackend::SherpaStreaming => {
                ActiveTranscriptionBackend::Sherpa(SherpaBackend {
                    model: settings.sherpa_model,
                })
            }
        });
        Ok(())
    }
}

impl Default for WhisperTranscriber {
    fn default() -> Self {
        Self {
            active_model: None,
            context: None,
        }
    }
}

impl WhisperTranscriber {
    fn transcribe(
        &mut self,
        recording: &Recording,
        settings: &Settings,
        model_path: &Path,
    ) -> Result<String, String> {
        if recording.pcm_i16.is_empty() {
            return Err("No audio samples were captured".to_string());
        }

        if !model_path.is_file() {
            return Err(format!(
                "Whisper model is missing: {}",
                model_path.display()
            ));
        }

        self.ensure_context(settings.model, model_path)?;

        let context = self
            .context
            .as_ref()
            .ok_or_else(|| "Whisper context is unavailable".to_string())?;
        let mut state = context
            .create_state()
            .map_err(|err| format!("Failed to create Whisper state: {err}"))?;
        let audio = resample_i16_to_16khz_f32(&recording.pcm_i16, recording.sample_rate);
        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 0 });
        params.set_n_threads(default_thread_count());
        params.set_print_special(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);
        params.set_no_context(true);

        if settings.language == "auto" {
            params.set_language(None);
            params.set_detect_language(true);
        } else {
            params.set_language(Some(&settings.language));
        }

        state
            .full(params, &audio)
            .map_err(|err| format!("Whisper transcription failed: {err}"))?;

        let transcript = state
            .as_iter()
            .map(|segment| segment.to_string())
            .collect::<Vec<_>>()
            .join(" ")
            .trim()
            .to_string();

        if transcript.is_empty() {
            return Err("No speech was transcribed".to_string());
        }

        Ok(transcript)
    }

    fn ensure_context(&mut self, model: WhisperModel, model_path: &Path) -> Result<(), String> {
        if self.active_model == Some(model) && self.context.is_some() {
            return Ok(());
        }

        let params = WhisperContextParameters::default();
        let path = path_to_string(model_path)?;
        let context = WhisperContext::new_with_params(&path, params)
            .map_err(|err| format!("Failed to load Whisper model: {err}"))?;

        self.context = Some(context);
        self.active_model = Some(model);
        Ok(())
    }
}

impl SherpaSessionHandle {
    fn start(model: SherpaModel, paths: crate::models::SherpaModelPaths) -> Result<Self, String> {
        let (audio_tx, audio_rx) = mpsc::sync_channel::<AudioFrame>(12);
        let (control_tx, control_rx) = mpsc::channel::<SherpaControl>();
        let (result_tx, result_rx) = mpsc::channel::<Result<String, String>>();
        let worker = thread::Builder::new()
            .name("sherpa-streaming-transcription".to_string())
            .spawn(move || {
                let result = run_sherpa_session(model, paths, audio_rx, control_rx);
                let _ = result_tx.send(result);
            })
            .map_err(|err| format!("Failed to start Sherpa worker: {err}"))?;

        Ok(Self {
            audio_tx,
            control_tx,
            result_rx,
            worker: Some(worker),
        })
    }

    fn finish(mut self) -> Result<String, String> {
        let _ = self.control_tx.send(SherpaControl::Finish);
        drop(self.audio_tx);
        let result = self
            .result_rx
            .recv()
            .map_err(|_| "Sherpa worker stopped without a transcript".to_string())?;
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        result
    }

    fn cancel(mut self) {
        let _ = self.control_tx.send(SherpaControl::Cancel);
        drop(self.audio_tx);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn run_sherpa_session(
    _model: SherpaModel,
    paths: crate::models::SherpaModelPaths,
    audio_rx: Receiver<AudioFrame>,
    control_rx: Receiver<SherpaControl>,
) -> Result<String, String> {
    let recognizer = create_sherpa_recognizer(&paths)?;
    let stream = recognizer.create_stream();
    let mut latest = String::new();

    loop {
        match control_rx.try_recv() {
            Ok(SherpaControl::Finish) => break,
            Ok(SherpaControl::Cancel) => {
                return Err("Sherpa transcription was cancelled".to_string())
            }
            Err(mpsc::TryRecvError::Disconnected) => break,
            Err(mpsc::TryRecvError::Empty) => {}
        }

        match audio_rx.recv_timeout(std::time::Duration::from_millis(20)) {
            Ok(frame) => {
                feed_sherpa_samples(
                    &recognizer,
                    &stream,
                    &frame.pcm_i16,
                    frame.sample_rate,
                    &mut latest,
                );
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }

    for frame in audio_rx.try_iter() {
        feed_sherpa_samples(
            &recognizer,
            &stream,
            &frame.pcm_i16,
            frame.sample_rate,
            &mut latest,
        );
    }

    let tail_padding = vec![0.0_f32; 4_800];
    stream.accept_waveform(16_000, &tail_padding);
    stream.input_finished();
    while recognizer.is_ready(&stream) {
        recognizer.decode(&stream);
        if let Some(result) = recognizer.get_result(&stream) {
            if !result.text.trim().is_empty() {
                latest = result.text.trim().to_string();
            }
        }
    }

    if latest.trim().is_empty() {
        return Err("No speech was transcribed".to_string());
    }
    Ok(latest.trim().to_string())
}

fn create_sherpa_recognizer(
    paths: &crate::models::SherpaModelPaths,
) -> Result<OnlineRecognizer, String> {
    let mut config = OnlineRecognizerConfig::default();
    config.model_config.transducer.encoder = Some(path_to_string(&paths.encoder)?);
    config.model_config.transducer.decoder = Some(path_to_string(&paths.decoder)?);
    config.model_config.transducer.joiner = Some(path_to_string(&paths.joiner)?);
    config.model_config.tokens = Some(path_to_string(&paths.tokens)?);
    config.model_config.provider = Some("cpu".to_string());
    config.model_config.num_threads = default_thread_count();
    config.enable_endpoint = true;
    config.decoding_method = Some("greedy_search".to_string());
    OnlineRecognizer::create(&config)
        .ok_or_else(|| "Failed to create Sherpa recognizer".to_string())
}

fn feed_sherpa_samples(
    recognizer: &OnlineRecognizer,
    stream: &OnlineStream,
    samples: &[i16],
    sample_rate: u32,
    latest: &mut String,
) {
    let audio = resample_i16_to_16khz_f32(samples, sample_rate);
    stream.accept_waveform(16_000, &audio);
    while recognizer.is_ready(stream) {
        recognizer.decode(stream);
        if let Some(result) = recognizer.get_result(stream) {
            if !result.text.trim().is_empty() {
                *latest = result.text.trim().to_string();
            }
        }
        if recognizer.is_endpoint(stream) {
            recognizer.reset(stream);
        }
    }
}

fn resample_i16_to_16khz_f32(samples: &[i16], source_rate: u32) -> Vec<f32> {
    if source_rate == 16_000 {
        return samples
            .iter()
            .map(|sample| f32::from(*sample) / 32768.0)
            .collect();
    }

    let output_len = (samples.len() as u64 * 16_000 / u64::from(source_rate.max(1))) as usize;
    let mut output = Vec::with_capacity(output_len);
    let ratio = source_rate as f64 / 16_000.0;

    for index in 0..output_len {
        let source_pos = index as f64 * ratio;
        let lower = source_pos.floor() as usize;
        let upper = (lower + 1).min(samples.len().saturating_sub(1));
        let weight = (source_pos - lower as f64) as f32;
        let a = f32::from(samples[lower]) / 32768.0;
        let b = f32::from(samples[upper]) / 32768.0;
        output.push(a + (b - a) * weight);
    }

    output
}

fn default_thread_count() -> i32 {
    std::thread::available_parallelism()
        .map(|threads| threads.get().min(4) as i32)
        .unwrap_or(1)
}

fn path_to_string(path: &Path) -> Result<String, String> {
    let absolute: PathBuf = path
        .canonicalize()
        .map_err(|err| format!("Failed to resolve model path: {err}"))?;
    absolute
        .to_str()
        .map(ToOwned::to_owned)
        .ok_or_else(|| "Model path is not valid UTF-8".to_string())
}

#[cfg(test)]
mod tests {
    use super::resample_i16_to_16khz_f32;

    #[test]
    fn converts_16khz_i16_to_f32_without_resampling() {
        let audio = resample_i16_to_16khz_f32(&[0, 16_384, -16_384], 16_000);

        assert_eq!(audio.len(), 3);
        assert_eq!(audio[0], 0.0);
        assert!((audio[1] - 0.5).abs() < 0.001);
        assert!((audio[2] + 0.5).abs() < 0.001);
    }

    #[test]
    fn downsamples_to_16khz_length() {
        let input = vec![1_000_i16; 48_000];
        let audio = resample_i16_to_16khz_f32(&input, 48_000);

        assert_eq!(audio.len(), 16_000);
    }

    #[test]
    fn handles_empty_input() {
        let audio = resample_i16_to_16khz_f32(&[], 48_000);

        assert!(audio.is_empty());
    }
}
