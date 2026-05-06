mod audio;
mod clipboard;
mod models;
mod settings;
mod transcription;

use audio::AudioService;
use clipboard::ClipboardService;
use models::{ModelService, ModelStatus, WhisperModel};
use serde::Serialize;
use settings::{Settings, SettingsService};
use std::sync::Mutex;
use tauri::{AppHandle, Manager, State};
use transcription::TranscriptionService;

const MIN_RECORDING_SECONDS: f32 = 0.35;
const SILENCE_RMS_THRESHOLD: f32 = 0.001;
const SILENCE_PEAK_THRESHOLD: f32 = 0.008;

struct AppServices {
    audio: Mutex<AudioService>,
    clipboard: ClipboardService,
    models: ModelService,
    settings: Mutex<SettingsService>,
    transcription: Mutex<TranscriptionService>,
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
    services
        .settings
        .lock()
        .map_err(|_| "Settings service lock failed".to_string())?
        .save(settings)
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
fn start_recording(services: State<'_, AppServices>) -> Result<(), String> {
    let settings = services
        .settings
        .lock()
        .map_err(|_| "Settings service lock failed".to_string())?
        .current();

    services
        .audio
        .lock()
        .map_err(|_| "Audio service lock failed".to_string())?
        .start(settings.max_recording_seconds)
}

#[tauri::command]
fn stop_and_transcribe(services: State<'_, AppServices>) -> Result<String, String> {
    let recording = services
        .audio
        .lock()
        .map_err(|_| "Audio service lock failed".to_string())?
        .stop()?;
    let stats = recording.stats();

    if stats.duration_seconds < MIN_RECORDING_SECONDS {
        return Err("Recording was too short".to_string());
    }

    if stats.rms < SILENCE_RMS_THRESHOLD && stats.peak < SILENCE_PEAK_THRESHOLD {
        return Err("No speech detected - raise input gain or change microphone".to_string());
    }

    let settings = services
        .settings
        .lock()
        .map_err(|_| "Settings service lock failed".to_string())?
        .current();

    let transcript = services
        .transcription
        .lock()
        .map_err(|_| "Transcription service lock failed".to_string())?
        .transcribe(
            &recording,
            &settings,
            &services.models.path_for(settings.model),
        )?;

    services.clipboard.write_text(&transcript)?;
    Ok(transcript)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .manage(AppServices {
            audio: Mutex::new(AudioService::default()),
            clipboard: ClipboardService::default(),
            models: ModelService::default(),
            settings: Mutex::new(SettingsService::default()),
            transcription: Mutex::new(TranscriptionService::default()),
        })
        .invoke_handler(tauri::generate_handler![
            get_app_status,
            get_settings,
            save_settings,
            get_model_status,
            prepare_model,
            open_settings_window,
            close_settings_window,
            start_recording,
            stop_and_transcribe,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
