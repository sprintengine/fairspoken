mod audio;
mod clipboard;
mod host;
mod models;
mod remote_transcription;
mod settings;
mod transcription;

pub use host::run_transcription_host;

use audio::AudioService;
use clipboard::ClipboardService;
use models::{
    ModelPrepareProgress, ModelService, ModelStatus, SherpaModel, TranscriptionModelStatus,
    WhisperModel,
};
use remote_transcription::{
    start_remote_streaming_session,
    test_remote_transcription_host as check_remote_transcription_host, RemoteHealth,
    RemoteStreamingSession,
};
use serde::{Deserialize, Serialize};
use settings::{Settings, SettingsService, TranscriptionBackend, TranscriptionLocation};
use std::sync::atomic::AtomicU64;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use std::thread;
use tauri::{AppHandle, Emitter, Manager, State};
use transcription::{TranscriptionCancelHandle, TranscriptionService};

const MIN_RECORDING_SECONDS: f32 = 0.35;
const SILENCE_RMS_THRESHOLD: f32 = 0.001;
const SILENCE_PEAK_THRESHOLD: f32 = 0.008;
static BACKEND_EVENT_ID: AtomicU64 = AtomicU64::new(1);

struct AppServices {
    audio: Mutex<AudioService>,
    clipboard: ClipboardService,
    models: ModelService,
    model_prepare_running: Arc<AtomicBool>,
    transcription_cancel_requested: Arc<AtomicBool>,
    local_transcription_cancel: Mutex<Option<TranscriptionCancelHandle>>,
    settings: Mutex<SettingsService>,
    transcription: Mutex<TranscriptionService>,
    remote_transcription: Mutex<Option<RemoteStreamingSession>>,
}

#[derive(Serialize)]
struct BackendStatus {
    state: &'static str,
    message: &'static str,
}

#[tauri::command]
fn get_app_status() -> BackendStatus {
    BackendStatus {
        state: "idle",
        message: "Ready",
    }
}

#[tauri::command]
fn get_settings(services: State<'_, AppServices>) -> Result<Settings, String> {
    services
        .settings
        .lock()
        .map_err(|_| "Settings service lock failed".to_string())
        .map(|service| service.current())
}

#[tauri::command]
fn save_settings(settings: Settings, services: State<'_, AppServices>) -> Result<(), String> {
    let current_settings = services
        .settings
        .lock()
        .map_err(|_| "Settings service lock failed".to_string())?
        .current();

    if services
        .audio
        .lock()
        .map_err(|_| "Audio service lock failed".to_string())?
        .is_recording()
    {
        return Err("Settings cannot be changed while recording is active".to_string());
    }

    if current_settings.requires_transcription_unload(&settings) {
        services
            .transcription
            .lock()
            .map_err(|_| "Transcription service lock failed".to_string())?
            .unload();
    }

    services
        .settings
        .lock()
        .map_err(|_| "Settings service lock failed".to_string())?
        .save(settings)
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TranscriptionModelRequest {
    backend: TranscriptionBackend,
    model: Option<WhisperModel>,
    sherpa_model: Option<SherpaModel>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ModelPrepareProgressEvent {
    backend: TranscriptionBackend,
    model: String,
    stage: String,
    message: String,
    percentage: u8,
    done: bool,
    error: Option<String>,
    status: Option<TranscriptionModelStatus>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct BackendLogEvent {
    id: String,
    level: &'static str,
    message: String,
}

#[tauri::command]
fn get_model_status(
    model: WhisperModel,
    services: State<'_, AppServices>,
) -> Result<ModelStatus, String> {
    Ok(services.models.status(model))
}

#[tauri::command]
fn prepare_model(
    model: WhisperModel,
    services: State<'_, AppServices>,
) -> Result<ModelStatus, String> {
    services.models.prepare(model)
}

#[tauri::command]
fn get_transcription_model_status(
    request: TranscriptionModelRequest,
    services: State<'_, AppServices>,
) -> Result<TranscriptionModelStatus, String> {
    let settings = services
        .settings
        .lock()
        .map_err(|_| "Settings service lock failed".to_string())?
        .current();
    Ok(services.models.transcription_status(
        request.backend,
        request.model.unwrap_or(settings.model),
        request.sherpa_model.unwrap_or(settings.sherpa_model),
    ))
}

#[tauri::command]
fn prepare_transcription_model(
    request: TranscriptionModelRequest,
    services: State<'_, AppServices>,
) -> Result<TranscriptionModelStatus, String> {
    let settings = services
        .settings
        .lock()
        .map_err(|_| "Settings service lock failed".to_string())?
        .current();
    services.models.prepare_transcription_model(
        request.backend,
        request.model.unwrap_or(settings.model),
        request.sherpa_model.unwrap_or(settings.sherpa_model),
    )
}

#[tauri::command]
fn begin_prepare_transcription_model(
    app: AppHandle,
    request: TranscriptionModelRequest,
    services: State<'_, AppServices>,
) -> Result<(), String> {
    if services
        .model_prepare_running
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return Err("A model is already being prepared".to_string());
    }

    let settings = services
        .settings
        .lock()
        .map_err(|_| "Settings service lock failed".to_string())?
        .current();
    let backend = request.backend;
    let whisper_model = request.model.unwrap_or(settings.model);
    let sherpa_model = request.sherpa_model.unwrap_or(settings.sherpa_model);
    let model_id = selected_prepare_model_id(backend, whisper_model, sherpa_model).to_string();
    let models = services.models.clone();
    let running = Arc::clone(&services.model_prepare_running);

    thread::Builder::new()
        .name("model-preparation".to_string())
        .spawn(move || {
            emit_model_prepare_event(
                &app,
                backend,
                &model_id,
                ModelPrepareProgress {
                    stage: "starting",
                    message: "Preparing model".to_string(),
                    percentage: 0,
                },
                false,
                None,
                None,
            );

            let result = models.prepare_transcription_model_with_progress(
                backend,
                whisper_model,
                sherpa_model,
                |progress| {
                    emit_model_prepare_event(&app, backend, &model_id, progress, false, None, None);
                },
            );

            match result {
                Ok(status) => emit_model_prepare_event(
                    &app,
                    backend,
                    &model_id,
                    ModelPrepareProgress {
                        stage: "ready",
                        message: status.message.clone(),
                        percentage: 100,
                    },
                    true,
                    None,
                    Some(status),
                ),
                Err(err) => emit_model_prepare_event(
                    &app,
                    backend,
                    &model_id,
                    ModelPrepareProgress {
                        stage: "error",
                        message: err.clone(),
                        percentage: 0,
                    },
                    true,
                    Some(err),
                    None,
                ),
            }

            running.store(false, Ordering::SeqCst);
        })
        .map_err(|err| {
            services
                .model_prepare_running
                .store(false, Ordering::SeqCst);
            format!("Failed to start model preparation worker: {err}")
        })?;

    Ok(())
}

fn emit_model_prepare_event(
    app: &AppHandle,
    backend: TranscriptionBackend,
    model: &str,
    progress: ModelPrepareProgress,
    done: bool,
    error: Option<String>,
    status: Option<TranscriptionModelStatus>,
) {
    let _ = app.emit(
        "model-prepare-progress",
        ModelPrepareProgressEvent {
            backend,
            model: model.to_string(),
            stage: progress.stage.to_string(),
            message: progress.message,
            percentage: progress.percentage,
            done,
            error,
            status,
        },
    );
}

fn selected_prepare_model_id(
    backend: TranscriptionBackend,
    whisper_model: WhisperModel,
    sherpa_model: SherpaModel,
) -> &'static str {
    match backend {
        TranscriptionBackend::Whisper => whisper_model.model_id(),
        TranscriptionBackend::SherpaStreaming => sherpa_model.model_id(),
    }
}

#[tauri::command]
fn test_remote_transcription_host(
    services: State<'_, AppServices>,
) -> Result<RemoteHealth, String> {
    let settings = services
        .settings
        .lock()
        .map_err(|_| "Settings service lock failed".to_string())?
        .current();
    check_remote_transcription_host(&settings)
}

#[tauri::command]
fn open_settings_window(app: AppHandle) -> Result<(), String> {
    let window = app
        .get_webview_window("settings")
        .ok_or_else(|| "Settings window is not configured".to_string())?;

    window.show().map_err(|err| err.to_string())?;
    window.set_focus().map_err(|err| err.to_string())?;
    Ok(())
}

#[tauri::command]
fn close_settings_window(app: AppHandle) -> Result<(), String> {
    if let Some(window) = app.get_webview_window("settings") {
        window.hide().map_err(|err| err.to_string())?;
    }
    Ok(())
}

#[tauri::command]
fn start_recording(app: AppHandle, services: State<'_, AppServices>) -> Result<u16, String> {
    services
        .transcription_cancel_requested
        .store(false, Ordering::SeqCst);

    let settings = services
        .settings
        .lock()
        .map_err(|_| "Settings service lock failed".to_string())?
        .current();

    let stream_sink = match settings.transcription_location {
        TranscriptionLocation::Local => {
            let start = services
                .transcription
                .lock()
                .map_err(|_| "Transcription service lock failed".to_string())?
                .start_session_with_cancel(&settings, &services.models)?;
            *services
                .local_transcription_cancel
                .lock()
                .map_err(|_| "Transcription cancellation lock failed".to_string())? =
                Some(start.cancel_handle);
            start.audio_tx
        }
        TranscriptionLocation::RemoteHost => {
            emit_backend_event(
                &app,
                "info",
                "Starting remote streaming transcription session",
            );
            let (session, stream_sink) = start_remote_streaming_session(&settings)?;
            let mut remote = services
                .remote_transcription
                .lock()
                .map_err(|_| "Remote transcription service lock failed".to_string())?;
            if remote.is_some() {
                session.cancel();
                return Err("Remote transcription session already in progress".to_string());
            }
            *remote = Some(session);
            Some(stream_sink)
        }
    };

    if let Err(err) = services
        .audio
        .lock()
        .map_err(|_| "Audio service lock failed".to_string())?
        .start(
            settings.max_recording_seconds,
            settings.input_gain,
            stream_sink,
        )
    {
        if settings.transcription_location == TranscriptionLocation::Local {
            services
                .transcription
                .lock()
                .map_err(|_| "Transcription service lock failed".to_string())?
                .cancel_session();
            clear_local_transcription_cancel(&services)?;
        } else {
            cancel_remote_transcription_if_needed(&services)?;
        }
        return Err(err);
    }

    emit_backend_event(
        &app,
        "info",
        format!(
            "Recording started: location={:?}, backend={:?}, gain={}x",
            settings.transcription_location, settings.transcription_backend, settings.input_gain
        ),
    );
    Ok(settings.max_recording_seconds)
}

#[tauri::command]
fn stop_and_transcribe(app: AppHandle, services: State<'_, AppServices>) -> Result<String, String> {
    services
        .transcription_cancel_requested
        .store(false, Ordering::SeqCst);

    let settings = services
        .settings
        .lock()
        .map_err(|_| "Settings service lock failed".to_string())?
        .current();
    let recording = match services
        .audio
        .lock()
        .map_err(|_| "Audio service lock failed".to_string())?
        .stop()
    {
        Ok(recording) => recording,
        Err(err) => {
            cancel_transcription_for_location(&services, settings.transcription_location)?;
            return Err(err);
        }
    };
    let stats = recording.stats();
    emit_backend_event(
        &app,
        "info",
        format!(
            "Recording stopped: {:.2}s, peak {:.4}, rms {:.4}",
            stats.duration_seconds, stats.peak, stats.rms
        ),
    );

    if stats.duration_seconds < MIN_RECORDING_SECONDS {
        emit_backend_event(&app, "warning", "Recording rejected as too short");
        cancel_transcription_for_location(&services, settings.transcription_location)?;
        return Err("Recording was too short".to_string());
    }

    if stats.rms < SILENCE_RMS_THRESHOLD && stats.peak < SILENCE_PEAK_THRESHOLD {
        emit_backend_event(
            &app,
            "warning",
            format!(
                "Recording rejected as silence: peak {:.4}, rms {:.4}",
                stats.peak, stats.rms
            ),
        );
        cancel_transcription_for_location(&services, settings.transcription_location)?;
        return Err("No speech detected - raise input gain or change microphone".to_string());
    }

    emit_backend_event(
        &app,
        "info",
        format!(
            "Finishing transcription with location={:?}, backend={:?}",
            settings.transcription_location, settings.transcription_backend
        ),
    );
    let transcript = match settings.transcription_location {
        TranscriptionLocation::Local => {
            let result = services
                .transcription
                .lock()
                .map_err(|_| "Transcription service lock failed".to_string())?
                .finish_session(&recording, &settings, &services.models);
            clear_local_transcription_cancel(&services)?;
            match result {
                Ok(transcript) => transcript,
                Err(_)
                    if services
                        .transcription_cancel_requested
                        .load(Ordering::SeqCst) =>
                {
                    emit_backend_event(&app, "info", "Transcription cancelled");
                    return Err("Transcription was cancelled".to_string());
                }
                Err(err) => return Err(err),
            }
        }
        TranscriptionLocation::RemoteHost => {
            services
                .remote_transcription
                .lock()
                .map_err(|_| "Remote transcription service lock failed".to_string())?
                .take()
                .ok_or_else(|| "No remote transcription session is active".to_string())?
                .finish()?
                .text
        }
    };

    if services
        .transcription_cancel_requested
        .load(Ordering::SeqCst)
    {
        emit_backend_event(&app, "info", "Transcription cancelled");
        return Err("Transcription was cancelled".to_string());
    }

    services.clipboard.write_text(&transcript)?;
    emit_backend_event(&app, "info", "Transcript copied to clipboard");
    Ok(transcript)
}

#[tauri::command]
fn cancel_transcription(app: AppHandle, services: State<'_, AppServices>) -> Result<(), String> {
    services
        .transcription_cancel_requested
        .store(true, Ordering::SeqCst);
    if let Some(handle) = services
        .local_transcription_cancel
        .lock()
        .map_err(|_| "Transcription cancellation lock failed".to_string())?
        .as_ref()
        .cloned()
    {
        handle.cancel();
    }
    emit_backend_event(&app, "info", "Transcription cancellation requested");
    Ok(())
}

fn emit_backend_event(app: &AppHandle, level: &'static str, message: impl Into<String>) {
    let id = BACKEND_EVENT_ID.fetch_add(1, Ordering::SeqCst);
    let _ = app.emit(
        "backend-event",
        BackendLogEvent {
            id: format!("backend-{id}"),
            level,
            message: message.into(),
        },
    );
}

fn cancel_transcription_for_location(
    services: &State<'_, AppServices>,
    location: TranscriptionLocation,
) -> Result<(), String> {
    match location {
        TranscriptionLocation::Local => cancel_local_transcription_if_needed(services),
        TranscriptionLocation::RemoteHost => cancel_remote_transcription_if_needed(services),
    }
}

fn cancel_local_transcription_if_needed(services: &State<'_, AppServices>) -> Result<(), String> {
    services
        .transcription
        .lock()
        .map_err(|_| "Transcription service lock failed".to_string())?
        .cancel_session();
    clear_local_transcription_cancel(services)?;
    Ok(())
}

fn clear_local_transcription_cancel(services: &State<'_, AppServices>) -> Result<(), String> {
    *services
        .local_transcription_cancel
        .lock()
        .map_err(|_| "Transcription cancellation lock failed".to_string())? = None;
    Ok(())
}

fn cancel_remote_transcription_if_needed(services: &State<'_, AppServices>) -> Result<(), String> {
    if let Some(session) = services
        .remote_transcription
        .lock()
        .map_err(|_| "Remote transcription service lock failed".to_string())?
        .take()
    {
        session.cancel();
    }
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .plugin(tauri_plugin_opener::init())
        .manage(AppServices {
            audio: Mutex::new(AudioService::default()),
            clipboard: ClipboardService::default(),
            models: ModelService::default(),
            model_prepare_running: Arc::new(AtomicBool::new(false)),
            transcription_cancel_requested: Arc::new(AtomicBool::new(false)),
            local_transcription_cancel: Mutex::new(None),
            settings: Mutex::new(SettingsService::default()),
            transcription: Mutex::new(TranscriptionService::default()),
            remote_transcription: Mutex::new(None),
        })
        .invoke_handler(tauri::generate_handler![
            get_app_status,
            get_settings,
            save_settings,
            get_model_status,
            prepare_model,
            get_transcription_model_status,
            prepare_transcription_model,
            begin_prepare_transcription_model,
            test_remote_transcription_host,
            open_settings_window,
            close_settings_window,
            start_recording,
            stop_and_transcribe,
            cancel_transcription,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
