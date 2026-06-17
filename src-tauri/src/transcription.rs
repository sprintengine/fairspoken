use crate::audio::{AudioFrame, Recording};
use crate::models::{ModelService, SttModel};
use crate::settings::Settings;
use parakeet_rs::{ParakeetTDT, Transcriber};
use std::path::Path;
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

#[cfg(feature = "whisper")]
use whisper_rs::{
    FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters, WhisperSegment,
};

const STREAM_CHANNEL_DEPTH: usize = 64;
const CHUNK_OVERLAP_SECONDS: usize = 1;
const SPEECH_PEAK_THRESHOLD: f32 = 0.010;
const SPEECH_RMS_THRESHOLD: f32 = 0.0015;
const SPEECH_WINDOW_MS: usize = 30;
const SPEECH_WINDOW_PEAK_THRESHOLD: f32 = 0.020;
const SPEECH_WINDOW_RMS_THRESHOLD: f32 = 0.004;
const MIN_ACTIVE_SPEECH_MS: usize = 120;
#[cfg(feature = "whisper")]
const NO_SPEECH_PROBABILITY_THRESHOLD: f32 = 0.60;
#[cfg(feature = "whisper")]
const MIN_AVG_TOKEN_PROBABILITY: f32 = 0.12;

/// Sentinel returned by the chunking worker when no audio was ever streamed, so
/// the caller can fall back to a direct (whole-recording) transcription.
const NO_STREAMED_AUDIO: &str = "No audio frames were streamed to the transcriber";

/// A loaded speech-to-text engine that can transcribe a single block of PCM.
/// Both Parakeet and (optionally) Whisper implement this so the chunked
/// streaming machinery is engine-agnostic.
pub trait ChunkTranscriber: Send + Sync {
    /// `language` and `initial_prompt` are honoured by Whisper; Parakeet
    /// ignores them.
    fn transcribe_pcm(
        &self,
        samples: &[i16],
        sample_rate: u32,
        language: &str,
        initial_prompt: Option<&str>,
    ) -> Result<String, String>;
}

#[derive(Default)]
pub struct TranscriptionService {
    active: ActiveEngine,
    session: Option<SessionHandle>,
}

pub struct TranscriptionSessionStart {
    pub audio_tx: Option<SyncSender<AudioFrame>>,
    pub cancel_handle: TranscriptionCancelHandle,
}

pub type TranscriptionPreviewSender = mpsc::Sender<TranscriptPreview>;

#[derive(Clone, Debug)]
pub struct TranscriptPreview {
    pub index: usize,
    pub text: String,
    pub final_preview: bool,
}

#[derive(Clone)]
pub struct TranscriptionCancelHandle {
    sender: mpsc::Sender<SessionControl>,
}

struct SessionHandle {
    audio_tx: SyncSender<AudioFrame>,
    control_tx: mpsc::Sender<SessionControl>,
    result_rx: Receiver<Result<String, String>>,
    worker: Option<JoinHandle<()>>,
}

enum SessionControl {
    Finish,
    Cancel,
}

/// Holds the currently loaded engine and the identity it was loaded for, so a
/// repeated request with the same model/GPU setting reuses it.
#[derive(Default)]
struct ActiveEngine {
    active_model: Option<SttModel>,
    active_use_gpu: Option<bool>,
    transcriber: Option<Arc<dyn ChunkTranscriber>>,
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

        self.active
            .ensure(settings.model, models, settings.use_gpu)?;
        Ok(None)
    }

    pub fn start_session_with_cancel(
        &mut self,
        settings: &Settings,
        models: &ModelService,
        preview_tx: Option<TranscriptionPreviewSender>,
    ) -> Result<TranscriptionSessionStart, String> {
        if self.session.is_some() {
            return Err("Transcription session already in progress".to_string());
        }

        self.active
            .ensure(settings.model, models, settings.use_gpu)?;
        let handle = self.active.start_chunked_session(settings, preview_tx)?;
        let audio_tx = handle.audio_tx.clone();
        let cancel_handle = handle.cancel_handle();
        self.session = Some(handle);
        Ok(TranscriptionSessionStart {
            audio_tx: Some(audio_tx),
            cancel_handle,
        })
    }

    /// Loads (or reuses) the engine for the given settings without starting a
    /// session, so a host worker can hold the model warm before the first job
    /// arrives.
    pub fn preload(&mut self, settings: &Settings, models: &ModelService) -> Result<(), String> {
        self.active.ensure(settings.model, models, settings.use_gpu)
    }

    pub fn finish_session(
        &mut self,
        recording: &Recording,
        settings: &Settings,
        models: &ModelService,
    ) -> Result<String, String> {
        if let Some(handle) = self.session.take() {
            match handle.finish() {
                Ok(transcript) => return Ok(transcript),
                Err(err) if err == NO_STREAMED_AUDIO => {}
                Err(err) => return Err(err),
            }
        }

        self.active
            .ensure(settings.model, models, settings.use_gpu)?;
        self.active.transcribe_recording(recording, settings)
    }

    pub fn cancel_session(&mut self) {
        if let Some(handle) = self.session.take() {
            handle.cancel();
        }
    }

    pub fn unload(&mut self) {
        self.cancel_session();
        self.active = ActiveEngine::default();
    }
}

impl TranscriptionCancelHandle {
    pub fn cancel(&self) {
        let _ = self.sender.send(SessionControl::Cancel);
    }
}

impl ActiveEngine {
    fn ensure(
        &mut self,
        model: SttModel,
        models: &ModelService,
        use_gpu: bool,
    ) -> Result<(), String> {
        if self.active_model == Some(model)
            && self.active_use_gpu == Some(use_gpu)
            && self.transcriber.is_some()
        {
            return Ok(());
        }

        if !models.files_present(model) {
            return Err(format!(
                "Model files are missing: {}",
                models.path_for(model).display()
            ));
        }

        let path = models.path_for(model);
        let transcriber: Arc<dyn ChunkTranscriber> = match model {
            SttModel::Parakeet => Arc::new(ParakeetTranscriber::load(&path)?),
            #[cfg(feature = "whisper")]
            SttModel::Whisper(_) => Arc::new(WhisperTranscriber::load(&path, use_gpu)?),
        };

        self.transcriber = Some(transcriber);
        self.active_model = Some(model);
        self.active_use_gpu = Some(use_gpu);
        Ok(())
    }

    fn start_chunked_session(
        &self,
        settings: &Settings,
        preview_tx: Option<TranscriptionPreviewSender>,
    ) -> Result<SessionHandle, String> {
        let transcriber = self
            .transcriber
            .clone()
            .ok_or_else(|| "Transcription engine is unavailable".to_string())?;
        let initial_prompt = if settings.model.is_whisper() {
            settings.whisper_initial_prompt()
        } else {
            None
        };
        SessionHandle::start(
            transcriber,
            settings.language.clone(),
            initial_prompt,
            settings.whisper_chunk_seconds,
            preview_tx,
        )
    }

    fn transcribe_recording(
        &self,
        recording: &Recording,
        settings: &Settings,
    ) -> Result<String, String> {
        if recording.pcm_i16.is_empty() {
            return Err("No audio samples were captured".to_string());
        }
        let transcriber = self
            .transcriber
            .as_ref()
            .ok_or_else(|| "Transcription engine is unavailable".to_string())?;
        let initial_prompt = if settings.model.is_whisper() {
            settings.whisper_initial_prompt()
        } else {
            None
        };
        transcriber.transcribe_pcm(
            &recording.pcm_i16,
            recording.sample_rate,
            &settings.language,
            initial_prompt.as_deref(),
        )
    }
}

/// The default (and only, in a default build) engine: NVIDIA Parakeet TDT run
/// in-process through `parakeet-rs`. The model is wrapped in a `Mutex` because
/// transcription holds an exclusive ONNX session and the chunk worker calls it
/// from a dedicated thread.
struct ParakeetTranscriber {
    model: Mutex<ParakeetTDT>,
}

impl ParakeetTranscriber {
    fn load(model_dir: &Path) -> Result<Self, String> {
        // `None` execution config defaults to CPU inference.
        let model = ParakeetTDT::from_pretrained(model_dir, None)
            .map_err(|err| format!("Failed to load Parakeet model: {err}"))?;
        Ok(Self {
            model: Mutex::new(model),
        })
    }
}

impl ChunkTranscriber for ParakeetTranscriber {
    fn transcribe_pcm(
        &self,
        samples: &[i16],
        sample_rate: u32,
        _language: &str,
        _initial_prompt: Option<&str>,
    ) -> Result<String, String> {
        if samples.is_empty() {
            return Err("No audio samples were captured".to_string());
        }
        if !contains_probable_speech(samples, sample_rate) {
            return Err("No speech was transcribed".to_string());
        }

        // Parakeet expects 16 kHz mono f32; resample to match before handing
        // it the block. No timestamps are requested.
        let audio = resample_i16_to_16khz_f32(samples, sample_rate);
        let mut model = self
            .model
            .lock()
            .map_err(|_| "Parakeet model lock was poisoned".to_string())?;
        let result = model
            .transcribe_samples(audio, 16_000, 1, None)
            .map_err(|err| format!("Parakeet transcription failed: {err}"))?;
        let transcript = result.text.trim().to_string();
        if transcript.is_empty() {
            return Err("No speech was transcribed".to_string());
        }
        Ok(transcript)
    }
}

#[cfg(feature = "whisper")]
struct WhisperTranscriber {
    context: Arc<WhisperContext>,
}

#[cfg(feature = "whisper")]
impl WhisperTranscriber {
    fn load(model_path: &Path, use_gpu: bool) -> Result<Self, String> {
        let mut params = WhisperContextParameters::default();
        params.use_gpu(use_gpu);
        // Flash attention is the fast path for GPU decode but is only worth
        // forcing on builds that compile a GPU backend; CPU inference keeps
        // the standard attention kernels.
        if use_gpu
            && cfg!(any(
                target_os = "macos",
                feature = "cuda",
                feature = "vulkan"
            ))
        {
            params.flash_attn(true);
        }
        let path = path_to_string(model_path)?;
        let context = WhisperContext::new_with_params(&path, params)
            .map_err(|err| format!("Failed to load Whisper model: {err}"))?;
        Ok(Self {
            context: Arc::new(context),
        })
    }
}

#[cfg(feature = "whisper")]
impl ChunkTranscriber for WhisperTranscriber {
    fn transcribe_pcm(
        &self,
        samples: &[i16],
        sample_rate: u32,
        language: &str,
        initial_prompt: Option<&str>,
    ) -> Result<String, String> {
        transcribe_whisper_pcm(
            Arc::clone(&self.context),
            samples,
            sample_rate,
            language,
            initial_prompt,
        )
    }
}

impl SessionHandle {
    fn start(
        transcriber: Arc<dyn ChunkTranscriber>,
        language: String,
        initial_prompt: Option<String>,
        chunk_seconds: u16,
        preview_tx: Option<TranscriptionPreviewSender>,
    ) -> Result<Self, String> {
        let (audio_tx, audio_rx) = mpsc::sync_channel::<AudioFrame>(STREAM_CHANNEL_DEPTH);
        let (control_tx, control_rx) = mpsc::channel::<SessionControl>();
        let (result_tx, result_rx) = mpsc::channel::<Result<String, String>>();
        let worker = thread::Builder::new()
            .name("chunked-transcription".to_string())
            .spawn(move || {
                let result = run_chunked_session(
                    transcriber,
                    language,
                    initial_prompt,
                    chunk_seconds,
                    audio_rx,
                    control_rx,
                    preview_tx,
                );
                let _ = result_tx.send(result);
            })
            .map_err(|err| format!("Failed to start transcription worker: {err}"))?;

        Ok(Self {
            audio_tx,
            control_tx,
            result_rx,
            worker: Some(worker),
        })
    }

    fn finish(mut self) -> Result<String, String> {
        let _ = self.control_tx.send(SessionControl::Finish);
        drop(self.audio_tx);
        let result = self
            .result_rx
            .recv()
            .map_err(|_| "Transcription worker stopped without a transcript".to_string())?;
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        result
    }

    fn cancel(mut self) {
        let _ = self.control_tx.send(SessionControl::Cancel);
        drop(self.audio_tx);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }

    fn cancel_handle(&self) -> TranscriptionCancelHandle {
        TranscriptionCancelHandle {
            sender: self.control_tx.clone(),
        }
    }
}

struct WhisperChunkJob {
    index: usize,
    pcm_i16: Vec<i16>,
    sample_rate: u32,
}

struct ChunkResult {
    index: usize,
    text: String,
}

#[allow(clippy::too_many_arguments)]
fn run_chunked_session(
    transcriber: Arc<dyn ChunkTranscriber>,
    language: String,
    initial_prompt: Option<String>,
    chunk_seconds: u16,
    audio_rx: Receiver<AudioFrame>,
    control_rx: Receiver<SessionControl>,
    preview_tx: Option<TranscriptionPreviewSender>,
) -> Result<String, String> {
    let chunk_seconds = usize::from(chunk_seconds.clamp(5, 60));
    let (job_tx, job_rx) = mpsc::channel::<WhisperChunkJob>();
    let (chunk_result_tx, chunk_result_rx) = mpsc::channel::<Result<ChunkResult, String>>();
    let worker = thread::Builder::new()
        .name("transcription-chunk-worker".to_string())
        .spawn(move || {
            for job in job_rx {
                let result = transcriber
                    .transcribe_pcm(
                        &job.pcm_i16,
                        job.sample_rate,
                        &language,
                        initial_prompt.as_deref(),
                    )
                    .map(|text| ChunkResult {
                        index: job.index,
                        text,
                    })
                    .or_else(|err| {
                        if err == "No speech was transcribed" {
                            Ok(ChunkResult {
                                index: job.index,
                                text: String::new(),
                            })
                        } else {
                            Err(err)
                        }
                    });

                let should_stop = result.is_err();
                if chunk_result_tx.send(result).is_err() || should_stop {
                    break;
                }
            }
        })
        .map_err(|err| format!("Failed to start transcription chunk worker: {err}"))?;

    let mut buffer = Vec::new();
    let mut sample_rate = 0;
    let mut chunk_index = 0;
    let mut dispatched_chunks = 0;
    let mut received_audio = false;
    let mut chunks = Vec::new();

    loop {
        match control_rx.try_recv() {
            Ok(SessionControl::Finish) => break,
            Ok(SessionControl::Cancel) => {
                drop(job_tx);
                return Err("Transcription was cancelled".to_string());
            }
            Err(mpsc::TryRecvError::Disconnected) => break,
            Err(mpsc::TryRecvError::Empty) => {}
        }

        match audio_rx.recv_timeout(std::time::Duration::from_millis(20)) {
            Ok(frame) => {
                if frame.pcm_i16.is_empty() {
                    continue;
                }
                received_audio = true;
                sample_rate = frame.sample_rate;
                buffer.extend_from_slice(&frame.pcm_i16);
                dispatch_ready_whisper_chunks(
                    &mut buffer,
                    sample_rate,
                    chunk_seconds,
                    &job_tx,
                    &mut chunk_index,
                    &mut dispatched_chunks,
                )?;
                collect_available_chunk_results(&chunk_result_rx, &mut chunks, &preview_tx)?;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
        collect_available_chunk_results(&chunk_result_rx, &mut chunks, &preview_tx)?;
    }

    for frame in audio_rx.try_iter() {
        if frame.pcm_i16.is_empty() {
            continue;
        }
        received_audio = true;
        sample_rate = frame.sample_rate;
        buffer.extend_from_slice(&frame.pcm_i16);
        dispatch_ready_whisper_chunks(
            &mut buffer,
            sample_rate,
            chunk_seconds,
            &job_tx,
            &mut chunk_index,
            &mut dispatched_chunks,
        )?;
        collect_available_chunk_results(&chunk_result_rx, &mut chunks, &preview_tx)?;
    }

    if !received_audio {
        drop(job_tx);
        let _ = worker.join();
        return Err(NO_STREAMED_AUDIO.to_string());
    }

    dispatch_final_whisper_chunk(
        &mut buffer,
        sample_rate,
        &job_tx,
        &mut chunk_index,
        dispatched_chunks,
    )?;
    drop(job_tx);
    let _ = worker.join();

    for result in chunk_result_rx {
        chunks.push(result?);
        emit_chunk_preview(&chunks, &preview_tx, false);
    }

    chunks.sort_by_key(|chunk| chunk.index);
    let transcript = merge_chunk_results(&chunks);
    if transcript.is_empty() {
        return Err("No speech was transcribed".to_string());
    }
    emit_preview(&preview_tx, chunk_index, transcript.clone(), true);
    Ok(transcript)
}

fn collect_available_chunk_results(
    chunk_result_rx: &Receiver<Result<ChunkResult, String>>,
    chunks: &mut Vec<ChunkResult>,
    preview_tx: &Option<TranscriptionPreviewSender>,
) -> Result<(), String> {
    loop {
        match chunk_result_rx.try_recv() {
            Ok(result) => {
                chunks.push(result?);
                emit_chunk_preview(chunks, preview_tx, false);
            }
            Err(mpsc::TryRecvError::Empty) => return Ok(()),
            Err(mpsc::TryRecvError::Disconnected) => return Ok(()),
        }
    }
}

fn emit_chunk_preview(
    chunks: &[ChunkResult],
    preview_tx: &Option<TranscriptionPreviewSender>,
    final_preview: bool,
) {
    let mut sorted = chunks
        .iter()
        .map(|chunk| ChunkResult {
            index: chunk.index,
            text: chunk.text.clone(),
        })
        .collect::<Vec<_>>();
    sorted.sort_by_key(|chunk| chunk.index);
    let text = merge_chunk_results(&sorted);
    if !text.is_empty() {
        let index = sorted.last().map(|chunk| chunk.index).unwrap_or_default();
        emit_preview(preview_tx, index, text, final_preview);
    }
}

fn emit_preview(
    preview_tx: &Option<TranscriptionPreviewSender>,
    index: usize,
    text: String,
    final_preview: bool,
) {
    if let Some(preview_tx) = preview_tx {
        let _ = preview_tx.send(TranscriptPreview {
            index,
            text,
            final_preview,
        });
    }
}

fn dispatch_ready_whisper_chunks(
    buffer: &mut Vec<i16>,
    sample_rate: u32,
    chunk_seconds: usize,
    job_tx: &mpsc::Sender<WhisperChunkJob>,
    chunk_index: &mut usize,
    dispatched_chunks: &mut usize,
) -> Result<(), String> {
    let chunk_samples = sample_rate as usize * chunk_seconds;
    let overlap_samples = sample_rate as usize * CHUNK_OVERLAP_SECONDS;
    if chunk_samples == 0 {
        return Ok(());
    }

    while buffer.len() >= chunk_samples {
        let chunk = &buffer[..chunk_samples];
        if contains_probable_speech(chunk, sample_rate) {
            job_tx
                .send(WhisperChunkJob {
                    index: *chunk_index,
                    pcm_i16: chunk.to_vec(),
                    sample_rate,
                })
                .map_err(|_| "Transcription chunk worker stopped while receiving audio".to_string())?;
            *dispatched_chunks += 1;
        }
        *chunk_index += 1;

        let drain_samples = chunk_samples
            .saturating_sub(overlap_samples)
            .max(1)
            .min(buffer.len());
        buffer.drain(..drain_samples);
    }

    Ok(())
}

fn dispatch_final_whisper_chunk(
    buffer: &mut Vec<i16>,
    sample_rate: u32,
    job_tx: &mpsc::Sender<WhisperChunkJob>,
    chunk_index: &mut usize,
    dispatched_chunks: usize,
) -> Result<(), String> {
    if buffer.is_empty() {
        return Ok(());
    }

    let overlap_samples = sample_rate as usize * CHUNK_OVERLAP_SECONDS;
    if dispatched_chunks > 0 && buffer.len() <= overlap_samples {
        return Ok(());
    }
    if !contains_probable_speech(buffer, sample_rate) {
        buffer.clear();
        return Ok(());
    }

    job_tx
        .send(WhisperChunkJob {
            index: *chunk_index,
            pcm_i16: std::mem::take(buffer),
            sample_rate,
        })
        .map_err(|_| "Transcription chunk worker stopped while receiving final audio".to_string())?;
    *chunk_index += 1;
    Ok(())
}

#[cfg(feature = "whisper")]
fn transcribe_whisper_pcm(
    context: Arc<WhisperContext>,
    samples: &[i16],
    sample_rate: u32,
    language: &str,
    initial_prompt: Option<&str>,
) -> Result<String, String> {
    if samples.is_empty() {
        return Err("No audio samples were captured".to_string());
    }
    if !contains_probable_speech(samples, sample_rate) {
        return Err("No speech was transcribed".to_string());
    }

    let mut state = context
        .create_state()
        .map_err(|err| format!("Failed to create Whisper state: {err}"))?;
    let audio = resample_i16_to_16khz_f32(samples, sample_rate);
    let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 0 });
    params.set_n_threads(default_thread_count());
    params.set_print_special(false);
    params.set_print_progress(false);
    params.set_print_realtime(false);
    params.set_print_timestamps(false);
    params.set_no_context(true);
    params.set_suppress_nst(false);
    if let Some(initial_prompt) = initial_prompt.filter(|prompt| !prompt.trim().is_empty()) {
        params.set_initial_prompt(initial_prompt);
    }

    if language == "auto" {
        params.set_language(None);
        params.set_detect_language(true);
    } else {
        params.set_language(Some(language));
    }

    state
        .full(params, &audio)
        .map_err(|err| format!("Whisper transcription failed: {err}"))?;

    let transcript = state
        .as_iter()
        .filter_map(|segment| accepted_segment_text(&segment))
        .collect::<Vec<_>>()
        .join(" ")
        .trim()
        .to_string();

    if transcript.is_empty() {
        return Err("No speech was transcribed".to_string());
    }

    Ok(transcript)
}

#[cfg(feature = "whisper")]
fn accepted_segment_text(segment: &WhisperSegment<'_>) -> Option<String> {
    let text = segment.to_string().trim().to_string();
    if text.is_empty() || is_non_speech_annotation(&text) {
        return None;
    }

    let no_speech_probability = segment.no_speech_probability();
    if no_speech_probability.is_finite() && no_speech_probability >= NO_SPEECH_PROBABILITY_THRESHOLD
    {
        return None;
    }

    if let Some(avg_probability) = average_segment_token_probability(segment) {
        if avg_probability < MIN_AVG_TOKEN_PROBABILITY {
            return None;
        }
    }

    Some(text)
}

#[cfg(feature = "whisper")]
fn average_segment_token_probability(segment: &WhisperSegment<'_>) -> Option<f32> {
    let mut sum = 0.0;
    let mut count = 0;
    for index in 0..segment.n_tokens() {
        let Some(token) = segment.get_token(index) else {
            continue;
        };
        let probability = token.token_probability();
        if probability.is_finite() {
            sum += probability;
            count += 1;
        }
    }

    (count > 0).then_some(sum / count as f32)
}

#[cfg_attr(not(feature = "whisper"), allow(dead_code))]
fn is_non_speech_annotation(text: &str) -> bool {
    let trimmed = text.trim();
    let bracketed = (trimmed.starts_with('[') && trimmed.ends_with(']'))
        || (trimmed.starts_with('(') && trimmed.ends_with(')'));
    if !bracketed {
        return false;
    }

    let normalized = trimmed
        .trim_matches(|ch: char| !ch.is_alphanumeric() && !ch.is_whitespace())
        .to_ascii_lowercase();
    [
        "audio",
        "blank",
        "inaudible",
        "applause",
        "laugh",
        "laughter",
        "music",
        "noise",
        "silence",
        "sound",
        "speaking",
        "speech",
    ]
    .iter()
    .any(|needle| normalized.contains(needle))
}

fn merge_chunk_results(chunks: &[ChunkResult]) -> String {
    let mut transcript = String::new();
    for chunk in chunks {
        let text = chunk.text.trim();
        if text.is_empty() {
            continue;
        }
        transcript = merge_transcript_text(&transcript, text);
    }
    transcript
}

fn merge_transcript_text(previous: &str, next: &str) -> String {
    let previous = previous.trim();
    let next = next.trim();
    if previous.is_empty() {
        return next.to_string();
    }
    if next.is_empty() {
        return previous.to_string();
    }

    let previous_words = previous.split_whitespace().collect::<Vec<_>>();
    let next_words = next.split_whitespace().collect::<Vec<_>>();
    let max_overlap = previous_words.len().min(next_words.len()).min(20);

    for overlap in (1..=max_overlap).rev() {
        if transcript_overlap_matches(
            &previous_words[previous_words.len() - overlap..],
            &next_words[..overlap],
        ) {
            let remainder = next_words[overlap..].join(" ");
            if remainder.is_empty() {
                return previous.to_string();
            }
            return format!("{previous} {remainder}");
        }
    }

    format!("{previous} {next}")
}

fn transcript_overlap_matches(previous_tail: &[&str], next_head: &[&str]) -> bool {
    let overlap = previous_tail.len();
    if overlap != next_head.len() || overlap == 0 {
        return false;
    }

    let allowed_mismatches = usize::from(overlap >= 4);
    let mut mismatches = 0;
    for (previous, next) in previous_tail.iter().zip(next_head.iter()) {
        let previous = normalize_transcript_word(previous);
        let next = normalize_transcript_word(next);
        if previous.is_empty() || next.is_empty() || !transcript_words_similar(&previous, &next) {
            mismatches += 1;
            if mismatches > allowed_mismatches {
                return false;
            }
        }
    }

    true
}

fn transcript_words_similar(left: &str, right: &str) -> bool {
    left == right || (left.len().min(right.len()) >= 4 && edit_distance_at_most_one(left, right))
}

fn edit_distance_at_most_one(left: &str, right: &str) -> bool {
    let left_chars = left.chars().collect::<Vec<_>>();
    let right_chars = right.chars().collect::<Vec<_>>();
    if left_chars.len().abs_diff(right_chars.len()) > 1 {
        return false;
    }

    if left_chars.len() == right_chars.len() {
        return left_chars
            .iter()
            .zip(right_chars.iter())
            .filter(|(left, right)| left != right)
            .count()
            <= 1;
    }

    let (shorter, longer) = if left_chars.len() < right_chars.len() {
        (&left_chars, &right_chars)
    } else {
        (&right_chars, &left_chars)
    };
    let mut short_index = 0;
    let mut long_index = 0;
    let mut edits = 0;
    while short_index < shorter.len() && long_index < longer.len() {
        if shorter[short_index] == longer[long_index] {
            short_index += 1;
            long_index += 1;
        } else {
            edits += 1;
            if edits > 1 {
                return false;
            }
            long_index += 1;
        }
    }

    true
}

fn normalize_transcript_word(word: &str) -> String {
    word.trim_matches(|ch: char| !ch.is_alphanumeric())
        .to_lowercase()
}

fn contains_probable_speech(samples: &[i16], sample_rate: u32) -> bool {
    let stats = audio_activity_stats(samples, sample_rate);
    if stats.peak < SPEECH_PEAK_THRESHOLD {
        return false;
    }

    stats.rms >= SPEECH_RMS_THRESHOLD || stats.active_speech_ms >= MIN_ACTIVE_SPEECH_MS
}

#[derive(Clone, Copy, Debug)]
struct AudioActivityStats {
    peak: f32,
    rms: f32,
    active_speech_ms: usize,
}

fn audio_activity_stats(samples: &[i16], sample_rate: u32) -> AudioActivityStats {
    let (peak, rms) = peak_and_rms(samples);
    let window_samples = (sample_rate as usize * SPEECH_WINDOW_MS / 1000).max(1);
    let mut active_windows = 0;
    for window in samples.chunks(window_samples) {
        let (window_peak, window_rms) = peak_and_rms(window);
        if window_peak >= SPEECH_WINDOW_PEAK_THRESHOLD || window_rms >= SPEECH_WINDOW_RMS_THRESHOLD {
            active_windows += 1;
        }
    }

    AudioActivityStats {
        peak,
        rms,
        active_speech_ms: active_windows * SPEECH_WINDOW_MS,
    }
}

fn peak_and_rms(samples: &[i16]) -> (f32, f32) {
    if samples.is_empty() {
        return (0.0, 0.0);
    }

    let mut peak = 0.0_f32;
    let mut sum_sq = 0.0_f64;
    for sample in samples {
        let normalized = f32::from(*sample) / 32768.0;
        let abs = normalized.abs();
        peak = peak.max(abs);
        sum_sq += f64::from(normalized * normalized);
    }

    (peak, (sum_sq / samples.len() as f64).sqrt() as f32)
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

#[cfg(feature = "whisper")]
fn default_thread_count() -> i32 {
    std::thread::available_parallelism()
        .map(|threads| threads.get().min(4) as i32)
        .unwrap_or(1)
}

#[cfg(feature = "whisper")]
fn path_to_string(path: &Path) -> Result<String, String> {
    let absolute: std::path::PathBuf = path
        .canonicalize()
        .map_err(|err| format!("Failed to resolve model path: {err}"))?;
    absolute
        .to_str()
        .map(ToOwned::to_owned)
        .ok_or_else(|| "Model path is not valid UTF-8".to_string())
}

#[cfg(test)]
mod tests {
    use super::{
        contains_probable_speech, dispatch_final_whisper_chunk, dispatch_ready_whisper_chunks,
        is_non_speech_annotation, merge_transcript_text, resample_i16_to_16khz_f32, WhisperChunkJob,
        CHUNK_OVERLAP_SECONDS,
    };
    use std::sync::mpsc;

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

    #[test]
    fn dispatches_ready_whisper_chunks_with_overlap_retained() {
        let sample_rate = 10;
        let chunk_seconds = 20;
        let chunk_samples = sample_rate as usize * chunk_seconds;
        let overlap_samples = sample_rate as usize * CHUNK_OVERLAP_SECONDS;
        let mut buffer = vec![2_000_i16; chunk_samples];
        let (tx, rx) = mpsc::channel::<WhisperChunkJob>();
        let mut chunk_index = 0;
        let mut dispatched_chunks = 0;

        dispatch_ready_whisper_chunks(
            &mut buffer,
            sample_rate,
            chunk_seconds,
            &tx,
            &mut chunk_index,
            &mut dispatched_chunks,
        )
        .expect("dispatch ready chunk");

        let job = rx.try_recv().expect("chunk job");
        assert_eq!(job.index, 0);
        assert_eq!(job.sample_rate, sample_rate);
        assert_eq!(job.pcm_i16.len(), chunk_samples);
        assert_eq!(buffer.len(), overlap_samples);
        assert_eq!(chunk_index, 1);
        assert_eq!(dispatched_chunks, 1);
    }

    #[test]
    fn skips_ready_whisper_chunks_without_probable_speech() {
        let sample_rate = 10;
        let chunk_seconds = 20;
        let chunk_samples = sample_rate as usize * chunk_seconds;
        let overlap_samples = sample_rate as usize * CHUNK_OVERLAP_SECONDS;
        let mut buffer = vec![1_i16; chunk_samples];
        let (tx, rx) = mpsc::channel::<WhisperChunkJob>();
        let mut chunk_index = 0;
        let mut dispatched_chunks = 0;

        dispatch_ready_whisper_chunks(
            &mut buffer,
            sample_rate,
            chunk_seconds,
            &tx,
            &mut chunk_index,
            &mut dispatched_chunks,
        )
        .expect("dispatch ready chunk");

        assert!(rx.try_recv().is_err());
        assert_eq!(buffer.len(), overlap_samples);
        assert_eq!(chunk_index, 1);
        assert_eq!(dispatched_chunks, 0);
    }

    #[test]
    fn skips_final_chunk_when_only_overlap_remains() {
        let sample_rate = 10;
        let overlap_samples = sample_rate as usize * CHUNK_OVERLAP_SECONDS;
        let mut buffer = vec![1; overlap_samples];
        let (tx, rx) = mpsc::channel::<WhisperChunkJob>();
        let mut chunk_index = 1;

        dispatch_final_whisper_chunk(&mut buffer, sample_rate, &tx, &mut chunk_index, 1)
            .expect("dispatch final chunk");

        assert!(rx.try_recv().is_err());
        assert_eq!(buffer.len(), overlap_samples);
        assert_eq!(chunk_index, 1);
    }

    #[test]
    fn dispatches_final_chunk_when_new_audio_follows_overlap() {
        let sample_rate = 10;
        let overlap_samples = sample_rate as usize * CHUNK_OVERLAP_SECONDS;
        let mut buffer = vec![2_000_i16; overlap_samples + 5];
        let (tx, rx) = mpsc::channel::<WhisperChunkJob>();
        let mut chunk_index = 1;

        dispatch_final_whisper_chunk(&mut buffer, sample_rate, &tx, &mut chunk_index, 1)
            .expect("dispatch final chunk");

        let job = rx.try_recv().expect("final chunk job");
        assert_eq!(job.index, 1);
        assert_eq!(job.pcm_i16.len(), overlap_samples + 5);
        assert!(buffer.is_empty());
        assert_eq!(chunk_index, 2);
    }

    #[test]
    fn skips_final_chunk_without_probable_speech() {
        let sample_rate = 10;
        let overlap_samples = sample_rate as usize * CHUNK_OVERLAP_SECONDS;
        let mut buffer = vec![1_i16; overlap_samples + 5];
        let (tx, rx) = mpsc::channel::<WhisperChunkJob>();
        let mut chunk_index = 1;

        dispatch_final_whisper_chunk(&mut buffer, sample_rate, &tx, &mut chunk_index, 1)
            .expect("dispatch final chunk");

        assert!(rx.try_recv().is_err());
        assert!(buffer.is_empty());
        assert_eq!(chunk_index, 1);
    }

    #[test]
    fn merge_transcript_text_deduplicates_overlap_words() {
        let merged = merge_transcript_text(
            "we should ship this carefully",
            "ship this carefully and measure latency",
        );

        assert_eq!(merged, "we should ship this carefully and measure latency");
    }

    #[test]
    fn merge_transcript_text_ignores_overlap_punctuation_and_case() {
        let merged = merge_transcript_text("Hello, world.", "world again");

        assert_eq!(merged, "Hello, world. again");
    }

    #[test]
    fn merge_transcript_text_tolerates_one_overlap_mishearing() {
        let merged = merge_transcript_text(
            "it repeats the last three or four words",
            "last three or four word when chunking fails",
        );

        assert_eq!(
            merged,
            "it repeats the last three or four words when chunking fails"
        );
    }

    #[test]
    fn merge_transcript_text_does_not_fuzzy_merge_short_overlap() {
        let merged = merge_transcript_text("open mail", "male settings");

        assert_eq!(merged, "open mail male settings");
    }

    #[test]
    fn detects_probable_speech_from_active_windows() {
        let mut samples = vec![0_i16; 48_000];
        for sample in samples.iter_mut().take(8_000).skip(4_000) {
            *sample = 2_000;
        }

        assert!(contains_probable_speech(&samples, 48_000));
    }

    #[test]
    fn rejects_low_level_audio_as_probable_speech() {
        let samples = vec![30_i16; 48_000];

        assert!(!contains_probable_speech(&samples, 48_000));
    }

    #[test]
    fn identifies_common_non_speech_annotations() {
        assert!(is_non_speech_annotation("[BLANK_AUDIO]"));
        assert!(is_non_speech_annotation("(background noise)"));
        assert!(is_non_speech_annotation("[applause]"));
        assert!(is_non_speech_annotation("(laughter)"));
        assert!(!is_non_speech_annotation("thanks for watching"));
    }
}
