//! The host's routes: `handle_request` dispatches each request to its
//! handler.

use super::*;

pub(super) const DASHBOARD_HTML: &str = include_str!("dashboard.html");

pub(super) fn handle_request(
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
            let inventory = runtime.model_inventory();
            let body = {
                let metrics = metrics
                    .lock()
                    .map_err(|_| "Host metrics lock failed".to_string())?;
                stats_json(&metrics, bind_addr, &runtime.live, inventory)?
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

pub(super) fn stats_json(
    metrics: &HostMetrics,
    bind_addr: &str,
    live: &HostLiveConfig,
    inventory: ModelInventory,
) -> Result<String, String> {
    serde_json::to_string(&metrics.snapshot(bind_addr, live, inventory))
        .map_err(|err| format!("Failed to serialize stats response: {err}"))
}

/// Streams live host activity to a dashboard as Server-Sent Events: a
/// `snapshot` frame (the `/v1/stats` body), then one frame per metrics change.
/// The connection gets its own thread so a long-lived stream never holds up
/// the accept loop.
pub(super) fn spawn_event_stream(
    request: Request,
    runtime: Arc<HostRuntime>,
    bind_addr: &str,
) -> Result<(), String> {
    // Subscribe and snapshot under one metrics lock: every mutation publishes
    // while holding it, so no event falls between the snapshot and the feed.
    // The disk checks happen first, outside it.
    let inventory = runtime.model_inventory();
    let subscribed = {
        let metrics = runtime
            .metrics
            .lock()
            .map_err(|_| "Host metrics lock failed".to_string())?;
        match metrics.events.subscribe() {
            Ok(subscription) => Ok((
                subscription,
                stats_json(&metrics, bind_addr, &runtime.live, inventory)?,
            )),
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

#[derive(Clone, Copy)]
pub(super) enum TranscriptionRequestKind {
    Batch,
    Stream,
}

pub(super) fn spawn_transcription_request(
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

pub(super) fn handle_batch_transcription(
    mut request: Request,
    runtime: Arc<HostRuntime>,
) -> Result<(), String> {
    let client = client_ip(&request);
    runtime.record_client_request(client.as_deref());
    // Admitted before the body is read: each upload in flight may buffer up
    // to `max_batch_body_bytes`, so only as many as the workers and queue
    // could ever take are let in.
    let Some(_admission) = runtime.try_admit_batch() else {
        runtime.record_rejection(client.as_deref());
        return respond_error(
            request,
            StatusCode(429),
            "Server is at batch upload capacity",
        );
    };
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
    let mut settings = match settings_from_headers(&request) {
        Ok(settings) => settings,
        Err(err) => return respond_error(request, StatusCode(400), &err),
    };
    if let Err((status, err)) = check_wav_header(&body, runtime.max_recording_seconds()) {
        if status == StatusCode(413) {
            runtime.record_rejection(client.as_deref());
        }
        return respond_error(request, status, &err);
    }
    let recording = match decode_wav(&body) {
        Ok(recording) => recording,
        Err(err) => return respond_error(request, StatusCode(400), &err),
    };
    let duration_seconds = recording.stats().duration_seconds;
    if let Err(err) = runtime.validate_recording_duration(duration_seconds) {
        runtime.record_rejection(client.as_deref());
        return respond_error(request, StatusCode(413), &err);
    }
    let super_request = runtime.request_super_mode(&request, &mut settings);
    let outcome = match runtime.transcribe(recording, settings, "batch", client) {
        Ok(outcome) => outcome,
        Err(HostRuntimeError::QueueFull(message)) => {
            runtime.cancel_super_grant(super_request);
            return respond_error(request, StatusCode(429), &message);
        }
        Err(HostRuntimeError::WorkerFailed(message)) => {
            return respond_error(request, StatusCode(500), &message)
        }
    };
    let response = transcription_response(outcome, duration_seconds, super_request);
    respond_json(
        request,
        StatusCode(200),
        serde_json::to_string(&response)
            .map_err(|err| format!("Failed to serialize transcription response: {err}"))?,
    )
}

pub(super) fn handle_stream_transcription(
    mut request: Request,
    runtime: Arc<HostRuntime>,
) -> Result<(), String> {
    let client = client_ip(&request);
    runtime.record_client_request(client.as_deref());
    let mut settings = match settings_from_headers(&request) {
        Ok(settings) => settings,
        Err(err) => return respond_error(request, StatusCode(400), &err),
    };
    let mut stream_guard = match runtime.try_begin_stream(client.clone()) {
        Ok(guard) => guard,
        Err(message) => return respond_error(request, StatusCode(429), &message),
    };
    // Decided once per dictation, before any audio arrives.
    let super_request = runtime.request_super_mode(&request, &mut settings);
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
            runtime.cancel_super_grant(super_request);
            return respond_error(request, StatusCode(429), &message);
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
    let response = transcription_response(outcome, duration_seconds, super_request);
    respond_json(
        request,
        StatusCode(200),
        serde_json::to_string(&response).map_err(|err| {
            format!("Failed to serialize streaming transcription response: {err}")
        })?,
    )
}

pub(super) const MAX_CONFIG_BODY_BYTES: u64 = 4 * 1024;

pub(super) fn handle_config_update(
    mut request: Request,
    runtime: Arc<HostRuntime>,
) -> Result<(), String> {
    let update: HostConfigUpdate = match read_json_body(
        &mut request,
        MAX_CONFIG_BODY_BYTES,
        |body, err| {
            // serde quotes offending values, which could be the password.
            if String::from_utf8_lossy(body).contains("pairingPassword") {
                "Invalid config update: pairingPassword must be a string or null, and every other field as documented".to_string()
            } else {
                format!("Invalid config update: {err}")
            }
        },
    ) {
        Ok(update) => update,
        Err((status, message)) => return respond_error(request, status, &message),
    };

    // The env var wins on every start, so an edit here would be undone (or a
    // cleared password silently come back) at the next restart.
    if update.pairing_password.is_some() && runtime.live.auth().pairing_password_from_env {
        return respond_error(
            request,
            StatusCode(409),
            &format!("The pairing password is set by {PAIRING_PASSWORD_ENV} on the server; change or unset it there"),
        );
    }
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

pub(super) const MAX_PAIR_BODY_BYTES: u64 = 4 * 1024;

/// `POST /v1/pair`: the pairing password in, the host token out. Answers
/// never echo the request, which carries the password.
pub(super) fn handle_pair(mut request: Request, runtime: &HostRuntime) -> Result<(), String> {
    let pair: PairRequest = match read_json_body(&mut request, MAX_PAIR_BODY_BYTES, |_, _| {
        r#"Invalid pair request: expected {"password": "<string>", "clientName": "<string>"}"#
            .to_string()
    }) {
        Ok(pair) => pair,
        Err((status, message)) => return respond_error(request, status, &message),
    };
    let outcome = {
        let auth = runtime.live.auth();
        let mut limiter = lock_unpoisoned(&runtime.pairing);
        attempt_pairing(
            &auth,
            &mut limiter,
            &pairing_rate_key(&request),
            &pair.password,
            Instant::now(),
        )
    };
    // A refused-for-rate attempt never reached the password check; an event
    // each would let a guessing client flood every dashboard with toasts.
    if !matches!(outcome, PairOutcome::RateLimited { .. }) {
        if let Ok(metrics) = runtime.metrics.lock() {
            metrics.publish(HostEventKind::Pairing {
                client: client_ip(&request),
                client_name: pair.client_name(),
                ok: matches!(outcome, PairOutcome::Paired { .. }),
            });
        }
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

pub(super) fn respond_update_status(
    request: Request,
    status: StatusCode,
    updater: &Updater,
) -> Result<(), String> {
    let body = serde_json::to_string(&updater.status())
        .map_err(|err| format!("Failed to serialize update status: {err}"))?;
    respond_json(request, status, body)
}

pub(super) fn handle_update_settings(
    mut request: Request,
    updater: &Arc<Updater>,
) -> Result<(), String> {
    let settings: UpdateSettingsRequest =
        match read_json_body(&mut request, MAX_CONFIG_BODY_BYTES, |_, err| {
            format!("Invalid update settings: {err}")
        }) {
            Ok(settings) => settings,
            Err((status, message)) => return respond_error(request, status, &message),
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
pub(super) struct ModelDownloadRequest {
    pub(super) model: String,
}

/// Downloads a model file onto the host so a missing configured model is
/// recoverable from the dashboard instead of requiring the desktop app or a
/// manual file copy on the host machine. One download runs at a time and its
/// progress is published through `/v1/stats`.
pub(super) fn handle_model_download(
    mut request: Request,
    runtime: Arc<HostRuntime>,
) -> Result<(), String> {
    let download: ModelDownloadRequest =
        match read_json_body(&mut request, MAX_CONFIG_BODY_BYTES, |_, err| {
            format!("Invalid model download request: {err}")
        }) {
            Ok(download) => download,
            Err((status, message)) => return respond_error(request, status, &message),
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

pub(super) fn transcription_response(
    outcome: TranscriptionOutcome,
    duration_seconds: f32,
    super_request: super_mode::SuperModeRequest,
) -> RemoteTranscriptionResponse {
    let (super_mode, secondary_model) =
        super_mode::response_fields(super_request, outcome.super_mode.as_ref());
    RemoteTranscriptionResponse {
        text: outcome.text,
        duration_seconds,
        backend: outcome.backend,
        model: outcome.model,
        server_version: Some(SERVER_VERSION.to_string()),
        super_mode,
        secondary_model,
    }
}

/// How much audio a stream upload delivered.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct ReceivedStream {
    pub(super) samples: usize,
    pub(super) sample_rate: u32,
}

impl ReceivedStream {
    pub(super) fn duration_seconds(self) -> f32 {
        self.samples as f32 / self.sample_rate as f32
    }
}

/// Reads stream frames until the client closes the upload, handing each one
/// on as it arrives. Rejects a sample-rate change, an over-long recording and
/// an upload with no audio at all.
pub(super) fn forward_stream_frames(
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

/// Checks a WAV upload's header before it is decoded (which copies every
/// sample): a sample rate the body limit wasn't sized for is `400`, and a
/// header claiming more audio than the host takes is `413`. Malformed files
/// are left to `decode_wav`, which reports them.
pub(super) fn check_wav_header(
    body: &[u8],
    max_recording_seconds: u16,
) -> Result<(), (StatusCode, String)> {
    let Ok(reader) = hound::WavReader::new(std::io::Cursor::new(body)) else {
        return Ok(());
    };
    let sample_rate = reader.spec().sample_rate;
    if !(1..=MAX_STREAM_SAMPLE_RATE).contains(&sample_rate) {
        return Err((
            StatusCode(400),
            format!("WAV sample rate must be 1 to {MAX_STREAM_SAMPLE_RATE} Hz"),
        ));
    }
    if u64::from(reader.duration()) > u64::from(sample_rate) * u64::from(max_recording_seconds) {
        return Err((
            StatusCode(413),
            format!("Recording exceeds host maximum of {max_recording_seconds} seconds"),
        ));
    }
    Ok(())
}

pub(super) fn stream_recording_error_status(message: &str) -> StatusCode {
    if message == "No audio samples were captured" {
        StatusCode(400)
    } else {
        StatusCode(413)
    }
}

pub(super) fn settings_from_headers(request: &Request) -> Result<Settings, String> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::test_support::*;
    use std::io::Cursor;

    fn write_test_stream_frame(sample_rate: u32, samples: &[i16], target: &mut Vec<u8>) {
        target.extend_from_slice(&sample_rate.to_le_bytes());
        target.extend_from_slice(&(samples.len() as u32).to_le_bytes());
        for sample in samples {
            target.extend_from_slice(&sample.to_le_bytes());
        }
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

    #[test]
    fn config_refuses_pairing_password_edits_while_the_env_sets_it() {
        let dir = tempfile::tempdir().unwrap();
        let (auth, _) = super::HostAuth::resolve(
            Some("secret".to_string()),
            None,
            Some("env-password".to_string()),
            None,
        )
        .unwrap();
        let runtime = token_runtime(dir.path().join("host-config.json"), auth);
        let addr = serve_test_host(Arc::clone(&runtime));
        let post = |body: &str| {
            exchange(
                addr,
                &format!(
                    "POST /v1/config HTTP/1.1\r\nHost: test\r\nAuthorization: Bearer secret\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}",
                    body.len()
                ),
            )
        };

        for body in [
            r#"{"pairingPassword":"another-one"}"#,
            r#"{"pairingPassword":null}"#,
            r#"{"maxActiveStreams":3,"pairingPassword":""}"#,
        ] {
            let (status, answer) = post(body);
            assert_eq!(status, 409, "{body}: {answer}");
            assert!(answer.contains("FAIRSPOKEN_HOST_PAIRING_PASSWORD"));
            assert!(!answer.contains("env-password"));
        }
        // Nothing applied, nothing saved.
        assert_eq!(
            runtime.live.auth().pairing_password.as_deref(),
            Some("env-password")
        );
        assert_eq!(runtime.live.max_active_streams(), 1);
        assert!(!runtime.config_path.exists());

        let (status, _) = post(r#"{"maxActiveStreams":3}"#);
        assert_eq!(status, 200, "other fields still apply");
        let (status, stats) = exchange(
            addr,
            "GET /v1/stats?token=secret HTTP/1.1\r\nHost: test\r\nConnection: close\r\n\r\n",
        );
        assert_eq!(status, 200);
        let stats: serde_json::Value = serde_json::from_str(&stats).expect("stats json");
        assert_eq!(stats["pairingPasswordSource"], "env");
        assert_eq!(stats["pairingEnabled"], true);
        assert_eq!(stats["maxActiveStreams"], 3);
    }

    #[test]
    fn pairing_rate_limit_keys_on_the_last_forwarded_hop_and_stays_quiet_when_limited() {
        let dir = tempfile::tempdir().unwrap();
        let (auth, _) = super::HostAuth::resolve(
            Some("secret".to_string()),
            None,
            Some("right-password".to_string()),
            None,
        )
        .unwrap();
        let runtime = token_runtime(dir.path().join("host-config.json"), auth);
        let subscription = runtime
            .metrics
            .lock()
            .unwrap()
            .events
            .subscribe()
            .expect("subscribe");
        let addr = serve_test_host(Arc::clone(&runtime));
        let pair = |forwarded: &str, password: &str| {
            let body = format!(r#"{{"password":"{password}"}}"#);
            exchange(
                addr,
                &format!(
                    "POST /v1/pair HTTP/1.1\r\nHost: test\r\nX-Forwarded-For: {forwarded}\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}",
                    body.len()
                ),
            )
            .0
        };

        // A client that rotates the address it claims, behind a proxy that
        // appends the one it saw, is still one client.
        for guess in 0..5 {
            assert_eq!(
                pair(&format!("10.9.0.{guess}, 100.64.0.7"), "wrong-guess"),
                401
            );
        }
        assert_eq!(pair("10.9.0.99, 100.64.0.7", "wrong-guess"), 429);
        assert_eq!(pair("10.9.0.98, 100.64.0.7", "right-password"), 429);
        assert_eq!(pair("100.64.0.8", "right-password"), 200);

        let events: Vec<_> = std::iter::from_fn(|| subscription.try_next())
            .filter(|frame| frame.starts_with("event: pairing\n"))
            .collect();
        // Five refusals and the success; nothing for the rate-limited tries.
        assert_eq!(events.len(), 6, "{events:?}");
        // Displayed as before: the first forwarded address.
        assert!(
            events[0].contains("\"client\":\"10.9.0.0\""),
            "{}",
            events[0]
        );
        assert!(events[5].contains("\"ok\":true"));
    }

    #[test]
    fn wav_header_checks_the_rate_and_claimed_length_before_decoding() {
        use super::check_wav_header;
        use tiny_http::StatusCode;
        let wav = |sample_rate: u32, frames: usize| {
            let mut bytes = Cursor::new(Vec::new());
            let spec = hound::WavSpec {
                channels: 1,
                sample_rate,
                bits_per_sample: 16,
                sample_format: hound::SampleFormat::Int,
            };
            let mut writer = hound::WavWriter::new(&mut bytes, spec).unwrap();
            for _ in 0..frames {
                writer.write_sample(0_i16).unwrap();
            }
            writer.finalize().unwrap();
            bytes.into_inner()
        };

        assert!(check_wav_header(&wav(16_000, 16_000 * 10), 10).is_ok());
        assert!(check_wav_header(&wav(192_000, 10), 10).is_ok());
        assert_eq!(
            check_wav_header(&wav(384_000, 10), 10).unwrap_err().0,
            StatusCode(400)
        );
        let (status, message) = check_wav_header(&wav(16_000, 16_000 * 10 + 1), 10).unwrap_err();
        assert_eq!(status, StatusCode(413));
        assert!(message.contains("exceeds host maximum of 10 seconds"));
        // Not a WAV at all: decode_wav reports that.
        assert!(check_wav_header(b"garbage", 10).is_ok());
    }
}
