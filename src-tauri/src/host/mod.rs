mod catalog;
mod cli;
mod config;
mod events;
mod pairing;
mod update;

use crate::audio::{AudioFrame, Recording};
use crate::models::{ModelService, SttModel};
use crate::remote_transcription::{
    decode_wav, read_stream_frame, RemoteHealth, RemoteTranscriptionResponse, BACKEND_ID,
};
use crate::settings::Settings;
use crate::transcription::TranscriptionService;
use catalog::{model_snapshots, ModelSnapshot};
use config::{
    apply_config_update, default_host_config_path, load_persisted_config, overlay_persisted_config,
    persist_live_config, HostConfigUpdate, HostLiveConfig, HostRuntimeConfig, DEFAULT_HOST_MODEL,
};
#[cfg(test)]
use config::{parse_bool, parse_worker_models, PersistedHostConfig};
use events::{
    sse_frame, EventHub, FrameWriter, HostEvent, HostEventKind, SseWriter, HEARTBEAT_INTERVAL,
};
use pairing::{
    attempt_pairing, resolve_host_name, Hello, HostAuth, PairOutcome, PairRequest, PairingLimiter,
    HOST_NAME_ENV, PAIRING_PASSWORD_ENV,
};
use serde::{Deserialize, Serialize};
use std::cmp::Reverse;
use std::collections::{HashMap, VecDeque};
use std::env;
use std::io::Read;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tiny_http::{HTTPVersion, Header, Method, Request, Response, Server, StatusCode};
use update::{UpdateSettingsRequest, Updater, UpdaterOptions, RESTARTED_ENV};

const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");
const DASHBOARD_HTML: &str = include_str!("dashboard.html");
const RECENT_CAPACITY: usize = 50;
const MAX_STREAM_SAMPLE_RATE: u32 = 192_000;
const MAX_BATCH_WAV_BYTES_PER_SECOND: u64 = (MAX_STREAM_SAMPLE_RATE as u64) * 2;
const MAX_BATCH_WAV_HEADER_BYTES: u64 = 64 * 1024;
const MAX_TRACKED_CLIENTS: usize = 32;
// How long an idle worker blocks on the job queue before re-checking its
// assigned model, so runtime model changes and freshly downloaded model files
// are picked up without a job arriving.
const WORKER_IDLE_POLL: Duration = Duration::from_millis(250);

pub fn run_transcription_host() -> Result<(), String> {
    crate::app_dirs::migrate_legacy_dirs();
    let args: Vec<String> = env::args_os()
        .skip(1)
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    match cli::parse_args(&args) {
        Ok(cli::HostCommand::Serve) => {}
        Ok(command) => {
            let code = cli::run(command)?;
            if code != 0 {
                std::process::exit(code);
            }
            return Ok(());
        }
        Err(message) => {
            eprintln!("{message}\n\n{}", cli::USAGE);
            std::process::exit(2);
        }
    }

    let addr = crate::app_dirs::env_var("FAIRSPOKEN_HOST_ADDR")
        .unwrap_or_else(|| "127.0.0.1:48173".to_string());
    let mut config = HostRuntimeConfig::from_env()?;
    // Dashboard edits are the durable configuration; the environment only
    // seeds the first boot (or a deleted config file).
    let config_path = default_host_config_path();
    let persisted = load_persisted_config(&config_path)?;
    let update_prefs = persisted
        .as_ref()
        .map(|persisted| persisted.update.clone())
        .unwrap_or_default();
    // Unlike the live settings, the token, pairing password and name from
    // the environment win over the file on every start.
    let (auth, generated_token) = HostAuth::resolve(
        crate::app_dirs::env_var("FAIRSPOKEN_HOST_TOKEN"),
        persisted
            .as_ref()
            .and_then(|persisted| persisted.token.clone()),
        crate::app_dirs::env_var(PAIRING_PASSWORD_ENV),
        persisted
            .as_ref()
            .and_then(|persisted| persisted.pairing_password.clone()),
    )?;
    let saved_name = persisted
        .as_ref()
        .and_then(|persisted| persisted.name.clone());
    let host_name = resolve_host_name(
        crate::app_dirs::env_var(HOST_NAME_ENV),
        saved_name.as_deref(),
    );
    if let Some(persisted) = persisted {
        overlay_persisted_config(&mut config, persisted);
    }
    let update_options = UpdaterOptions::from_env(SERVER_VERSION)?;
    let server = bind_server(&addr)?;
    println!("Fairspoken transcription host {SERVER_VERSION} listening on http://{addr}");

    let models = ModelService::default();
    let metrics = Arc::new(Mutex::new(HostMetrics::new(&config)));
    let runtime = Arc::new(HostRuntime::start(
        models.clone(),
        config,
        Arc::clone(&metrics),
        config_path,
    )?);
    runtime.live.set_update_prefs(update_prefs);
    runtime.live.set_auth(auth);
    runtime.live.set_name(host_name, saved_name);
    if generated_token {
        // Pairing hands out the token, so it must survive a restart.
        persist_live_config(&runtime.config_path, &runtime.live)
            .map_err(|err| format!("Failed to save the generated pairing token: {err}"))?;
        println!(
            "Generated a host token for pairing; saved in {}",
            runtime.config_path.display()
        );
    }
    println!(
        "Host name \"{}\"; pairing {}",
        runtime.live.name(),
        if runtime.live.pairing_enabled() {
            "on (clients pair with the password)"
        } else {
            "off"
        }
    );

    let idle_metrics = Arc::clone(&metrics);
    let updater = Arc::new(Updater::new(
        update_options,
        Arc::clone(&runtime.live),
        runtime.config_path.clone(),
        Box::new(move || {
            idle_metrics
                .lock()
                .map(|metrics| metrics.is_idle())
                .unwrap_or(true)
        }),
    ));
    let update_status = updater.status();
    println!(
        "Updates: {} channel ({:?}), automatic checks {}, auto-install {}",
        update_status.channel.as_str(),
        update_status.channel_source,
        if update_status.checks_enabled {
            "on"
        } else {
            "off"
        },
        if update_status.auto_update {
            "on"
        } else {
            "off"
        },
    );
    updater.spawn_periodic_checks();

    for request in server.incoming_requests() {
        let response = handle_request(
            request,
            Arc::clone(&runtime),
            Arc::clone(&metrics),
            &updater,
            &addr,
        );
        if let Err(err) = response {
            eprintln!("Failed to handle transcription host request: {err}");
        }
    }

    Ok(())
}

/// A host re-executed after an update may start before the old process has
/// released the port (Windows); it retries briefly instead of failing.
fn bind_server(addr: &str) -> Result<Server, String> {
    let deadline = crate::app_dirs::env_var_os(RESTARTED_ENV)
        .map(|_| Instant::now() + Duration::from_secs(15));
    loop {
        match Server::http(addr) {
            Ok(server) => return Ok(server),
            Err(_) if deadline.is_some_and(|deadline| Instant::now() < deadline) => {
                thread::sleep(Duration::from_millis(250));
            }
            Err(err) => {
                return Err(format!(
                    "Failed to start transcription host on {addr}: {err}"
                ))
            }
        }
    }
}

fn handle_request(
    request: Request,
    runtime: Arc<HostRuntime>,
    metrics: Arc<Mutex<HostMetrics>>,
    updater: &Arc<Updater>,
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

    // Discovery and pairing are how a client without the token finds the
    // host and gets one; they reveal nothing else.
    if matches!((request.method(), path), (&Method::Get, "/v1/hello")) {
        let name = runtime.live.name();
        let hello = Hello::new(&name, SERVER_VERSION, runtime.live.auth().mode());
        let body = serde_json::to_string(&hello)
            .map_err(|err| format!("Failed to serialize hello response: {err}"))?;
        return respond_json(request, StatusCode(200), body);
    }
    if matches!((request.method(), path), (&Method::Post, "/v1/pair")) {
        return handle_pair(request, &runtime);
    }

    if !authorized(&request, runtime.live.token().as_deref()) {
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
            let body = {
                let metrics = metrics
                    .lock()
                    .map_err(|_| "Host metrics lock failed".to_string())?;
                stats_json(&metrics, bind_addr, &runtime)?
            };
            respond_json(request, StatusCode(200), body)
        }
        (&Method::Get, "/v1/events") => spawn_event_stream(request, runtime, bind_addr),
        (&Method::Post, "/v1/config") => handle_config_update(request, runtime),
        (&Method::Post, "/v1/models/download") => handle_model_download(request, runtime),
        (&Method::Get, "/v1/update") => respond_update_status(request, StatusCode(200), updater),
        (&Method::Post, "/v1/update/check") => match updater.begin_check(false) {
            Ok(()) => respond_update_status(request, StatusCode(202), updater),
            Err(message) => respond_error(request, StatusCode(409), &message),
        },
        (&Method::Post, "/v1/update/install") => match updater.begin_install(false) {
            Ok(()) => respond_update_status(request, StatusCode(202), updater),
            Err(message) => respond_error(request, StatusCode(409), &message),
        },
        (&Method::Post, "/v1/update/restart") => match updater.begin_restart() {
            Ok(()) => respond_update_status(request, StatusCode(202), updater),
            Err(message) => respond_error(request, StatusCode(409), &message),
        },
        (&Method::Post, "/v1/update/settings") => handle_update_settings(request, updater),
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

fn stats_json(
    metrics: &HostMetrics,
    bind_addr: &str,
    runtime: &HostRuntime,
) -> Result<String, String> {
    serde_json::to_string(&metrics.snapshot(bind_addr, &runtime.live, &runtime.models))
        .map_err(|err| format!("Failed to serialize stats response: {err}"))
}

/// Streams live host activity to a dashboard as Server-Sent Events: a
/// `snapshot` frame (the `/v1/stats` body), then one frame per metrics change.
/// The connection gets its own thread so a long-lived stream never holds up
/// the accept loop.
fn spawn_event_stream(
    request: Request,
    runtime: Arc<HostRuntime>,
    bind_addr: &str,
) -> Result<(), String> {
    // Subscribe and snapshot under one metrics lock: every mutation publishes
    // while holding it, so no event falls between the snapshot and the feed.
    let subscribed = {
        let metrics = runtime
            .metrics
            .lock()
            .map_err(|_| "Host metrics lock failed".to_string())?;
        match metrics.events.subscribe() {
            Ok(subscription) => Ok((subscription, stats_json(&metrics, bind_addr, &runtime)?)),
            Err(message) => Err(message),
        }
    };
    let (subscription, snapshot) = match subscribed {
        Ok(subscribed) => subscribed,
        Err(message) => return respond_error(request, StatusCode(503), &message),
    };
    let chunked = *request.http_version() != HTTPVersion(1, 0);
    thread::Builder::new()
        .name("transcription-host-events".to_string())
        .spawn(move || {
            let streamed =
                SseWriter::start(request.into_writer(), chunked).and_then(|mut writer| {
                    writer.write_frame(sse_frame("snapshot", &snapshot).as_bytes())?;
                    subscription.pump(&mut writer, HEARTBEAT_INTERVAL)
                });
            // A closed tab surfaces as a write error — the normal way a
            // stream ends — and dropping the subscription frees its slot.
            drop(streamed);
        })
        .map(|_| ())
        .map_err(|err| format!("Failed to start event stream worker: {err}"))
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
    let mut stream_guard = match runtime.try_begin_stream(client.clone()) {
        Ok(guard) => guard,
        Err(message) => return respond_error(request, StatusCode(429), &message),
    };
    // Queue the job before reading the body so a worker starts decoding while
    // the client is still speaking.
    let (frame_tx, frame_rx) = mpsc::channel::<StreamInput>();
    let result_rx = match runtime.enqueue(
        JobAudio::Stream(frame_rx),
        settings,
        "stream",
        client.clone(),
        0.0,
    ) {
        Ok(result_rx) => result_rx,
        Err(HostRuntimeError::QueueFull(message)) => {
            return respond_error(request, StatusCode(429), &message)
        }
        Err(HostRuntimeError::WorkerFailed(message)) => {
            return respond_error(request, StatusCode(500), &message)
        }
    };
    let received = forward_stream_frames(
        &mut request.as_reader(),
        runtime.max_recording_seconds(),
        |frame| {
            // A worker that already failed has dropped its receiver; keep
            // draining the body so the client still gets the worker's error.
            let _ = frame_tx.send(StreamInput::Frame(frame));
        },
    );
    let duration_seconds = match received {
        Ok(received) => received.duration_seconds(),
        Err(err) => {
            let _ = frame_tx.send(StreamInput::Abort);
            runtime.record_rejection(client.as_deref());
            return respond_error(request, stream_recording_error_status(&err), &err);
        }
    };
    drop(frame_tx);
    stream_guard.audio_seconds = duration_seconds;
    drop(stream_guard);

    let outcome = match runtime.await_result(result_rx) {
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

const MAX_CONFIG_BODY_BYTES: u64 = 4 * 1024;

fn handle_config_update(mut request: Request, runtime: Arc<HostRuntime>) -> Result<(), String> {
    let body = match read_limited_body(&mut request.as_reader(), MAX_CONFIG_BODY_BYTES) {
        Ok(body) => body,
        Err(err) => return respond_error(request, StatusCode(413), &err),
    };
    let update: HostConfigUpdate = match serde_json::from_slice(&body) {
        Ok(update) => update,
        Err(err) => {
            // serde quotes offending values, which could be the password.
            let message = if String::from_utf8_lossy(&body).contains("pairingPassword") {
                "Invalid config update: pairingPassword must be a string or null, and every other field as documented".to_string()
            } else {
                format!("Invalid config update: {err}")
            };
            return respond_error(request, StatusCode(400), &message);
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
        "pairingEnabled": runtime.live.pairing_enabled(),
    })
    .to_string();
    respond_json(request, StatusCode(200), body)
}

const MAX_PAIR_BODY_BYTES: u64 = 4 * 1024;

/// `POST /v1/pair`: the pairing password in, the host token out. Answers
/// never echo the request, which carries the password.
fn handle_pair(mut request: Request, runtime: &HostRuntime) -> Result<(), String> {
    let body = match read_limited_body(&mut request.as_reader(), MAX_PAIR_BODY_BYTES) {
        Ok(body) => body,
        Err(err) => return respond_error(request, StatusCode(413), &err),
    };
    let Ok(pair) = serde_json::from_slice::<PairRequest>(&body) else {
        return respond_error(
            request,
            StatusCode(400),
            r#"Invalid pair request: expected {"password": "<string>", "clientName": "<string>"}"#,
        );
    };
    let client = client_ip(&request);
    let outcome = {
        let auth = runtime.live.auth();
        let mut limiter = runtime
            .pairing
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        attempt_pairing(
            &auth,
            &mut limiter,
            client.as_deref().unwrap_or("unknown"),
            &pair.password,
            Instant::now(),
        )
    };
    if let Ok(metrics) = runtime.metrics.lock() {
        metrics.publish(HostEventKind::Pairing {
            client,
            client_name: pair.client_name(),
            ok: matches!(outcome, PairOutcome::Paired { .. }),
        });
    }
    let name = runtime.live.name();
    match outcome {
        PairOutcome::Paired { token } => respond_json(
            request,
            StatusCode(200),
            serde_json::json!({ "token": token, "name": name }).to_string(),
        ),
        PairOutcome::WrongPassword => respond_error(request, StatusCode(401), "wrong password"),
        PairOutcome::Disabled => respond_error(request, StatusCode(404), "pairing disabled"),
        PairOutcome::RateLimited {
            retry_after_seconds,
        } => {
            let retry_after = Header::from_bytes(
                &b"Retry-After"[..],
                retry_after_seconds.to_string().as_bytes(),
            )
            .map_err(|_| "Failed to create Retry-After header".to_string())?;
            let body = serde_json::json!({
                "error": "too many attempts",
                "retryAfterSeconds": retry_after_seconds,
            })
            .to_string();
            respond_json_with(request, StatusCode(429), body, Some(retry_after))
        }
    }
}

fn respond_update_status(
    request: Request,
    status: StatusCode,
    updater: &Updater,
) -> Result<(), String> {
    let body = serde_json::to_string(&updater.status())
        .map_err(|err| format!("Failed to serialize update status: {err}"))?;
    respond_json(request, status, body)
}

fn handle_update_settings(mut request: Request, updater: &Arc<Updater>) -> Result<(), String> {
    let body = match read_limited_body(&mut request.as_reader(), MAX_CONFIG_BODY_BYTES) {
        Ok(body) => body,
        Err(err) => return respond_error(request, StatusCode(413), &err),
    };
    let settings: UpdateSettingsRequest = match serde_json::from_slice(&body) {
        Ok(settings) => settings,
        Err(err) => {
            return respond_error(
                request,
                StatusCode(400),
                &format!("Invalid update settings: {err}"),
            )
        }
    };
    if let Err(message) = updater.apply_settings(&settings) {
        let status = if message.starts_with("Saving") {
            500
        } else {
            400
        };
        return respond_error(request, StatusCode(status), &message);
    }
    respond_update_status(request, StatusCode(200), updater)
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
    let Some(model) = SttModel::from_model_id(&download.model) else {
        return respond_error(
            request,
            StatusCode(400),
            &format!("Unsupported model: {}", download.model),
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
        metrics.set_model_download(ModelDownloadState {
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
                    metrics.set_model_download(ModelDownloadState {
                        model: model.model_id().to_string(),
                        stage: progress.stage.to_string(),
                        percentage: progress.percentage,
                        error: None,
                    });
                }
            });
            if let Err(err) = result {
                if let Ok(mut metrics) = metrics.lock() {
                    metrics.set_model_download(ModelDownloadState {
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
            metrics.set_model_download(ModelDownloadState {
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
    let peer = request.remote_addr()?.ip();
    // Behind `tailscale serve` every request arrives from loopback, so the
    // proxy's forwarding headers are the only way to tell tailnet devices
    // apart. They are only trusted from loopback, where a local proxy set them.
    if peer.is_loopback() {
        let forwarded = header_value(request, "x-forwarded-for")
            .or_else(|| header_value(request, "tailscale-user-login"))
            .and_then(|value| value.split(',').next())
            .map(str::trim)
            .filter(|value| !value.is_empty());
        if let Some(client) = forwarded {
            return Some(client.to_string());
        }
    }
    Some(peer.to_string())
}

struct HostRuntime {
    job_tx: SyncSender<TranscriptionJob>,
    metrics: Arc<Mutex<HostMetrics>>,
    live: Arc<HostLiveConfig>,
    models: ModelService,
    config_path: PathBuf,
    next_job_id: AtomicU64,
    pairing: Mutex<PairingLimiter>,
}

struct TranscriptionJob {
    id: u64,
    settings: Settings,
    audio: JobAudio,
    source: &'static str,
    client: Option<String>,
    result_tx: mpsc::Sender<Result<TranscriptionOutcome, String>>,
    accepted_at: Instant,
}

/// Audio for a job: a complete recording (batch uploads), or frames still
/// arriving from a client that is speaking, which the worker transcribes in
/// chunks as they land so only the tail is left to decode at release.
enum JobAudio {
    Recording(Recording),
    Stream(Receiver<StreamInput>),
}

enum StreamInput {
    Frame(AudioFrame),
    /// The upload failed part-way; the request handler already answered the
    /// client, so the worker drops the session without recording a failure.
    Abort,
}

/// What a worker hands back for a completed job. The model is the worker's
/// assigned model — the host's choice, not the client's — and is reported in
/// the response so clients always learn what actually ran.
#[derive(Debug)]
struct TranscriptionOutcome {
    text: String,
    backend: String,
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
    /// Audio received, reported when the stream finishes; stays 0 for an
    /// upload that failed part-way.
    audio_seconds: f32,
}

impl Drop for ActiveStreamGuard {
    fn drop(&mut self) {
        if let Ok(mut metrics) = self.metrics.lock() {
            metrics.finish_stream(self.stream_id, self.audio_seconds);
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
            pairing: Mutex::new(PairingLimiter::default()),
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
            audio_seconds: 0.0,
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
        settings: Settings,
        source: &'static str,
        client: Option<String>,
    ) -> Result<TranscriptionOutcome, HostRuntimeError> {
        let duration_seconds = recording.stats().duration_seconds;
        if let Err(err) = self.validate_recording_duration(duration_seconds) {
            self.metrics
                .lock()
                .map(|mut metrics| metrics.reject_job(client.as_deref()))
                .ok();
            return Err(HostRuntimeError::QueueFull(err));
        }

        let result_rx = self.enqueue(
            JobAudio::Recording(recording),
            settings,
            source,
            client,
            duration_seconds,
        )?;
        self.await_result(result_rx)
    }

    fn enqueue(
        &self,
        audio: JobAudio,
        mut settings: Settings,
        source: &'static str,
        client: Option<String>,
        duration_seconds: f32,
    ) -> Result<Receiver<Result<TranscriptionOutcome, String>>, HostRuntimeError> {
        // GPU use is an operator decision for the whole host, not a
        // per-request client choice.
        settings.use_gpu = self.live.use_gpu();
        let job_id = self.next_job_id.fetch_add(1, Ordering::Relaxed);
        let (result_tx, result_rx) = mpsc::channel::<Result<TranscriptionOutcome, String>>();
        let job = TranscriptionJob {
            id: job_id,
            settings,
            audio,
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
                let message = "Transcription queue is full".to_string();
                if let Ok(mut metrics) = self.metrics.lock() {
                    metrics.drop_queued_job(job_id, &message);
                    metrics.reject_job(client.as_deref());
                }
                return Err(HostRuntimeError::QueueFull(message));
            }
            Err(TrySendError::Disconnected(_)) => {
                let message = "Transcription worker queue is unavailable".to_string();
                if let Ok(mut metrics) = self.metrics.lock() {
                    metrics.drop_queued_job(job_id, &message);
                }
                return Err(HostRuntimeError::WorkerFailed(message));
            }
        }
        Ok(result_rx)
    }

    fn await_result(
        &self,
        result_rx: Receiver<Result<TranscriptionOutcome, String>>,
    ) -> Result<TranscriptionOutcome, HostRuntimeError> {
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
    // Mirrors the model/GPU pair held by this worker's loaded engine, so the
    // dashboard can show a "loading model" phase when an engine (re)load is in
    // flight. Each worker owns its own engine instance, so workers transcribe
    // in parallel off the shared job queue without sharing a model.
    let mut loaded: Option<(SttModel, bool)> = None;
    // Last (model, GPU, file mtime) whose load failed; retried only when the
    // target or the file on disk changes, so a corrupt model file is not
    // re-read on every idle poll.
    let mut last_failed: Option<(SttModel, bool, Option<SystemTime>)> = None;
    loop {
        // The served model is host configuration: load the assigned model
        // while idle so the first dictation never pays the load wait, and so
        // runtime model changes apply without a job arriving.
        let assigned_model = live.worker_model(worker_index);
        let target = (assigned_model, live.use_gpu());
        if loaded != Some(target) {
            if !models.files_present(assigned_model) {
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
                        assigned_model.model_id().to_string(),
                        format!(
                            "Model {} is not installed on this host",
                            assigned_model.model_id()
                        ),
                    );
                }
            } else {
                let mtime = models.installed_mtime(assigned_model);
                if last_failed != Some((target.0, target.1, mtime)) {
                    if let Ok(mut metrics) = metrics.lock() {
                        metrics
                            .worker_preloading(worker_index, assigned_model.model_id().to_string());
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
                                metrics.worker_model_unavailable(
                                    worker_index,
                                    assigned_model.model_id().to_string(),
                                    err,
                                );
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
        // A live stream's length is only known once the client stops sending.
        let mut duration_seconds = match &job.audio {
            JobAudio::Recording(recording) => recording.stats().duration_seconds,
            JobAudio::Stream(_) => 0.0,
        };
        let backend = job_backend(assigned_model).to_string();
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
                queue_wait,
                needs_load,
                RunningJobInfo {
                    id: job.id,
                    model: model.clone(),
                    source,
                    client: client.clone(),
                    audio_seconds: duration_seconds,
                    started_at: Instant::now(),
                },
            );
        }

        let mut started = Instant::now();
        let model_ready = std::cell::Cell::new(false);
        let on_model_ready = || {
            model_ready.set(true);
            if let Ok(mut metrics) = metrics.lock() {
                metrics.worker_model_ready(worker_index, model.clone());
            }
        };
        let result = match job.audio {
            JobAudio::Recording(recording) => transcribe_recording(
                &mut transcription,
                &models,
                &settings,
                recording,
                on_model_ready,
            ),
            JobAudio::Stream(frames) => transcribe_stream(
                &mut transcription,
                &models,
                &settings,
                frames,
                on_model_ready,
            )
            .map(|streamed| {
                duration_seconds = streamed.duration_seconds;
                // Report the wait after the client stopped sending, not the
                // time spent listening to it speak.
                started = streamed.upload_finished_at;
                streamed.text
            }),
        };
        let processing_time = started.elapsed();
        if model_ready.get() {
            loaded = Some(target);
        }

        match &result {
            Err(err) if err == STREAM_ABORTED => {
                if let Ok(mut metrics) = metrics.lock() {
                    metrics.abandon_job(worker_index);
                }
            }
            Ok(_) => {
                if let Ok(mut metrics) = metrics.lock() {
                    metrics.complete_job(
                        worker_index,
                        duration_seconds,
                        backend.clone(),
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
            backend: backend.clone(),
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
        backend: outcome.backend,
        model: outcome.model,
        server_version: Some(SERVER_VERSION.to_string()),
    }
}

/// The engine that ran a job. Jobs used to all report the protocol's default
/// `parakeet` id, which mislabelled Whisper work in the dashboard and in
/// clients' transcript history.
fn job_backend(model: SttModel) -> &'static str {
    if model.is_whisper() {
        "whisper"
    } else {
        BACKEND_ID
    }
}

fn respond_error(request: Request, status: StatusCode, message: &str) -> Result<(), String> {
    let body = serde_json::json!({ "error": message }).to_string();
    respond_json(request, status, body)
}

/// How much audio a stream upload delivered.
#[derive(Debug, Clone, Copy, PartialEq)]
struct ReceivedStream {
    samples: usize,
    sample_rate: u32,
}

impl ReceivedStream {
    fn duration_seconds(self) -> f32 {
        self.samples as f32 / self.sample_rate as f32
    }
}

/// Reads stream frames until the client closes the upload, handing each one
/// on as it arrives. Rejects a sample-rate change, an over-long recording and
/// an upload with no audio at all.
fn forward_stream_frames(
    reader: &mut impl Read,
    max_recording_seconds: u16,
    mut on_frame: impl FnMut(AudioFrame),
) -> Result<ReceivedStream, String> {
    let mut samples = 0usize;
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
        samples = samples.saturating_add(frame.pcm_i16.len());
        if samples > max_samples {
            return Err(format!(
                "Recording exceeds host maximum of {max_recording_seconds} seconds"
            ));
        }
        on_frame(frame);
    }

    match recording_sample_rate {
        Some(sample_rate) if samples > 0 => Ok(ReceivedStream {
            samples,
            sample_rate,
        }),
        _ => Err("No audio samples were captured".to_string()),
    }
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

const STREAM_ABORTED: &str = "Stream upload aborted";

struct StreamedTranscript {
    text: String,
    duration_seconds: f32,
    upload_finished_at: Instant,
}

/// Feeds frames into a chunked session while the client is still speaking,
/// keeping a copy of the full recording so a session that loses frames can
/// fall back to transcribing it whole.
fn transcribe_stream(
    transcription: &mut TranscriptionService,
    models: &ModelService,
    settings: &Settings,
    frames: Receiver<StreamInput>,
    on_model_ready: impl FnOnce(),
) -> Result<StreamedTranscript, String> {
    let mut sink = transcription
        .start_session_traced(settings, models, None, None)?
        .audio_tx;
    on_model_ready();
    let mut recording = Recording {
        pcm_i16: Vec::new(),
        sample_rate: 16_000,
        dropped_stream_frames: 0,
    };
    while let Ok(input) = frames.recv() {
        let frame = match input {
            StreamInput::Frame(frame) => frame,
            StreamInput::Abort => {
                transcription.cancel_session();
                return Err(STREAM_ABORTED.to_string());
            }
        };
        recording.sample_rate = frame.sample_rate;
        recording.pcm_i16.extend_from_slice(&frame.pcm_i16);
        if sink.as_ref().is_some_and(|tx| tx.send(frame).is_err()) {
            // The session stopped taking audio; finish_session transcribes
            // the full recording instead.
            sink = None;
            recording.dropped_stream_frames += 1;
        }
    }
    let upload_finished_at = Instant::now();
    drop(sink);
    let duration_seconds = recording.stats().duration_seconds;
    let text = transcription.finish_session(&recording, settings, models)?;
    Ok(StreamedTranscript {
        text,
        duration_seconds,
        upload_finished_at,
    })
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
    let backend = protocol_header(request, "backend").unwrap_or(BACKEND_ID);
    let language = protocol_header(request, "language")
        .unwrap_or("en")
        .to_string();
    let vocabulary_hints = protocol_header(request, "vocabulary-hints")
        .map(percent_decode)
        .and_then(|value| serde_json::from_str::<Vec<String>>(&value).ok())
        .unwrap_or_default();

    let settings = Settings {
        language,
        vocabulary_hints,
        ..Default::default()
    };
    if backend != "parakeet" && backend != "whisper" {
        return Err(format!("Unsupported transcription backend: {backend}"));
    }
    // The served model is host configuration. Clients still send an
    // x-fairspoken-model header; it is deliberately ignored — never an error —
    // and the response's `model` field reports what actually ran.
    Ok(settings)
}

fn authorized(request: &Request, token: Option<&str>) -> bool {
    let Some(token) = token.filter(|value| !value.trim().is_empty()) else {
        return true;
    };
    if header_value(request, "authorization")
        .and_then(|value| value.strip_prefix("Bearer "))
        .is_some_and(|value| constant_time_eq(value.as_bytes(), token.as_bytes()))
    {
        return true;
    }
    // Allow GETs to authenticate via ?token=… so the host operator can
    // bookmark a single URL in the browser. Mutating routes need the header,
    // which keeps the token out of proxy and access logs for writes.
    request.method() == &Method::Get
        && query_param(request.url(), "token")
            .is_some_and(|value| constant_time_eq(value.as_bytes(), token.as_bytes()))
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
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

/// An `x-fairspoken-<name>` request header. Clients from before the rename
/// send `x-multivoice-<name>`, which is still accepted.
fn protocol_header<'a>(request: &'a Request, name: &str) -> Option<&'a str> {
    header_value(request, &format!("x-fairspoken-{name}"))
        .or_else(|| header_value(request, &format!("x-multivoice-{name}")))
}

fn header_value<'a>(request: &'a Request, name: &str) -> Option<&'a str> {
    request
        .headers()
        .iter()
        .find(|header| header.field.to_string().eq_ignore_ascii_case(name))
        .map(|header| header.value.as_str())
}

fn respond_json(request: Request, status: StatusCode, body: String) -> Result<(), String> {
    respond_json_with(request, status, body, None)
}

fn respond_json_with(
    request: Request,
    status: StatusCode,
    body: String,
    extra: Option<Header>,
) -> Result<(), String> {
    let content_type = Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..])
        .map_err(|_| "Failed to create response content-type header".to_string())?;
    let mut response = Response::from_string(body)
        .with_status_code(status)
        .with_header(content_type);
    if let Some(header) = extra {
        response = response.with_header(header);
    }
    request
        .respond(response)
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
    /// Last state/model pair sent as a `worker_state` event.
    published: Option<(WorkerState, Option<String>)>,
}

impl WorkerStatus {
    fn new() -> Self {
        Self {
            state: WorkerState::Idle,
            loaded_model: None,
            job: None,
            completed_jobs: 0,
            last_error: None,
            published: None,
        }
    }
}

struct RunningJobInfo {
    id: u64,
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
    events: Arc<EventHub>,
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
            events: Arc::new(EventHub::default()),
        }
    }

    fn publish(&self, kind: HostEventKind) {
        self.events.publish(HostEvent {
            at: now_epoch_ms(),
            kind,
        });
    }

    /// Publishes a worker's state when it differs from the last one sent, so
    /// the idle poll re-reporting a missing model doesn't flood subscribers.
    /// `model` is what the worker holds or is working towards.
    fn publish_worker_state(&mut self, worker_index: usize, model: Option<String>) {
        let Some(worker) = self.workers.get_mut(worker_index) else {
            return;
        };
        let current = (worker.state, model);
        if worker.published.as_ref() == Some(&current) {
            return;
        }
        worker.published = Some(current.clone());
        self.publish(HostEventKind::WorkerState {
            worker: worker_index,
            state: current.0.as_str(),
            model: current.1,
        });
    }

    /// Frees a worker after its job ended, publishing the job's terminal
    /// event (built from the job it held) before the worker's idle state.
    fn release_worker(
        &mut self,
        worker_index: usize,
        terminal_event: impl FnOnce(RunningJobInfo) -> HostEventKind,
    ) {
        let Some(worker) = self.workers.get_mut(worker_index) else {
            return;
        };
        worker.state = WorkerState::Idle;
        let job = worker.job.take();
        let model = worker.loaded_model.clone();
        if let Some(job) = job {
            self.publish(terminal_event(job));
        }
        self.publish_worker_state(worker_index, model);
    }

    fn set_model_download(&mut self, state: ModelDownloadState) {
        // Multi-file downloads report the same overall percentage several
        // times; only changes are worth an event.
        let unchanged = self.model_download.as_ref().is_some_and(|current| {
            current.model == state.model
                && current.stage == state.stage
                && current.percentage == state.percentage
                && current.error == state.error
        });
        if unchanged {
            return;
        }
        self.publish(HostEventKind::ModelDownload {
            model: state.model.clone(),
            stage: state.stage.clone(),
            percentage: state.percentage,
            error: state.error.clone(),
        });
        self.model_download = Some(state);
    }

    fn active_stream_count(&self) -> u32 {
        self.active_streams.len() as u32
    }

    fn running_job_count(&self) -> u32 {
        self.workers.iter().filter(|w| w.job.is_some()).count() as u32
    }

    /// Nothing streaming, queued or running: safe to restart for an update.
    fn is_idle(&self) -> bool {
        self.active_streams.is_empty() && self.queued.is_empty() && self.running_job_count() == 0
    }

    fn begin_stream(&mut self, client: Option<String>) -> u64 {
        self.next_stream_id = self.next_stream_id.saturating_add(1);
        let id = self.next_stream_id;
        self.publish(HostEventKind::StreamStarted {
            stream_id: id,
            client: client.clone(),
        });
        self.active_streams.push(ActiveStreamInfo {
            id,
            client,
            started_at: Instant::now(),
        });
        id
    }

    fn finish_stream(&mut self, stream_id: u64, audio_seconds: f32) {
        let Some(position) = self
            .active_streams
            .iter()
            .position(|stream| stream.id == stream_id)
        else {
            return;
        };
        let stream = self.active_streams.remove(position);
        self.publish(HostEventKind::StreamFinished {
            stream_id,
            client: stream.client,
            audio_seconds,
        });
    }

    fn enqueue_job(&mut self, job: QueuedJobInfo) {
        self.publish(HostEventKind::JobQueued {
            job_id: job.id,
            client: job.client.clone(),
            source: job.source,
            model: job.model.clone(),
            audio_seconds: job.audio_seconds,
        });
        self.queued.push_back(job);
    }

    fn dequeue_job(&mut self, job_id: u64) {
        self.queued.retain(|job| job.id != job_id);
    }

    /// Removes a job that never reached a worker (the queue was full or gone).
    fn drop_queued_job(&mut self, job_id: u64, error: &str) {
        let client = self
            .queued
            .iter()
            .find(|job| job.id == job_id)
            .and_then(|job| job.client.clone());
        self.dequeue_job(job_id);
        self.publish(HostEventKind::JobFailed {
            job_id,
            worker: None,
            client,
            error: error.to_string(),
        });
    }

    fn start_job(
        &mut self,
        worker_index: usize,
        queue_wait: Duration,
        needs_load: bool,
        job: RunningJobInfo,
    ) {
        self.dequeue_job(job.id);
        self.started_jobs = self.started_jobs.saturating_add(1);
        self.total_queue_wait_ms = self
            .total_queue_wait_ms
            .saturating_add(queue_wait.as_millis());
        self.publish(HostEventKind::JobStarted {
            job_id: job.id,
            worker: worker_index,
            model: job.model.clone(),
            client: job.client.clone(),
            queue_wait_ms: queue_wait.as_millis() as u64,
        });
        let model = job.model.clone();
        if let Some(worker) = self.workers.get_mut(worker_index) {
            worker.state = if needs_load {
                WorkerState::Loading
            } else {
                WorkerState::Transcribing
            };
            worker.job = Some(job);
        }
        self.publish_worker_state(worker_index, Some(model));
    }

    fn worker_model_ready(&mut self, worker_index: usize, model: String) {
        if let Some(worker) = self.workers.get_mut(worker_index) {
            worker.loaded_model = Some(model.clone());
            worker.state = WorkerState::Transcribing;
        }
        self.publish_worker_state(worker_index, Some(model));
    }

    /// Marks a worker as loading its assigned model outside a job (the warm
    /// preload at startup or after a runtime model change).
    fn worker_preloading(&mut self, worker_index: usize, model: String) {
        if let Some(worker) = self.workers.get_mut(worker_index) {
            worker.state = WorkerState::Loading;
        }
        self.publish_worker_state(worker_index, Some(model));
    }

    fn worker_preload_ready(&mut self, worker_index: usize, model: String) {
        if let Some(worker) = self.workers.get_mut(worker_index) {
            worker.state = WorkerState::Idle;
            worker.loaded_model = Some(model.clone());
            worker.last_error = None;
        }
        self.publish_worker_state(worker_index, Some(model));
    }

    fn worker_model_unavailable(&mut self, worker_index: usize, model: String, message: String) {
        if let Some(worker) = self.workers.get_mut(worker_index) {
            worker.state = WorkerState::ModelUnavailable;
            worker.loaded_model = None;
            worker.last_error = Some(message);
        }
        self.publish_worker_state(worker_index, Some(model));
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

    /// Frees a worker whose job ended because the client's upload failed. The
    /// request handler already counted that as a rejection; subscribers still
    /// see the job end, so every `job_queued` gets exactly one terminal event.
    fn abandon_job(&mut self, worker_index: usize) {
        self.release_worker(worker_index, |job| HostEventKind::JobFailed {
            job_id: job.id,
            worker: Some(worker_index),
            client: job.client,
            error: STREAM_ABORTED.to_string(),
        });
    }

    fn fail_job(&mut self, worker_index: usize, client: Option<&str>, error: &str) {
        self.failed_jobs = self.failed_jobs.saturating_add(1);
        if let Some(worker) = self.workers.get_mut(worker_index) {
            worker.last_error = Some(error.to_string());
        }
        self.release_worker(worker_index, |job| HostEventKind::JobFailed {
            job_id: job.id,
            worker: Some(worker_index),
            client: job.client,
            error: error.to_string(),
        });
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
            worker.completed_jobs = worker.completed_jobs.saturating_add(1);
            worker.last_error = None;
        }
        self.release_worker(worker_index, |job| HostEventKind::JobCompleted {
            job_id: job.id,
            worker: worker_index,
            model: model.clone(),
            client: job.client,
            audio_seconds: duration_seconds,
            processing_ms: processing_time.as_millis() as u64,
        });
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
                    model_available: models.files_present(assigned),
                    loaded_model: worker.loaded_model.clone(),
                    completed_jobs: worker.completed_jobs,
                    last_error: worker.last_error.clone(),
                    job: worker.job.as_ref().map(|job| RunningJobSnapshot {
                        id: job.id,
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
                id: stream.id,
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
            pairing_enabled: live.pairing_enabled(),
            model: live.model_summary(),
            models: model_snapshots(models, &assigned_models),
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
    id: u64,
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
    id: u64,
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
    /// Whether clients can pair with a password; the password itself is
    /// never reported.
    pairing_enabled: bool,
    /// The served model: one id when uniform across workers, else "mixed".
    model: String,
    /// Every model this build can serve, installed or not, with the workers
    /// assigned to each.
    models: Vec<ModelSnapshot>,
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
        constant_time_eq, forward_stream_frames, max_batch_body_bytes, parse_worker_models,
        read_limited_body, request_path, HostLiveConfig, HostMetrics, HostRuntime,
        HostRuntimeConfig, HostRuntimeError, ModelDownloadState, QueuedJobInfo, RunningJobInfo,
        TranscriptionOutcome, MAX_TRACKED_CLIENTS,
    };
    use crate::audio::Recording;
    #[cfg(feature = "whisper")]
    use crate::models::WhisperModel;
    use crate::models::{ModelService, SttModel};
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

    #[test]
    fn constant_time_eq_matches_only_identical_tokens() {
        assert!(constant_time_eq(b"secret", b"secret"));
        assert!(!constant_time_eq(b"secret", b"secreT"));
        assert!(!constant_time_eq(b"secret", b"secret2"));
        assert!(!constant_time_eq(b"", b"secret"));
    }

    fn test_config() -> HostRuntimeConfig {
        HostRuntimeConfig {
            worker_count: 1,
            queue_capacity: 1,
            max_active_streams: 1,
            max_recording_seconds: 10,
            use_gpu: true,
            worker_models: vec![SttModel::Parakeet],
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
                "fairspoken-host-config-test-{}.json",
                std::process::id()
            )),
            next_job_id: AtomicU64::new(1),
            pairing: Mutex::new(super::PairingLimiter::default()),
        }
    }

    fn test_outcome(text: &str) -> TranscriptionOutcome {
        TranscriptionOutcome {
            text: text.to_string(),
            backend: "whisper".to_string(),
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

    fn test_running_job(id: u64, client: Option<&str>) -> RunningJobInfo {
        RunningJobInfo {
            id,
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
                backend: "whisper".to_string(),
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

        let err = forward_stream_frames(&mut encoded.as_slice(), 10, |_| {})
            .expect_err("mixed sample rates reject");

        assert!(err.contains("sample rate changed"));
    }

    #[test]
    fn stream_recording_preserves_consistent_sample_rate() {
        let mut encoded = Vec::new();
        write_test_stream_frame(16_000, &[1, 2, 3], &mut encoded);
        write_test_stream_frame(16_000, &[4, 5, 6], &mut encoded);

        let mut pcm_i16 = Vec::new();
        let received = forward_stream_frames(&mut encoded.as_slice(), 10, |frame| {
            assert_eq!(frame.sample_rate, 16_000);
            pcm_i16.extend_from_slice(&frame.pcm_i16);
        })
        .expect("recording");

        assert_eq!(received.sample_rate, 16_000);
        assert_eq!(received.samples, 6);
        assert_eq!(pcm_i16, vec![1, 2, 3, 4, 5, 6]);
    }

    #[test]
    fn stream_upload_without_audio_is_rejected() {
        let mut encoded = Vec::new();
        write_test_stream_frame(16_000, &[], &mut encoded);

        let err = forward_stream_frames(&mut encoded.as_slice(), 10, |_| {})
            .expect_err("empty upload rejects");

        assert_eq!(err, "No audio samples were captured");
    }

    #[test]
    fn stream_upload_over_host_maximum_is_rejected() {
        let mut encoded = Vec::new();
        write_test_stream_frame(4, &[0; 4], &mut encoded);
        write_test_stream_frame(4, &[0; 4], &mut encoded);

        let err = forward_stream_frames(&mut encoded.as_slice(), 1, |_| {})
            .expect_err("over-long upload rejects");

        assert!(err.contains("exceeds host maximum"));
    }

    #[test]
    fn metrics_snapshot_separates_stream_queue_and_running_work() {
        let config = HostRuntimeConfig {
            worker_count: 2,
            queue_capacity: 4,
            max_active_streams: 3,
            max_recording_seconds: 120,
            use_gpu: true,
            worker_models: vec![SttModel::Parakeet, SttModel::Parakeet],
        };
        let live = HostLiveConfig::new(&config);
        let models = ModelService::default();
        let mut metrics = HostMetrics::new(&config);

        metrics.begin_stream(Some("192.168.1.31".to_string()));
        metrics.client_request(Some("192.168.1.31"));
        metrics.enqueue_job(test_queued_job(1, Some("192.168.1.31")));
        metrics.start_job(
            0,
            Duration::from_millis(30),
            true,
            test_running_job(1, Some("192.168.1.31")),
        );
        metrics.worker_model_ready(0, "parakeet-tdt-0.6b-v3".to_string());
        metrics.complete_job(
            0,
            2.5,
            "parakeet".to_string(),
            "parakeet-tdt-0.6b-v3".to_string(),
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

        assert_eq!(snapshot.model, "parakeet-tdt-0.6b-v3");
        assert_eq!(snapshot.workers.len(), 2);
        assert_eq!(snapshot.workers[0].state, "idle");
        assert_eq!(snapshot.workers[0].assigned_model, "parakeet-tdt-0.6b-v3");
        assert_eq!(
            snapshot.workers[0].loaded_model.as_deref(),
            Some("parakeet-tdt-0.6b-v3")
        );
        assert_eq!(snapshot.workers[0].completed_jobs, 1);
        assert_eq!(snapshot.workers[1].state, "idle");
        assert_eq!(snapshot.workers[1].assigned_model, "parakeet-tdt-0.6b-v3");
        assert_eq!(snapshot.workers[1].loaded_model, None);

        assert_eq!(snapshot.streams.len(), 1);
        assert_eq!(snapshot.streams[0].client.as_deref(), Some("192.168.1.31"));

        assert_eq!(snapshot.clients.len(), 1);
        assert_eq!(snapshot.clients[0].address, "192.168.1.31");
        assert_eq!(snapshot.clients[0].requests, 1);
        assert_eq!(snapshot.clients[0].completed, 1);
        assert_eq!(
            snapshot.clients[0].last_model.as_deref(),
            Some("parakeet-tdt-0.6b-v3")
        );
    }

    #[test]
    fn failed_job_clears_running_state_and_counts_failure() {
        let config = test_config();
        let mut metrics = HostMetrics::new(&config);

        metrics.enqueue_job(test_queued_job(1, None));
        metrics.start_job(
            0,
            Duration::from_millis(5),
            false,
            test_running_job(1, None),
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
        metrics.start_job(0, Duration::from_millis(5), true, test_running_job(1, None));
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
                pairing_password: None,
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
                pairing_password: None,
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
                pairing_password: None,
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
            pairing_password: None,
        }
    }

    #[test]
    fn config_update_sets_served_model_for_all_workers() {
        let config = HostRuntimeConfig {
            worker_count: 2,
            worker_models: vec![SttModel::Parakeet, SttModel::Parakeet],
            ..test_config()
        };
        let live = HostLiveConfig::new(&config);

        super::apply_config_update(&model_update(Some("parakeet"), None), &live)
            .expect("model update applies");
        assert_eq!(
            live.worker_models(),
            vec![SttModel::Parakeet, SttModel::Parakeet]
        );
        assert_eq!(live.model_summary(), "parakeet-tdt-0.6b-v3");

        let err = super::apply_config_update(&model_update(Some("gpt-4"), None), &live)
            .expect_err("unknown model rejects");
        assert!(err.contains("Unsupported model"));
        assert_eq!(live.model_summary(), "parakeet-tdt-0.6b-v3");
    }

    #[test]
    fn config_update_rejects_ambiguous_and_wrong_length_model_lists() {
        let config = HostRuntimeConfig {
            worker_count: 2,
            worker_models: vec![SttModel::Parakeet, SttModel::Parakeet],
            ..test_config()
        };
        let live = HostLiveConfig::new(&config);

        let err = super::apply_config_update(&model_update(None, Some(vec!["parakeet"])), &live)
            .expect_err("wrong list length rejects");
        assert!(err.contains("exactly 2"));

        let err = super::apply_config_update(
            &model_update(Some("parakeet"), Some(vec!["parakeet", "parakeet"])),
            &live,
        )
        .expect_err("ambiguous update rejects");
        assert!(err.contains("not both"));
    }

    #[cfg(feature = "whisper")]
    #[test]
    fn config_update_assigns_whisper_models_per_worker() {
        let config = HostRuntimeConfig {
            worker_count: 2,
            worker_models: vec![SttModel::Parakeet, SttModel::Parakeet],
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
            vec![
                SttModel::Whisper(WhisperModel::LargeV3Turbo),
                SttModel::Whisper(WhisperModel::Small)
            ]
        );
        assert_eq!(live.model_summary(), "mixed");
    }

    #[test]
    fn parse_worker_models_expands_defaults_and_rejects_bad_values() {
        assert_eq!(
            parse_worker_models(None, 2).expect("default models"),
            vec![SttModel::Parakeet, SttModel::Parakeet]
        );
        assert_eq!(
            parse_worker_models(Some("parakeet"), 2).expect("single model expands"),
            vec![SttModel::Parakeet, SttModel::Parakeet]
        );

        let err = parse_worker_models(Some("turbo-9000"), 1).expect_err("unknown model fails");
        assert!(err.contains("unsupported model"));
        let err = parse_worker_models(Some("parakeet,parakeet,parakeet"), 2)
            .expect_err("wrong count fails");
        assert!(err.contains("3 models for 2 workers"));
    }

    #[cfg(feature = "whisper")]
    #[test]
    fn parse_worker_models_assigns_whisper_models_per_worker() {
        assert_eq!(
            parse_worker_models(Some(" large-v3-turbo , small "), 2).expect("per-worker list"),
            vec![
                SttModel::Whisper(WhisperModel::LargeV3Turbo),
                SttModel::Whisper(WhisperModel::Small)
            ]
        );
    }

    #[test]
    fn persisted_config_round_trips_dashboard_edits() {
        let path = std::env::temp_dir().join(format!(
            "fairspoken-host-config-roundtrip-{}.json",
            std::process::id()
        ));
        let config = HostRuntimeConfig {
            worker_count: 2,
            worker_models: vec![SttModel::Parakeet, SttModel::Parakeet],
            ..test_config()
        };
        let live = HostLiveConfig::new(&config);
        super::apply_config_update(
            &super::HostConfigUpdate {
                max_active_streams: Some(8),
                max_recording_seconds: Some(120),
                use_gpu: Some(false),
                model: None,
                worker_models: Some(vec!["parakeet".to_string(), "parakeet".to_string()]),
                pairing_password: None,
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
            worker_models: vec![SttModel::Parakeet, SttModel::Parakeet],
            ..test_config()
        };
        super::overlay_persisted_config(&mut restarted, persisted);
        assert_eq!(restarted.max_active_streams, 8);
        assert_eq!(restarted.max_recording_seconds, 120);
        assert!(!restarted.use_gpu);
        assert_eq!(
            restarted.worker_models,
            vec![SttModel::Parakeet, SttModel::Parakeet]
        );

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn config_update_sets_and_clears_the_pairing_password() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("host-config.json");
        let config = test_config();
        let live = HostLiveConfig::new(&config);
        let update = |body: &str| -> Result<(), String> {
            let update: super::HostConfigUpdate =
                serde_json::from_str(body).map_err(|err| err.to_string())?;
            super::apply_config_update(&update, &live)
        };

        // Too short: rejected with nothing applied, and the error doesn't echo it.
        let err = update(r#"{"maxActiveStreams":3,"pairingPassword":"abc"}"#).unwrap_err();
        assert!(!err.contains("abc"));
        assert_eq!(live.max_active_streams(), 1);
        assert!(!live.pairing_enabled());

        // A host with no token generates one so pairing has something to hand out.
        update(r#"{"pairingPassword":"123456"}"#).unwrap();
        assert!(live.pairing_enabled());
        let token = live.token().expect("generated token");
        super::persist_live_config(&path, &live).unwrap();
        let saved = super::load_persisted_config(&path).unwrap().unwrap();
        assert_eq!(saved.token.as_deref(), Some(token.as_str()));
        assert_eq!(saved.pairing_password.as_deref(), Some("123456"));

        let metrics = HostMetrics::new(&config);
        let stats =
            serde_json::to_value(metrics.snapshot("127.0.0.1:0", &live, &ModelService::default()))
                .unwrap();
        assert_eq!(stats["pairingEnabled"], true);
        assert!(!stats.to_string().contains("123456"));

        // Absent leaves it alone; null and "" turn it off and keep the token.
        update(r#"{"useGpu":false}"#).unwrap();
        assert!(live.pairing_enabled());
        update(r#"{"pairingPassword":null}"#).unwrap();
        assert!(!live.pairing_enabled());
        assert_eq!(live.token(), Some(token));
        update(r#"{"pairingPassword":"654321"}"#).unwrap();
        update(r#"{"pairingPassword":""}"#).unwrap();
        assert!(!live.pairing_enabled());
        assert!(update(r#"{"pairingPassword":123456}"#).is_err());
    }

    #[test]
    fn overlay_clamps_ranges_and_adapts_stale_worker_model_lists() {
        let mut config = HostRuntimeConfig {
            worker_count: 2,
            worker_models: vec![SttModel::Parakeet, SttModel::Parakeet],
            ..test_config()
        };
        super::overlay_persisted_config(
            &mut config,
            super::PersistedHostConfig {
                max_active_streams: 999,
                max_recording_seconds: 1,
                use_gpu: false,
                // Written when the host ran three workers; now it runs two.
                worker_models: vec![SttModel::Parakeet, SttModel::Parakeet, SttModel::Parakeet],
                update: Default::default(),
                token: None,
                pairing_password: None,
                name: None,
            },
        );
        assert_eq!(config.max_active_streams, 32);
        assert_eq!(config.max_recording_seconds, 10);
        assert!(!config.use_gpu);
        assert_eq!(
            config.worker_models,
            vec![SttModel::Parakeet, SttModel::Parakeet]
        );
    }

    #[test]
    fn missing_config_file_is_first_boot_and_corrupt_file_fails() {
        let missing = std::env::temp_dir().join(format!(
            "fairspoken-host-config-missing-{}.json",
            std::process::id()
        ));
        assert!(super::load_persisted_config(&missing)
            .expect("missing file is fine")
            .is_none());

        let corrupt = std::env::temp_dir().join(format!(
            "fairspoken-host-config-corrupt-{}.json",
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

        metrics.worker_model_unavailable(
            0,
            "base".to_string(),
            "Model base is not installed on this host".into(),
        );
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

        metrics.worker_preloading(0, "base".to_string());
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

    fn drain_event_names(subscription: &super::events::EventSubscription) -> Vec<String> {
        std::iter::from_fn(|| subscription.try_next())
            .map(|frame| {
                assert!(
                    !frame.contains("\"text\""),
                    "events never carry transcripts"
                );
                frame
                    .lines()
                    .next()
                    .and_then(|line| line.strip_prefix("event: "))
                    .unwrap_or_default()
                    .to_string()
            })
            .collect()
    }

    #[test]
    fn metrics_mutations_publish_one_lifecycle_per_job() {
        let config = test_config();
        let mut metrics = HostMetrics::new(&config);
        let subscription = metrics.events.subscribe().expect("subscribe");

        let stream_id = metrics.begin_stream(Some("192.168.1.31".to_string()));
        metrics.enqueue_job(test_queued_job(1, Some("192.168.1.31")));
        metrics.start_job(
            0,
            Duration::from_millis(4),
            true,
            test_running_job(1, Some("192.168.1.31")),
        );
        metrics.worker_model_ready(0, "large-v3-turbo".to_string());
        metrics.finish_stream(stream_id, 2.5);
        metrics.complete_job(
            0,
            2.5,
            "whisper".to_string(),
            "large-v3-turbo".to_string(),
            "stream",
            Some("192.168.1.31"),
            Duration::from_millis(4),
            Duration::from_millis(80),
        );

        assert_eq!(
            drain_event_names(&subscription),
            [
                "stream_started",
                "job_queued",
                "job_started",
                "worker_state",
                "worker_state",
                "stream_finished",
                "job_completed",
                "worker_state",
            ]
        );
        assert_eq!(metrics.recent[0].backend, "whisper");

        // A job the queue turned away still ends, with no worker.
        metrics.enqueue_job(test_queued_job(2, None));
        metrics.drop_queued_job(2, "Transcription queue is full");
        let frames: Vec<_> = std::iter::from_fn(|| subscription.try_next()).collect();
        assert_eq!(frames.len(), 2);
        assert!(frames[1].starts_with("event: job_failed\n"));
        assert!(frames[1].contains("\"jobId\":2,\"worker\":null,"));
        assert!(metrics.queued.is_empty());
    }

    #[test]
    fn worker_state_events_are_deduplicated_across_idle_polls() {
        let config = test_config();
        let mut metrics = HostMetrics::new(&config);
        let subscription = metrics.events.subscribe().expect("subscribe");

        for _ in 0..5 {
            metrics.worker_model_unavailable(0, "base".to_string(), "missing".to_string());
        }
        metrics.worker_preloading(0, "base".to_string());
        metrics.worker_preload_ready(0, "base".to_string());

        let frames: Vec<_> = std::iter::from_fn(|| subscription.try_next()).collect();
        assert_eq!(frames.len(), 3);
        assert!(frames[0].contains("\"state\":\"model-unavailable\",\"model\":\"base\""));
        assert!(frames[1].contains("\"state\":\"loading\""));
        assert!(frames[2].contains("\"state\":\"idle\""));
    }

    #[test]
    fn abandoned_and_failed_jobs_publish_job_failed() {
        let config = test_config();
        let mut metrics = HostMetrics::new(&config);
        metrics.start_job(0, Duration::ZERO, false, test_running_job(3, None));
        let subscription = metrics.events.subscribe().expect("subscribe");

        metrics.abandon_job(0);
        metrics.start_job(0, Duration::ZERO, false, test_running_job(4, None));
        metrics.fail_job(0, None, "Whisper context failed");

        let frames: Vec<_> = std::iter::from_fn(|| subscription.try_next()).collect();
        let failures: Vec<_> = frames
            .iter()
            .filter(|frame| frame.starts_with("event: job_failed\n"))
            .collect();
        assert_eq!(failures.len(), 2);
        assert!(failures[0].contains("\"jobId\":3,\"worker\":0,"));
        assert!(failures[0].contains(super::STREAM_ABORTED));
        assert!(failures[1].contains("\"error\":\"Whisper context failed\""));
        assert_eq!(metrics.failed_jobs, 1);
    }

    #[test]
    fn model_download_events_skip_repeated_progress() {
        let config = test_config();
        let mut metrics = HostMetrics::new(&config);
        let subscription = metrics.events.subscribe().expect("subscribe");
        let progress = |percentage| ModelDownloadState {
            model: "base".to_string(),
            stage: "downloading".to_string(),
            percentage,
            error: None,
        };

        metrics.set_model_download(progress(10));
        metrics.set_model_download(progress(10));
        metrics.set_model_download(progress(11));

        let frames: Vec<_> = std::iter::from_fn(|| subscription.try_next()).collect();
        assert_eq!(frames.len(), 2);
        assert!(frames[1].contains("\"stage\":\"downloading\",\"percentage\":11"));
        assert_eq!(metrics.model_download.as_ref().unwrap().percentage, 11);
    }

    #[test]
    fn stats_snapshot_lists_servable_models_with_worker_assignments() {
        let config = HostRuntimeConfig {
            worker_count: 2,
            worker_models: vec![SttModel::Parakeet, SttModel::ParakeetUltra],
            ..test_config()
        };
        let live = HostLiveConfig::new(&config);
        let models = ModelService::default();
        let metrics = HostMetrics::new(&config);

        let json = serde_json::to_value(metrics.snapshot("127.0.0.1:48173", &live, &models))
            .expect("serialize snapshot");
        let listed = json["models"].as_array().expect("models array");
        assert!(listed.len() >= 3);
        let by_id = |id: &str| {
            listed
                .iter()
                .find(|model| model["id"] == id)
                .unwrap_or_else(|| panic!("{id} listed"))
        };
        let ultra = by_id("parakeet-ultra");
        assert_eq!(ultra["name"], "Parakeet Ultra 0.6B");
        assert_eq!(ultra["publisher"], "Moondream");
        assert_eq!(ultra["assignedWorkers"], serde_json::json!([1]));
        assert!(ultra["sizeBytes"].as_u64().unwrap() > 0);
        assert!(ultra["installed"].is_boolean());
        assert_eq!(
            by_id("parakeet-tdt-0.6b-v3")["assignedWorkers"],
            serde_json::json!([0])
        );
        assert_eq!(
            by_id("parakeet-tdt-0.6b-v2")["assignedWorkers"],
            serde_json::json!([])
        );
    }

    #[cfg(feature = "whisper")]
    #[test]
    fn jobs_report_the_engine_that_ran_them() {
        assert_eq!(super::job_backend(SttModel::Parakeet), "parakeet");
        assert_eq!(super::job_backend(SttModel::ParakeetUltra), "parakeet");
        assert_eq!(
            super::job_backend(SttModel::Whisper(WhisperModel::LargeV3Turbo)),
            "whisper"
        );
    }

    #[test]
    fn event_stream_flushes_each_event_over_a_real_connection() {
        use std::io::{BufRead, BufReader, Write};

        let config = test_config();
        let metrics = Arc::new(Mutex::new(HostMetrics::new(&config)));
        let (job_tx, _job_rx) = mpsc::sync_channel(1);
        let runtime = Arc::new(test_runtime(config, job_tx, Arc::clone(&metrics)));
        runtime.live.set_auth(super::HostAuth {
            token: Some("secret".to_string()),
            ..Default::default()
        });
        let updater = test_updater(&runtime, "0.2.0", "http://127.0.0.1:9/");
        let server = tiny_http::Server::http("127.0.0.1:0").expect("bind");
        let addr = server.server_addr().to_ip().expect("ip address");
        let accept_metrics = Arc::clone(&metrics);
        thread::spawn(move || {
            let request = server.recv().expect("request");
            super::handle_request(request, runtime, accept_metrics, &updater, "127.0.0.1:0")
                .expect("handled");
            // Keep the listener (and the connection it owns) alive while the
            // stream thread runs.
            thread::sleep(Duration::from_secs(5));
        });

        let mut socket = std::net::TcpStream::connect(addr).expect("connect");
        socket
            .set_read_timeout(Some(Duration::from_secs(3)))
            .expect("timeout");
        socket
            .write_all(b"GET /v1/events?token=secret HTTP/1.1\r\nHost: test\r\n\r\n")
            .expect("send request");
        let mut reader = BufReader::new(socket);
        let mut read_line = || {
            let mut line = String::new();
            reader.read_line(&mut line).expect("line before timeout");
            line
        };

        assert_eq!(read_line(), "HTTP/1.1 200 OK\r\n");
        let mut head = Vec::new();
        loop {
            let line = read_line();
            if line == "\r\n" {
                break;
            }
            head.push(line.to_ascii_lowercase());
        }
        assert!(head.contains(&"content-type: text/event-stream\r\n".to_string()));
        assert!(head.contains(&"cache-control: no-cache\r\n".to_string()));
        assert!(head.contains(&"transfer-encoding: chunked\r\n".to_string()));

        let _snapshot_size = read_line();
        assert_eq!(read_line(), "event: snapshot\n");
        assert!(read_line().starts_with("data: {\"serverVersion\""));
        assert_eq!(read_line(), "\n");
        assert_eq!(read_line(), "\r\n");

        // A single small event arrives on its own, well under the timeout.
        metrics
            .lock()
            .expect("metrics")
            .begin_stream(Some("10.0.0.2".to_string()));
        let _event_size = read_line();
        assert_eq!(read_line(), "event: stream_started\n");
        assert!(read_line().contains("\"streamId\":1,\"client\":\"10.0.0.2\""));
    }

    #[test]
    fn protocol_headers_prefer_fairspoken_and_accept_the_legacy_prefix() {
        use std::io::Write;

        let server = tiny_http::Server::http("127.0.0.1:0").expect("bind");
        let addr = server.server_addr().to_ip().expect("ip address");
        let parse = |headers: &str| {
            let mut socket = std::net::TcpStream::connect(addr).expect("connect");
            socket
                .write_all(
                    format!("POST /v1/transcriptions HTTP/1.1\r\nHost: test\r\n{headers}Content-Length: 0\r\n\r\n")
                        .as_bytes(),
                )
                .expect("send request");
            let request = server.recv().expect("request");
            super::settings_from_headers(&request)
        };

        assert_eq!(
            parse("x-fairspoken-language: de\r\n").unwrap().language,
            "de"
        );
        assert_eq!(
            parse("x-multivoice-language: fr\r\n").unwrap().language,
            "fr"
        );
        assert_eq!(
            parse("x-multivoice-language: fr\r\nx-fairspoken-language: de\r\n")
                .unwrap()
                .language,
            "de"
        );
        assert!(parse("x-fairspoken-backend: nonsense\r\n").is_err());
        assert!(parse("x-multivoice-backend: nonsense\r\n").is_err());
        assert_eq!(parse("").unwrap().language, "en");
    }

    fn test_updater(
        runtime: &HostRuntime,
        current_version: &str,
        feed_base: &str,
    ) -> Arc<super::Updater> {
        test_updater_with(
            runtime,
            current_version,
            feed_base,
            super::update::UPDATER_PUBLIC_KEY,
            std::env::temp_dir().join("transcription-host-never-replaced"),
        )
    }

    fn test_updater_with(
        runtime: &HostRuntime,
        current_version: &str,
        feed_base: &str,
        public_key: &str,
        exe_path: std::path::PathBuf,
    ) -> Arc<super::Updater> {
        Arc::new(super::Updater::new(
            super::UpdaterOptions {
                current_version: current_version.to_string(),
                feed_base: feed_base.to_string(),
                public_key: public_key.to_string(),
                exe_path,
                restart_mode: super::update::RestartMode::Reexec,
                checks_enabled: false,
                env_channel: None,
            },
            Arc::clone(&runtime.live),
            runtime.config_path.clone(),
            Box::new(|| true),
        ))
    }

    fn updater_runtime(config_path: std::path::PathBuf) -> HostRuntime {
        let config = test_config();
        let metrics = Arc::new(Mutex::new(HostMetrics::new(&config)));
        let (job_tx, _job_rx) = mpsc::sync_channel(1);
        let mut runtime = test_runtime(config, job_tx, metrics);
        runtime.config_path = config_path;
        runtime
    }

    /// Serves files from `routes` (path → body) until the test ends.
    fn serve_files(routes: Vec<(String, Vec<u8>)>) -> String {
        let server = tiny_http::Server::http("127.0.0.1:0").expect("bind feed");
        let addr = server.server_addr().to_ip().expect("ip address");
        thread::spawn(move || {
            for request in server.incoming_requests() {
                let found = routes
                    .iter()
                    .find(|(path, _)| path == request.url())
                    .map(|(_, body)| body.clone());
                let _ = match found {
                    Some(body) => request.respond(tiny_http::Response::from_data(body)),
                    None => request
                        .respond(tiny_http::Response::from_string("missing").with_status_code(404)),
                };
            }
        });
        format!("http://{addr}/")
    }

    fn wait_for_state(
        updater: &super::Updater,
        busy: &[super::update::UpdatePhase],
    ) -> super::update::UpdateStatus {
        let started = Instant::now();
        loop {
            let status = updater.status();
            if !busy.contains(&status.state) {
                return status;
            }
            assert!(
                started.elapsed() < Duration::from_secs(20),
                "updater stuck in {:?}",
                status.state
            );
            thread::sleep(Duration::from_millis(20));
        }
    }

    fn host_manifest(version: &str, channel: &str, asset: serde_json::Value) -> Vec<u8> {
        let platform = super::update::current_platform_key().unwrap_or("linux-x86_64");
        serde_json::json!({
            "version": version,
            "channel": channel,
            "pub_date": "2026-10-05T12:00:00Z",
            "notes": format!("https://github.com/sprintengine/fairspoken/releases/tag/v{version}"),
            "platforms": { platform: asset },
        })
        .to_string()
        .into_bytes()
    }

    #[test]
    fn update_settings_persist_and_resolve_the_channel() {
        use super::update::{ChannelSource, UpdateChannel, UpdateSettingsRequest};
        let dir = tempfile::tempdir().unwrap();
        let runtime = updater_runtime(dir.path().join("host-config.json"));
        let updater = test_updater(&runtime, "0.3.0-nightly.20261005.4", "http://127.0.0.1:9/");

        let status = updater.status();
        assert_eq!(status.channel, UpdateChannel::Nightly);
        assert_eq!(status.channel_source, ChannelSource::Version);
        assert!(!status.auto_update);

        let settings: UpdateSettingsRequest =
            serde_json::from_str(r#"{"channel":"stable","autoUpdate":true}"#).unwrap();
        updater.apply_settings(&settings).expect("settings apply");
        let status = updater.status();
        assert_eq!(status.channel, UpdateChannel::Stable);
        assert_eq!(status.channel_source, ChannelSource::Saved);
        assert!(status.auto_update);

        let persisted = super::load_persisted_config(&runtime.config_path)
            .unwrap()
            .expect("saved");
        assert_eq!(persisted.update.update_channel, Some(UpdateChannel::Stable));
        assert!(persisted.update.auto_update);
        // The dashboard configuration is saved alongside, unchanged.
        assert_eq!(persisted.max_active_streams, 1);

        // A config edit keeps the update settings.
        super::persist_live_config(&runtime.config_path, &runtime.live).unwrap();
        let persisted = super::load_persisted_config(&runtime.config_path)
            .unwrap()
            .unwrap();
        assert!(persisted.update.auto_update);

        let bad: UpdateSettingsRequest = serde_json::from_str(r#"{"channel":"beta"}"#).unwrap();
        assert!(updater.apply_settings(&bad).is_err());
        assert_eq!(updater.status().channel, UpdateChannel::Stable);

        // null follows the running version again.
        let follow: UpdateSettingsRequest = serde_json::from_str(r#"{"channel":null}"#).unwrap();
        updater.apply_settings(&follow).unwrap();
        assert_eq!(updater.status().channel_source, ChannelSource::Version);
        let _ = wait_for_state(&updater, &[super::update::UpdatePhase::Checking]);
    }

    #[test]
    fn update_check_reads_the_channel_feed_and_offers_switch_to_stable() {
        use super::update::UpdatePhase;
        let asset = serde_json::json!({
            "url": "https://example.test/h.tar.gz",
            "sha256": "a".repeat(64),
            "signature": "c2ln",
            "format": "tar.gz",
        });
        let feed = serve_files(vec![
            (
                "/host-stable.json".to_string(),
                host_manifest("0.2.0", "stable", asset.clone()),
            ),
            (
                "/host-nightly.json".to_string(),
                host_manifest("0.3.0-nightly.20261005.4", "nightly", asset),
            ),
        ]);
        let dir = tempfile::tempdir().unwrap();

        // A stable build on the stable feed: up to date.
        let runtime = updater_runtime(dir.path().join("a.json"));
        let updater = test_updater(&runtime, "0.2.0", &feed);
        updater.begin_check(false).unwrap();
        let status = wait_for_state(&updater, &[UpdatePhase::Checking]);
        assert_eq!(status.state, UpdatePhase::Idle, "{:?}", status.error);
        assert!(status.last_checked_ms.is_some());

        // A nightly build switched to stable: the lower stable is offered.
        let runtime = updater_runtime(dir.path().join("b.json"));
        runtime.live.set_update_prefs(super::update::UpdatePrefs {
            update_channel: Some(super::update::UpdateChannel::Stable),
            auto_update: false,
        });
        let updater = test_updater(&runtime, "0.2.1-nightly.20261001.1", &feed);
        updater.begin_check(false).unwrap();
        let status = wait_for_state(&updater, &[UpdatePhase::Checking]);
        assert_eq!(status.state, UpdatePhase::Available, "{:?}", status.error);
        let available = status.available.expect("offer");
        assert_eq!(available.version, "0.2.0");
        assert!(available.switch_to_stable);

        // A stable build on the nightly channel gets the nightly.
        let runtime = updater_runtime(dir.path().join("c.json"));
        runtime.live.set_update_prefs(super::update::UpdatePrefs {
            update_channel: Some(super::update::UpdateChannel::Nightly),
            auto_update: false,
        });
        let updater = test_updater(&runtime, "0.2.0", &feed);
        updater.begin_check(false).unwrap();
        let status = wait_for_state(&updater, &[UpdatePhase::Checking]);
        assert_eq!(
            status.available.map(|update| update.version).as_deref(),
            Some("0.3.0-nightly.20261005.4")
        );

        // No feed published yet (404 before the first release): up to date.
        let runtime = updater_runtime(dir.path().join("e.json"));
        let updater = test_updater(&runtime, "0.2.0", &serve_files(Vec::new()));
        updater.begin_check(false).unwrap();
        let status = wait_for_state(&updater, &[UpdatePhase::Checking]);
        assert_eq!(status.state, UpdatePhase::Idle, "{:?}", status.error);
        assert!(status.available.is_none() && status.error.is_none());

        // Nothing to install yet → install refuses; a dead feed is an error.
        let runtime = updater_runtime(dir.path().join("d.json"));
        let updater = test_updater(&runtime, "0.2.0", "http://127.0.0.1:9/");
        assert!(updater.begin_install(false).is_err());
        updater.begin_check(false).unwrap();
        let status = wait_for_state(&updater, &[UpdatePhase::Checking]);
        assert_eq!(status.state, UpdatePhase::Error);
        assert!(status.error.is_some());
        assert!(updater.begin_restart().is_err());
    }

    #[cfg(unix)]
    #[test]
    fn update_installs_end_to_end_from_a_local_feed() {
        use super::update::test_support::{script, sha256_hex, sign, tar_gz, test_key};
        use super::update::UpdatePhase;
        let key = test_key();
        let archive = tar_gz(&[
            (
                "fairspoken-0.3.0-transcription-host-linux-x64/LICENSE",
                b"mit",
            ),
            (
                "fairspoken-0.3.0-transcription-host-linux-x64/transcription-host",
                &script("0.3.0", true),
            ),
        ]);
        let base = serve_files(vec![("/h.tar.gz".to_string(), archive.clone())]);
        let asset = serde_json::json!({
            "url": format!("{base}h.tar.gz"),
            "sha256": sha256_hex(&archive),
            "signature": sign(&key, &archive),
            "format": "tar.gz",
        });
        let feed = serve_files(vec![(
            "/host-stable.json".to_string(),
            host_manifest("0.3.0", "stable", asset),
        )]);

        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("transcription-host");
        std::fs::write(&exe, script("0.2.0", true)).unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let runtime = updater_runtime(dir.path().join("host-config.json"));
        let updater = test_updater_with(&runtime, "0.2.0", &feed, &key.public_base64, exe.clone());

        updater.begin_check(false).unwrap();
        let status = wait_for_state(&updater, &[UpdatePhase::Checking]);
        assert_eq!(status.state, UpdatePhase::Available, "{:?}", status.error);
        updater.begin_install(false).unwrap();
        let status = wait_for_state(&updater, &[UpdatePhase::Downloading]);
        assert_eq!(status.state, UpdatePhase::Ready, "{:?}", status.error);
        assert_eq!(status.installed_version.as_deref(), Some("0.3.0"));
        super::update::smoke_check(&exe, "0.3.0").expect("new binary installed");
        super::update::smoke_check(&dir.path().join("transcription-host.previous"), "0.2.0")
            .expect("previous kept");
        // Ready: a further check or install is refused until the restart.
        assert!(updater.begin_check(false).is_err());
        assert!(updater.begin_install(false).is_err());
    }
}
