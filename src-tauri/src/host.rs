use crate::audio::AudioFrame;
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
use std::sync::{Arc, Mutex, TryLockError};
use std::thread;
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use tiny_http::{Header, Method, Request, Response, Server, StatusCode};

const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");
const DASHBOARD_HTML: &str = include_str!("host_dashboard.html");
const RECENT_CAPACITY: usize = 50;

pub fn run_transcription_host() -> Result<(), String> {
    let addr = env::var("MULTIVOICE_HOST_ADDR").unwrap_or_else(|_| "127.0.0.1:48173".to_string());
    let token = env::var("MULTIVOICE_HOST_TOKEN").ok();
    let server = Server::http(&addr)
        .map_err(|err| format!("Failed to start transcription host on {addr}: {err}"))?;
    println!("multivoice transcription host listening on http://{addr}");

    let models = ModelService::default();
    let transcription = Arc::new(Mutex::new(TranscriptionService::default()));
    let metrics = Arc::new(Mutex::new(HostMetrics::new()));

    for request in server.incoming_requests() {
        let response = handle_request(
            request,
            token.as_deref(),
            models.clone(),
            Arc::clone(&transcription),
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
    models: ModelService,
    transcription: Arc<Mutex<TranscriptionService>>,
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
        (&Method::Post, "/v1/transcriptions") => spawn_transcription_request(
            request,
            models,
            transcription,
            metrics,
            TranscriptionRequestKind::Batch,
        ),
        (&Method::Post, "/v1/transcriptions/stream") => spawn_transcription_request(
            request,
            models,
            transcription,
            metrics,
            TranscriptionRequestKind::Stream,
        ),
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
    models: ModelService,
    transcription: Arc<Mutex<TranscriptionService>>,
    metrics: Arc<Mutex<HostMetrics>>,
    kind: TranscriptionRequestKind,
) -> Result<(), String> {
    thread::Builder::new()
        .name("transcription-host-request".to_string())
        .spawn(move || {
            let result = match kind {
                TranscriptionRequestKind::Batch => {
                    handle_batch_transcription(request, models, transcription, metrics)
                }
                TranscriptionRequestKind::Stream => {
                    handle_stream_transcription(request, models, transcription, metrics)
                }
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
    models: ModelService,
    transcription: Arc<Mutex<TranscriptionService>>,
    metrics: Arc<Mutex<HostMetrics>>,
) -> Result<(), String> {
    let mut transcription = match transcription.try_lock() {
        Ok(transcription) => transcription,
        Err(TryLockError::WouldBlock) => {
            return respond_error(
                request,
                StatusCode(429),
                "A transcription is already running",
            )
        }
        Err(TryLockError::Poisoned(_)) => {
            return Err("Transcription service lock failed".to_string())
        }
    };

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
    begin_host_session(&metrics)?;
    let text = match transcribe_recording(&mut transcription, &models, &settings, recording) {
        Ok(text) => text,
        Err(err) => {
            abort_host_session(&metrics)?;
            return respond_error(request, StatusCode(500), &err);
        }
    };
    complete_host_session(&metrics, duration_seconds, &settings, "batch")?;
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
    models: ModelService,
    transcription: Arc<Mutex<TranscriptionService>>,
    metrics: Arc<Mutex<HostMetrics>>,
) -> Result<(), String> {
    let mut transcription = match transcription.try_lock() {
        Ok(transcription) => transcription,
        Err(TryLockError::WouldBlock) => {
            return respond_error(
                request,
                StatusCode(429),
                "A transcription is already running",
            )
        }
        Err(TryLockError::Poisoned(_)) => {
            return Err("Transcription service lock failed".to_string())
        }
    };

    let settings = match settings_from_headers(&request) {
        Ok(settings) => settings,
        Err(err) => return respond_error(request, StatusCode(400), &err),
    };
    begin_host_session(&metrics)?;
    let (text, duration_seconds) =
        match transcribe_stream(&mut transcription, &models, &settings, &mut request) {
            Ok(result) => result,
            Err(err) => {
                abort_host_session(&metrics)?;
                return respond_error(request, StatusCode(500), &err);
            }
        };
    complete_host_session(&metrics, duration_seconds, &settings, "stream")?;
    let response = transcription_response(text, duration_seconds, &settings);
    respond_json(
        request,
        StatusCode(200),
        serde_json::to_string(&response).map_err(|err| {
            format!("Failed to serialize streaming transcription response: {err}")
        })?,
    )
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

fn begin_host_session(metrics: &Arc<Mutex<HostMetrics>>) -> Result<(), String> {
    metrics
        .lock()
        .map_err(|_| "Host metrics lock failed".to_string())?
        .begin_session();
    Ok(())
}

fn abort_host_session(metrics: &Arc<Mutex<HostMetrics>>) -> Result<(), String> {
    metrics
        .lock()
        .map_err(|_| "Host metrics lock failed".to_string())?
        .abort_session();
    Ok(())
}

fn complete_host_session(
    metrics: &Arc<Mutex<HostMetrics>>,
    duration_seconds: f32,
    settings: &Settings,
    source: &'static str,
) -> Result<(), String> {
    metrics
        .lock()
        .map_err(|_| "Host metrics lock failed".to_string())?
        .complete_session(
            duration_seconds,
            backend_id(settings.transcription_backend).to_string(),
            selected_model_id(settings).to_string(),
            source,
        );
    Ok(())
}

fn respond_error(request: Request, status: StatusCode, message: &str) -> Result<(), String> {
    let body = serde_json::json!({ "error": message }).to_string();
    respond_json(request, status, body)
}

fn transcribe_stream(
    transcription: &mut TranscriptionService,
    models: &ModelService,
    settings: &Settings,
    request: &mut Request,
) -> Result<(String, f32), String> {
    let stream_sink = transcription.start_session(settings, models)?;
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
        pcm_i16.extend_from_slice(&frame.pcm_i16);
        if let Some(sink) = &stream_sink {
            if sink.send(frame).is_err() {
                transcription.cancel_session();
                return Err(
                    "Sherpa transcription worker stopped while receiving stream".to_string()
                );
            }
        }
    }

    let recording = crate::audio::Recording {
        pcm_i16,
        sample_rate,
        dropped_stream_frames: 0,
    };
    let duration_seconds = recording.stats().duration_seconds;
    let text = transcription.finish_session(&recording, settings, models)?;
    Ok((text, duration_seconds))
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

/// Lightweight in-process metrics for the standalone transcription host.
/// Lives in the single request-loop thread, so no synchronisation is needed.
struct HostMetrics {
    started: Instant,
    active_sessions: u32,
    total_transcriptions: u64,
    total_audio_seconds: f64,
    next_record_id: u64,
    recent: VecDeque<TranscriptionRecord>,
}

impl HostMetrics {
    fn new() -> Self {
        Self {
            started: Instant::now(),
            active_sessions: 0,
            total_transcriptions: 0,
            total_audio_seconds: 0.0,
            next_record_id: 0,
            recent: VecDeque::with_capacity(RECENT_CAPACITY),
        }
    }

    fn begin_session(&mut self) {
        self.active_sessions = self.active_sessions.saturating_add(1);
    }

    fn abort_session(&mut self) {
        self.active_sessions = self.active_sessions.saturating_sub(1);
    }

    fn complete_session(
        &mut self,
        duration_seconds: f32,
        backend: String,
        model: String,
        source: &'static str,
    ) {
        self.active_sessions = self.active_sessions.saturating_sub(1);
        self.total_transcriptions = self.total_transcriptions.saturating_add(1);
        self.total_audio_seconds += duration_seconds as f64;
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
            active_sessions: self.active_sessions,
            total_transcriptions: self.total_transcriptions,
            total_audio_seconds: self.total_audio_seconds,
            recent: self.recent.iter().collect(),
        }
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
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct StatsSnapshot<'a> {
    server_version: &'static str,
    bind_addr: String,
    uptime_seconds: u64,
    active_sessions: u32,
    total_transcriptions: u64,
    total_audio_seconds: f64,
    recent: Vec<&'a TranscriptionRecord>,
}
