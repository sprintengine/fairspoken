use crate::audio::{AudioFrame, Recording};
use crate::models::{ModelService, SherpaModel, WhisperModel};
use crate::remote_transcription::{
    backend_id, decode_wav, read_stream_frame, selected_model_id, RemoteHealth,
    RemoteTranscriptionResponse,
};
use crate::settings::{Settings, TranscriptionBackend};
use crate::transcription::TranscriptionService;
use serde::Serialize;
use std::collections::VecDeque;
use std::env;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tiny_http::{Header, Method, Request, Response, Server, StatusCode};

const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");
const DASHBOARD_HTML: &str = include_str!("host_dashboard.html");
const RECENT_CAPACITY: usize = 50;
const DEFAULT_HOST_WORKERS: usize = 1;
const DEFAULT_HOST_QUEUE_CAPACITY: usize = 8;
const DEFAULT_HOST_MAX_ACTIVE_STREAMS: u32 = 4;
const DEFAULT_HOST_MAX_RECORDING_SECONDS: u16 = 120;

pub fn run_transcription_host() -> Result<(), String> {
    let addr = env::var("MULTIVOICE_HOST_ADDR").unwrap_or_else(|_| "127.0.0.1:48173".to_string());
    let token = env::var("MULTIVOICE_HOST_TOKEN").ok();
    let server = Server::http(&addr)
        .map_err(|err| format!("Failed to start transcription host on {addr}: {err}"))?;
    println!("multivoice transcription host listening on http://{addr}");

    let models = ModelService::default();
    let config = HostRuntimeConfig::from_env();
    let metrics = Arc::new(Mutex::new(HostMetrics::new(&config)));
    let runtime = Arc::new(HostRuntime::start(
        models.clone(),
        config,
        Arc::clone(&metrics),
    )?);

    for request in server.incoming_requests() {
        let response = handle_request(
            request,
            token.as_deref(),
            Arc::clone(&runtime),
            Arc::clone(&metrics),
            &addr,
        );
        if let Err(err) = response {
            eprintln!("Failed to handle transcription host request: {err}");
        }
    }

    Ok(())
}

fn handle_request(
    request: Request,
    token: Option<&str>,
    runtime: Arc<HostRuntime>,
    metrics: Arc<Mutex<HostMetrics>>,
    bind_addr: &str,
) -> Result<(), String> {
    // The dashboard HTML itself is safe to serve without auth — it contains no secrets
    // and gates its own data calls behind the token. Every other route still authenticates.
    if matches!((request.method(), request.url()), (&Method::Get, "/")) {
        return respond_html(request, DASHBOARD_HTML);
    }

    if !authorized(&request, token) {
        return respond_json(
            request,
            StatusCode(401),
            r#"{"error":"unauthorized"}"#.to_string(),
        );
    }

    match (request.method(), request.url()) {
        (&Method::Get, "/v1/health") => {
            let health = RemoteHealth {
                ok: true,
                mode: "standalone-host".to_string(),
                backend: "client-selected".to_string(),
                server_version: Some(SERVER_VERSION.to_string()),
            };
            respond_json(
                request,
                StatusCode(200),
                serde_json::to_string(&health)
                    .map_err(|err| format!("Failed to serialize health response: {err}"))?,
            )
        }
        (&Method::Get, "/v1/stats") => {
            let metrics = metrics
                .lock()
                .map_err(|_| "Host metrics lock failed".to_string())?;
            let snapshot = metrics.snapshot(bind_addr);
            respond_json(
                request,
                StatusCode(200),
                serde_json::to_string(&snapshot)
                    .map_err(|err| format!("Failed to serialize stats response: {err}"))?,
            )
        }
        (&Method::Post, "/v1/transcriptions") => {
            spawn_transcription_request(request, runtime, TranscriptionRequestKind::Batch)
        }
        (&Method::Post, "/v1/transcriptions/stream") => {
            spawn_transcription_request(request, runtime, TranscriptionRequestKind::Stream)
        }
        _ => respond_json(
            request,
            StatusCode(404),
            r#"{"error":"not_found"}"#.to_string(),
        ),
    }
}

#[derive(Clone, Copy)]
enum TranscriptionRequestKind {
    Batch,
    Stream,
}

fn spawn_transcription_request(
    request: Request,
    runtime: Arc<HostRuntime>,
    kind: TranscriptionRequestKind,
) -> Result<(), String> {
    thread::Builder::new()
        .name("transcription-host-request".to_string())
        .spawn(move || {
            let result = match kind {
                TranscriptionRequestKind::Batch => handle_batch_transcription(request, runtime),
                TranscriptionRequestKind::Stream => handle_stream_transcription(request, runtime),
            };
            if let Err(err) = result {
                eprintln!("Failed to handle transcription host request: {err}");
            }
        })
        .map(|_| ())
        .map_err(|err| format!("Failed to start transcription request worker: {err}"))
}

fn handle_batch_transcription(
    mut request: Request,
    runtime: Arc<HostRuntime>,
) -> Result<(), String> {
    let mut body = Vec::new();
    if let Err(err) = request.as_reader().read_to_end(&mut body) {
        return respond_error(
            request,
            StatusCode(400),
            &format!("Failed to read transcription request body: {err}"),
        );
    }
    let settings = match settings_from_headers(&request) {
        Ok(settings) => settings,
        Err(err) => return respond_error(request, StatusCode(400), &err),
    };
    let recording = match decode_wav(&body) {
        Ok(recording) => recording,
        Err(err) => return respond_error(request, StatusCode(400), &err),
    };
    let duration_seconds = recording.stats().duration_seconds;
    if let Err(err) = runtime.validate_recording_duration(duration_seconds) {
        runtime.record_rejection();
        return respond_error(request, StatusCode(413), &err);
    }
    let text = match runtime.transcribe(recording, settings.clone(), "batch") {
        Ok(text) => text,
        Err(HostRuntimeError::QueueFull(message)) => {
            return respond_error(request, StatusCode(429), &message)
        }
        Err(HostRuntimeError::WorkerFailed(message)) => {
            return respond_error(request, StatusCode(500), &message)
        }
    };
    let response = transcription_response(text, duration_seconds, &settings);
    respond_json(
        request,
        StatusCode(200),
        serde_json::to_string(&response)
            .map_err(|err| format!("Failed to serialize transcription response: {err}"))?,
    )
}

fn handle_stream_transcription(
    mut request: Request,
    runtime: Arc<HostRuntime>,
) -> Result<(), String> {
    let settings = match settings_from_headers(&request) {
        Ok(settings) => settings,
        Err(err) => return respond_error(request, StatusCode(400), &err),
    };
    let stream_guard = match runtime.try_begin_stream() {
        Ok(guard) => guard,
        Err(message) => return respond_error(request, StatusCode(429), &message),
    };
    let recording = match read_stream_recording(&mut request, runtime.max_recording_seconds()) {
        Ok(recording) => recording,
        Err(err) => {
            runtime.record_rejection();
            return respond_error(request, stream_recording_error_status(&err), &err);
        }
    };
    drop(stream_guard);

    let duration_seconds = recording.stats().duration_seconds;
    let text = match runtime.transcribe(recording, settings.clone(), "stream") {
        Ok(text) => text,
        Err(HostRuntimeError::QueueFull(message)) => {
            return respond_error(request, StatusCode(429), &message)
        }
        Err(HostRuntimeError::WorkerFailed(message)) => {
            return respond_error(request, StatusCode(500), &message)
        }
    };
    let response = transcription_response(text, duration_seconds, &settings);
    respond_json(
        request,
        StatusCode(200),
        serde_json::to_string(&response).map_err(|err| {
            format!("Failed to serialize streaming transcription response: {err}")
        })?,
    )
}

#[derive(Clone, Copy)]
struct HostRuntimeConfig {
    worker_count: usize,
    queue_capacity: usize,
    max_active_streams: u32,
    max_recording_seconds: u16,
}

impl HostRuntimeConfig {
    fn from_env() -> Self {
        Self {
            worker_count: env_usize("MULTIVOICE_HOST_WORKERS", DEFAULT_HOST_WORKERS).clamp(1, 4),
            queue_capacity: env_usize(
                "MULTIVOICE_HOST_QUEUE_CAPACITY",
                DEFAULT_HOST_QUEUE_CAPACITY,
            )
            .clamp(1, 64),
            max_active_streams: env_u32(
                "MULTIVOICE_HOST_MAX_ACTIVE_STREAMS",
                DEFAULT_HOST_MAX_ACTIVE_STREAMS,
            )
            .clamp(1, 32),
            max_recording_seconds: env_u16(
                "MULTIVOICE_HOST_MAX_RECORDING_SECONDS",
                DEFAULT_HOST_MAX_RECORDING_SECONDS,
            )
            .clamp(10, 300),
        }
    }
}

fn env_usize(name: &str, default: usize) -> usize {
    env::var(name)
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(default)
}

fn env_u32(name: &str, default: u32) -> u32 {
    env::var(name)
        .ok()
        .and_then(|value| value.parse::<u32>().ok())
        .unwrap_or(default)
}

fn env_u16(name: &str, default: u16) -> u16 {
    env::var(name)
        .ok()
        .and_then(|value| value.parse::<u16>().ok())
        .unwrap_or(default)
}

struct HostRuntime {
    job_tx: SyncSender<TranscriptionJob>,
    metrics: Arc<Mutex<HostMetrics>>,
    config: HostRuntimeConfig,
    next_job_id: AtomicU64,
}

struct TranscriptionJob {
    id: u64,
    settings: Settings,
    recording: Recording,
    source: &'static str,
    result_tx: mpsc::Sender<Result<String, String>>,
    accepted_at: Instant,
}

enum HostRuntimeError {
    QueueFull(String),
    WorkerFailed(String),
}

struct ActiveStreamGuard {
    metrics: Arc<Mutex<HostMetrics>>,
}

impl Drop for ActiveStreamGuard {
    fn drop(&mut self) {
        if let Ok(mut metrics) = self.metrics.lock() {
            metrics.finish_stream();
        }
    }
}

impl HostRuntime {
    fn start(
        models: ModelService,
        config: HostRuntimeConfig,
        metrics: Arc<Mutex<HostMetrics>>,
    ) -> Result<Self, String> {
        let (job_tx, job_rx) = mpsc::sync_channel::<TranscriptionJob>(config.queue_capacity);
        let job_rx = Arc::new(Mutex::new(job_rx));

        for worker_index in 0..config.worker_count {
            let worker_rx = Arc::clone(&job_rx);
            let worker_models = models.clone();
            let worker_metrics = Arc::clone(&metrics);
            thread::Builder::new()
                .name(format!("transcription-host-worker-{worker_index}"))
                .spawn(move || {
                    run_host_worker(worker_index, worker_rx, worker_models, worker_metrics);
                })
                .map_err(|err| format!("Failed to start transcription host worker: {err}"))?;
        }

        Ok(Self {
            job_tx,
            metrics,
            config,
            next_job_id: AtomicU64::new(1),
        })
    }

    fn max_recording_seconds(&self) -> u16 {
        self.config.max_recording_seconds
    }

    fn try_begin_stream(&self) -> Result<ActiveStreamGuard, String> {
        let mut metrics = self
            .metrics
            .lock()
            .map_err(|_| "Host metrics lock failed".to_string())?;
        if metrics.active_streams >= self.config.max_active_streams {
            metrics.reject_job();
            return Err("Server is at active stream capacity".to_string());
        }
        metrics.begin_stream();
        Ok(ActiveStreamGuard {
            metrics: Arc::clone(&self.metrics),
        })
    }

    fn validate_recording_duration(&self, duration_seconds: f32) -> Result<(), String> {
        if duration_seconds > f32::from(self.config.max_recording_seconds) {
            return Err(format!(
                "Recording exceeds host maximum of {} seconds",
                self.config.max_recording_seconds
            ));
        }
        Ok(())
    }

    fn record_rejection(&self) {
        if let Ok(mut metrics) = self.metrics.lock() {
            metrics.reject_job();
        }
    }

    fn transcribe(
        &self,
        recording: Recording,
        settings: Settings,
        source: &'static str,
    ) -> Result<String, HostRuntimeError> {
        let duration_seconds = recording.stats().duration_seconds;
        if let Err(err) = self.validate_recording_duration(duration_seconds) {
            self.metrics
                .lock()
                .map(|mut metrics| metrics.reject_job())
                .ok();
            return Err(HostRuntimeError::QueueFull(err));
        }

        let (result_tx, result_rx) = mpsc::channel::<Result<String, String>>();
        let job = TranscriptionJob {
            id: self.next_job_id.fetch_add(1, Ordering::Relaxed),
            settings,
            recording,
            source,
            result_tx,
            accepted_at: Instant::now(),
        };

        if let Ok(mut metrics) = self.metrics.lock() {
            metrics.enqueue_job();
        }

        match self.job_tx.try_send(job) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {
                if let Ok(mut metrics) = self.metrics.lock() {
                    metrics.dequeue_job();
                    metrics.reject_job();
                }
                return Err(HostRuntimeError::QueueFull(
                    "Transcription queue is full".to_string(),
                ));
            }
            Err(TrySendError::Disconnected(_)) => {
                if let Ok(mut metrics) = self.metrics.lock() {
                    metrics.dequeue_job();
                }
                return Err(HostRuntimeError::WorkerFailed(
                    "Transcription worker queue is unavailable".to_string(),
                ));
            }
        }

        result_rx
            .recv()
            .map_err(|_| {
                HostRuntimeError::WorkerFailed(
                    "Transcription worker stopped without a result".to_string(),
                )
            })?
            .map_err(HostRuntimeError::WorkerFailed)
    }
}

fn run_host_worker(
    worker_index: usize,
    job_rx: Arc<Mutex<Receiver<TranscriptionJob>>>,
    models: ModelService,
    metrics: Arc<Mutex<HostMetrics>>,
) {
    let mut transcription = TranscriptionService::default();
    loop {
        let job = {
            let receiver = match job_rx.lock() {
                Ok(receiver) => receiver,
                Err(_) => {
                    eprintln!("Transcription host worker {worker_index} receiver lock failed");
                    break;
                }
            };
            match receiver.recv() {
                Ok(job) => job,
                Err(_) => break,
            }
        };

        let queue_wait = job.accepted_at.elapsed();
        if let Ok(mut metrics) = metrics.lock() {
            metrics.start_job(queue_wait);
        }

        let started = Instant::now();
        let duration_seconds = job.recording.stats().duration_seconds;
        let backend = backend_id(job.settings.transcription_backend).to_string();
        let model = selected_model_id(&job.settings).to_string();
        let source = job.source;
        let result =
            transcribe_recording(&mut transcription, &models, &job.settings, job.recording);
        let processing_time = started.elapsed();

        match &result {
            Ok(_) => {
                if let Ok(mut metrics) = metrics.lock() {
                    metrics.complete_job(
                        duration_seconds,
                        backend,
                        model,
                        source,
                        queue_wait,
                        processing_time,
                    );
                }
            }
            Err(_) => {
                if let Ok(mut metrics) = metrics.lock() {
                    metrics.fail_job();
                }
            }
        }

        if job.result_tx.send(result).is_err() {
            eprintln!(
                "Transcription host worker {worker_index} completed job {} after requester disconnected",
                job.id
            );
        }
    }
}

fn transcription_response(
    text: String,
    duration_seconds: f32,
    settings: &Settings,
) -> RemoteTranscriptionResponse {
    RemoteTranscriptionResponse {
        text,
        duration_seconds,
        backend: backend_id(settings.transcription_backend).to_string(),
        model: selected_model_id(settings).to_string(),
        server_version: Some(SERVER_VERSION.to_string()),
    }
}

fn respond_error(request: Request, status: StatusCode, message: &str) -> Result<(), String> {
    let body = serde_json::json!({ "error": message }).to_string();
    respond_json(request, status, body)
}

fn read_stream_recording(
    request: &mut Request,
    max_recording_seconds: u16,
) -> Result<Recording, String> {
    let mut pcm_i16 = Vec::new();
    let mut sample_rate = 16_000;

    loop {
        let Some(frame) = read_stream_frame(&mut request.as_reader())? else {
            break;
        };
        if frame.pcm_i16.is_empty() {
            continue;
        }
        sample_rate = frame.sample_rate;
        let max_samples = sample_rate as usize * usize::from(max_recording_seconds);
        if pcm_i16.len().saturating_add(frame.pcm_i16.len()) > max_samples {
            return Err(format!(
                "Recording exceeds host maximum of {max_recording_seconds} seconds"
            ));
        }
        pcm_i16.extend_from_slice(&frame.pcm_i16);
    }

    if pcm_i16.is_empty() {
        return Err("No audio samples were captured".to_string());
    }

    Ok(Recording {
        pcm_i16,
        sample_rate,
        dropped_stream_frames: 0,
    })
}

fn stream_recording_error_status(message: &str) -> StatusCode {
    if message == "No audio samples were captured" {
        StatusCode(400)
    } else {
        StatusCode(413)
    }
}

fn transcribe_recording(
    transcription: &mut TranscriptionService,
    models: &ModelService,
    settings: &Settings,
    recording: crate::audio::Recording,
) -> Result<String, String> {
    let stream_sink = transcription.start_session(settings, models)?;
    if let Some(sink) = stream_sink {
        for chunk in recording.pcm_i16.chunks(3200) {
            if sink
                .send(AudioFrame {
                    pcm_i16: chunk.to_vec(),
                    sample_rate: recording.sample_rate,
                })
                .is_err()
            {
                transcription.cancel_session();
                return Err("Sherpa transcription worker stopped while receiving audio".to_string());
            }
        }
    }

    transcription.finish_session(&recording, settings, models)
}

fn settings_from_headers(request: &Request) -> Result<Settings, String> {
    let backend = header_value(request, "x-multivoice-backend").unwrap_or("whisper");
    let model = header_value(request, "x-multivoice-model").unwrap_or("base");
    let language = header_value(request, "x-multivoice-language")
        .unwrap_or("en")
        .to_string();
    let whisper_chunk_seconds = header_value(request, "x-multivoice-whisper-chunk-seconds")
        .and_then(|value| value.parse::<u16>().ok())
        .unwrap_or(Settings::default().whisper_chunk_seconds);
    let vocabulary_hints = header_value(request, "x-multivoice-vocabulary-hints")
        .map(percent_decode)
        .and_then(|value| serde_json::from_str::<Vec<String>>(&value).ok())
        .unwrap_or_default();

    let mut settings = Settings::default();
    settings.language = language;
    settings.whisper_chunk_seconds = whisper_chunk_seconds.clamp(5, 60);
    settings.vocabulary_hints = vocabulary_hints;
    settings.transcription_backend = match backend {
        "whisper" => {
            settings.model = WhisperModel::from_model_id(model)
                .ok_or_else(|| format!("Unsupported Whisper model: {model}"))?;
            TranscriptionBackend::Whisper
        }
        "sherpa-streaming" | "sherpa" => {
            settings.sherpa_model = SherpaModel::from_model_id(model)
                .ok_or_else(|| format!("Unsupported Sherpa model: {model}"))?;
            TranscriptionBackend::SherpaStreaming
        }
        other => return Err(format!("Unsupported transcription backend: {other}")),
    };
    Ok(settings)
}

fn authorized(request: &Request, token: Option<&str>) -> bool {
    let Some(token) = token.filter(|value| !value.trim().is_empty()) else {
        return true;
    };
    let bearer = format!("Bearer {token}");
    if header_value(request, "authorization")
        .map(|value| value == bearer)
        .unwrap_or(false)
    {
        return true;
    }
    // Allow the dashboard's GET polling to authenticate via ?token=… so the host
    // operator can bookmark a single URL in the browser.
    query_param(request.url(), "token")
        .map(|value| value == token)
        .unwrap_or(false)
}

fn query_param(url: &str, name: &str) -> Option<String> {
    let query_start = url.find('?')? + 1;
    let query = &url[query_start..];
    for pair in query.split('&') {
        let mut parts = pair.splitn(2, '=');
        let key = parts.next()?;
        if key == name {
            let raw_value = parts.next().unwrap_or("");
            return Some(percent_decode(raw_value));
        }
    }
    None
}

fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'+' {
            out.push(b' ');
            i += 1;
        } else if b == b'%' && i + 2 < bytes.len() {
            let hi = (bytes[i + 1] as char).to_digit(16);
            let lo = (bytes[i + 2] as char).to_digit(16);
            match (hi, lo) {
                (Some(hi), Some(lo)) => {
                    out.push(((hi << 4) | lo) as u8);
                    i += 3;
                }
                _ => {
                    out.push(b);
                    i += 1;
                }
            }
        } else {
            out.push(b);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn header_value<'a>(request: &'a Request, name: &str) -> Option<&'a str> {
    request
        .headers()
        .iter()
        .find(|header| header.field.to_string().eq_ignore_ascii_case(name))
        .map(|header| header.value.as_str())
}

fn respond_json(request: Request, status: StatusCode, body: String) -> Result<(), String> {
    let content_type = Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..])
        .map_err(|_| "Failed to create response content-type header".to_string())?;
    request
        .respond(
            Response::from_string(body)
                .with_status_code(status)
                .with_header(content_type),
        )
        .map_err(|err| format!("Failed to send response: {err}"))
}

fn respond_html(request: Request, body: &str) -> Result<(), String> {
    let content_type = Header::from_bytes(&b"Content-Type"[..], &b"text/html; charset=utf-8"[..])
        .map_err(|_| "Failed to create response content-type header".to_string())?;
    request
        .respond(
            Response::from_string(body)
                .with_status_code(StatusCode(200))
                .with_header(content_type),
        )
        .map_err(|err| format!("Failed to send response: {err}"))
}

struct HostMetrics {
    started: Instant,
    worker_count: usize,
    queue_capacity: usize,
    active_streams: u32,
    queued_jobs: u32,
    running_jobs: u32,
    rejected_jobs: u64,
    failed_jobs: u64,
    started_jobs: u64,
    total_transcriptions: u64,
    total_audio_seconds: f64,
    total_queue_wait_ms: u128,
    total_processing_ms: u128,
    next_record_id: u64,
    recent: VecDeque<TranscriptionRecord>,
}

impl HostMetrics {
    fn new(config: &HostRuntimeConfig) -> Self {
        Self {
            started: Instant::now(),
            worker_count: config.worker_count,
            queue_capacity: config.queue_capacity,
            active_streams: 0,
            queued_jobs: 0,
            running_jobs: 0,
            rejected_jobs: 0,
            failed_jobs: 0,
            started_jobs: 0,
            total_transcriptions: 0,
            total_audio_seconds: 0.0,
            total_queue_wait_ms: 0,
            total_processing_ms: 0,
            next_record_id: 0,
            recent: VecDeque::with_capacity(RECENT_CAPACITY),
        }
    }

    fn begin_stream(&mut self) {
        self.active_streams = self.active_streams.saturating_add(1);
    }

    fn finish_stream(&mut self) {
        self.active_streams = self.active_streams.saturating_sub(1);
    }

    fn enqueue_job(&mut self) {
        self.queued_jobs = self.queued_jobs.saturating_add(1);
    }

    fn dequeue_job(&mut self) {
        self.queued_jobs = self.queued_jobs.saturating_sub(1);
    }

    fn start_job(&mut self, queue_wait: Duration) {
        self.dequeue_job();
        self.running_jobs = self.running_jobs.saturating_add(1);
        self.started_jobs = self.started_jobs.saturating_add(1);
        self.total_queue_wait_ms = self
            .total_queue_wait_ms
            .saturating_add(queue_wait.as_millis());
    }

    fn reject_job(&mut self) {
        self.rejected_jobs = self.rejected_jobs.saturating_add(1);
    }

    fn fail_job(&mut self) {
        self.running_jobs = self.running_jobs.saturating_sub(1);
        self.failed_jobs = self.failed_jobs.saturating_add(1);
    }

    fn complete_job(
        &mut self,
        duration_seconds: f32,
        backend: String,
        model: String,
        source: &'static str,
        queue_wait: Duration,
        processing_time: Duration,
    ) {
        self.running_jobs = self.running_jobs.saturating_sub(1);
        self.total_transcriptions = self.total_transcriptions.saturating_add(1);
        self.total_audio_seconds += duration_seconds as f64;
        self.total_processing_ms = self
            .total_processing_ms
            .saturating_add(processing_time.as_millis());
        self.next_record_id = self.next_record_id.saturating_add(1);

        let completed_at_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);

        self.recent.push_front(TranscriptionRecord {
            id: self.next_record_id,
            completed_at_ms,
            duration_seconds,
            backend,
            model,
            source,
            queue_wait_ms: queue_wait.as_millis() as u64,
            processing_ms: processing_time.as_millis() as u64,
        });
        while self.recent.len() > RECENT_CAPACITY {
            self.recent.pop_back();
        }
    }

    fn snapshot(&self, bind_addr: &str) -> StatsSnapshot<'_> {
        StatsSnapshot {
            server_version: SERVER_VERSION,
            bind_addr: bind_addr.to_string(),
            uptime_seconds: self.started.elapsed().as_secs(),
            active_sessions: self.active_streams.saturating_add(self.running_jobs),
            active_streams: self.active_streams,
            queued_jobs: self.queued_jobs,
            running_jobs: self.running_jobs,
            worker_count: self.worker_count,
            queue_capacity: self.queue_capacity,
            rejected_jobs: self.rejected_jobs,
            failed_jobs: self.failed_jobs,
            total_transcriptions: self.total_transcriptions,
            total_audio_seconds: self.total_audio_seconds,
            average_queue_ms: average_ms(self.total_queue_wait_ms, self.started_jobs),
            average_processing_ms: average_ms(self.total_processing_ms, self.total_transcriptions),
            recent: self.recent.iter().collect(),
        }
    }
}

fn average_ms(total: u128, count: u64) -> u64 {
    if count == 0 {
        0
    } else {
        (total / u128::from(count)) as u64
    }
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct TranscriptionRecord {
    id: u64,
    completed_at_ms: u64,
    duration_seconds: f32,
    backend: String,
    model: String,
    source: &'static str,
    queue_wait_ms: u64,
    processing_ms: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct StatsSnapshot<'a> {
    server_version: &'static str,
    bind_addr: String,
    uptime_seconds: u64,
    active_sessions: u32,
    active_streams: u32,
    queued_jobs: u32,
    running_jobs: u32,
    worker_count: usize,
    queue_capacity: usize,
    rejected_jobs: u64,
    failed_jobs: u64,
    total_transcriptions: u64,
    total_audio_seconds: f64,
    average_queue_ms: u64,
    average_processing_ms: u64,
    recent: Vec<&'a TranscriptionRecord>,
}

#[cfg(test)]
mod tests {
    use super::{HostMetrics, HostRuntime, HostRuntimeConfig, HostRuntimeError};
    use crate::audio::Recording;
    use crate::settings::Settings;
    use std::sync::atomic::AtomicU64;
    use std::sync::mpsc;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    fn test_config() -> HostRuntimeConfig {
        HostRuntimeConfig {
            worker_count: 1,
            queue_capacity: 1,
            max_active_streams: 1,
            max_recording_seconds: 10,
        }
    }

    fn test_runtime(
        config: HostRuntimeConfig,
        job_tx: mpsc::SyncSender<super::TranscriptionJob>,
        metrics: Arc<Mutex<HostMetrics>>,
    ) -> HostRuntime {
        HostRuntime {
            job_tx,
            metrics,
            config,
            next_job_id: AtomicU64::new(1),
        }
    }

    fn test_recording(seconds: usize) -> Recording {
        Recording {
            pcm_i16: vec![1; 16_000 * seconds],
            sample_rate: 16_000,
            dropped_stream_frames: 0,
        }
    }

    #[test]
    fn stream_guard_releases_active_stream_capacity_on_drop() {
        let config = test_config();
        let metrics = Arc::new(Mutex::new(HostMetrics::new(&config)));
        let (job_tx, _job_rx) = mpsc::sync_channel(1);
        let runtime = test_runtime(config, job_tx, Arc::clone(&metrics));

        let guard = runtime.try_begin_stream().expect("first stream admitted");
        let err = match runtime.try_begin_stream() {
            Ok(_) => panic!("second stream should exceed capacity"),
            Err(err) => err,
        };

        assert_eq!(err, "Server is at active stream capacity");
        assert_eq!(metrics.lock().expect("metrics").active_streams, 1);

        drop(guard);

        assert_eq!(metrics.lock().expect("metrics").active_streams, 0);
        let _next_guard = runtime
            .try_begin_stream()
            .expect("capacity should be released");
    }

    #[test]
    fn full_queue_rejects_without_leaving_queued_metrics() {
        let config = HostRuntimeConfig {
            queue_capacity: 0,
            ..test_config()
        };
        let metrics = Arc::new(Mutex::new(HostMetrics::new(&config)));
        let (job_tx, _job_rx) = mpsc::sync_channel(0);
        let runtime = test_runtime(config, job_tx, Arc::clone(&metrics));

        let err = runtime
            .transcribe(test_recording(1), Settings::default(), "stream")
            .expect_err("zero-capacity queue should reject");

        match err {
            HostRuntimeError::QueueFull(message) => {
                assert_eq!(message, "Transcription queue is full");
            }
            HostRuntimeError::WorkerFailed(message) => {
                panic!("unexpected worker failure: {message}");
            }
        }

        let metrics = metrics.lock().expect("metrics");
        assert_eq!(metrics.queued_jobs, 0);
        assert_eq!(metrics.rejected_jobs, 1);
        assert_eq!(metrics.running_jobs, 0);
    }

    #[test]
    fn oversized_recording_rejects_before_enqueue() {
        let config = test_config();
        let metrics = Arc::new(Mutex::new(HostMetrics::new(&config)));
        let (job_tx, _job_rx) = mpsc::sync_channel(1);
        let runtime = test_runtime(config, job_tx, Arc::clone(&metrics));

        let err = runtime
            .transcribe(test_recording(11), Settings::default(), "batch")
            .expect_err("oversized recording should reject");

        match err {
            HostRuntimeError::QueueFull(message) => {
                assert!(message.contains("Recording exceeds host maximum"));
            }
            HostRuntimeError::WorkerFailed(message) => {
                panic!("unexpected worker failure: {message}");
            }
        }

        let metrics = metrics.lock().expect("metrics");
        assert_eq!(metrics.queued_jobs, 0);
        assert_eq!(metrics.rejected_jobs, 1);
        assert_eq!(metrics.running_jobs, 0);
    }

    #[test]
    fn metrics_snapshot_separates_stream_queue_and_running_work() {
        let config = HostRuntimeConfig {
            worker_count: 2,
            queue_capacity: 4,
            max_active_streams: 3,
            max_recording_seconds: 120,
        };
        let mut metrics = HostMetrics::new(&config);

        metrics.begin_stream();
        metrics.enqueue_job();
        metrics.start_job(Duration::from_millis(30));
        metrics.complete_job(
            2.5,
            "whisper".to_string(),
            "large-v3-turbo".to_string(),
            "stream",
            Duration::from_millis(30),
            Duration::from_millis(90),
        );

        let snapshot = metrics.snapshot("127.0.0.1:48173");

        assert_eq!(snapshot.active_streams, 1);
        assert_eq!(snapshot.queued_jobs, 0);
        assert_eq!(snapshot.running_jobs, 0);
        assert_eq!(snapshot.worker_count, 2);
        assert_eq!(snapshot.queue_capacity, 4);
        assert_eq!(snapshot.total_transcriptions, 1);
        assert_eq!(snapshot.average_queue_ms, 30);
        assert_eq!(snapshot.average_processing_ms, 90);
        assert_eq!(snapshot.recent.len(), 1);
        assert_eq!(snapshot.recent[0].queue_wait_ms, 30);
        assert_eq!(snapshot.recent[0].processing_ms, 90);
    }

    #[test]
    fn failed_job_clears_running_state_and_counts_failure() {
        let config = test_config();
        let mut metrics = HostMetrics::new(&config);

        metrics.enqueue_job();
        metrics.start_job(Duration::from_millis(5));
        metrics.fail_job();

        assert_eq!(metrics.queued_jobs, 0);
        assert_eq!(metrics.running_jobs, 0);
        assert_eq!(metrics.failed_jobs, 1);
        assert_eq!(metrics.total_transcriptions, 0);
    }
}
