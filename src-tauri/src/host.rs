use crate::audio::{AudioFrame, Recording};
use crate::models::{ModelService, WhisperModel};
use crate::remote_transcription::{
    decode_wav, read_stream_frame, RemoteHealth, RemoteTranscriptionResponse, BACKEND_ID,
};
use crate::settings::Settings;
use crate::transcription::TranscriptionService;
use serde::{Deserialize, Serialize};
use std::cmp::Reverse;
use std::collections::{HashMap, VecDeque};
use std::env;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tiny_http::{Header, Method, Request, Response, Server, StatusCode};

const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");
const DASHBOARD_HTML: &str = include_str!("host_dashboard.html");
const RECENT_CAPACITY: usize = 50;
const DEFAULT_HOST_WORKERS: usize = 1;
const DEFAULT_HOST_QUEUE_CAPACITY: usize = 8;
const DEFAULT_HOST_MAX_ACTIVE_STREAMS: u32 = 4;
// Matches the longest client max-recording option so the client setting is
// the effective limit; this also sizes the host's upload-buffer bounds.
const DEFAULT_HOST_MAX_RECORDING_SECONDS: u16 = 600;
const MAX_STREAM_SAMPLE_RATE: u32 = 192_000;
const MAX_BATCH_WAV_BYTES_PER_SECOND: u64 = (MAX_STREAM_SAMPLE_RATE as u64) * 2;
const MAX_BATCH_WAV_HEADER_BYTES: u64 = 64 * 1024;
const MAX_TRACKED_CLIENTS: usize = 32;
const MIN_HOST_MAX_ACTIVE_STREAMS: u32 = 1;
const MAX_HOST_MAX_ACTIVE_STREAMS: u32 = 32;
const MIN_HOST_MAX_RECORDING_SECONDS: u16 = 10;
const MAX_HOST_MAX_RECORDING_SECONDS: u16 = 600;
const DEFAULT_HOST_MODEL: WhisperModel = WhisperModel::Base;
// How long an idle worker blocks on the job queue before re-checking its
// assigned model, so runtime model changes and freshly downloaded model files
// are picked up without a job arriving.
const WORKER_IDLE_POLL: Duration = Duration::from_millis(250);

pub fn run_transcription_host() -> Result<(), String> {
    let addr = env::var("MULTIVOICE_HOST_ADDR").unwrap_or_else(|_| "127.0.0.1:48173".to_string());
    let token = env::var("MULTIVOICE_HOST_TOKEN").ok();
    let mut config = HostRuntimeConfig::from_env()?;
    // Dashboard edits are the durable configuration; the environment only
    // seeds the first boot (or a deleted config file).
    let config_path = default_host_config_path();
    if let Some(persisted) = load_persisted_config(&config_path)? {
        overlay_persisted_config(&mut config, persisted);
    }
    let server = Server::http(&addr)
        .map_err(|err| format!("Failed to start transcription host on {addr}: {err}"))?;
    println!("multivoice transcription host listening on http://{addr}");

    let models = ModelService::default();
    let metrics = Arc::new(Mutex::new(HostMetrics::new(&config)));
    let runtime = Arc::new(HostRuntime::start(
        models.clone(),
        config,
        Arc::clone(&metrics),
        config_path,
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
    let path = request_path(request.url());
    // The dashboard HTML itself is safe to serve without auth — it contains no secrets
    // and gates its own data calls behind the token. Every other route still authenticates.
    if matches!((request.method(), path), (&Method::Get, "/")) {
        return respond_html(request, DASHBOARD_HTML);
    }
    // Browsers request a favicon alongside the dashboard; answer it before
    // auth so it never surfaces as a 401 in the operator's console.
    if matches!((request.method(), path), (&Method::Get, "/favicon.ico")) {
        return request
            .respond(Response::empty(StatusCode(204)))
            .map_err(|err| format!("Failed to send favicon response: {err}"));
    }

    if !authorized(&request, token) {
        return respond_json(
            request,
            StatusCode(401),
            r#"{"error":"unauthorized"}"#.to_string(),
        );
    }

    match (request.method(), path) {
        (&Method::Get, "/v1/health") => {
            let health = RemoteHealth {
                ok: true,
                mode: "standalone-host".to_string(),
                backend: BACKEND_ID.to_string(),
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
            let snapshot = metrics.snapshot(bind_addr, &runtime.live, &runtime.models);
            respond_json(
                request,
                StatusCode(200),
                serde_json::to_string(&snapshot)
                    .map_err(|err| format!("Failed to serialize stats response: {err}"))?,
            )
        }
        (&Method::Post, "/v1/config") => handle_config_update(request, runtime),
        (&Method::Post, "/v1/models/download") => handle_model_download(request, runtime),
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

fn request_path(url: &str) -> &str {
    url.split_once('?').map(|(path, _)| path).unwrap_or(url)
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
    let client = client_ip(&request);
    runtime.record_client_request(client.as_deref());
    let body = match read_limited_body(
        &mut request.as_reader(),
        max_batch_body_bytes(runtime.max_recording_seconds()),
    ) {
        Ok(body) => body,
        Err(err) => {
            runtime.record_rejection(client.as_deref());
            return respond_error(request, StatusCode(413), &err);
        }
    };
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
        runtime.record_rejection(client.as_deref());
        return respond_error(request, StatusCode(413), &err);
    }
    let outcome = match runtime.transcribe(recording, settings, "batch", client) {
        Ok(outcome) => outcome,
        Err(HostRuntimeError::QueueFull(message)) => {
            return respond_error(request, StatusCode(429), &message)
        }
        Err(HostRuntimeError::WorkerFailed(message)) => {
            return respond_error(request, StatusCode(500), &message)
        }
    };
    let response = transcription_response(outcome, duration_seconds);
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
    let client = client_ip(&request);
    runtime.record_client_request(client.as_deref());
    let settings = match settings_from_headers(&request) {
        Ok(settings) => settings,
        Err(err) => return respond_error(request, StatusCode(400), &err),
    };
    let stream_guard = match runtime.try_begin_stream(client.clone()) {
        Ok(guard) => guard,
        Err(message) => return respond_error(request, StatusCode(429), &message),
    };
    let recording = match read_stream_recording(&mut request, runtime.max_recording_seconds()) {
        Ok(recording) => recording,
        Err(err) => {
            runtime.record_rejection(client.as_deref());
            return respond_error(request, stream_recording_error_status(&err), &err);
        }
    };
    drop(stream_guard);

    let duration_seconds = recording.stats().duration_seconds;
    let outcome = match runtime.transcribe(recording, settings, "stream", client) {
        Ok(outcome) => outcome,
        Err(HostRuntimeError::QueueFull(message)) => {
            return respond_error(request, StatusCode(429), &message)
        }
        Err(HostRuntimeError::WorkerFailed(message)) => {
            return respond_error(request, StatusCode(500), &message)
        }
    };
    let response = transcription_response(outcome, duration_seconds);
    respond_json(
        request,
        StatusCode(200),
        serde_json::to_string(&response).map_err(|err| {
            format!("Failed to serialize streaming transcription response: {err}")
        })?,
    )
}

#[derive(Clone)]
struct HostRuntimeConfig {
    worker_count: usize,
    queue_capacity: usize,
    max_active_streams: u32,
    max_recording_seconds: u16,
    use_gpu: bool,
    worker_models: Vec<WhisperModel>,
}

impl HostRuntimeConfig {
    fn from_env() -> Result<Self, String> {
        let worker_count =
            env_usize("MULTIVOICE_HOST_WORKERS", DEFAULT_HOST_WORKERS).clamp(1, 4);
        let worker_models = parse_worker_models(
            env::var("MULTIVOICE_HOST_MODEL").ok().as_deref(),
            worker_count,
        )?;
        Ok(Self {
            worker_count,
            queue_capacity: env_usize(
                "MULTIVOICE_HOST_QUEUE_CAPACITY",
                DEFAULT_HOST_QUEUE_CAPACITY,
            )
            .clamp(1, 64),
            max_active_streams: env_u32(
                "MULTIVOICE_HOST_MAX_ACTIVE_STREAMS",
                DEFAULT_HOST_MAX_ACTIVE_STREAMS,
            )
            .clamp(MIN_HOST_MAX_ACTIVE_STREAMS, MAX_HOST_MAX_ACTIVE_STREAMS),
            max_recording_seconds: env_u16(
                "MULTIVOICE_HOST_MAX_RECORDING_SECONDS",
                DEFAULT_HOST_MAX_RECORDING_SECONDS,
            )
            .clamp(
                MIN_HOST_MAX_RECORDING_SECONDS,
                MAX_HOST_MAX_RECORDING_SECONDS,
            ),
            use_gpu: env_bool("MULTIVOICE_HOST_USE_GPU", true),
            worker_models,
        })
    }
}

/// Parses `MULTIVOICE_HOST_MODEL`: a single model id serves on every worker,
/// while a comma-separated list assigns exactly one model per worker. The
/// served model is operator configuration, so an invalid value fails startup
/// instead of silently serving a default.
fn parse_worker_models(
    raw: Option<&str>,
    worker_count: usize,
) -> Result<Vec<WhisperModel>, String> {
    let Some(raw) = raw.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(vec![DEFAULT_HOST_MODEL; worker_count]);
    };
    let models = raw
        .split(',')
        .map(|id| {
            let id = id.trim();
            WhisperModel::from_model_id(id).ok_or_else(|| {
                format!("MULTIVOICE_HOST_MODEL has an unsupported Whisper model: {id}")
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    if models.len() == 1 {
        return Ok(vec![models[0]; worker_count]);
    }
    if models.len() != worker_count {
        return Err(format!(
            "MULTIVOICE_HOST_MODEL lists {} models for {worker_count} workers; provide one model or exactly one per worker",
            models.len()
        ));
    }
    Ok(models)
}

/// The dashboard-edited configuration persisted on the host, mirroring how
/// the desktop app persists its settings: the operator configures the served
/// model in the UI and it survives restarts. Environment variables only seed
/// the first boot.
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct PersistedHostConfig {
    max_active_streams: u32,
    max_recording_seconds: u16,
    use_gpu: bool,
    worker_models: Vec<WhisperModel>,
}

fn default_host_config_path() -> PathBuf {
    if let Some(path) = env::var_os("MULTIVOICE_HOST_CONFIG_PATH") {
        return PathBuf::from(path);
    }

    if let Some(local_app_data) = env::var_os("LOCALAPPDATA") {
        return PathBuf::from(local_app_data)
            .join("Multivoice Tauri")
            .join("host-config.json");
    }

    if let Some(home) = env::var_os("HOME") {
        return PathBuf::from(home)
            .join(".config")
            .join("multivoice-tauri")
            .join("host-config.json");
    }

    PathBuf::from("host-config.json")
}

/// A missing file is a normal first boot. An unreadable or invalid file fails
/// startup so the host never silently serves a configuration the operator
/// didn't choose.
fn load_persisted_config(path: &Path) -> Result<Option<PersistedHostConfig>, String> {
    let raw = match fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => {
            return Err(format!(
                "Failed to read host config {}: {err}",
                path.display()
            ))
        }
    };
    serde_json::from_str(&raw).map(Some).map_err(|err| {
        format!(
            "Invalid host config {} (fix or delete it): {err}",
            path.display()
        )
    })
}

/// Applies the persisted dashboard configuration over the env-seeded startup
/// values. Worker count stays env-owned: a persisted model list that no
/// longer matches it falls back to its first model on every worker, with a
/// logged warning instead of a silent partial assignment.
fn overlay_persisted_config(config: &mut HostRuntimeConfig, persisted: PersistedHostConfig) {
    config.max_active_streams = persisted
        .max_active_streams
        .clamp(MIN_HOST_MAX_ACTIVE_STREAMS, MAX_HOST_MAX_ACTIVE_STREAMS);
    config.max_recording_seconds = persisted
        .max_recording_seconds
        .clamp(MIN_HOST_MAX_RECORDING_SECONDS, MAX_HOST_MAX_RECORDING_SECONDS);
    config.use_gpu = persisted.use_gpu;
    if persisted.worker_models.len() == config.worker_count {
        config.worker_models = persisted.worker_models;
    } else if let Some(first) = persisted.worker_models.first().copied() {
        if persisted.worker_models.len() != 1 {
            eprintln!(
                "Host config lists {} models for {} workers; serving {} on every worker",
                persisted.worker_models.len(),
                config.worker_count,
                first.model_id()
            );
        }
        config.worker_models = vec![first; config.worker_count];
    } else {
        eprintln!(
            "Host config lists no worker models; serving {} on every worker",
            config
                .worker_models
                .first()
                .copied()
                .unwrap_or(DEFAULT_HOST_MODEL)
                .model_id()
        );
    }
}

fn persist_live_config(path: &Path, live: &HostLiveConfig) -> Result<(), String> {
    let persisted = PersistedHostConfig {
        max_active_streams: live.max_active_streams(),
        max_recording_seconds: live.max_recording_seconds(),
        use_gpu: live.use_gpu(),
        worker_models: live.worker_models(),
    };
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|err| format!("Failed to create host config directory: {err}"))?;
    }
    let payload = serde_json::to_string_pretty(&persisted)
        .map_err(|err| format!("Failed to serialize host config: {err}"))?;
    fs::write(path, payload).map_err(|err| format!("Failed to write host config: {err}"))
}

/// Knobs an operator may change at runtime through `POST /v1/config`.
/// Worker count and queue capacity stay restart-only because they size the
/// worker threads and the bounded job channel at startup; the served model is
/// per-worker host configuration, never a per-request client choice.
struct HostLiveConfig {
    max_active_streams: AtomicU32,
    max_recording_seconds: AtomicU32,
    use_gpu: AtomicBool,
    worker_models: Mutex<Vec<WhisperModel>>,
}

impl HostLiveConfig {
    fn new(config: &HostRuntimeConfig) -> Self {
        Self {
            max_active_streams: AtomicU32::new(config.max_active_streams),
            max_recording_seconds: AtomicU32::new(u32::from(config.max_recording_seconds)),
            use_gpu: AtomicBool::new(config.use_gpu),
            worker_models: Mutex::new(config.worker_models.clone()),
        }
    }

    fn max_active_streams(&self) -> u32 {
        self.max_active_streams.load(Ordering::Relaxed)
    }

    fn max_recording_seconds(&self) -> u16 {
        self.max_recording_seconds.load(Ordering::Relaxed) as u16
    }

    fn use_gpu(&self) -> bool {
        self.use_gpu.load(Ordering::Relaxed)
    }

    fn worker_models_lock(&self) -> MutexGuard<'_, Vec<WhisperModel>> {
        // The critical sections only read or swap the Vec, so a poisoned lock
        // still holds a usable value.
        self.worker_models
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn worker_model(&self, worker_index: usize) -> WhisperModel {
        self.worker_models_lock()
            .get(worker_index)
            .copied()
            .unwrap_or(DEFAULT_HOST_MODEL)
    }

    fn worker_models(&self) -> Vec<WhisperModel> {
        self.worker_models_lock().clone()
    }

    fn set_worker_models(&self, next: Vec<WhisperModel>) {
        *self.worker_models_lock() = next;
    }

    /// One model id when every worker serves the same model, otherwise
    /// "mixed" — used where a job cannot know which worker will run it.
    fn model_summary(&self) -> String {
        let models = self.worker_models_lock();
        match models.split_first() {
            Some((first, rest)) if rest.iter().all(|model| model == first) => {
                first.model_id().to_string()
            }
            Some(_) => "mixed".to_string(),
            None => DEFAULT_HOST_MODEL.model_id().to_string(),
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct HostConfigUpdate {
    max_active_streams: Option<u32>,
    max_recording_seconds: Option<u16>,
    use_gpu: Option<bool>,
    /// One model id applied to every worker.
    model: Option<String>,
    /// One model id per worker; length must match the worker count.
    worker_models: Option<Vec<String>>,
}

const MAX_CONFIG_BODY_BYTES: u64 = 4 * 1024;

fn handle_config_update(mut request: Request, runtime: Arc<HostRuntime>) -> Result<(), String> {
    let body = match read_limited_body(&mut request.as_reader(), MAX_CONFIG_BODY_BYTES) {
        Ok(body) => body,
        Err(err) => return respond_error(request, StatusCode(413), &err),
    };
    let update: HostConfigUpdate = match serde_json::from_slice(&body) {
        Ok(update) => update,
        Err(err) => {
            return respond_error(
                request,
                StatusCode(400),
                &format!("Invalid config update: {err}"),
            )
        }
    };

    if let Err(message) = apply_config_update(&update, &runtime.live) {
        return respond_error(request, StatusCode(400), &message);
    }
    // The dashboard is the durable configuration surface: a successful edit
    // must survive a restart, so a failed write is an explicit error rather
    // than a silently session-only change.
    if let Err(err) = persist_live_config(&runtime.config_path, &runtime.live) {
        return respond_error(
            request,
            StatusCode(500),
            &format!("Config applied for this session only — saving it failed: {err}"),
        );
    }

    let body = serde_json::json!({
        "maxActiveStreams": runtime.live.max_active_streams(),
        "maxRecordingSeconds": runtime.live.max_recording_seconds(),
        "useGpu": runtime.live.use_gpu(),
        "model": runtime.live.model_summary(),
        "workerModels": runtime
            .live
            .worker_models()
            .iter()
            .map(|model| model.model_id())
            .collect::<Vec<_>>(),
    })
    .to_string();
    respond_json(request, StatusCode(200), body)
}

/// Validates every supplied field before applying any of them, so a rejected
/// update never partially changes the host configuration.
fn apply_config_update(update: &HostConfigUpdate, live: &HostLiveConfig) -> Result<(), String> {
    if let Some(value) = update.max_active_streams {
        if !(MIN_HOST_MAX_ACTIVE_STREAMS..=MAX_HOST_MAX_ACTIVE_STREAMS).contains(&value) {
            return Err(format!(
                "maxActiveStreams must be between {MIN_HOST_MAX_ACTIVE_STREAMS} and {MAX_HOST_MAX_ACTIVE_STREAMS}"
            ));
        }
    }
    if let Some(value) = update.max_recording_seconds {
        if !(MIN_HOST_MAX_RECORDING_SECONDS..=MAX_HOST_MAX_RECORDING_SECONDS).contains(&value) {
            return Err(format!(
                "maxRecordingSeconds must be between {MIN_HOST_MAX_RECORDING_SECONDS} and {MAX_HOST_MAX_RECORDING_SECONDS}"
            ));
        }
    }
    let worker_count = live.worker_models_lock().len();
    let next_worker_models = match (&update.model, &update.worker_models) {
        (Some(_), Some(_)) => {
            return Err("Provide either model or workerModels, not both".to_string())
        }
        (Some(id), None) => {
            let model = WhisperModel::from_model_id(id)
                .ok_or_else(|| format!("Unsupported Whisper model: {id}"))?;
            Some(vec![model; worker_count])
        }
        (None, Some(ids)) => {
            if ids.len() != worker_count {
                return Err(format!(
                    "workerModels must list exactly {worker_count} models (one per worker)"
                ));
            }
            Some(
                ids.iter()
                    .map(|id| {
                        WhisperModel::from_model_id(id)
                            .ok_or_else(|| format!("Unsupported Whisper model: {id}"))
                    })
                    .collect::<Result<Vec<_>, _>>()?,
            )
        }
        (None, None) => None,
    };

    if let Some(value) = update.max_active_streams {
        live.max_active_streams.store(value, Ordering::Relaxed);
    }
    if let Some(value) = update.max_recording_seconds {
        live.max_recording_seconds
            .store(u32::from(value), Ordering::Relaxed);
    }
    if let Some(value) = update.use_gpu {
        live.use_gpu.store(value, Ordering::Relaxed);
    }
    if let Some(models) = next_worker_models {
        live.set_worker_models(models);
    }
    Ok(())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ModelDownloadRequest {
    model: String,
}

/// Downloads a model file onto the host so a missing configured model is
/// recoverable from the dashboard instead of requiring the desktop app or a
/// manual file copy on the host machine. One download runs at a time and its
/// progress is published through `/v1/stats`.
fn handle_model_download(mut request: Request, runtime: Arc<HostRuntime>) -> Result<(), String> {
    let body = match read_limited_body(&mut request.as_reader(), MAX_CONFIG_BODY_BYTES) {
        Ok(body) => body,
        Err(err) => return respond_error(request, StatusCode(413), &err),
    };
    let download: ModelDownloadRequest = match serde_json::from_slice(&body) {
        Ok(download) => download,
        Err(err) => {
            return respond_error(
                request,
                StatusCode(400),
                &format!("Invalid model download request: {err}"),
            )
        }
    };
    let Some(model) = WhisperModel::from_model_id(&download.model) else {
        return respond_error(
            request,
            StatusCode(400),
            &format!("Unsupported Whisper model: {}", download.model),
        );
    };

    {
        let mut metrics = runtime
            .metrics
            .lock()
            .map_err(|_| "Host metrics lock failed".to_string())?;
        if metrics
            .model_download
            .as_ref()
            .is_some_and(ModelDownloadState::in_progress)
        {
            return respond_error(
                request,
                StatusCode(409),
                "A model download is already in progress",
            );
        }
        metrics.model_download = Some(ModelDownloadState {
            model: model.model_id().to_string(),
            stage: "starting".to_string(),
            percentage: 0,
            error: None,
        });
    }

    let models = runtime.models.clone();
    let metrics = Arc::clone(&runtime.metrics);
    let spawned = thread::Builder::new()
        .name("transcription-host-model-download".to_string())
        .spawn(move || {
            let progress_metrics = Arc::clone(&metrics);
            let result = models.prepare_with_progress(model, move |progress| {
                if let Ok(mut metrics) = progress_metrics.lock() {
                    metrics.model_download = Some(ModelDownloadState {
                        model: model.model_id().to_string(),
                        stage: progress.stage.to_string(),
                        percentage: progress.percentage,
                        error: None,
                    });
                }
            });
            if let Err(err) = result {
                if let Ok(mut metrics) = metrics.lock() {
                    metrics.model_download = Some(ModelDownloadState {
                        model: model.model_id().to_string(),
                        stage: "error".to_string(),
                        percentage: 0,
                        error: Some(err),
                    });
                }
            }
        });
    if let Err(err) = spawned {
        // Clear the in-progress marker so the failure doesn't wedge every
        // future download behind a permanent 409.
        if let Ok(mut metrics) = runtime.metrics.lock() {
            metrics.model_download = Some(ModelDownloadState {
                model: model.model_id().to_string(),
                stage: "error".to_string(),
                percentage: 0,
                error: Some(format!("Failed to start model download worker: {err}")),
            });
        }
        return respond_error(
            request,
            StatusCode(500),
            "Failed to start the model download",
        );
    }

    let body = serde_json::json!({
        "model": model.model_id(),
        "status": "downloading",
    })
    .to_string();
    respond_json(request, StatusCode(202), body)
}

fn client_ip(request: &Request) -> Option<String> {
    request.remote_addr().map(|addr| addr.ip().to_string())
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

fn env_bool(name: &str, default: bool) -> bool {
    env::var(name)
        .ok()
        .and_then(|value| parse_bool(&value))
        .unwrap_or(default)
}

fn parse_bool(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" => Some(true),
        "0" | "false" => Some(false),
        _ => None,
    }
}

struct HostRuntime {
    job_tx: SyncSender<TranscriptionJob>,
    metrics: Arc<Mutex<HostMetrics>>,
    live: Arc<HostLiveConfig>,
    models: ModelService,
    config_path: PathBuf,
    next_job_id: AtomicU64,
}

struct TranscriptionJob {
    id: u64,
    settings: Settings,
    recording: Recording,
    source: &'static str,
    client: Option<String>,
    result_tx: mpsc::Sender<Result<TranscriptionOutcome, String>>,
    accepted_at: Instant,
}

/// What a worker hands back for a completed job. The model is the worker's
/// assigned model — the host's choice, not the client's — and is reported in
/// the response so clients always learn what actually ran.
#[derive(Debug)]
struct TranscriptionOutcome {
    text: String,
    model: String,
}

#[derive(Debug)]
enum HostRuntimeError {
    QueueFull(String),
    WorkerFailed(String),
}

struct ActiveStreamGuard {
    metrics: Arc<Mutex<HostMetrics>>,
    stream_id: u64,
}

impl Drop for ActiveStreamGuard {
    fn drop(&mut self) {
        if let Ok(mut metrics) = self.metrics.lock() {
            metrics.finish_stream(self.stream_id);
        }
    }
}

impl HostRuntime {
    fn start(
        models: ModelService,
        config: HostRuntimeConfig,
        metrics: Arc<Mutex<HostMetrics>>,
        config_path: PathBuf,
    ) -> Result<Self, String> {
        let (job_tx, job_rx) = mpsc::sync_channel::<TranscriptionJob>(config.queue_capacity);
        let job_rx = Arc::new(Mutex::new(job_rx));
        let live = Arc::new(HostLiveConfig::new(&config));

        for worker_index in 0..config.worker_count {
            let worker_rx = Arc::clone(&job_rx);
            let worker_models = models.clone();
            let worker_metrics = Arc::clone(&metrics);
            let worker_live = Arc::clone(&live);
            thread::Builder::new()
                .name(format!("transcription-host-worker-{worker_index}"))
                .spawn(move || {
                    run_host_worker(
                        worker_index,
                        worker_rx,
                        worker_models,
                        worker_metrics,
                        worker_live,
                    );
                })
                .map_err(|err| format!("Failed to start transcription host worker: {err}"))?;
        }

        Ok(Self {
            job_tx,
            metrics,
            live,
            models,
            config_path,
            next_job_id: AtomicU64::new(1),
        })
    }

    fn max_recording_seconds(&self) -> u16 {
        self.live.max_recording_seconds()
    }

    fn try_begin_stream(&self, client: Option<String>) -> Result<ActiveStreamGuard, String> {
        let mut metrics = self
            .metrics
            .lock()
            .map_err(|_| "Host metrics lock failed".to_string())?;
        if metrics.active_stream_count() >= self.live.max_active_streams() {
            metrics.reject_job(client.as_deref());
            return Err("Server is at active stream capacity".to_string());
        }
        let stream_id = metrics.begin_stream(client);
        Ok(ActiveStreamGuard {
            metrics: Arc::clone(&self.metrics),
            stream_id,
        })
    }

    fn validate_recording_duration(&self, duration_seconds: f32) -> Result<(), String> {
        let max_recording_seconds = self.live.max_recording_seconds();
        if duration_seconds > f32::from(max_recording_seconds) {
            return Err(format!(
                "Recording exceeds host maximum of {max_recording_seconds} seconds"
            ));
        }
        Ok(())
    }

    fn record_rejection(&self, client: Option<&str>) {
        if let Ok(mut metrics) = self.metrics.lock() {
            metrics.reject_job(client);
        }
    }

    fn record_client_request(&self, client: Option<&str>) {
        if let Ok(mut metrics) = self.metrics.lock() {
            metrics.client_request(client);
        }
    }

    fn transcribe(
        &self,
        recording: Recording,
        mut settings: Settings,
        source: &'static str,
        client: Option<String>,
    ) -> Result<TranscriptionOutcome, HostRuntimeError> {
        // GPU use is an operator decision for the whole host, not a
        // per-request client choice.
        settings.use_gpu = self.live.use_gpu();
        let duration_seconds = recording.stats().duration_seconds;
        if let Err(err) = self.validate_recording_duration(duration_seconds) {
            self.metrics
                .lock()
                .map(|mut metrics| metrics.reject_job(client.as_deref()))
                .ok();
            return Err(HostRuntimeError::QueueFull(err));
        }

        let job_id = self.next_job_id.fetch_add(1, Ordering::Relaxed);
        let (result_tx, result_rx) = mpsc::channel::<Result<TranscriptionOutcome, String>>();
        let job = TranscriptionJob {
            id: job_id,
            settings: settings.clone(),
            recording,
            source,
            client: client.clone(),
            result_tx,
            accepted_at: Instant::now(),
        };

        if let Ok(mut metrics) = self.metrics.lock() {
            metrics.enqueue_job(QueuedJobInfo {
                id: job_id,
                // The serving worker isn't known yet; report the host's
                // configured model (or "mixed" when workers differ).
                model: self.live.model_summary(),
                source,
                client: client.clone(),
                audio_seconds: duration_seconds,
                enqueued_at: Instant::now(),
            });
        }

        match self.job_tx.try_send(job) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {
                if let Ok(mut metrics) = self.metrics.lock() {
                    metrics.dequeue_job(job_id);
                    metrics.reject_job(client.as_deref());
                }
                return Err(HostRuntimeError::QueueFull(
                    "Transcription queue is full".to_string(),
                ));
            }
            Err(TrySendError::Disconnected(_)) => {
                if let Ok(mut metrics) = self.metrics.lock() {
                    metrics.dequeue_job(job_id);
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
    live: Arc<HostLiveConfig>,
) {
    let mut transcription = TranscriptionService::default();
    // Mirrors the model/GPU pair held by this worker's Whisper context, so
    // the dashboard can show a "loading model" phase when a context (re)load
    // is in flight.
    let mut loaded: Option<(WhisperModel, bool)> = None;
    // Last (model, GPU, file mtime) whose load failed; retried only when the
    // target or the file on disk changes, so a corrupt model file is not
    // re-read on every idle poll.
    let mut last_failed: Option<(WhisperModel, bool, Option<SystemTime>)> = None;
    loop {
        // The served model is host configuration: load the assigned model
        // while idle so the first dictation never pays the load wait, and so
        // runtime model changes apply without a job arriving.
        let assigned_model = live.worker_model(worker_index);
        let target = (assigned_model, live.use_gpu());
        if loaded != Some(target) {
            let path = models.path_for(assigned_model);
            if !path.is_file() {
                if loaded.is_some() {
                    // Free the previous context: serving the old model would
                    // be a silent fallback, so the worker holds nothing.
                    transcription.unload();
                    loaded = None;
                }
                last_failed = None;
                if let Ok(mut metrics) = metrics.lock() {
                    metrics.worker_model_unavailable(
                        worker_index,
                        format!(
                            "Model {} is not installed on this host",
                            assigned_model.model_id()
                        ),
                    );
                }
            } else {
                let mtime = fs::metadata(&path).and_then(|meta| meta.modified()).ok();
                if last_failed != Some((target.0, target.1, mtime)) {
                    if let Ok(mut metrics) = metrics.lock() {
                        metrics.worker_preloading(worker_index);
                    }
                    let settings = Settings {
                        model: assigned_model,
                        use_gpu: target.1,
                        ..Settings::default()
                    };
                    match transcription.preload(&settings, &models) {
                        Ok(()) => {
                            loaded = Some(target);
                            last_failed = None;
                            if let Ok(mut metrics) = metrics.lock() {
                                metrics.worker_preload_ready(
                                    worker_index,
                                    assigned_model.model_id().to_string(),
                                );
                            }
                        }
                        Err(err) => {
                            last_failed = Some((target.0, target.1, mtime));
                            if let Ok(mut metrics) = metrics.lock() {
                                metrics.worker_model_unavailable(worker_index, err);
                            }
                        }
                    }
                }
            }
        }

        let job = {
            let receiver = match job_rx.lock() {
                Ok(receiver) => receiver,
                Err(_) => {
                    eprintln!("Transcription host worker {worker_index} receiver lock failed");
                    break;
                }
            };
            match receiver.recv_timeout(WORKER_IDLE_POLL) {
                Ok(job) => job,
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => break,
            }
        };

        let queue_wait = job.accepted_at.elapsed();
        let duration_seconds = job.recording.stats().duration_seconds;
        let backend = BACKEND_ID.to_string();
        // The worker's assigned model serves every job it takes; the client's
        // requested model (if any header survived) is deliberately ignored.
        let model = assigned_model.model_id().to_string();
        let settings = Settings {
            model: assigned_model,
            use_gpu: target.1,
            ..job.settings
        };
        let source = job.source;
        let client = job.client.clone();
        let needs_load = loaded != Some(target);
        if let Ok(mut metrics) = metrics.lock() {
            metrics.start_job(
                worker_index,
                job.id,
                queue_wait,
                needs_load,
                RunningJobInfo {
                    model: model.clone(),
                    source,
                    client: client.clone(),
                    audio_seconds: duration_seconds,
                    started_at: Instant::now(),
                },
            );
        }

        let started = Instant::now();
        let model_ready = std::cell::Cell::new(false);
        let result = transcribe_recording(
            &mut transcription,
            &models,
            &settings,
            job.recording,
            || {
                model_ready.set(true);
                if let Ok(mut metrics) = metrics.lock() {
                    metrics.worker_model_ready(worker_index, model.clone());
                }
            },
        );
        let processing_time = started.elapsed();
        if model_ready.get() {
            loaded = Some(target);
        }

        match &result {
            Ok(_) => {
                if let Ok(mut metrics) = metrics.lock() {
                    metrics.complete_job(
                        worker_index,
                        duration_seconds,
                        backend,
                        model.clone(),
                        source,
                        client.as_deref(),
                        queue_wait,
                        processing_time,
                    );
                }
            }
            Err(err) => {
                if let Ok(mut metrics) = metrics.lock() {
                    metrics.fail_job(worker_index, client.as_deref(), err);
                }
            }
        }

        let outcome = result.map(|text| TranscriptionOutcome {
            text,
            model: model.clone(),
        });
        if job.result_tx.send(outcome).is_err() {
            eprintln!(
                "Transcription host worker {worker_index} completed job {} after requester disconnected",
                job.id
            );
        }
    }
}

fn transcription_response(
    outcome: TranscriptionOutcome,
    duration_seconds: f32,
) -> RemoteTranscriptionResponse {
    RemoteTranscriptionResponse {
        text: outcome.text,
        duration_seconds,
        backend: BACKEND_ID.to_string(),
        model: outcome.model,
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
    read_stream_recording_from_reader(&mut request.as_reader(), max_recording_seconds)
}

fn read_stream_recording_from_reader(
    reader: &mut impl Read,
    max_recording_seconds: u16,
) -> Result<Recording, String> {
    let mut pcm_i16 = Vec::new();
    let mut recording_sample_rate = None;

    while let Some(frame) = read_stream_frame(reader)? {
        if frame.pcm_i16.is_empty() {
            continue;
        }

        let sample_rate = match recording_sample_rate {
            Some(sample_rate) if sample_rate != frame.sample_rate => {
                return Err("Remote stream sample rate changed during recording".to_string())
            }
            Some(sample_rate) => sample_rate,
            None => {
                recording_sample_rate = Some(frame.sample_rate);
                frame.sample_rate
            }
        };
        let max_samples = sample_rate as usize * usize::from(max_recording_seconds);
        if pcm_i16.len().saturating_add(frame.pcm_i16.len()) > max_samples {
            return Err(format!(
                "Recording exceeds host maximum of {max_recording_seconds} seconds"
            ));
        }
        pcm_i16.extend_from_slice(&frame.pcm_i16);
    }

    let sample_rate = recording_sample_rate.unwrap_or(16_000);
    if pcm_i16.is_empty() {
        return Err("No audio samples were captured".to_string());
    }

    Ok(Recording {
        pcm_i16,
        sample_rate,
        dropped_stream_frames: 0,
    })
}

fn read_limited_body(reader: &mut impl Read, max_bytes: u64) -> Result<Vec<u8>, String> {
    let mut body = Vec::new();
    let mut limited = reader.take(max_bytes.saturating_add(1));
    limited
        .read_to_end(&mut body)
        .map_err(|err| format!("Failed to read transcription request body: {err}"))?;
    if body.len() as u64 > max_bytes {
        return Err("Transcription request body exceeds host maximum upload size".to_string());
    }
    Ok(body)
}

fn max_batch_body_bytes(max_recording_seconds: u16) -> u64 {
    MAX_BATCH_WAV_HEADER_BYTES.saturating_add(
        MAX_BATCH_WAV_BYTES_PER_SECOND.saturating_mul(u64::from(max_recording_seconds)),
    )
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
    on_model_ready: impl FnOnce(),
) -> Result<String, String> {
    // start_session loads (or reuses) the Whisper context, so the model is
    // resident once it returns.
    let stream_sink = transcription.start_session(settings, models)?;
    on_model_ready();
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
                return Err("Transcription worker stopped while receiving audio".to_string());
            }
        }
    }

    transcription.finish_session(&recording, settings, models)
}

fn settings_from_headers(request: &Request) -> Result<Settings, String> {
    let backend = header_value(request, "x-multivoice-backend").unwrap_or("whisper");
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

    let settings = Settings {
        language,
        whisper_chunk_seconds: whisper_chunk_seconds.clamp(5, 60),
        vocabulary_hints,
        ..Default::default()
    };
    if backend != "whisper" {
        return Err(format!("Unsupported transcription backend: {backend}"));
    }
    // The served model is host configuration. Older clients still send an
    // x-multivoice-model header; it is deliberately ignored — never an error —
    // and the response's `model` field reports what actually ran.
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

#[derive(Clone, Copy, PartialEq)]
enum WorkerState {
    Idle,
    Loading,
    Transcribing,
    /// The worker's assigned model cannot be served — missing or unloadable
    /// model file. A visible operator state, not a per-job surprise.
    ModelUnavailable,
}

impl WorkerState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Loading => "loading",
            Self::Transcribing => "transcribing",
            Self::ModelUnavailable => "model-unavailable",
        }
    }
}

struct WorkerStatus {
    state: WorkerState,
    loaded_model: Option<String>,
    job: Option<RunningJobInfo>,
    completed_jobs: u64,
    last_error: Option<String>,
}

impl WorkerStatus {
    fn new() -> Self {
        Self {
            state: WorkerState::Idle,
            loaded_model: None,
            job: None,
            completed_jobs: 0,
            last_error: None,
        }
    }
}

struct RunningJobInfo {
    model: String,
    source: &'static str,
    client: Option<String>,
    audio_seconds: f32,
    started_at: Instant,
}

struct QueuedJobInfo {
    id: u64,
    model: String,
    source: &'static str,
    client: Option<String>,
    audio_seconds: f32,
    enqueued_at: Instant,
}

struct ActiveStreamInfo {
    id: u64,
    client: Option<String>,
    started_at: Instant,
}

struct ClientStats {
    requests: u64,
    completed: u64,
    rejected: u64,
    failed: u64,
    total_audio_seconds: f64,
    last_seen_ms: u64,
    last_model: Option<String>,
}

impl ClientStats {
    fn new(now_ms: u64) -> Self {
        Self {
            requests: 0,
            completed: 0,
            rejected: 0,
            failed: 0,
            total_audio_seconds: 0.0,
            last_seen_ms: now_ms,
            last_model: None,
        }
    }
}

fn now_epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

struct HostMetrics {
    started: Instant,
    worker_count: usize,
    queue_capacity: usize,
    next_stream_id: u64,
    active_streams: Vec<ActiveStreamInfo>,
    queued: VecDeque<QueuedJobInfo>,
    workers: Vec<WorkerStatus>,
    rejected_jobs: u64,
    failed_jobs: u64,
    started_jobs: u64,
    total_transcriptions: u64,
    total_audio_seconds: f64,
    total_queue_wait_ms: u128,
    total_processing_ms: u128,
    next_record_id: u64,
    recent: VecDeque<TranscriptionRecord>,
    clients: HashMap<String, ClientStats>,
    model_download: Option<ModelDownloadState>,
}

/// Progress of an operator-initiated model download, published through
/// `/v1/stats` so the dashboard can show a missing model being recovered.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ModelDownloadState {
    model: String,
    stage: String,
    percentage: u8,
    error: Option<String>,
}

impl ModelDownloadState {
    fn in_progress(&self) -> bool {
        self.stage != "ready" && self.stage != "error"
    }
}

impl HostMetrics {
    fn new(config: &HostRuntimeConfig) -> Self {
        Self {
            started: Instant::now(),
            worker_count: config.worker_count,
            queue_capacity: config.queue_capacity,
            next_stream_id: 0,
            active_streams: Vec::new(),
            queued: VecDeque::new(),
            workers: (0..config.worker_count)
                .map(|_| WorkerStatus::new())
                .collect(),
            rejected_jobs: 0,
            failed_jobs: 0,
            started_jobs: 0,
            total_transcriptions: 0,
            total_audio_seconds: 0.0,
            total_queue_wait_ms: 0,
            total_processing_ms: 0,
            next_record_id: 0,
            recent: VecDeque::with_capacity(RECENT_CAPACITY),
            clients: HashMap::new(),
            model_download: None,
        }
    }

    fn active_stream_count(&self) -> u32 {
        self.active_streams.len() as u32
    }

    fn running_job_count(&self) -> u32 {
        self.workers.iter().filter(|w| w.job.is_some()).count() as u32
    }

    fn begin_stream(&mut self, client: Option<String>) -> u64 {
        self.next_stream_id = self.next_stream_id.saturating_add(1);
        let id = self.next_stream_id;
        self.active_streams.push(ActiveStreamInfo {
            id,
            client,
            started_at: Instant::now(),
        });
        id
    }

    fn finish_stream(&mut self, stream_id: u64) {
        self.active_streams.retain(|stream| stream.id != stream_id);
    }

    fn enqueue_job(&mut self, job: QueuedJobInfo) {
        self.queued.push_back(job);
    }

    fn dequeue_job(&mut self, job_id: u64) {
        self.queued.retain(|job| job.id != job_id);
    }

    fn start_job(
        &mut self,
        worker_index: usize,
        job_id: u64,
        queue_wait: Duration,
        needs_load: bool,
        job: RunningJobInfo,
    ) {
        self.dequeue_job(job_id);
        self.started_jobs = self.started_jobs.saturating_add(1);
        self.total_queue_wait_ms = self
            .total_queue_wait_ms
            .saturating_add(queue_wait.as_millis());
        if let Some(worker) = self.workers.get_mut(worker_index) {
            worker.state = if needs_load {
                WorkerState::Loading
            } else {
                WorkerState::Transcribing
            };
            worker.job = Some(job);
        }
    }

    fn worker_model_ready(&mut self, worker_index: usize, model: String) {
        if let Some(worker) = self.workers.get_mut(worker_index) {
            worker.loaded_model = Some(model);
            worker.state = WorkerState::Transcribing;
        }
    }

    /// Marks a worker as loading its assigned model outside a job (the warm
    /// preload at startup or after a runtime model change).
    fn worker_preloading(&mut self, worker_index: usize) {
        if let Some(worker) = self.workers.get_mut(worker_index) {
            worker.state = WorkerState::Loading;
        }
    }

    fn worker_preload_ready(&mut self, worker_index: usize, model: String) {
        if let Some(worker) = self.workers.get_mut(worker_index) {
            worker.state = WorkerState::Idle;
            worker.loaded_model = Some(model);
            worker.last_error = None;
        }
    }

    fn worker_model_unavailable(&mut self, worker_index: usize, message: String) {
        if let Some(worker) = self.workers.get_mut(worker_index) {
            worker.state = WorkerState::ModelUnavailable;
            worker.loaded_model = None;
            worker.last_error = Some(message);
        }
    }

    fn reject_job(&mut self, client: Option<&str>) {
        self.rejected_jobs = self.rejected_jobs.saturating_add(1);
        if let Some(stats) = self.touch_client(client) {
            stats.rejected = stats.rejected.saturating_add(1);
        }
    }

    fn client_request(&mut self, client: Option<&str>) {
        if let Some(stats) = self.touch_client(client) {
            stats.requests = stats.requests.saturating_add(1);
        }
    }

    /// Returns the (created-if-needed) stats entry for a client address and
    /// refreshes its last-seen time. Tracking is bounded: when a new address
    /// arrives at capacity, the least recently seen entry is evicted.
    fn touch_client(&mut self, client: Option<&str>) -> Option<&mut ClientStats> {
        let address = client?;
        let now_ms = now_epoch_ms();
        if !self.clients.contains_key(address) && self.clients.len() >= MAX_TRACKED_CLIENTS {
            if let Some(oldest) = self
                .clients
                .iter()
                .min_by_key(|(_, stats)| stats.last_seen_ms)
                .map(|(address, _)| address.clone())
            {
                self.clients.remove(&oldest);
            }
        }
        let stats = self
            .clients
            .entry(address.to_string())
            .or_insert_with(|| ClientStats::new(now_ms));
        stats.last_seen_ms = now_ms;
        Some(stats)
    }

    fn fail_job(&mut self, worker_index: usize, client: Option<&str>, error: &str) {
        self.failed_jobs = self.failed_jobs.saturating_add(1);
        if let Some(worker) = self.workers.get_mut(worker_index) {
            worker.state = WorkerState::Idle;
            worker.job = None;
            worker.last_error = Some(error.to_string());
        }
        if let Some(stats) = self.touch_client(client) {
            stats.failed = stats.failed.saturating_add(1);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn complete_job(
        &mut self,
        worker_index: usize,
        duration_seconds: f32,
        backend: String,
        model: String,
        source: &'static str,
        client: Option<&str>,
        queue_wait: Duration,
        processing_time: Duration,
    ) {
        self.total_transcriptions = self.total_transcriptions.saturating_add(1);
        self.total_audio_seconds += duration_seconds as f64;
        self.total_processing_ms = self
            .total_processing_ms
            .saturating_add(processing_time.as_millis());
        self.next_record_id = self.next_record_id.saturating_add(1);

        if let Some(worker) = self.workers.get_mut(worker_index) {
            worker.state = WorkerState::Idle;
            worker.job = None;
            worker.completed_jobs = worker.completed_jobs.saturating_add(1);
            worker.last_error = None;
        }
        if let Some(stats) = self.touch_client(client) {
            stats.completed = stats.completed.saturating_add(1);
            stats.total_audio_seconds += duration_seconds as f64;
            stats.last_model = Some(model.clone());
        }

        self.recent.push_front(TranscriptionRecord {
            id: self.next_record_id,
            completed_at_ms: now_epoch_ms(),
            duration_seconds,
            backend,
            model,
            source,
            client: client.map(str::to_string),
            queue_wait_ms: queue_wait.as_millis() as u64,
            processing_ms: processing_time.as_millis() as u64,
        });
        while self.recent.len() > RECENT_CAPACITY {
            self.recent.pop_back();
        }
    }

    fn snapshot(
        &self,
        bind_addr: &str,
        live: &HostLiveConfig,
        models: &ModelService,
    ) -> StatsSnapshot<'_> {
        let assigned_models = live.worker_models();
        let workers = self
            .workers
            .iter()
            .enumerate()
            .map(|(index, worker)| {
                let assigned = assigned_models
                    .get(index)
                    .copied()
                    .unwrap_or(DEFAULT_HOST_MODEL);
                WorkerSnapshot {
                    index,
                    state: worker.state.as_str(),
                    assigned_model: assigned.model_id(),
                    // A cheap existence check: load/checksum failures surface
                    // through the worker state and last error instead.
                    model_available: models.path_for(assigned).is_file(),
                    loaded_model: worker.loaded_model.clone(),
                    completed_jobs: worker.completed_jobs,
                    last_error: worker.last_error.clone(),
                    job: worker.job.as_ref().map(|job| RunningJobSnapshot {
                        model: job.model.clone(),
                        source: job.source,
                        client: job.client.clone(),
                        audio_seconds: job.audio_seconds,
                        elapsed_ms: job.started_at.elapsed().as_millis() as u64,
                    }),
                }
            })
            .collect();

        let queue = self
            .queued
            .iter()
            .map(|job| QueuedJobSnapshot {
                id: job.id,
                model: job.model.clone(),
                source: job.source,
                client: job.client.clone(),
                audio_seconds: job.audio_seconds,
                waiting_ms: job.enqueued_at.elapsed().as_millis() as u64,
            })
            .collect();

        let streams = self
            .active_streams
            .iter()
            .map(|stream| StreamSnapshot {
                client: stream.client.clone(),
                elapsed_ms: stream.started_at.elapsed().as_millis() as u64,
            })
            .collect();

        let mut clients: Vec<ClientSnapshot> = self
            .clients
            .iter()
            .map(|(address, stats)| ClientSnapshot {
                address: address.clone(),
                requests: stats.requests,
                completed: stats.completed,
                rejected: stats.rejected,
                failed: stats.failed,
                total_audio_seconds: stats.total_audio_seconds,
                last_seen_ms: stats.last_seen_ms,
                last_model: stats.last_model.clone(),
            })
            .collect();
        clients.sort_by_key(|client| Reverse(client.last_seen_ms));

        let active_streams = self.active_stream_count();
        let running_jobs = self.running_job_count();
        StatsSnapshot {
            server_version: SERVER_VERSION,
            bind_addr: bind_addr.to_string(),
            uptime_seconds: self.started.elapsed().as_secs(),
            active_sessions: active_streams.saturating_add(running_jobs),
            active_streams,
            queued_jobs: self.queued.len() as u32,
            running_jobs,
            worker_count: self.worker_count,
            queue_capacity: self.queue_capacity,
            max_active_streams: live.max_active_streams(),
            max_recording_seconds: live.max_recording_seconds(),
            use_gpu: live.use_gpu(),
            model: live.model_summary(),
            model_download: self.model_download.clone(),
            rejected_jobs: self.rejected_jobs,
            failed_jobs: self.failed_jobs,
            total_transcriptions: self.total_transcriptions,
            total_audio_seconds: self.total_audio_seconds,
            average_queue_ms: average_ms(self.total_queue_wait_ms, self.started_jobs),
            average_processing_ms: average_ms(self.total_processing_ms, self.total_transcriptions),
            workers,
            queue,
            streams,
            clients,
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
    client: Option<String>,
    queue_wait_ms: u64,
    processing_ms: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct WorkerSnapshot {
    index: usize,
    state: &'static str,
    assigned_model: &'static str,
    model_available: bool,
    loaded_model: Option<String>,
    completed_jobs: u64,
    last_error: Option<String>,
    job: Option<RunningJobSnapshot>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RunningJobSnapshot {
    model: String,
    source: &'static str,
    client: Option<String>,
    audio_seconds: f32,
    elapsed_ms: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct QueuedJobSnapshot {
    id: u64,
    model: String,
    source: &'static str,
    client: Option<String>,
    audio_seconds: f32,
    waiting_ms: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct StreamSnapshot {
    client: Option<String>,
    elapsed_ms: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ClientSnapshot {
    address: String,
    requests: u64,
    completed: u64,
    rejected: u64,
    failed: u64,
    total_audio_seconds: f64,
    last_seen_ms: u64,
    last_model: Option<String>,
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
    max_active_streams: u32,
    max_recording_seconds: u16,
    use_gpu: bool,
    /// The served model: one id when uniform across workers, else "mixed".
    model: String,
    model_download: Option<ModelDownloadState>,
    rejected_jobs: u64,
    failed_jobs: u64,
    total_transcriptions: u64,
    total_audio_seconds: f64,
    average_queue_ms: u64,
    average_processing_ms: u64,
    workers: Vec<WorkerSnapshot>,
    queue: Vec<QueuedJobSnapshot>,
    streams: Vec<StreamSnapshot>,
    clients: Vec<ClientSnapshot>,
    recent: Vec<&'a TranscriptionRecord>,
}

#[cfg(test)]
mod tests {
    use super::{
        max_batch_body_bytes, parse_worker_models, read_limited_body,
        read_stream_recording_from_reader, request_path, HostLiveConfig, HostMetrics, HostRuntime,
        HostRuntimeConfig, HostRuntimeError, ModelDownloadState, QueuedJobInfo, RunningJobInfo,
        TranscriptionOutcome, MAX_TRACKED_CLIENTS,
    };
    use crate::audio::Recording;
    use crate::models::{ModelService, WhisperModel};
    use crate::settings::Settings;
    use std::io::Cursor;
    use std::sync::atomic::AtomicU64;
    use std::sync::mpsc;
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::{Duration, Instant};

    #[test]
    fn request_path_ignores_query_string_for_route_matching() {
        assert_eq!(request_path("/?token=secret"), "/");
        assert_eq!(request_path("/v1/stats?token=secret"), "/v1/stats");
        assert_eq!(request_path("/v1/health"), "/v1/health");
    }

    fn test_config() -> HostRuntimeConfig {
        HostRuntimeConfig {
            worker_count: 1,
            queue_capacity: 1,
            max_active_streams: 1,
            max_recording_seconds: 10,
            use_gpu: true,
            worker_models: vec![WhisperModel::Base],
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
            live: Arc::new(HostLiveConfig::new(&config)),
            models: ModelService::default(),
            config_path: std::env::temp_dir().join(format!(
                "multivoice-tauri-host-config-test-{}.json",
                std::process::id()
            )),
            next_job_id: AtomicU64::new(1),
        }
    }

    fn test_outcome(text: &str) -> TranscriptionOutcome {
        TranscriptionOutcome {
            text: text.to_string(),
            model: "base".to_string(),
        }
    }

    fn test_queued_job(id: u64, client: Option<&str>) -> QueuedJobInfo {
        QueuedJobInfo {
            id,
            model: "large-v3-turbo".to_string(),
            source: "stream",
            client: client.map(str::to_string),
            audio_seconds: 2.5,
            enqueued_at: Instant::now(),
        }
    }

    fn test_running_job(client: Option<&str>) -> RunningJobInfo {
        RunningJobInfo {
            model: "large-v3-turbo".to_string(),
            source: "stream",
            client: client.map(str::to_string),
            audio_seconds: 2.5,
            started_at: Instant::now(),
        }
    }

    fn test_recording(seconds: usize) -> Recording {
        Recording {
            pcm_i16: vec![1; 16_000 * seconds],
            sample_rate: 16_000,
            dropped_stream_frames: 0,
        }
    }

    fn write_test_stream_frame(sample_rate: u32, samples: &[i16], target: &mut Vec<u8>) {
        target.extend_from_slice(&sample_rate.to_le_bytes());
        target.extend_from_slice(&(samples.len() as u32).to_le_bytes());
        for sample in samples {
            target.extend_from_slice(&sample.to_le_bytes());
        }
    }

    #[test]
    fn stream_guard_releases_active_stream_capacity_on_drop() {
        let config = test_config();
        let metrics = Arc::new(Mutex::new(HostMetrics::new(&config)));
        let (job_tx, _job_rx) = mpsc::sync_channel(1);
        let runtime = test_runtime(config, job_tx, Arc::clone(&metrics));

        let guard = runtime
            .try_begin_stream(None)
            .expect("first stream admitted");
        let err = match runtime.try_begin_stream(None) {
            Ok(_) => panic!("second stream should exceed capacity"),
            Err(err) => err,
        };

        assert_eq!(err, "Server is at active stream capacity");
        assert_eq!(metrics.lock().expect("metrics").active_stream_count(), 1);

        drop(guard);

        assert_eq!(metrics.lock().expect("metrics").active_stream_count(), 0);
        let _next_guard = runtime
            .try_begin_stream(None)
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
            .transcribe(test_recording(1), Settings::default(), "stream", None)
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
        assert_eq!(metrics.queued.len(), 0);
        assert_eq!(metrics.rejected_jobs, 1);
        assert_eq!(metrics.running_job_count(), 0);
    }

    #[test]
    fn queued_runtime_returns_results_to_multiple_waiting_callers() {
        let config = HostRuntimeConfig {
            queue_capacity: 2,
            ..test_config()
        };
        let metrics = Arc::new(Mutex::new(HostMetrics::new(&config)));
        let (job_tx, job_rx) = mpsc::sync_channel(2);
        let runtime = Arc::new(test_runtime(config, job_tx, Arc::clone(&metrics)));

        let first_runtime = Arc::clone(&runtime);
        let first = thread::spawn(move || {
            first_runtime.transcribe(test_recording(1), Settings::default(), "stream", None)
        });
        let second_runtime = Arc::clone(&runtime);
        let second = thread::spawn(move || {
            second_runtime.transcribe(test_recording(1), Settings::default(), "stream", None)
        });

        let first_job = job_rx.recv().expect("first queued job");
        let second_job = job_rx.recv().expect("second queued job");
        assert_eq!(metrics.lock().expect("metrics").queued.len(), 2);

        first_job
            .result_tx
            .send(Ok(test_outcome("first transcript")))
            .expect("send first result");
        second_job
            .result_tx
            .send(Ok(test_outcome("second transcript")))
            .expect("send second result");

        let mut results = vec![
            first
                .join()
                .expect("first caller joined")
                .expect("first ok")
                .text,
            second
                .join()
                .expect("second caller joined")
                .expect("second ok")
                .text,
        ];
        results.sort();

        assert_eq!(results, vec!["first transcript", "second transcript"]);
    }

    #[test]
    fn transcribe_reports_the_model_the_worker_actually_ran() {
        let config = test_config();
        let metrics = Arc::new(Mutex::new(HostMetrics::new(&config)));
        let (job_tx, job_rx) = mpsc::sync_channel(1);
        let runtime = Arc::new(test_runtime(config, job_tx, Arc::clone(&metrics)));

        let caller_runtime = Arc::clone(&runtime);
        let caller = thread::spawn(move || {
            caller_runtime.transcribe(test_recording(1), Settings::default(), "stream", None)
        });

        let job = job_rx.recv().expect("queued job");
        // The worker answers with its own assigned model, regardless of what
        // the request carried.
        job.result_tx
            .send(Ok(TranscriptionOutcome {
                text: "transcript".to_string(),
                model: "large-v3-turbo".to_string(),
            }))
            .expect("send result");

        let outcome = caller
            .join()
            .expect("caller joined")
            .expect("transcribe ok");
        assert_eq!(outcome.text, "transcript");
        assert_eq!(outcome.model, "large-v3-turbo");
    }

    #[test]
    fn host_gpu_config_overrides_client_settings() {
        let config = HostRuntimeConfig {
            use_gpu: false,
            ..test_config()
        };
        let metrics = Arc::new(Mutex::new(HostMetrics::new(&config)));
        let (job_tx, job_rx) = mpsc::sync_channel(1);
        let runtime = Arc::new(test_runtime(config, job_tx, Arc::clone(&metrics)));

        let client_settings = Settings::default();
        assert!(client_settings.use_gpu, "client default should request GPU");

        let worker_runtime = Arc::clone(&runtime);
        let caller = thread::spawn(move || {
            worker_runtime.transcribe(test_recording(1), client_settings, "batch", None)
        });

        let job = job_rx.recv().expect("queued job");
        assert!(
            !job.settings.use_gpu,
            "host config must force CPU inference"
        );

        job.result_tx
            .send(Ok(test_outcome("transcript")))
            .expect("send result");
        caller
            .join()
            .expect("caller joined")
            .expect("transcribe ok");
    }

    #[test]
    fn parse_bool_accepts_numeric_and_word_flags() {
        assert_eq!(super::parse_bool("1"), Some(true));
        assert_eq!(super::parse_bool(" TRUE "), Some(true));
        assert_eq!(super::parse_bool("0"), Some(false));
        assert_eq!(super::parse_bool("false"), Some(false));
        assert_eq!(super::parse_bool("yes"), None);
    }

    #[test]
    fn oversized_recording_rejects_before_enqueue() {
        let config = test_config();
        let metrics = Arc::new(Mutex::new(HostMetrics::new(&config)));
        let (job_tx, _job_rx) = mpsc::sync_channel(1);
        let runtime = test_runtime(config, job_tx, Arc::clone(&metrics));

        let err = runtime
            .transcribe(test_recording(11), Settings::default(), "batch", None)
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
        assert_eq!(metrics.queued.len(), 0);
        assert_eq!(metrics.rejected_jobs, 1);
        assert_eq!(metrics.running_job_count(), 0);
    }

    #[test]
    fn limited_batch_body_rejects_oversized_uploads() {
        let max_bytes = max_batch_body_bytes(10);
        let mut body = Cursor::new(vec![0_u8; max_bytes as usize + 1]);

        let err = read_limited_body(&mut body, max_bytes).expect_err("oversized body rejects");

        assert!(err.contains("maximum upload size"));
    }

    #[test]
    fn stream_recording_rejects_sample_rate_changes() {
        let mut encoded = Vec::new();
        write_test_stream_frame(16_000, &[1, 2, 3], &mut encoded);
        write_test_stream_frame(48_000, &[4, 5, 6], &mut encoded);

        let err = read_stream_recording_from_reader(&mut encoded.as_slice(), 10)
            .expect_err("mixed sample rates reject");

        assert!(err.contains("sample rate changed"));
    }

    #[test]
    fn stream_recording_preserves_consistent_sample_rate() {
        let mut encoded = Vec::new();
        write_test_stream_frame(16_000, &[1, 2, 3], &mut encoded);
        write_test_stream_frame(16_000, &[4, 5, 6], &mut encoded);

        let recording =
            read_stream_recording_from_reader(&mut encoded.as_slice(), 10).expect("recording");

        assert_eq!(recording.sample_rate, 16_000);
        assert_eq!(recording.pcm_i16, vec![1, 2, 3, 4, 5, 6]);
    }

    #[test]
    fn metrics_snapshot_separates_stream_queue_and_running_work() {
        let config = HostRuntimeConfig {
            worker_count: 2,
            queue_capacity: 4,
            max_active_streams: 3,
            max_recording_seconds: 120,
            use_gpu: true,
            worker_models: vec![WhisperModel::LargeV3Turbo, WhisperModel::Small],
        };
        let live = HostLiveConfig::new(&config);
        let models = ModelService::default();
        let mut metrics = HostMetrics::new(&config);

        metrics.begin_stream(Some("192.168.1.31".to_string()));
        metrics.client_request(Some("192.168.1.31"));
        metrics.enqueue_job(test_queued_job(1, Some("192.168.1.31")));
        metrics.start_job(
            0,
            1,
            Duration::from_millis(30),
            true,
            test_running_job(Some("192.168.1.31")),
        );
        metrics.worker_model_ready(0, "large-v3-turbo".to_string());
        metrics.complete_job(
            0,
            2.5,
            "whisper".to_string(),
            "large-v3-turbo".to_string(),
            "stream",
            Some("192.168.1.31"),
            Duration::from_millis(30),
            Duration::from_millis(90),
        );

        let snapshot = metrics.snapshot("127.0.0.1:48173", &live, &models);

        assert_eq!(snapshot.active_streams, 1);
        assert_eq!(snapshot.queued_jobs, 0);
        assert_eq!(snapshot.running_jobs, 0);
        assert_eq!(snapshot.worker_count, 2);
        assert_eq!(snapshot.queue_capacity, 4);
        assert_eq!(snapshot.max_active_streams, 3);
        assert_eq!(snapshot.max_recording_seconds, 120);
        assert!(snapshot.use_gpu);
        assert_eq!(snapshot.total_transcriptions, 1);
        assert_eq!(snapshot.average_queue_ms, 30);
        assert_eq!(snapshot.average_processing_ms, 90);
        assert_eq!(snapshot.recent.len(), 1);
        assert_eq!(snapshot.recent[0].queue_wait_ms, 30);
        assert_eq!(snapshot.recent[0].processing_ms, 90);
        assert_eq!(snapshot.recent[0].client.as_deref(), Some("192.168.1.31"));

        assert_eq!(snapshot.model, "mixed");
        assert_eq!(snapshot.workers.len(), 2);
        assert_eq!(snapshot.workers[0].state, "idle");
        assert_eq!(snapshot.workers[0].assigned_model, "large-v3-turbo");
        assert_eq!(
            snapshot.workers[0].loaded_model.as_deref(),
            Some("large-v3-turbo")
        );
        assert_eq!(snapshot.workers[0].completed_jobs, 1);
        assert_eq!(snapshot.workers[1].state, "idle");
        assert_eq!(snapshot.workers[1].assigned_model, "small");
        assert_eq!(snapshot.workers[1].loaded_model, None);

        assert_eq!(snapshot.streams.len(), 1);
        assert_eq!(snapshot.streams[0].client.as_deref(), Some("192.168.1.31"));

        assert_eq!(snapshot.clients.len(), 1);
        assert_eq!(snapshot.clients[0].address, "192.168.1.31");
        assert_eq!(snapshot.clients[0].requests, 1);
        assert_eq!(snapshot.clients[0].completed, 1);
        assert_eq!(
            snapshot.clients[0].last_model.as_deref(),
            Some("large-v3-turbo")
        );
    }

    #[test]
    fn failed_job_clears_running_state_and_counts_failure() {
        let config = test_config();
        let mut metrics = HostMetrics::new(&config);

        metrics.enqueue_job(test_queued_job(1, None));
        metrics.start_job(
            0,
            1,
            Duration::from_millis(5),
            false,
            test_running_job(None),
        );
        metrics.fail_job(0, None, "Whisper context failed");

        assert_eq!(metrics.queued.len(), 0);
        assert_eq!(metrics.running_job_count(), 0);
        assert_eq!(metrics.failed_jobs, 1);
        assert_eq!(metrics.total_transcriptions, 0);
        assert_eq!(
            metrics.workers[0].last_error.as_deref(),
            Some("Whisper context failed")
        );
    }

    #[test]
    fn worker_loading_state_transitions_to_transcribing_when_model_ready() {
        let config = test_config();
        let mut metrics = HostMetrics::new(&config);

        metrics.enqueue_job(test_queued_job(1, None));
        metrics.start_job(0, 1, Duration::from_millis(5), true, test_running_job(None));
        assert!(matches!(
            metrics.workers[0].state,
            super::WorkerState::Loading
        ));
        assert_eq!(metrics.workers[0].loaded_model, None);

        metrics.worker_model_ready(0, "large-v3-turbo".to_string());
        assert!(matches!(
            metrics.workers[0].state,
            super::WorkerState::Transcribing
        ));
        assert_eq!(
            metrics.workers[0].loaded_model.as_deref(),
            Some("large-v3-turbo")
        );
    }

    #[test]
    fn queue_snapshot_lists_waiting_jobs_in_arrival_order() {
        let config = test_config();
        let live = HostLiveConfig::new(&config);
        let models = ModelService::default();
        let mut metrics = HostMetrics::new(&config);

        metrics.enqueue_job(test_queued_job(1, Some("192.168.1.10")));
        metrics.enqueue_job(test_queued_job(2, Some("192.168.1.11")));

        let snapshot = metrics.snapshot("127.0.0.1:48173", &live, &models);
        assert_eq!(snapshot.queued_jobs, 2);
        assert_eq!(snapshot.queue.len(), 2);
        assert_eq!(snapshot.queue[0].id, 1);
        assert_eq!(snapshot.queue[1].id, 2);
        assert_eq!(snapshot.queue[0].client.as_deref(), Some("192.168.1.10"));

        metrics.dequeue_job(1);
        let snapshot = metrics.snapshot("127.0.0.1:48173", &live, &models);
        assert_eq!(snapshot.queue.len(), 1);
        assert_eq!(snapshot.queue[0].id, 2);
    }

    #[test]
    fn client_tracking_is_bounded_and_evicts_least_recently_seen() {
        let config = test_config();
        let mut metrics = HostMetrics::new(&config);

        for index in 0..MAX_TRACKED_CLIENTS {
            metrics.client_request(Some(&format!("10.0.0.{index}")));
        }
        assert_eq!(metrics.clients.len(), MAX_TRACKED_CLIENTS);

        // Refresh one entry, then force an eviction with a brand-new address.
        let oldest = metrics
            .clients
            .iter()
            .min_by_key(|(_, stats)| stats.last_seen_ms)
            .map(|(address, _)| address.clone())
            .expect("oldest client");
        metrics.client_request(Some("10.0.1.1"));

        assert_eq!(metrics.clients.len(), MAX_TRACKED_CLIENTS);
        assert!(!metrics.clients.contains_key(&oldest));
        assert!(metrics.clients.contains_key("10.0.1.1"));
    }

    #[test]
    fn config_update_applies_valid_values_and_rejects_out_of_range_atomically() {
        let config = test_config();
        let live = HostLiveConfig::new(&config);
        assert_eq!(live.max_active_streams(), 1);

        super::apply_config_update(
            &super::HostConfigUpdate {
                max_active_streams: Some(8),
                max_recording_seconds: Some(300),
                use_gpu: Some(false),
                model: None,
                worker_models: None,
            },
            &live,
        )
        .expect("valid update applies");
        assert_eq!(live.max_active_streams(), 8);
        assert_eq!(live.max_recording_seconds(), 300);
        assert!(!live.use_gpu());

        // One invalid field rejects the whole update without partial writes.
        let err = super::apply_config_update(
            &super::HostConfigUpdate {
                max_active_streams: Some(2),
                max_recording_seconds: Some(5_000),
                use_gpu: Some(true),
                model: None,
                worker_models: None,
            },
            &live,
        )
        .expect_err("out-of-range update rejects");
        assert!(err.contains("maxRecordingSeconds"));
        assert_eq!(live.max_active_streams(), 8);
        assert_eq!(live.max_recording_seconds(), 300);
        assert!(!live.use_gpu());

        let err = super::apply_config_update(
            &super::HostConfigUpdate {
                max_active_streams: Some(0),
                max_recording_seconds: None,
                use_gpu: None,
                model: None,
                worker_models: None,
            },
            &live,
        )
        .expect_err("zero streams rejects");
        assert!(err.contains("maxActiveStreams"));
    }

    fn model_update(
        model: Option<&str>,
        worker_models: Option<Vec<&str>>,
    ) -> super::HostConfigUpdate {
        super::HostConfigUpdate {
            max_active_streams: None,
            max_recording_seconds: None,
            use_gpu: None,
            model: model.map(str::to_string),
            worker_models: worker_models
                .map(|models| models.into_iter().map(str::to_string).collect()),
        }
    }

    #[test]
    fn config_update_sets_served_model_for_all_workers() {
        let config = HostRuntimeConfig {
            worker_count: 2,
            worker_models: vec![WhisperModel::Base, WhisperModel::Base],
            ..test_config()
        };
        let live = HostLiveConfig::new(&config);

        super::apply_config_update(&model_update(Some("large-v3-turbo"), None), &live)
            .expect("model update applies");
        assert_eq!(
            live.worker_models(),
            vec![WhisperModel::LargeV3Turbo, WhisperModel::LargeV3Turbo]
        );
        assert_eq!(live.model_summary(), "large-v3-turbo");

        let err = super::apply_config_update(&model_update(Some("gpt-4"), None), &live)
            .expect_err("unknown model rejects");
        assert!(err.contains("Unsupported Whisper model"));
        assert_eq!(live.model_summary(), "large-v3-turbo");
    }

    #[test]
    fn config_update_assigns_models_per_worker() {
        let config = HostRuntimeConfig {
            worker_count: 2,
            worker_models: vec![WhisperModel::Base, WhisperModel::Base],
            ..test_config()
        };
        let live = HostLiveConfig::new(&config);

        super::apply_config_update(
            &model_update(None, Some(vec!["large-v3-turbo", "small"])),
            &live,
        )
        .expect("per-worker update applies");
        assert_eq!(
            live.worker_models(),
            vec![WhisperModel::LargeV3Turbo, WhisperModel::Small]
        );
        assert_eq!(live.model_summary(), "mixed");

        let err =
            super::apply_config_update(&model_update(None, Some(vec!["small"])), &live)
                .expect_err("wrong list length rejects");
        assert!(err.contains("exactly 2"));

        let err = super::apply_config_update(
            &model_update(Some("small"), Some(vec!["small", "small"])),
            &live,
        )
        .expect_err("ambiguous update rejects");
        assert!(err.contains("not both"));
        assert_eq!(live.model_summary(), "mixed");
    }

    #[test]
    fn parse_worker_models_expands_defaults_and_rejects_bad_values() {
        assert_eq!(
            parse_worker_models(None, 2).expect("default models"),
            vec![WhisperModel::Base, WhisperModel::Base]
        );
        assert_eq!(
            parse_worker_models(Some("large-v3-turbo"), 2).expect("single model expands"),
            vec![WhisperModel::LargeV3Turbo, WhisperModel::LargeV3Turbo]
        );
        assert_eq!(
            parse_worker_models(Some(" large-v3-turbo , small "), 2).expect("per-worker list"),
            vec![WhisperModel::LargeV3Turbo, WhisperModel::Small]
        );

        let err = parse_worker_models(Some("turbo-9000"), 1).expect_err("unknown model fails");
        assert!(err.contains("unsupported Whisper model"));
        let err =
            parse_worker_models(Some("small,base,tiny"), 2).expect_err("wrong count fails");
        assert!(err.contains("3 models for 2 workers"));
    }

    #[test]
    fn persisted_config_round_trips_dashboard_edits() {
        let path = std::env::temp_dir().join(format!(
            "multivoice-tauri-host-config-roundtrip-{}.json",
            std::process::id()
        ));
        let config = HostRuntimeConfig {
            worker_count: 2,
            worker_models: vec![WhisperModel::Base, WhisperModel::Base],
            ..test_config()
        };
        let live = HostLiveConfig::new(&config);
        super::apply_config_update(
            &super::HostConfigUpdate {
                max_active_streams: Some(8),
                max_recording_seconds: Some(120),
                use_gpu: Some(false),
                model: None,
                worker_models: Some(vec!["large-v3-turbo".to_string(), "small".to_string()]),
            },
            &live,
        )
        .expect("dashboard edit applies");

        super::persist_live_config(&path, &live).expect("persist");
        let persisted = super::load_persisted_config(&path)
            .expect("load")
            .expect("config present after an edit");

        let mut restarted = HostRuntimeConfig {
            worker_count: 2,
            worker_models: vec![WhisperModel::Base, WhisperModel::Base],
            ..test_config()
        };
        super::overlay_persisted_config(&mut restarted, persisted);
        assert_eq!(restarted.max_active_streams, 8);
        assert_eq!(restarted.max_recording_seconds, 120);
        assert!(!restarted.use_gpu);
        assert_eq!(
            restarted.worker_models,
            vec![WhisperModel::LargeV3Turbo, WhisperModel::Small]
        );

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn overlay_clamps_ranges_and_adapts_stale_worker_model_lists() {
        let mut config = HostRuntimeConfig {
            worker_count: 2,
            worker_models: vec![WhisperModel::Base, WhisperModel::Base],
            ..test_config()
        };
        super::overlay_persisted_config(
            &mut config,
            super::PersistedHostConfig {
                max_active_streams: 999,
                max_recording_seconds: 1,
                use_gpu: false,
                // Written when the host ran three workers; now it runs two.
                worker_models: vec![
                    WhisperModel::Small,
                    WhisperModel::Tiny,
                    WhisperModel::Base,
                ],
            },
        );
        assert_eq!(config.max_active_streams, 32);
        assert_eq!(config.max_recording_seconds, 10);
        assert!(!config.use_gpu);
        assert_eq!(
            config.worker_models,
            vec![WhisperModel::Small, WhisperModel::Small]
        );
    }

    #[test]
    fn missing_config_file_is_first_boot_and_corrupt_file_fails() {
        let missing = std::env::temp_dir().join(format!(
            "multivoice-tauri-host-config-missing-{}.json",
            std::process::id()
        ));
        assert!(super::load_persisted_config(&missing)
            .expect("missing file is fine")
            .is_none());

        let corrupt = std::env::temp_dir().join(format!(
            "multivoice-tauri-host-config-corrupt-{}.json",
            std::process::id()
        ));
        std::fs::write(&corrupt, b"{not json").expect("write corrupt fixture");
        let err = super::load_persisted_config(&corrupt).expect_err("corrupt file fails");
        assert!(err.contains("Invalid host config"));
        let _ = std::fs::remove_file(corrupt);
    }

    #[test]
    fn worker_model_lifecycle_reports_unavailable_and_warm_states() {
        let config = test_config();
        let mut metrics = HostMetrics::new(&config);

        metrics.worker_model_unavailable(0, "Model base is not installed on this host".into());
        assert!(matches!(
            metrics.workers[0].state,
            super::WorkerState::ModelUnavailable
        ));
        assert_eq!(metrics.workers[0].loaded_model, None);
        assert!(metrics.workers[0]
            .last_error
            .as_deref()
            .unwrap()
            .contains("not installed"));

        metrics.worker_preloading(0);
        assert!(matches!(
            metrics.workers[0].state,
            super::WorkerState::Loading
        ));

        metrics.worker_preload_ready(0, "base".to_string());
        assert!(matches!(metrics.workers[0].state, super::WorkerState::Idle));
        assert_eq!(metrics.workers[0].loaded_model.as_deref(), Some("base"));
        assert_eq!(metrics.workers[0].last_error, None);
    }

    #[test]
    fn model_download_state_reports_progress_in_snapshot() {
        let config = test_config();
        let live = HostLiveConfig::new(&config);
        let models = ModelService::default();
        let mut metrics = HostMetrics::new(&config);

        assert!(metrics
            .snapshot("127.0.0.1:48173", &live, &models)
            .model_download
            .is_none());

        metrics.model_download = Some(ModelDownloadState {
            model: "base".to_string(),
            stage: "downloading".to_string(),
            percentage: 42,
            error: None,
        });
        let snapshot = metrics.snapshot("127.0.0.1:48173", &live, &models);
        let download = snapshot.model_download.expect("download state");
        assert_eq!(download.model, "base");
        assert_eq!(download.percentage, 42);
        assert!(download.in_progress());

        let finished = ModelDownloadState {
            model: "base".to_string(),
            stage: "ready".to_string(),
            percentage: 100,
            error: None,
        };
        assert!(!finished.in_progress());
    }

    #[test]
    fn rejection_with_client_attributes_to_client_stats() {
        let config = test_config();
        let mut metrics = HostMetrics::new(&config);

        metrics.client_request(Some("192.168.1.20"));
        metrics.reject_job(Some("192.168.1.20"));
        metrics.reject_job(None);

        assert_eq!(metrics.rejected_jobs, 2);
        let stats = metrics.clients.get("192.168.1.20").expect("client stats");
        assert_eq!(stats.requests, 1);
        assert_eq!(stats.rejected, 1);
    }
}
