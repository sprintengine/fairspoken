use crate::audio::AudioFrame;
use crate::models::{ModelService, SherpaModel, WhisperModel};
use crate::remote_transcription::{
    backend_id, decode_wav, read_stream_frame, selected_model_id, RemoteHealth,
    RemoteTranscriptionResponse,
};
use crate::settings::{Settings, TranscriptionBackend};
use crate::transcription::TranscriptionService;
use std::env;
use tiny_http::{Header, Method, Request, Response, Server, StatusCode};

const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");

pub fn run_transcription_host() -> Result<(), String> {
    let addr = env::var("MULTIVOICE_HOST_ADDR").unwrap_or_else(|_| "127.0.0.1:48173".to_string());
    let token = env::var("MULTIVOICE_HOST_TOKEN").ok();
    let server = Server::http(&addr)
        .map_err(|err| format!("Failed to start transcription host on {addr}: {err}"))?;
    println!("multivoice transcription host listening on http://{addr}");

    let models = ModelService::default();
    let mut transcription = TranscriptionService::default();

    for request in server.incoming_requests() {
        let response = handle_request(request, token.as_deref(), &models, &mut transcription);
        if let Err(err) = response {
            eprintln!("Failed to handle transcription host request: {err}");
        }
    }

    Ok(())
}

fn handle_request(
    mut request: Request,
    token: Option<&str>,
    models: &ModelService,
    transcription: &mut TranscriptionService,
) -> Result<(), String> {
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
        (&Method::Post, "/v1/transcriptions") => {
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
            let text = match transcribe_recording(transcription, models, &settings, recording) {
                Ok(text) => text,
                Err(err) => return respond_error(request, StatusCode(500), &err),
            };
            let response = RemoteTranscriptionResponse {
                text,
                duration_seconds,
                backend: backend_id(settings.transcription_backend).to_string(),
                model: selected_model_id(&settings).to_string(),
                server_version: Some(SERVER_VERSION.to_string()),
            };
            respond_json(
                request,
                StatusCode(200),
                serde_json::to_string(&response)
                    .map_err(|err| format!("Failed to serialize transcription response: {err}"))?,
            )
        }
        (&Method::Post, "/v1/transcriptions/stream") => {
            let settings = match settings_from_headers(&request) {
                Ok(settings) => settings,
                Err(err) => return respond_error(request, StatusCode(400), &err),
            };
            let (text, duration_seconds) =
                match transcribe_stream(transcription, models, &settings, &mut request) {
                    Ok(result) => result,
                    Err(err) => return respond_error(request, StatusCode(500), &err),
                };
            let response = RemoteTranscriptionResponse {
                text,
                duration_seconds,
                backend: backend_id(settings.transcription_backend).to_string(),
                model: selected_model_id(&settings).to_string(),
                server_version: Some(SERVER_VERSION.to_string()),
            };
            respond_json(
                request,
                StatusCode(200),
                serde_json::to_string(&response).map_err(|err| {
                    format!("Failed to serialize streaming transcription response: {err}")
                })?,
            )
        }
        _ => respond_json(
            request,
            StatusCode(404),
            r#"{"error":"not_found"}"#.to_string(),
        ),
    }
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

    let mut settings = Settings::default();
    settings.language = language;
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
    header_value(request, "authorization")
        .map(|value| value == format!("Bearer {token}"))
        .unwrap_or(false)
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
