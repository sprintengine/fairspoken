use crate::audio::{AudioFrame, Recording};
use crate::ensemble::{EnergyProfile, Hypothesis, Word};
use crate::models::{ModelService, SttModel};
use crate::settings::Settings;
use crate::super_mode::{SuperModeEngines, SuperModeOutcome};
use parakeet_rs::{ParakeetTDT, Transcriber};
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::{self, JoinHandle};

#[cfg(feature = "whisper")]
use whisper_rs::{
    FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters, WhisperSegment,
};

const STREAM_CHANNEL_DEPTH: usize = 64;
const CHUNK_OVERLAP_SECONDS: usize = 1;
/// Milliseconds the speech model has spent computing in the current session.
/// One dictation runs at a time, so a process-wide counter reset at session
/// start is enough for the performance readout.
static SPEECH_MODEL_BUSY_MS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Speech-model compute time of the session in progress or just finished.
pub fn speech_model_busy_ms() -> u64 {
    SPEECH_MODEL_BUSY_MS.load(std::sync::atomic::Ordering::Relaxed)
}

/// Longest chunk of uninterrupted speech while a live preview is showing.
const PREVIEW_MAX_CHUNK_SECONDS: u16 = 5;
/// Longest chunk of uninterrupted speech when nobody is watching a preview.
const MAX_CHUNK_SECONDS: u16 = 15;
/// Trailing silence that ends an utterance and cuts a chunk at the gap.
const GAP_SILENCE_MS: usize = 500;
/// Do not gap-cut before this much audio has accumulated, so brief pauses in
/// the first words do not produce confetti chunks.
const MIN_GAP_CHUNK_SECONDS: usize = 2;
/// Backpressure: while this many chunk jobs are still transcribing, defer
/// further cuts and let the buffer coalesce into fewer, larger chunks instead
/// of queueing unbounded PCM copies behind a slow engine.
const MAX_PENDING_CHUNK_JOBS: usize = 2;
/// Live polish rewrites at most this many non-empty ASR chunks; older text is
/// frozen so model cost stays O(window) rather than O(dictation).
const POLISH_WINDOW_CHUNKS: usize = 3;
const SPEECH_PEAK_THRESHOLD: f32 = 0.010;
const SPEECH_RMS_THRESHOLD: f32 = 0.0015;
const SPEECH_WINDOW_MS: usize = 30;
const SPEECH_WINDOW_PEAK_THRESHOLD: f32 = 0.020;
const SPEECH_WINDOW_RMS_THRESHOLD: f32 = 0.004;
const MIN_ACTIVE_SPEECH_MS: usize = 120;
/// What speech models decode from a breath, a key click or room noise once the
/// speaker has stopped: the closing lines of their training data.
const PHANTOM_PHRASES: &[&str] = &[
    "thank you",
    "thank you very much",
    "thank you so much",
    "thanks for watching",
    "thank you for watching",
    "bye",
    "bye bye",
    "you",
    "mm hmm",
    "mhm",
    "uh huh",
    "hmm",
];
/// Saying the shortest of those aloud takes about this much voiced audio. A
/// chunk with less that still "says" one did not hear it.
const PHANTOM_MIN_SPEECH_MS: usize = 450;
#[cfg(feature = "whisper")]
const NO_SPEECH_PROBABILITY_THRESHOLD: f32 = 0.60;
#[cfg(feature = "whisper")]
const MIN_AVG_TOKEN_PROBABILITY: f32 = 0.12;

/// How long super mode may stay shed (on battery) before its Whisper engine,
/// 0.5–3 GB, is freed. Plugging back in soon after keeps it warm.
const SHED_UNLOAD_AFTER: std::time::Duration = std::time::Duration::from_secs(10 * 60);

/// Sentinel returned by the chunking worker when no audio was ever streamed, so
/// the caller can fall back to a direct (whole-recording) transcription.
const NO_STREAMED_AUDIO: &str = "No audio frames were streamed to the transcriber";
/// What an engine answers for a chunk with nothing to transcribe.
pub(crate) const NO_SPEECH: &str = "No speech was transcribed";

/// A loaded speech-to-text engine that can transcribe a single block of PCM.
/// Both Parakeet and (optionally) Whisper implement this so the chunked
/// streaming machinery is engine-agnostic.
pub trait ChunkTranscriber: Send + Sync {
    fn ready(&self) -> Result<(), String> {
        Ok(())
    }
    fn load_failed(&self) -> bool {
        false
    }
    /// `language` and `initial_prompt` are honoured by Whisper; Parakeet
    /// ignores them.
    fn transcribe_pcm(
        &self,
        samples: &[i16],
        sample_rate: u32,
        language: &str,
        initial_prompt: Option<&str>,
    ) -> Result<String, String>;
    /// The same transcription word by word, with whatever confidences and
    /// timings the engine reports; super mode merges two of these. `abort`
    /// lets a caller that stopped waiting cut a long decode short.
    fn transcribe_detailed(
        &self,
        samples: &[i16],
        sample_rate: u32,
        language: &str,
        initial_prompt: Option<&str>,
        _abort: Option<Arc<AtomicBool>>,
    ) -> Result<Hypothesis, String> {
        self.transcribe_pcm(samples, sample_rate, language, initial_prompt)
            .map(|text| Hypothesis::from_text(&text))
    }
    /// The user has released the key: the chunks still to come are on the
    /// critical path to the text appearing.
    fn on_release(&self) {}
}

#[derive(Default)]
pub struct TranscriptionService {
    active: ActiveEngine,
    session: Option<SessionHandle>,
    /// Super mode's second engine when this app runs it locally.
    secondary: ActiveEngine,
    /// Since when super mode has been shed (battery) with the second engine
    /// still loaded; it is freed after `SHED_UNLOAD_AFTER`.
    secondary_shed_since: Option<std::time::Instant>,
    /// Engines a host worker lent the next session (`arm_super_mode`).
    armed: Option<SuperModeEngines>,
    /// The super mode result of the session in progress, then of the last one.
    super_mode: Option<SuperModeOutcome>,
    super_tally: Option<Arc<crate::super_mode::SessionTally>>,
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
    /// Bytes of `text` that polish may freeze. The max of the last silence-gap
    /// prefix and everything except the last three non-empty chunks. Chunk
    /// merging only ever appends, so this prefix can no longer be revised.
    pub sealed_len: usize,
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

        self.super_mode = None;
        self.active
            .ensure(settings.model, models, settings.use_gpu)?;
        Ok(None)
    }

    pub fn start_session_traced(
        &mut self,
        settings: &Settings,
        models: &ModelService,
        preview_tx: Option<TranscriptionPreviewSender>,
        trace: Option<crate::note_debug::Trace>,
    ) -> Result<TranscriptionSessionStart, String> {
        if self.session.is_some() {
            return Err("Transcription session already in progress".to_string());
        }

        self.active
            .ensure(settings.model, models, settings.use_gpu)?;
        let primary = self
            .active
            .transcriber
            .clone()
            .ok_or_else(|| "Transcription engine is unavailable".to_string())?;
        let transcriber = match self.super_mode_engines(settings, models, &primary) {
            Some(engines) => self.start_ensemble(engines, settings, trace.clone())?,
            None => primary,
        };
        let handle = self
            .active
            .start_chunked_session(transcriber, settings, preview_tx, trace)?;
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
        self.active
            .ensure(settings.model, models, settings.use_gpu)?;
        self.active
            .transcriber
            .as_ref()
            .ok_or("Transcription engine is unavailable")?
            .ready()
    }

    pub fn finish_session(
        &mut self,
        recording: &Recording,
        settings: &Settings,
        models: &ModelService,
    ) -> Result<String, String> {
        // A saturated callback channel must never lose words in the final text.
        // The full capture buffer is authoritative when any stream frame dropped.
        if recording.dropped_stream_frames > 0 {
            self.cancel_session();
        }
        if let Some(handle) = self.session.take() {
            let result = handle.finish();
            self.close_super_tally();
            match result {
                Ok(transcript) => return Ok(transcript),
                Err(err) if err == NO_STREAMED_AUDIO => {}
                Err(err) => return Err(err),
            }
        }

        self.active
            .ensure(settings.model, models, settings.use_gpu)?;
        // A whole recording (a host batch upload) with engines lent for it.
        if let Some(engines) = self.armed.take() {
            let model = engines.secondary_model.clone();
            self.super_mode = Some(SuperModeOutcome::new(
                crate::super_mode::SuperModeStatus::Used,
                Some(model),
                None,
            ));
            let ensemble = self.start_ensemble(engines, settings, None)?;
            let result = ensemble.transcribe_pcm(
                &recording.pcm_i16,
                recording.sample_rate,
                &settings.language,
                None,
            );
            drop(ensemble);
            self.close_super_tally();
            return result;
        }
        self.active.transcribe_recording(recording, settings)
    }

    pub fn cancel_session(&mut self) {
        if let Some(handle) = self.session.take() {
            handle.cancel();
        }
        self.super_tally = None;
    }

    pub fn unload(&mut self) {
        self.cancel_session();
        self.active = ActiveEngine::default();
        self.drop_secondary();
        self.armed = None;
    }

    /// The loaded engine, for a host worker to lend to a super mode session
    /// running on another worker. Engines are `Send + Sync`.
    pub fn loaded_engine(&self) -> Option<Arc<dyn ChunkTranscriber>> {
        self.active
            .transcriber
            .clone()
            .filter(|engine| engine.ready().is_ok())
    }

    /// Runs the next session (or whole recording) in super mode on these
    /// engines instead of deciding from the settings.
    pub fn arm_super_mode(&mut self, engines: SuperModeEngines) {
        self.armed = Some(engines);
    }

    /// What super mode did in the last session; `None` when it was not
    /// considered at all. Also drops engines armed for a session that never
    /// started.
    pub fn take_super_mode_outcome(&mut self) -> Option<SuperModeOutcome> {
        self.armed = None;
        self.super_mode.take()
    }

    fn super_mode_engines(
        &mut self,
        settings: &Settings,
        models: &ModelService,
        primary: &Arc<dyn ChunkTranscriber>,
    ) -> Option<SuperModeEngines> {
        use crate::super_mode::{LocalDecision, SuperModeStatus};
        self.super_tally = None;
        if let Some(engines) = self.armed.take() {
            self.super_mode = Some(SuperModeOutcome::new(
                SuperModeStatus::Used,
                Some(engines.secondary_model.clone()),
                None,
            ));
            return Some(engines);
        }
        match crate::super_mode::local_decision(settings, models) {
            LocalDecision::Off => {
                self.super_mode = None;
                self.drop_secondary();
                None
            }
            LocalDecision::Skip(outcome) => {
                if outcome.status == SuperModeStatus::Shed {
                    let since = *self
                        .secondary_shed_since
                        .get_or_insert_with(std::time::Instant::now);
                    if since.elapsed() >= SHED_UNLOAD_AFTER {
                        self.secondary = ActiveEngine::default();
                    }
                } else {
                    self.drop_secondary();
                }
                self.super_mode = Some(outcome);
                None
            }
            LocalDecision::Run(model) => {
                self.secondary_shed_since = None;
                let id = model.model_id().to_string();
                if let Err(err) = self.secondary.ensure(model, models, settings.use_gpu) {
                    self.drop_secondary();
                    self.super_mode = Some(SuperModeOutcome::new(
                        SuperModeStatus::Unavailable,
                        Some(id),
                        Some(&err),
                    ));
                    return None;
                }
                let secondary = self.secondary.transcriber.clone()?;
                self.super_mode = Some(SuperModeOutcome::new(SuperModeStatus::Used, Some(id.clone()), None));
                Some(SuperModeEngines {
                    primary: Arc::clone(primary),
                    secondary,
                    secondary_model: id,
                })
            }
        }
    }

    /// Frees super mode's Whisper engine while super mode cannot run.
    fn drop_secondary(&mut self) {
        self.secondary = ActiveEngine::default();
        self.secondary_shed_since = None;
    }

    fn start_ensemble(
        &mut self,
        engines: SuperModeEngines,
        settings: &Settings,
        trace: Option<crate::note_debug::Trace>,
    ) -> Result<Arc<dyn ChunkTranscriber>, String> {
        let tally = Arc::new(crate::super_mode::SessionTally::default());
        self.super_tally = Some(Arc::clone(&tally));
        let ensemble = crate::super_mode::EnsembleTranscriber::start(engines, settings, tally, trace)?;
        Ok(Arc::new(ensemble))
    }

    fn close_super_tally(&mut self) {
        if let (Some(tally), Some(outcome)) = (self.super_tally.take(), self.super_mode.as_mut()) {
            outcome.absorb(&tally);
        }
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
            && self
                .transcriber
                .as_ref()
                .map(|t| !t.load_failed())
                .unwrap_or(false)
        {
            return Ok(());
        }

        if !models.files_present(model) {
            return Err(format!(
                "Model files are missing: {}",
                models.path_for(model).display()
            ));
        }

        let transcriber = Arc::new(LazyTranscriber {
            model,
            path: models.path_for(model),
            use_gpu,
            loaded: OnceLock::new(),
        });
        // The chunk coordinator starts immediately and drains PCM while this
        // worker loads weights. Loading never occupies the Tauri/UI thread.
        let warm = transcriber.clone();
        thread::Builder::new()
            .name("speech-model-loader".into())
            .spawn(move || {
                let _ = warm.get();
            })
            .map_err(|e| format!("Could not start model loader: {e}"))?;
        self.transcriber = Some(transcriber);
        self.active_model = Some(model);
        self.active_use_gpu = Some(use_gpu);
        Ok(())
    }

    fn start_chunked_session(
        &self,
        transcriber: Arc<dyn ChunkTranscriber>,
        settings: &Settings,
        preview_tx: Option<TranscriptionPreviewSender>,
        trace: Option<crate::note_debug::Trace>,
    ) -> Result<SessionHandle, String> {
        let initial_prompt = if settings.model.is_whisper() {
            settings.whisper_initial_prompt()
        } else {
            None
        };
        // Chunks cut at silence gaps; these are only the forced-cut ceilings
        // for speech with no detectable pause. Interactive dictation needs a
        // preview during continuous speech too, so its ceiling is short. Where
        // text is *sealed* for polish is decided later, at sentence ends
        // (`polish_stream`), not by either of these.
        let chunk_seconds = if preview_tx.is_some() {
            PREVIEW_MAX_CHUNK_SECONDS
        } else {
            MAX_CHUNK_SECONDS
        };
        SessionHandle::start_traced(
            transcriber,
            settings.language.clone(),
            initial_prompt,
            chunk_seconds,
            preview_tx,
            trace,
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

/// One initialization shared by warmup and inference. The audio coordinator
/// never waits on this lock; it continues spooling PCM up to the capture limit.
struct LazyTranscriber {
    model: SttModel,
    path: std::path::PathBuf,
    #[cfg_attr(not(feature = "whisper"), allow(dead_code))]
    use_gpu: bool,
    loaded: OnceLock<Result<Arc<dyn ChunkTranscriber>, String>>,
}
impl LazyTranscriber {
    fn get(&self) -> Result<&Arc<dyn ChunkTranscriber>, String> {
        self.loaded
            .get_or_init(|| match self.model {
                SttModel::Parakeet | SttModel::ParakeetV2 | SttModel::ParakeetUltra => {
                    ParakeetTranscriber::load(&self.path)
                        .map(|m| Arc::new(m) as Arc<dyn ChunkTranscriber>)
                }
                #[cfg(feature = "whisper")]
                SttModel::Whisper(_) => WhisperTranscriber::load(&self.path, self.use_gpu)
                    .map(|m| Arc::new(m) as Arc<dyn ChunkTranscriber>),
            })
            .as_ref()
            .map_err(Clone::clone)
    }
}
impl ChunkTranscriber for LazyTranscriber {
    fn ready(&self) -> Result<(), String> {
        self.get().map(|_| ())
    }
    fn load_failed(&self) -> bool {
        matches!(self.loaded.get(), Some(Err(_)))
    }
    fn transcribe_pcm(
        &self,
        samples: &[i16],
        sample_rate: u32,
        language: &str,
        initial_prompt: Option<&str>,
    ) -> Result<String, String> {
        self.get()?
            .transcribe_pcm(samples, sample_rate, language, initial_prompt)
    }
    fn transcribe_detailed(
        &self,
        samples: &[i16],
        sample_rate: u32,
        language: &str,
        initial_prompt: Option<&str>,
        abort: Option<Arc<AtomicBool>>,
    ) -> Result<Hypothesis, String> {
        self.get()?
            .transcribe_detailed(samples, sample_rate, language, initial_prompt, abort)
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

impl ParakeetTranscriber {
    fn transcribe_tokens(
        &self,
        samples: &[i16],
        sample_rate: u32,
    ) -> Result<parakeet_rs::TranscriptionResult, String> {
        if samples.is_empty() {
            return Err("No audio samples were captured".to_string());
        }
        if !contains_probable_speech(samples, sample_rate) {
            return Err(NO_SPEECH.to_string());
        }

        // Parakeet expects 16 kHz mono f32; resample to match before handing
        // it the block. Token mode (the default) keeps the text exactly as
        // decoded; its per-token times come for free.
        let audio = resample_i16_to_16khz_f32(samples, sample_rate);
        let mut model = self
            .model
            .lock()
            .map_err(|_| "Parakeet model lock was poisoned".to_string())?;
        model
            .transcribe_samples(audio, 16_000, 1, None)
            .map_err(|err| format!("Parakeet transcription failed: {err}"))
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
        let result = self.transcribe_tokens(samples, sample_rate)?;
        let transcript = result.text.trim().to_string();
        if transcript.is_empty() {
            return Err(NO_SPEECH.to_string());
        }
        Ok(transcript)
    }

    /// Words with timings and no confidences: parakeet-rs's greedy TDT
    /// decoder discards the logits.
    fn transcribe_detailed(
        &self,
        samples: &[i16],
        sample_rate: u32,
        _language: &str,
        _initial_prompt: Option<&str>,
        _abort: Option<Arc<AtomicBool>>,
    ) -> Result<Hypothesis, String> {
        let result = self.transcribe_tokens(samples, sample_rate)?;
        let hypothesis = words_from_timed_tokens(
            result
                .tokens
                .iter()
                .map(|token| (token.text.as_str(), token.start, token.end)),
        );
        if hypothesis.is_empty() {
            return Err(NO_SPEECH.to_string());
        }
        Ok(hypothesis)
    }
}

/// Groups subword tokens into whitespace-separated words, each spanning its
/// tokens' times. The words are exactly the decoded text's.
fn words_from_timed_tokens<'a>(tokens: impl Iterator<Item = (&'a str, f32, f32)>) -> Hypothesis {
    let mut words: Vec<Word> = Vec::new();
    let mut open = false;
    for (text, start, end) in tokens {
        for ch in text.chars() {
            if ch.is_whitespace() {
                open = false;
                continue;
            }
            if !open {
                words.push(Word {
                    start: Some(start),
                    ..Word::default()
                });
                open = true;
            }
            let word = words.last_mut().expect("a word was just opened");
            word.text.push(ch);
            word.end = Some(end);
        }
    }
    Hypothesis { words }
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

    fn transcribe_detailed(
        &self,
        samples: &[i16],
        sample_rate: u32,
        language: &str,
        initial_prompt: Option<&str>,
        abort: Option<Arc<AtomicBool>>,
    ) -> Result<Hypothesis, String> {
        transcribe_whisper_detailed(
            &self.context,
            samples,
            sample_rate,
            language,
            initial_prompt,
            abort,
        )
    }
}

impl SessionHandle {
    #[cfg(test)]
    fn start(
        transcriber: Arc<dyn ChunkTranscriber>,
        language: String,
        initial_prompt: Option<String>,
        chunk_seconds: u16,
        preview_tx: Option<TranscriptionPreviewSender>,
    ) -> Result<Self, String> {
        Self::start_traced(
            transcriber,
            language,
            initial_prompt,
            chunk_seconds,
            preview_tx,
            None,
        )
    }
    fn start_traced(
        transcriber: Arc<dyn ChunkTranscriber>,
        language: String,
        initial_prompt: Option<String>,
        chunk_seconds: u16,
        preview_tx: Option<TranscriptionPreviewSender>,
        trace: Option<crate::note_debug::Trace>,
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
                    trace,
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
    /// True when this chunk repeats the previous chunk's tail (a forced
    /// wall-clock cut); false when the cut landed in a silence gap and the
    /// transcripts can simply be concatenated.
    overlaps_previous: bool,
}

struct ChunkResult {
    index: usize,
    text: String,
    overlaps_previous: bool,
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
    trace: Option<crate::note_debug::Trace>,
) -> Result<String, String> {
    let chunk_seconds = usize::from(chunk_seconds.clamp(5, 60));
    let release_hook = Arc::clone(&transcriber);
    let (job_tx, job_rx) = mpsc::channel::<WhisperChunkJob>();
    let (chunk_result_tx, chunk_result_rx) = mpsc::channel::<Result<ChunkResult, String>>();
    let worker = thread::Builder::new()
        .name("transcription-chunk-worker".to_string())
        .spawn(move || {
            SPEECH_MODEL_BUSY_MS.store(0, std::sync::atomic::Ordering::Relaxed);
            // Pack terms the dictation has already said join the prompt for
            // the chunks after them (Whisper only; Parakeet has no prompt).
            let mut earlier_text = String::new();
            for job in job_rx {
                let started = std::time::Instant::now();
                let prompt = initial_prompt.as_deref().map(|base| {
                    crate::vocabulary_packs::extend_whisper_prompt(base, &earlier_text)
                });
                if let Some(trace) = &trace { trace.event("asr-chunk-start", serde_json::json!({"index":job.index,"samples":job.pcm_i16.len(),"sampleRate":job.sample_rate,"durationSeconds":job.pcm_i16.len() as f64 / job.sample_rate.max(1) as f64,"overlapsPrevious":job.overlaps_previous,"language":language,"initialPrompt":prompt})); }
                let result = transcriber
                    .transcribe_pcm(
                        &job.pcm_i16,
                        job.sample_rate,
                        &language,
                        prompt.as_deref(),
                    )
                    .map(|text| ChunkResult {
                        index: job.index,
                        text: if is_phantom_phrase(&text, &job) {
                            String::new()
                        } else {
                            text
                        },
                        overlaps_previous: job.overlaps_previous,
                    })
                    .or_else(|err| {
                        if err == NO_SPEECH {
                            Ok(ChunkResult {
                                index: job.index,
                                text: String::new(),
                                overlaps_previous: job.overlaps_previous,
                            })
                        } else {
                            Err(err)
                        }
                    });

                SPEECH_MODEL_BUSY_MS.fetch_add(
                    started.elapsed().as_millis() as u64,
                    std::sync::atomic::Ordering::Relaxed,
                );
                if let (Some(_), Ok(chunk)) = (&initial_prompt, &result) {
                    earlier_text.push(' ');
                    earlier_text.push_str(&chunk.text);
                }
                if let Some(trace) = &trace { trace.event("asr-chunk-result", serde_json::json!({"index":job.index,"text":result.as_ref().ok().map(|r|&r.text),"activeSpeechMs":audio_activity_stats(&job.pcm_i16, job.sample_rate).active_speech_ms,"error":result.as_ref().err(),"durationMs":started.elapsed().as_millis() as u64})); }
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
    let mut cut_state = ChunkCutState::default();

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
                collect_available_chunk_results(&chunk_result_rx, &mut chunks, &preview_tx)?;
                dispatch_ready_chunks(
                    &mut buffer,
                    sample_rate,
                    chunk_seconds,
                    &job_tx,
                    &mut chunk_index,
                    &mut dispatched_chunks,
                    &mut cut_state,
                    chunks.len(),
                )?;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
        collect_available_chunk_results(&chunk_result_rx, &mut chunks, &preview_tx)?;
    }
    release_hook.on_release();

    for frame in audio_rx.try_iter() {
        if frame.pcm_i16.is_empty() {
            continue;
        }
        received_audio = true;
        sample_rate = frame.sample_rate;
        buffer.extend_from_slice(&frame.pcm_i16);
        collect_available_chunk_results(&chunk_result_rx, &mut chunks, &preview_tx)?;
        dispatch_ready_chunks(
            &mut buffer,
            sample_rate,
            chunk_seconds,
            &job_tx,
            &mut chunk_index,
            &mut dispatched_chunks,
            &mut cut_state,
            chunks.len(),
        )?;
    }

    if !received_audio {
        drop(job_tx);
        let _ = worker.join();
        return Err(NO_STREAMED_AUDIO.to_string());
    }

    dispatch_final_chunk(
        &mut buffer,
        sample_rate,
        &job_tx,
        &mut chunk_index,
        dispatched_chunks,
        cut_state.next_overlaps_previous,
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
        return Err(NO_SPEECH.to_string());
    }
    emit_preview(
        &preview_tx,
        chunk_index,
        transcript.clone(),
        polish_frozen_len(&chunks),
        true,
    );
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
            overlaps_previous: chunk.overlaps_previous,
        })
        .collect::<Vec<_>>();
    sorted.sort_by_key(|chunk| chunk.index);
    let text = merge_chunk_results(&sorted);
    if !text.is_empty() {
        let index = sorted.last().map(|chunk| chunk.index).unwrap_or_default();
        emit_preview(
            preview_tx,
            index,
            text,
            polish_frozen_len(&sorted),
            final_preview,
        );
    }
}

/// How much of the merged text sits behind the most recent silence gap.
///
/// A chunk with `overlaps_previous == false` began after a gap cut, so the
/// boundary before it landed in silence and the merge of everything earlier is
/// already final. Because `merge_chunk_results` only ever appends to its
/// accumulator, that merge is a byte prefix of the full text.
fn sealed_prefix_len(sorted: &[ChunkResult]) -> usize {
    let Some(boundary) = sorted.iter().rposition(|chunk| !chunk.overlaps_previous) else {
        return 0;
    };
    let sealed = merge_chunk_results(&sorted[..boundary]);
    let text = merge_chunk_results(sorted);
    if text.starts_with(&sealed) {
        sealed.len()
    } else {
        0
    }
}

/// How much of the merged text polish must not resend.
///
/// Gap freeze is the existing silence-boundary prefix. Window freeze keeps only
/// the last `POLISH_WINDOW_CHUNKS` non-empty chunks volatile, so continuous
/// speech without pauses stays bounded. Empty ASR results do not count toward
/// the window. The freeze is the further of the two, and is always a prefix of
/// the full merge when the append-only property holds.
fn polish_frozen_len(sorted: &[ChunkResult]) -> usize {
    let gap = sealed_prefix_len(sorted);
    let nonempty = sorted
        .iter()
        .filter(|chunk| !chunk.text.trim().is_empty())
        .count();
    let window = if nonempty <= POLISH_WINDOW_CHUNKS {
        0
    } else {
        let keep_from = nonempty - POLISH_WINDOW_CHUNKS;
        let mut seen = 0;
        let mut cut = 0;
        for (index, chunk) in sorted.iter().enumerate() {
            if chunk.text.trim().is_empty() {
                continue;
            }
            if seen == keep_from {
                cut = index;
                break;
            }
            seen += 1;
        }
        let frozen = merge_chunk_results(&sorted[..cut]);
        let text = merge_chunk_results(sorted);
        if text.starts_with(&frozen) {
            frozen.len()
        } else {
            0
        }
    };
    gap.max(window)
}

fn emit_preview(
    preview_tx: &Option<TranscriptionPreviewSender>,
    index: usize,
    text: String,
    sealed_len: usize,
    final_preview: bool,
) {
    if let Some(preview_tx) = preview_tx {
        let _ = preview_tx.send(TranscriptPreview {
            index,
            text,
            sealed_len,
            final_preview,
        });
    }
}

/// Tracks how the pending buffer relates to the previously dispatched chunk.
#[derive(Default)]
struct ChunkCutState {
    gap: Option<GapTracker>,
    /// True when the last cut retained the 1s overlap tail, so the next
    /// dispatched chunk's transcript repeats the previous chunk's ending.
    next_overlaps_previous: bool,
}

/// Incrementally classifies the pending buffer into 30ms active/silent
/// windows (same thresholds as `contains_probable_speech`) so chunk cuts can
/// land in real speech gaps instead of on a wall clock.
struct GapTracker {
    window_samples: usize,
    scanned_samples: usize,
    trailing_silence_ms: usize,
    has_speech: bool,
}

impl GapTracker {
    fn new(sample_rate: u32) -> Self {
        Self {
            window_samples: (sample_rate as usize * SPEECH_WINDOW_MS / 1000).max(1),
            scanned_samples: 0,
            trailing_silence_ms: 0,
            has_speech: false,
        }
    }

    fn scan(&mut self, buffer: &[i16]) {
        while self.scanned_samples + self.window_samples <= buffer.len() {
            let window = &buffer[self.scanned_samples..self.scanned_samples + self.window_samples];
            let (peak, rms) = peak_and_rms(window);
            if peak >= SPEECH_WINDOW_PEAK_THRESHOLD || rms >= SPEECH_WINDOW_RMS_THRESHOLD {
                self.trailing_silence_ms = 0;
                self.has_speech = true;
            } else {
                self.trailing_silence_ms += SPEECH_WINDOW_MS;
            }
            self.scanned_samples += self.window_samples;
        }
    }

    /// After a gap cut the buffer is empty and the next utterance starts
    /// fresh.
    fn reset(&mut self) {
        self.scanned_samples = 0;
        self.trailing_silence_ms = 0;
        self.has_speech = false;
    }

    /// After a forced cut the buffer keeps the overlap tail; a fresh gap cut
    /// must wait for new speech so it never dispatches an overlap-only chunk.
    fn after_drain(&mut self, drained: usize) {
        self.scanned_samples = self.scanned_samples.saturating_sub(drained);
        self.has_speech = false;
    }
}

#[allow(clippy::too_many_arguments)]
fn dispatch_ready_chunks(
    buffer: &mut Vec<i16>,
    sample_rate: u32,
    max_chunk_seconds: usize,
    job_tx: &mpsc::Sender<WhisperChunkJob>,
    chunk_index: &mut usize,
    dispatched_chunks: &mut usize,
    cut: &mut ChunkCutState,
    completed_chunks: usize,
) -> Result<(), String> {
    let max_chunk_samples = sample_rate as usize * max_chunk_seconds;
    let overlap_samples = sample_rate as usize * CHUNK_OVERLAP_SECONDS;
    let min_gap_samples = sample_rate as usize * MIN_GAP_CHUNK_SECONDS;
    if max_chunk_samples == 0 {
        return Ok(());
    }

    let gap = cut.gap.get_or_insert_with(|| GapTracker::new(sample_rate));
    gap.scan(buffer);

    loop {
        // Backpressure: while the engine is behind, keep accumulating (the
        // buffer is bounded by the recording cap) instead of queueing jobs.
        if dispatched_chunks.saturating_sub(completed_chunks) >= MAX_PENDING_CHUNK_JOBS {
            return Ok(());
        }

        if gap.has_speech
            && gap.trailing_silence_ms >= GAP_SILENCE_MS
            && buffer.len() >= min_gap_samples
        {
            // Gap cut: the boundary sits in silence, so the whole utterance
            // ships as one chunk and the next chunk needs no overlap.
            job_tx
                .send(WhisperChunkJob {
                    index: *chunk_index,
                    pcm_i16: std::mem::take(buffer),
                    sample_rate,
                    overlaps_previous: cut.next_overlaps_previous,
                })
                .map_err(|_| {
                    "Transcription chunk worker stopped while receiving audio".to_string()
                })?;
            *chunk_index += 1;
            *dispatched_chunks += 1;
            cut.next_overlaps_previous = false;
            gap.reset();
            return Ok(());
        }

        if buffer.len() < max_chunk_samples {
            return Ok(());
        }

        // Forced wall-clock cut mid-speech: keep the 1s overlap tail so the
        // text merge can reconcile the boundary, exactly as before.
        let chunk = &buffer[..max_chunk_samples];
        let has_speech = contains_probable_speech(chunk, sample_rate);
        if has_speech {
            job_tx
                .send(WhisperChunkJob {
                    index: *chunk_index,
                    pcm_i16: chunk.to_vec(),
                    sample_rate,
                    overlaps_previous: cut.next_overlaps_previous,
                })
                .map_err(|_| {
                    "Transcription chunk worker stopped while receiving audio".to_string()
                })?;
            *dispatched_chunks += 1;
        }
        *chunk_index += 1;

        let drain_samples = max_chunk_samples
            .saturating_sub(overlap_samples)
            .max(1)
            .min(buffer.len());
        buffer.drain(..drain_samples);
        gap.after_drain(drain_samples);
        // A silent forced chunk contributes no text, so the next chunk must
        // not fuzzy-merge against older transcript.
        cut.next_overlaps_previous = has_speech;
    }
}

fn dispatch_final_chunk(
    buffer: &mut Vec<i16>,
    sample_rate: u32,
    job_tx: &mpsc::Sender<WhisperChunkJob>,
    chunk_index: &mut usize,
    dispatched_chunks: usize,
    overlaps_previous: bool,
) -> Result<(), String> {
    if buffer.is_empty() {
        return Ok(());
    }

    let overlap_samples = sample_rate as usize * CHUNK_OVERLAP_SECONDS;
    if dispatched_chunks > 0 && overlaps_previous && buffer.len() <= overlap_samples {
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
            overlaps_previous,
        })
        .map_err(|_| {
            "Transcription chunk worker stopped while receiving final audio".to_string()
        })?;
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
        return Err(NO_SPEECH.to_string());
    }

    let mut state = context
        .create_state()
        .map_err(|err| format!("Failed to create Whisper state: {err}"))?;
    let audio = resample_i16_to_16khz_f32(samples, sample_rate);
    let params = whisper_params(language, initial_prompt);

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
        return Err(NO_SPEECH.to_string());
    }

    Ok(transcript)
}

#[cfg(feature = "whisper")]
fn whisper_params<'a>(language: &'a str, initial_prompt: Option<&str>) -> FullParams<'a, 'a> {
    let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 0 });
    params.set_n_threads(default_thread_count());
    params.set_print_special(false);
    params.set_print_progress(false);
    params.set_print_realtime(false);
    params.set_print_timestamps(false);
    params.set_no_context(true);
    params.set_suppress_nst(false);
    if let Some(initial_prompt) = initial_prompt.filter(|prompt| !prompt.trim().is_empty()) {
        // whisper.cpp takes a C string; a NUL would panic the binding.
        params.set_initial_prompt(&initial_prompt.replace('\0', " "));
    }

    if language == "auto" {
        params.set_language(None);
        params.set_detect_language(true);
    } else {
        params.set_language(Some(language));
    }
    params
}

/// Whisper's transcript word by word: each word's mean token probability
/// and its token-level timings, from the same accepted segments as
/// `transcribe_whisper_pcm`.
#[cfg(feature = "whisper")]
fn transcribe_whisper_detailed(
    context: &WhisperContext,
    samples: &[i16],
    sample_rate: u32,
    language: &str,
    initial_prompt: Option<&str>,
    abort: Option<Arc<AtomicBool>>,
) -> Result<Hypothesis, String> {
    if samples.is_empty() {
        return Err("No audio samples were captured".to_string());
    }
    if !contains_probable_speech(samples, sample_rate) {
        return Err(NO_SPEECH.to_string());
    }
    let mut state = context
        .create_state()
        .map_err(|err| format!("Failed to create Whisper state: {err}"))?;
    let audio = resample_i16_to_16khz_f32(samples, sample_rate);
    let mut params = whisper_params(language, initial_prompt);
    params.set_token_timestamps(true);
    // whisper-rs 0.16's `set_abort_callback_safe` hands whisper.cpp a pointer
    // to a boxed trait object but reads it back as the closure type, so it
    // aborts decodes at random. The raw callback reads our flag directly.
    unsafe extern "C" fn abort_requested(flag: *mut std::ffi::c_void) -> bool {
        // SAFETY: `flag` is the `AtomicBool` kept alive by `abort` below for
        // as long as `state.full` runs.
        unsafe { (*(flag as *const AtomicBool)).load(std::sync::atomic::Ordering::Relaxed) }
    }
    if let Some(abort) = &abort {
        // SAFETY: the callback only reads the flag, which outlives the decode.
        unsafe {
            params.set_abort_callback(Some(abort_requested));
            params.set_abort_callback_user_data(Arc::as_ptr(abort) as *mut std::ffi::c_void);
        }
    }
    let decoded = state.full(params, &audio);
    drop(abort);
    decoded.map_err(|err| format!("Whisper transcription failed: {err}"))?;

    /// A word while its tokens arrive; bytes because a multibyte character
    /// can be split across tokens.
    struct PendingWord {
        bytes: Vec<u8>,
        probabilities: Vec<f32>,
        start: Option<f32>,
        end: Option<f32>,
    }
    let end_of_text = context.token_eot();
    let mut words: Vec<PendingWord> = Vec::new();
    for segment in state.as_iter() {
        if accepted_segment_text(&segment).is_none() {
            continue;
        }
        for index in 0..segment.n_tokens() {
            let Some(token) = segment.get_token(index) else {
                continue;
            };
            // Special tokens (timestamps, start/end markers) sort after EOT.
            if token.token_id() >= end_of_text {
                continue;
            }
            let Ok(bytes) = token.to_bytes() else {
                continue;
            };
            let data = token.token_data();
            let time = |centiseconds: i64| (centiseconds >= 0).then(|| centiseconds as f32 / 100.0);
            let probability = token.token_probability();
            let starts_word = bytes.first().is_some_and(u8::is_ascii_whitespace) || words.is_empty();
            let text = bytes.trim_ascii();
            if text.is_empty() {
                continue;
            }
            if starts_word {
                words.push(PendingWord {
                    bytes: Vec::new(),
                    probabilities: Vec::new(),
                    start: time(data.t0),
                    end: None,
                });
            }
            let word = words.last_mut().expect("a word was just opened");
            word.bytes.extend_from_slice(text);
            if probability.is_finite() {
                word.probabilities.push(probability);
            }
            word.end = time(data.t1);
        }
    }
    let words: Vec<Word> = words
        .into_iter()
        .map(|word| Word {
            text: String::from_utf8_lossy(&word.bytes).into_owned(),
            confidence: (!word.probabilities.is_empty()).then(|| {
                word.probabilities.iter().sum::<f32>() / word.probabilities.len() as f32
            }),
            start: word.start,
            end: word.end,
        })
        .filter(|word| !word.text.is_empty())
        .collect();
    if words.is_empty() {
        return Err(NO_SPEECH.to_string());
    }
    Ok(Hypothesis { words })
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
        transcript = if chunk.overlaps_previous {
            merge_transcript_text(&transcript, text)
        } else {
            join_transcript_text(&transcript, text)
        };
    }
    transcript
}

/// Plain concatenation for gap-cut boundaries: the cut landed in silence, so
/// nothing was transcribed twice and fuzzy overlap matching would only risk
/// eating genuinely repeated words.
fn join_transcript_text(previous: &str, next: &str) -> String {
    let previous = previous.trim();
    let next = next.trim();
    if previous.is_empty() {
        return next.to_string();
    }
    if next.is_empty() {
        return previous.to_string();
    }
    format!("{previous} {next}")
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

/// True when a chunk's whole transcript is a stock closing phrase and its audio
/// holds too little voiced sound for anyone to have said it. Both must hold:
/// the phrase alone is something people really dictate ("Thank you." closes
/// many emails), and quiet audio alone is often real speech. Only a chunk that
/// starts fresh after a silence gap qualifies — one that overlaps its
/// predecessor carries real speech by construction.
fn is_phantom_phrase(text: &str, job: &WhisperChunkJob) -> bool {
    if job.overlaps_previous {
        return false;
    }
    let spoken = text
        .split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_lowercase)
        .collect::<Vec<_>>()
        .join(" ");
    PHANTOM_PHRASES.contains(&spoken.as_str())
        && audio_activity_stats(&job.pcm_i16, job.sample_rate).active_speech_ms
            < PHANTOM_MIN_SPEECH_MS
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
        if window_peak >= SPEECH_WINDOW_PEAK_THRESHOLD || window_rms >= SPEECH_WINDOW_RMS_THRESHOLD
        {
            active_windows += 1;
        }
    }

    AudioActivityStats {
        peak,
        rms,
        active_speech_ms: active_windows * SPEECH_WINDOW_MS,
    }
}

/// Which 30 ms windows of a chunk carry speech-level energy, with the same
/// thresholds the chunker cuts on.
pub(crate) fn speech_activity_profile(samples: &[i16], sample_rate: u32) -> EnergyProfile {
    let window_samples = (sample_rate as usize * SPEECH_WINDOW_MS / 1000).max(1);
    EnergyProfile {
        window_seconds: window_samples as f32 / sample_rate.max(1) as f32,
        active: samples
            .chunks(window_samples)
            .map(|window| {
                let (peak, rms) = peak_and_rms(window);
                peak >= SPEECH_WINDOW_PEAK_THRESHOLD || rms >= SPEECH_WINDOW_RMS_THRESHOLD
            })
            .collect(),
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

pub(crate) fn resample_i16_to_16khz_f32(samples: &[i16], source_rate: u32) -> Vec<f32> {
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
        contains_probable_speech, dispatch_final_chunk, dispatch_ready_chunks,
        is_non_speech_annotation, is_phantom_phrase, merge_chunk_results, merge_transcript_text, polish_frozen_len,
        resample_i16_to_16khz_f32, sealed_prefix_len, ChunkCutState, ChunkResult, ChunkTranscriber,
        SessionHandle, WhisperChunkJob, CHUNK_OVERLAP_SECONDS, GAP_SILENCE_MS,
        MAX_PENDING_CHUNK_JOBS, MIN_GAP_CHUNK_SECONDS, NO_STREAMED_AUDIO,
    };
    use crate::audio::AudioFrame;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc;
    use std::sync::{Arc, Mutex};

    /// A fake engine that records how many chunks it was asked to transcribe and
    /// returns a distinct token per call, so a merged transcript reveals both
    /// the chunk count and their order. Used to exercise the engine-agnostic
    /// streaming pipeline (the same path Parakeet runs through) without loading
    /// a real model.
    struct CountingTranscriber {
        calls: Arc<AtomicUsize>,
        seen_rates: Arc<Mutex<Vec<u32>>>,
    }

    impl ChunkTranscriber for CountingTranscriber {
        fn transcribe_pcm(
            &self,
            samples: &[i16],
            sample_rate: u32,
            _language: &str,
            _initial_prompt: Option<&str>,
        ) -> Result<String, String> {
            assert!(!samples.is_empty(), "engine received an empty chunk");
            self.seen_rates.lock().unwrap().push(sample_rate);
            let index = self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(format!("c{index}"))
        }
    }

    fn speech_frame(sample_rate: u32, samples: usize) -> AudioFrame {
        AudioFrame {
            pcm_i16: vec![2_000_i16; samples],
            sample_rate,
        }
    }

    #[test]
    fn cold_engine_does_not_block_audio_spooling_or_lose_opening_samples() {
        struct DelayedEngine {
            release: Mutex<mpsc::Receiver<()>>,
            started: mpsc::Sender<()>,
            seen: Arc<Mutex<Vec<i16>>>,
            first: std::sync::atomic::AtomicBool,
        }
        impl ChunkTranscriber for DelayedEngine {
            fn transcribe_pcm(
                &self,
                samples: &[i16],
                _: u32,
                _: &str,
                _: Option<&str>,
            ) -> Result<String, String> {
                if self.first.swap(false, Ordering::SeqCst) {
                    self.started.send(()).unwrap();
                    self.release.lock().unwrap().recv().unwrap();
                }
                self.seen.lock().unwrap().extend_from_slice(samples);
                Ok("words".into())
            }
        }
        let (release_tx, release_rx) = mpsc::channel();
        let (started_tx, started_rx) = mpsc::channel();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let engine = Arc::new(DelayedEngine {
            release: Mutex::new(release_rx),
            started: started_tx,
            seen: seen.clone(),
            first: std::sync::atomic::AtomicBool::new(true),
        });
        let handle = SessionHandle::start(engine, "en".into(), None, 5, None).unwrap();
        handle
            .audio_tx
            .send(AudioFrame {
                pcm_i16: vec![2000; 5000],
                sample_rate: 1000,
            })
            .unwrap();
        started_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap();
        // More than the callback channel depth must drain even with inference held.
        for n in 0..100 {
            handle
                .audio_tx
                .send(AudioFrame {
                    pcm_i16: vec![3000 + n; 100],
                    sample_rate: 1000,
                })
                .unwrap();
        }
        // Finish before releasing the cold engine: finalization must retain everything.
        let finished = std::thread::spawn(move || handle.finish());
        release_tx.send(()).unwrap();
        assert!(!finished.join().unwrap().unwrap().is_empty());
        let samples = seen.lock().unwrap();
        assert_eq!(samples[0], 2000);
        assert!(samples.len() >= 15000); // overlaps may repeat, never omit
        for n in 0..100 {
            assert!(samples.contains(&(3000 + n)));
        }
        assert_eq!(*samples.last().unwrap(), 3099);
    }

    #[test]
    fn streaming_session_transcribes_and_merges_chunks_in_order() {
        let calls = Arc::new(AtomicUsize::new(0));
        let seen_rates = Arc::new(Mutex::new(Vec::new()));
        let transcriber: Arc<dyn ChunkTranscriber> = Arc::new(CountingTranscriber {
            calls: Arc::clone(&calls),
            seen_rates: Arc::clone(&seen_rates),
        });
        let sample_rate = 16_000;
        let chunk_seconds = 5_u16;
        let trace = crate::note_debug::Trace::new(true);
        let handle = SessionHandle::start_traced(
            transcriber,
            "en".to_string(),
            Some("Vocabulary hint".into()),
            chunk_seconds,
            None,
            trace.clone(),
        )
        .expect("start session");

        // Three chunks' worth of speech so the dispatcher emits several jobs.
        let chunk_samples = sample_rate as usize * usize::from(chunk_seconds);
        handle
            .audio_tx
            .send(speech_frame(sample_rate, chunk_samples * 3))
            .expect("send audio frame");

        let transcript = handle.finish().expect("finish session");

        let count = calls.load(Ordering::SeqCst);
        assert!(count >= 2, "expected multiple chunks, transcribed {count}");
        // Distinct, non-overlapping tokens merge into one ordered transcript.
        let expected = (0..count)
            .map(|index| format!("c{index}"))
            .collect::<Vec<_>>()
            .join(" ");
        assert_eq!(transcript, expected);
        if let Some(trace) = trace {
            let metadata = trace.finish(&transcript, &transcript, &transcript, &transcript);
            let starts: Vec<_> = metadata
                .events
                .iter()
                .filter(|e| e.kind == "asr-chunk-start")
                .collect();
            let results: Vec<_> = metadata
                .events
                .iter()
                .filter(|e| e.kind == "asr-chunk-result")
                .collect();
            assert_eq!(starts.len(), count);
            assert_eq!(results.len(), count);
            for (index, (start, result)) in starts.iter().zip(&results).enumerate() {
                assert_eq!(start.data["index"], index);
                assert_eq!(start.data["initialPrompt"], "Vocabulary hint");
                assert_eq!(result.data["index"], index);
                assert_eq!(result.data["text"], format!("c{index}"));
                assert!(result.data["error"].is_null());
                assert!(start.sequence < result.sequence);
            }
        }
        // Every chunk is handed to the engine at the captured sample rate.
        assert_eq!(seen_rates.lock().unwrap().len(), count);
        assert!(seen_rates
            .lock()
            .unwrap()
            .iter()
            .all(|rate| *rate == sample_rate));
    }

    #[test]
    fn streaming_session_without_audio_reports_no_streamed_audio() {
        let transcriber: Arc<dyn ChunkTranscriber> = Arc::new(CountingTranscriber {
            calls: Arc::new(AtomicUsize::new(0)),
            seen_rates: Arc::new(Mutex::new(Vec::new())),
        });
        let handle = SessionHandle::start(transcriber, "en".to_string(), None, 5, None)
            .expect("start session");

        let err = handle.finish().expect_err("no audio should error");
        assert_eq!(err, NO_STREAMED_AUDIO);
    }

    #[test]
    fn streaming_session_cancel_stops_the_worker_without_a_transcript() {
        let calls = Arc::new(AtomicUsize::new(0));
        let transcriber: Arc<dyn ChunkTranscriber> = Arc::new(CountingTranscriber {
            calls: Arc::clone(&calls),
            seen_rates: Arc::new(Mutex::new(Vec::new())),
        });
        let handle = SessionHandle::start(transcriber, "en".to_string(), None, 5, None)
            .expect("start session");
        handle
            .audio_tx
            .send(speech_frame(16_000, 16_000))
            .expect("send audio frame");

        // Cancelling must join the worker (and its chunk thread) without hanging.
        handle.cancel();
    }

    #[test]
    fn the_seal_point_tracks_the_last_silence_gap_and_is_always_a_prefix() {
        let chunk = |index: usize, text: &str, overlaps_previous: bool| ChunkResult {
            index,
            text: text.to_string(),
            overlaps_previous,
        };
        // Two forced cuts then a gap cut: only what precedes the gap-cut chunk
        // is final, because the merge can still revise across an overlap.
        let chunks = [
            chunk(0, "book it for", false),
            chunk(1, "for thursday", true),
            chunk(2, "no friday", false),
        ];
        let text = merge_chunk_results(&chunks);
        let sealed = sealed_prefix_len(&chunks);
        assert_eq!(&text[..sealed], "book it for thursday");
        assert!(text.starts_with(&text[..sealed]));

        // A lone opening chunk has no earlier boundary to seal against.
        assert_eq!(sealed_prefix_len(&chunks[..1]), 0);

        // Every chunk still overlapping means nothing has been sealed.
        let overlapping = [chunk(0, "hello", true), chunk(1, "hello there", true)];
        assert_eq!(sealed_prefix_len(&overlapping), 0);
    }

    #[test]
    fn polish_freezes_everything_but_the_last_three_nonempty_chunks() {
        let chunk = |index: usize, text: &str, overlaps_previous: bool| ChunkResult {
            index,
            text: text.to_string(),
            overlaps_previous,
        };
        let three = [
            chunk(0, "one", true),
            chunk(1, "one two", true),
            chunk(2, "two three", true),
        ];
        assert_eq!(
            polish_frozen_len(&three),
            0,
            "a window of three stays volatile"
        );

        let four = [
            chunk(0, "one", true),
            chunk(1, "one two", true),
            chunk(2, "two three", true),
            chunk(3, "three four", true),
        ];
        let text = merge_chunk_results(&four);
        let frozen = polish_frozen_len(&four);
        assert_eq!(&text[..frozen], merge_chunk_results(&four[..1]));
        assert!(text.starts_with(&text[..frozen]));

        // Empty results do not spend a window slot, and a later gap freeze can
        // lock more than the three-chunk window.
        let mixed = [
            chunk(0, "book it for thursday", false),
            chunk(1, "", true),
            chunk(2, "no friday", false),
            chunk(3, "and tell sam", true),
        ];
        let text = merge_chunk_results(&mixed);
        let frozen = polish_frozen_len(&mixed);
        assert_eq!(&text[..frozen], "book it for thursday");
    }

    #[test]
    fn streaming_session_emits_a_final_preview() {
        let transcriber: Arc<dyn ChunkTranscriber> = Arc::new(CountingTranscriber {
            calls: Arc::new(AtomicUsize::new(0)),
            seen_rates: Arc::new(Mutex::new(Vec::new())),
        });
        let (preview_tx, preview_rx) = mpsc::channel();
        let sample_rate = 16_000;
        let chunk_seconds = 5_u16;
        let handle = SessionHandle::start(
            transcriber,
            "en".to_string(),
            None,
            chunk_seconds,
            Some(preview_tx),
        )
        .expect("start session");
        let chunk_samples = sample_rate as usize * usize::from(chunk_seconds);
        handle
            .audio_tx
            .send(speech_frame(sample_rate, chunk_samples * 2))
            .expect("send audio frame");

        let transcript = handle.finish().expect("finish session");
        let previews: Vec<_> = preview_rx.iter().collect();
        assert!(!previews.is_empty(), "expected at least one preview");
        let final_preview = previews
            .iter()
            .rfind(|preview| preview.final_preview)
            .expect("a final preview");
        assert_eq!(final_preview.text, transcript);
    }

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

    /// Synthetic PCM: `speech_ms` of loud samples followed by `silence_ms`
    /// of near-silence, at `sample_rate`.
    fn speech_then_silence(sample_rate: u32, speech_ms: usize, silence_ms: usize) -> Vec<i16> {
        let per_ms = sample_rate as usize / 1000;
        let mut samples = vec![2_000_i16; speech_ms * per_ms];
        samples.extend(vec![0_i16; silence_ms * per_ms]);
        samples
    }

    #[test]
    fn forced_cut_retains_overlap_and_marks_next_chunk_overlapping() {
        let sample_rate = 1_000;
        let chunk_seconds = 20;
        let chunk_samples = sample_rate as usize * chunk_seconds;
        let overlap_samples = sample_rate as usize * CHUNK_OVERLAP_SECONDS;
        let mut buffer = vec![2_000_i16; chunk_samples];
        let (tx, rx) = mpsc::channel::<WhisperChunkJob>();
        let mut chunk_index = 0;
        let mut dispatched_chunks = 0;
        let mut cut = ChunkCutState::default();

        dispatch_ready_chunks(
            &mut buffer,
            sample_rate,
            chunk_seconds,
            &tx,
            &mut chunk_index,
            &mut dispatched_chunks,
            &mut cut,
            0,
        )
        .expect("dispatch ready chunk");

        let job = rx.try_recv().expect("chunk job");
        assert_eq!(job.index, 0);
        assert_eq!(job.sample_rate, sample_rate);
        assert_eq!(job.pcm_i16.len(), chunk_samples);
        assert!(!job.overlaps_previous, "first chunk has nothing to overlap");
        assert_eq!(buffer.len(), overlap_samples);
        assert_eq!(chunk_index, 1);
        assert_eq!(dispatched_chunks, 1);
        assert!(
            cut.next_overlaps_previous,
            "next chunk repeats the retained overlap tail"
        );
    }

    #[test]
    fn a_stock_phrase_over_near_silence_is_a_phantom_but_a_spoken_one_is_kept() {
        let sample_rate = 16_000_u32;
        let job = |pcm_i16: Vec<i16>, overlaps_previous: bool| WhisperChunkJob {
            index: 3,
            pcm_i16,
            sample_rate,
            overlaps_previous,
        };
        // A key click: 40 ms of loud samples in a second of silence.
        let mut click = vec![0_i16; sample_rate as usize];
        for sample in click.iter_mut().skip(8_000).take(640) {
            *sample = 9_000;
        }
        // A second of sustained voiced-level audio.
        let voiced: Vec<i16> = (0..sample_rate as usize)
            .map(|i| if i % 2 == 0 { 4_000 } else { -4_000 })
            .collect();

        assert!(is_phantom_phrase("Thank you.", &job(click.clone(), false)));
        assert!(is_phantom_phrase("Mm-hmm.", &job(click.clone(), false)));
        assert!(!is_phantom_phrase("Thank you.", &job(voiced, false)));
        assert!(!is_phantom_phrase("Thank you, Sam.", &job(click.clone(), false)));
        assert!(!is_phantom_phrase("Thank you.", &job(click, true)));
    }

    #[test]
    fn gap_cut_dispatches_whole_utterance_without_overlap() {
        let sample_rate = 1_000;
        let mut buffer = speech_then_silence(
            sample_rate,
            MIN_GAP_CHUNK_SECONDS * 1000 + 500,
            GAP_SILENCE_MS + 100,
        );
        let total = buffer.len();
        let (tx, rx) = mpsc::channel::<WhisperChunkJob>();
        let mut chunk_index = 0;
        let mut dispatched_chunks = 0;
        let mut cut = ChunkCutState::default();

        dispatch_ready_chunks(
            &mut buffer,
            sample_rate,
            20,
            &tx,
            &mut chunk_index,
            &mut dispatched_chunks,
            &mut cut,
            0,
        )
        .expect("dispatch gap chunk");

        let job = rx.try_recv().expect("gap chunk job");
        assert_eq!(job.index, 0);
        assert_eq!(
            job.pcm_i16.len(),
            total,
            "gap cut ships the whole utterance"
        );
        assert!(!job.overlaps_previous);
        assert!(buffer.is_empty(), "gap cut leaves no overlap tail");
        assert_eq!(dispatched_chunks, 1);
        assert!(
            !cut.next_overlaps_previous,
            "the next chunk starts fresh after a gap cut"
        );
    }

    #[test]
    fn gap_cut_waits_for_minimum_audio_and_a_real_gap() {
        let sample_rate = 1_000;
        let (tx, rx) = mpsc::channel::<WhisperChunkJob>();
        let mut chunk_index = 0;
        let mut dispatched_chunks = 0;
        let mut cut = ChunkCutState::default();

        // Speech with a short (sub-threshold) pause: no cut.
        let mut buffer = speech_then_silence(sample_rate, 2_500, GAP_SILENCE_MS - 200);
        dispatch_ready_chunks(
            &mut buffer,
            sample_rate,
            20,
            &tx,
            &mut chunk_index,
            &mut dispatched_chunks,
            &mut cut,
            0,
        )
        .expect("dispatch");
        assert!(rx.try_recv().is_err(), "short pauses must not cut");
        assert!(!buffer.is_empty());

        // Too little audio before the gap: no cut either.
        let mut short_buffer = speech_then_silence(sample_rate, 500, GAP_SILENCE_MS + 200);
        let mut short_cut = ChunkCutState::default();
        dispatch_ready_chunks(
            &mut short_buffer,
            sample_rate,
            20,
            &tx,
            &mut chunk_index,
            &mut dispatched_chunks,
            &mut short_cut,
            0,
        )
        .expect("dispatch");
        assert!(
            rx.try_recv().is_err(),
            "tiny utterances wait for more audio"
        );
    }

    #[test]
    fn backpressure_defers_cuts_while_jobs_are_pending() {
        let sample_rate = 1_000;
        let mut buffer = speech_then_silence(sample_rate, 3_000, GAP_SILENCE_MS + 100);
        let before = buffer.len();
        let (tx, rx) = mpsc::channel::<WhisperChunkJob>();
        let mut chunk_index = MAX_PENDING_CHUNK_JOBS;
        let mut dispatched_chunks = MAX_PENDING_CHUNK_JOBS;
        let mut cut = ChunkCutState::default();

        dispatch_ready_chunks(
            &mut buffer,
            sample_rate,
            20,
            &tx,
            &mut chunk_index,
            &mut dispatched_chunks,
            &mut cut,
            0, // nothing completed yet: MAX_PENDING_CHUNK_JOBS still in flight
        )
        .expect("dispatch");

        assert!(
            rx.try_recv().is_err(),
            "no new job while the engine is behind"
        );
        assert_eq!(buffer.len(), before, "audio keeps accumulating instead");
        assert_eq!(dispatched_chunks, MAX_PENDING_CHUNK_JOBS);
    }

    #[test]
    fn skips_final_chunk_when_only_overlap_remains() {
        let sample_rate = 10;
        let overlap_samples = sample_rate as usize * CHUNK_OVERLAP_SECONDS;
        let mut buffer = vec![1; overlap_samples];
        let (tx, rx) = mpsc::channel::<WhisperChunkJob>();
        let mut chunk_index = 1;

        dispatch_final_chunk(&mut buffer, sample_rate, &tx, &mut chunk_index, 1, true)
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

        dispatch_final_chunk(&mut buffer, sample_rate, &tx, &mut chunk_index, 1, true)
            .expect("dispatch final chunk");

        let job = rx.try_recv().expect("final chunk job");
        assert_eq!(job.index, 1);
        assert_eq!(job.pcm_i16.len(), overlap_samples + 5);
        assert!(job.overlaps_previous);
        assert!(buffer.is_empty());
        assert_eq!(chunk_index, 2);
    }

    #[test]
    fn dispatches_small_final_chunk_after_a_gap_cut() {
        // After a gap cut the buffer holds fresh audio only, so even a
        // sub-overlap-length tail must be transcribed rather than skipped.
        let sample_rate = 1_000;
        let overlap_samples = sample_rate as usize * CHUNK_OVERLAP_SECONDS;
        let mut buffer = vec![2_000_i16; overlap_samples / 2];
        let (tx, rx) = mpsc::channel::<WhisperChunkJob>();
        let mut chunk_index = 1;

        dispatch_final_chunk(&mut buffer, sample_rate, &tx, &mut chunk_index, 1, false)
            .expect("dispatch final chunk");

        let job = rx.try_recv().expect("final chunk job");
        assert!(!job.overlaps_previous);
        assert_eq!(chunk_index, 2);
    }

    #[test]
    fn skips_final_chunk_without_probable_speech() {
        let sample_rate = 10;
        let overlap_samples = sample_rate as usize * CHUNK_OVERLAP_SECONDS;
        let mut buffer = vec![1_i16; overlap_samples + 5];
        let (tx, rx) = mpsc::channel::<WhisperChunkJob>();
        let mut chunk_index = 1;

        dispatch_final_chunk(&mut buffer, sample_rate, &tx, &mut chunk_index, 1, true)
            .expect("dispatch final chunk");

        assert!(rx.try_recv().is_err());
        assert!(buffer.is_empty());
        assert_eq!(chunk_index, 1);
    }

    #[test]
    fn gap_cut_chunks_concatenate_and_forced_cut_chunks_fuzzy_merge() {
        let chunks = vec![
            ChunkResult {
                index: 0,
                text: "let's go".to_string(),
                overlaps_previous: false,
            },
            ChunkResult {
                index: 1,
                text: "go now".to_string(),
                overlaps_previous: false,
            },
        ];
        // Gap boundaries: genuinely repeated words survive.
        assert_eq!(merge_chunk_results(&chunks), "let's go go now");

        let chunks = vec![
            ChunkResult {
                index: 0,
                text: "we should ship this carefully".to_string(),
                overlaps_previous: false,
            },
            ChunkResult {
                index: 1,
                text: "ship this carefully and measure latency".to_string(),
                overlaps_previous: true,
            },
        ];
        // Forced-cut boundaries keep the overlap dedup.
        assert_eq!(
            merge_chunk_results(&chunks),
            "we should ship this carefully and measure latency"
        );
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
    fn parakeet_tokens_group_into_the_decoded_words_with_their_times() {
        let tokens = [(" De", 0.1, 0.2), ("ploy", 0.2, 0.4), (" it", 0.4, 0.5), (".", 0.5, 0.51), (" ", 0.6, 0.6), ("Now", 0.7, 0.9)];
        let hypothesis = super::words_from_timed_tokens(tokens.into_iter());
        assert_eq!(hypothesis.text(), "Deploy it. Now");
        assert_eq!(hypothesis.words[0].start, Some(0.1));
        assert_eq!(hypothesis.words[0].end, Some(0.4));
        assert_eq!(hypothesis.words[1].end, Some(0.51));
        assert!(hypothesis.words.iter().all(|w| w.confidence.is_none()));
    }

    #[test]
    fn identifies_common_non_speech_annotations() {
        assert!(is_non_speech_annotation("[BLANK_AUDIO]"));
        assert!(is_non_speech_annotation("(background noise)"));
        assert!(is_non_speech_annotation("[applause]"));
        assert!(is_non_speech_annotation("(laughter)"));
        assert!(!is_non_speech_annotation("thanks for watching"));
    }

    fn counting_engine() -> Arc<dyn ChunkTranscriber> {
        Arc::new(CountingTranscriber {
            calls: Arc::new(AtomicUsize::new(0)),
            seen_rates: Arc::new(Mutex::new(Vec::new())),
        })
    }

    #[test]
    fn super_mode_off_frees_the_second_engine() {
        let mut service = super::TranscriptionService::default();
        service.secondary.transcriber = Some(counting_engine());
        service.secondary.active_model = Some(crate::models::SttModel::Parakeet);
        let settings = crate::settings::Settings {
            super_mode: crate::settings::SuperModeSetting::Off,
            ..crate::settings::Settings::default()
        };
        let engines = service.super_mode_engines(
            &settings,
            &crate::models::ModelService::default(),
            &counting_engine(),
        );
        assert!(engines.is_none());
        assert!(service.secondary.transcriber.is_none());
        assert!(service.secondary.active_model.is_none());
    }
}
