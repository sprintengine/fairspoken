mod audio;
mod clipboard;
mod host;
mod models;
mod notes;
mod post_processing;
mod remote_transcription;
mod settings;
mod speed_test;
mod transcript_history;
mod transcription;
mod usage_stats;

pub use host::run_transcription_host;

use audio::{AudioService, Recording};
use clipboard::ClipboardService;
use models::{
    ModelPrepareProgress, ModelService, ModelStatus, SherpaModel, TranscriptionModelStatus,
    WhisperModel,
};
use notes::{NewNote, Note, NotesService};
use post_processing::apply_transcript_post_processing;
use remote_transcription::{
    start_remote_streaming_session,
    test_remote_transcription_host as check_remote_transcription_host, RemoteHealth,
    RemoteStreamingSession,
};
use serde::{Deserialize, Serialize};
use settings::{
    Settings, SettingsService, Snippet, TranscriptCorrection, TranscriptionBackend,
    TranscriptionLocation,
};
use speed_test::{
    build_capture_result, SpeedTestCapture, SpeedTestRecord, SpeedTestService, SpeedTestSummary,
};
use std::sync::atomic::AtomicU64;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc, Arc, Mutex,
};
use std::thread;
use tauri::{AppHandle, Emitter, LogicalPosition, Manager, Position, State, WindowEvent};
use transcript_history::{
    NewTranscriptHistoryItem, TranscriptHistoryItem, TranscriptHistoryService,
};
use transcription::{
    TranscriptPreview, TranscriptionCancelHandle, TranscriptionPreviewSender, TranscriptionService,
};
use usage_stats::{UsageStatsService, UsageStatsSummary};

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
    transcript_history: Mutex<TranscriptHistoryService>,
    notes: Mutex<NotesService>,
    usage_stats: Mutex<UsageStatsService>,
    speed_test: Mutex<SpeedTestService>,
    transcription: Mutex<TranscriptionService>,
    remote_transcription: Mutex<Option<RemoteStreamingSession>>,
    transcript_shelf_positioned: AtomicBool,
}

#[derive(Serialize)]
struct BackendStatus {
    state: &'static str,
    message: &'static str,
}

#[tauri::command]
fn get_app_status(services: State<'_, AppServices>) -> BackendStatus {
    if services
        .audio
        .lock()
        .map(|audio| audio.is_recording())
        .unwrap_or(false)
    {
        return BackendStatus {
            state: "recording",
            message: "Recording",
        };
    }

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
fn save_settings(
    app: AppHandle,
    mut settings: Settings,
    services: State<'_, AppServices>,
) -> Result<(), String> {
    let current_settings = services
        .settings
        .lock()
        .map_err(|_| "Settings service lock failed".to_string())?
        .current();

    // The dictionary (vocabulary, corrections, snippets) is owned by the
    // Dictionary screen via save_dictionary; the Settings screen must not be
    // able to clear it, so preserve those fields regardless of the payload.
    settings.vocabulary_hints = current_settings.vocabulary_hints.clone();
    settings.transcript_corrections = current_settings.transcript_corrections.clone();
    settings.snippets = current_settings.snippets.clone();

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

    let normalized = {
        let mut service = services
            .settings
            .lock()
            .map_err(|_| "Settings service lock failed".to_string())?;
        service.save(settings)?;
        service.current()
    };

    let _ = app.emit("settings-updated", &normalized);
    Ok(())
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DictionaryUpdate {
    #[serde(default)]
    vocabulary_hints: Vec<String>,
    #[serde(default)]
    transcript_corrections: Vec<TranscriptCorrection>,
    #[serde(default)]
    snippets: Vec<Snippet>,
}

#[tauri::command]
fn save_dictionary(
    update: DictionaryUpdate,
    services: State<'_, AppServices>,
) -> Result<(), String> {
    let mut settings = services
        .settings
        .lock()
        .map_err(|_| "Settings service lock failed".to_string())?
        .current();
    settings.vocabulary_hints = update.vocabulary_hints;
    settings.transcript_corrections = update.transcript_corrections;
    settings.snippets = update.snippets;
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

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct TranscriptPreviewEvent {
    index: usize,
    text: String,
    final_preview: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct TranscriptHistoryUpdatedEvent {
    item: TranscriptHistoryItem,
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
fn open_home_window(app: AppHandle, screen: Option<String>) -> Result<(), String> {
    let window = app
        .get_webview_window("home")
        .ok_or_else(|| "Home window is not configured".to_string())?;

    window.show().map_err(|err| err.to_string())?;
    window.set_focus().map_err(|err| err.to_string())?;

    // The window is created hidden at startup, so its webview listeners are
    // already live before the first show; emitting here lands reliably.
    if let Some(screen) = screen {
        let _ = app.emit("home-navigate", screen);
    }
    Ok(())
}

#[tauri::command]
fn show_transcript_shelf_window(
    app: AppHandle,
    services: State<'_, AppServices>,
) -> Result<(), String> {
    let window = app
        .get_webview_window("transcript-shelf")
        .ok_or_else(|| "Transcript shelf window is not configured".to_string())?;

    if !services.transcript_shelf_positioned.load(Ordering::SeqCst) {
        if let Some(monitor) = window.current_monitor().map_err(|err| err.to_string())? {
            let position = monitor.position();
            let size = monitor.size();
            let scale = monitor.scale_factor();
            let x = f64::from(position.x) / scale + 18.0;
            let y = (f64::from(position.y) + f64::from(size.height)) / scale - 240.0 - 56.0;
            window
                .set_position(Position::Logical(LogicalPosition { x, y: y.max(18.0) }))
                .map_err(|err| err.to_string())?;
            services
                .transcript_shelf_positioned
                .store(true, Ordering::SeqCst);
        }
    }

    // Transcript previews are passive status updates. Showing the shelf must not
    // steal focus from the application the user is dictating or typing into.
    window.show().map_err(|err| err.to_string())?;
    Ok(())
}

#[tauri::command]
fn hide_transcript_shelf_window(app: AppHandle) -> Result<(), String> {
    if let Some(window) = app.get_webview_window("transcript-shelf") {
        window.hide().map_err(|err| err.to_string())?;
    }
    Ok(())
}

#[tauri::command]
fn get_notes(services: State<'_, AppServices>) -> Result<Vec<Note>, String> {
    services
        .notes
        .lock()
        .map_err(|_| "Notes service lock failed".to_string())
        .map(|notes| notes.list())
}

#[tauri::command]
fn update_note(id: String, text: String, services: State<'_, AppServices>) -> Result<Note, String> {
    services
        .notes
        .lock()
        .map_err(|_| "Notes service lock failed".to_string())?
        .update_text(&id, &text)
}

#[tauri::command]
fn set_note_pinned(
    id: String,
    pinned: bool,
    services: State<'_, AppServices>,
) -> Result<Note, String> {
    services
        .notes
        .lock()
        .map_err(|_| "Notes service lock failed".to_string())?
        .set_pinned(&id, pinned)
}

#[tauri::command]
fn delete_note(id: String, services: State<'_, AppServices>) -> Result<(), String> {
    services
        .notes
        .lock()
        .map_err(|_| "Notes service lock failed".to_string())?
        .delete(&id)
}

#[tauri::command]
fn copy_note(id: String, services: State<'_, AppServices>) -> Result<Note, String> {
    let note = services
        .notes
        .lock()
        .map_err(|_| "Notes service lock failed".to_string())?
        .find(&id)
        .ok_or_else(|| "Note was not found".to_string())?;
    services.clipboard.write_text(&note.text)?;
    Ok(note)
}

#[tauri::command]
fn get_usage_stats(services: State<'_, AppServices>) -> Result<UsageStatsSummary, String> {
    services
        .usage_stats
        .lock()
        .map_err(|_| "Usage stats service lock failed".to_string())
        .map(|service| service.summary(current_epoch_seconds()))
}

#[tauri::command]
fn get_transcript_history(
    services: State<'_, AppServices>,
) -> Result<Vec<TranscriptHistoryItem>, String> {
    services
        .transcript_history
        .lock()
        .map_err(|_| "Transcript history service lock failed".to_string())
        .map(|history| history.list())
}

#[tauri::command]
fn clear_transcript_history(
    app: AppHandle,
    services: State<'_, AppServices>,
) -> Result<(), String> {
    services
        .transcript_history
        .lock()
        .map_err(|_| "Transcript history service lock failed".to_string())?
        .clear()?;
    emit_backend_event(&app, "info", "Transcript history cleared");
    Ok(())
}

#[tauri::command]
fn copy_transcript_history_item(
    app: AppHandle,
    id: String,
    services: State<'_, AppServices>,
) -> Result<TranscriptHistoryItem, String> {
    let item = services
        .transcript_history
        .lock()
        .map_err(|_| "Transcript history service lock failed".to_string())?
        .find(&id)
        .ok_or_else(|| "Transcript history item was not found".to_string())?;

    services.clipboard.write_text(&item.text)?;
    let _ = app.emit("transcript-copied", &item);
    Ok(item)
}

#[tauri::command]
fn delete_transcript_history_item(
    id: String,
    services: State<'_, AppServices>,
) -> Result<(), String> {
    services
        .transcript_history
        .lock()
        .map_err(|_| "Transcript history service lock failed".to_string())?
        .delete(&id)
}

#[tauri::command]
fn start_recording(app: AppHandle, services: State<'_, AppServices>) -> Result<u16, String> {
    services
        .transcription_cancel_requested
        .store(false, Ordering::SeqCst);

    let recording_active = services
        .audio
        .lock()
        .map_err(|_| "Audio service lock failed".to_string())?
        .is_recording();
    if recording_active {
        return Err("Recording already in progress".to_string());
    }

    let settings = services
        .settings
        .lock()
        .map_err(|_| "Settings service lock failed".to_string())?
        .current();

    // If a previous start failed after opening the transcription worker but before
    // audio capture became active, clear that stale worker before the next attempt.
    cancel_transcription_for_location(&services, settings.transcription_location)?;

    let stream_sink = match settings.transcription_location {
        TranscriptionLocation::Local => {
            let preview_tx = start_transcript_preview_forwarder(app.clone());
            let start = services
                .transcription
                .lock()
                .map_err(|_| "Transcription service lock failed".to_string())?
                .start_session_with_cancel(&settings, &services.models, Some(preview_tx))?;
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
    let recording = stop_and_validate_recording(&app, &services, &settings)?;
    let stats = recording.stats();

    emit_backend_event(
        &app,
        "info",
        format!(
            "Finishing transcription with location={:?}, backend={:?}",
            settings.transcription_location, settings.transcription_backend
        ),
    );
    let raw_transcript = finish_transcription_raw(&app, &services, &recording, &settings)?;

    let processed = apply_transcript_post_processing(&raw_transcript, &settings);
    if processed.corrections_applied > 0 {
        emit_backend_event(
            &app,
            "info",
            format!(
                "Applied {} transcript correction{}",
                processed.corrections_applied,
                if processed.corrections_applied == 1 {
                    ""
                } else {
                    "s"
                }
            ),
        );
    }
    let transcript = processed.text;

    let stored_item = services
        .transcript_history
        .lock()
        .map_err(|_| "Transcript history service lock failed".to_string())?
        .add(NewTranscriptHistoryItem {
            text: transcript.clone(),
            backend: backend_id(settings.transcription_backend).to_string(),
            location: location_id(settings.transcription_location).to_string(),
            duration_seconds: stats.duration_seconds,
        })?;

    services.clipboard.write_text(&transcript)?;
    let _ = app.emit(
        "transcript-history-updated",
        TranscriptHistoryUpdatedEvent {
            item: stored_item.clone(),
        },
    );
    emit_backend_event(&app, "info", "Transcript copied to clipboard");

    // Usage stats are best-effort: a stats write must never fail the dictation
    // the user just completed.
    let word_count = transcript.split_whitespace().count() as u64;
    if let Err(err) = record_usage_stats(&services, word_count, stats.duration_seconds) {
        emit_backend_event(
            &app,
            "warning",
            format!("Could not update usage stats: {err}"),
        );
    }

    // Save to the durable notes library (also best-effort), then notify the
    // home window so the Notes screen refreshes live.
    match save_note(&services, transcript.clone(), stats.duration_seconds) {
        Ok(note) => {
            let _ = app.emit("notes-updated", &note);
        }
        Err(err) => emit_backend_event(&app, "warning", format!("Could not save note: {err}")),
    }

    Ok(transcript)
}

/// Stop audio capture and reject recordings that are too short or silent.
/// Shared by the dictation commit path and the speed test so neither forks the
/// validation. On rejection the in-flight transcription session is cancelled.
fn stop_and_validate_recording(
    app: &AppHandle,
    services: &State<'_, AppServices>,
    settings: &Settings,
) -> Result<Recording, String> {
    let recording = match services
        .audio
        .lock()
        .map_err(|_| "Audio service lock failed".to_string())?
        .stop()
    {
        Ok(recording) => recording,
        Err(err) => {
            cancel_transcription_for_location(services, settings.transcription_location)?;
            return Err(err);
        }
    };
    let stats = recording.stats();
    emit_backend_event(
        app,
        "info",
        format!(
            "Recording stopped: {:.2}s, peak {:.4}, rms {:.4}, dropped stream frames {}",
            stats.duration_seconds, stats.peak, stats.rms, stats.dropped_stream_frames
        ),
    );

    if stats.duration_seconds < MIN_RECORDING_SECONDS {
        emit_backend_event(app, "warning", "Recording rejected as too short");
        cancel_transcription_for_location(services, settings.transcription_location)?;
        return Err("Recording was too short".to_string());
    }

    if stats.rms < SILENCE_RMS_THRESHOLD && stats.peak < SILENCE_PEAK_THRESHOLD {
        emit_backend_event(
            app,
            "warning",
            format!(
                "Recording rejected as silence: peak {:.4}, rms {:.4}",
                stats.peak, stats.rms
            ),
        );
        cancel_transcription_for_location(services, settings.transcription_location)?;
        return Err("No speech detected - raise input gain or change microphone".to_string());
    }

    Ok(recording)
}

/// Finish the active transcription session and return the raw transcript (no
/// post-processing). Shared by the dictation commit path and the speed test.
fn finish_transcription_raw(
    app: &AppHandle,
    services: &State<'_, AppServices>,
    recording: &Recording,
    settings: &Settings,
) -> Result<String, String> {
    let raw_transcript = match settings.transcription_location {
        TranscriptionLocation::Local => {
            let result = services
                .transcription
                .lock()
                .map_err(|_| "Transcription service lock failed".to_string())?
                .finish_session(recording, settings, &services.models);
            clear_local_transcription_cancel(services)?;
            match result {
                Ok(transcript) => transcript,
                Err(_)
                    if services
                        .transcription_cancel_requested
                        .load(Ordering::SeqCst) =>
                {
                    emit_backend_event(app, "info", "Transcription cancelled");
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
        emit_backend_event(app, "info", "Transcription cancelled");
        return Err("Transcription was cancelled".to_string());
    }

    Ok(raw_transcript)
}

fn stop_side_effect_free_test_capture(
    app: AppHandle,
    services: State<'_, AppServices>,
) -> Result<SpeedTestCapture, String> {
    services
        .transcription_cancel_requested
        .store(false, Ordering::SeqCst);

    let settings = services
        .settings
        .lock()
        .map_err(|_| "Settings service lock failed".to_string())?
        .current();

    let recording = stop_and_validate_recording(&app, &services, &settings)?;
    let stats = recording.stats();

    let started = std::time::Instant::now();
    let transcript = finish_transcription_raw(&app, &services, &recording, &settings)?;
    let transcribe_ms = started.elapsed().as_millis().min(u128::from(u32::MAX)) as u32;

    Ok(build_capture_result(
        transcript,
        stats.duration_seconds,
        transcribe_ms,
        &settings,
    ))
}

/// The speed test's speaking leg. Reuses the real capture + transcription path
/// (started via `start_recording`) but commits nothing — no clipboard, history,
/// notes, usage stats, or speed-test result persistence — so a benchmark run
/// never pollutes real data.
#[tauri::command]
fn stop_speed_test_capture(
    app: AppHandle,
    services: State<'_, AppServices>,
) -> Result<SpeedTestCapture, String> {
    stop_side_effect_free_test_capture(app, services)
}

/// The voice accuracy test capture. This intentionally shares the same
/// side-effect-free path as the speed test while exposing a command name that
/// does not imply speed-test result persistence.
#[tauri::command]
fn stop_voice_test_capture(
    app: AppHandle,
    services: State<'_, AppServices>,
) -> Result<SpeedTestCapture, String> {
    stop_side_effect_free_test_capture(app, services)
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SpeedTestResultInput {
    typing_wpm: u32,
    speaking_wpm: u32,
    multiplier: f32,
    accuracy: u32,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SpeedTestOutcome {
    summary: SpeedTestSummary,
    is_best: bool,
}

#[tauri::command]
fn save_speed_test_result(
    input: SpeedTestResultInput,
    services: State<'_, AppServices>,
) -> Result<SpeedTestOutcome, String> {
    let record = SpeedTestRecord {
        typing_wpm: input.typing_wpm,
        speaking_wpm: input.speaking_wpm,
        multiplier: input.multiplier,
        accuracy: input.accuracy,
        recorded_at: current_epoch_seconds(),
    };
    let (summary, is_best) = services
        .speed_test
        .lock()
        .map_err(|_| "Speed test service lock failed".to_string())?
        .record(record)?;
    Ok(SpeedTestOutcome { summary, is_best })
}

#[tauri::command]
fn get_speed_test_summary(services: State<'_, AppServices>) -> Result<SpeedTestSummary, String> {
    services
        .speed_test
        .lock()
        .map_err(|_| "Speed test service lock failed".to_string())
        .map(|service| service.summary())
}

#[tauri::command]
fn set_measured_typing_wpm(
    app: AppHandle,
    wpm: f64,
    services: State<'_, AppServices>,
) -> Result<UsageStatsSummary, String> {
    let summary = {
        let mut service = services
            .usage_stats
            .lock()
            .map_err(|_| "Usage stats service lock failed".to_string())?;
        service.set_measured_typing_wpm(wpm)?;
        service.summary(current_epoch_seconds())
    };
    let _ = app.emit("usage-stats-updated", &summary);
    Ok(summary)
}

fn save_note(
    services: &State<'_, AppServices>,
    text: String,
    duration_seconds: f32,
) -> Result<Note, String> {
    services
        .notes
        .lock()
        .map_err(|_| "Notes service lock failed".to_string())?
        .add(NewNote {
            text,
            duration_seconds,
        })
}

fn record_usage_stats(
    services: &State<'_, AppServices>,
    words: u64,
    recording_seconds: f32,
) -> Result<(), String> {
    services
        .usage_stats
        .lock()
        .map_err(|_| "Usage stats service lock failed".to_string())?
        .record(words, recording_seconds, current_epoch_seconds())
}

fn current_epoch_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
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

fn start_transcript_preview_forwarder(app: AppHandle) -> TranscriptionPreviewSender {
    let (tx, rx) = mpsc::channel::<TranscriptPreview>();
    thread::Builder::new()
        .name("transcript-preview-forwarder".to_string())
        .spawn(move || {
            for preview in rx {
                let _ = app.emit(
                    "transcript-preview",
                    TranscriptPreviewEvent {
                        index: preview.index,
                        text: preview.text,
                        final_preview: preview.final_preview,
                    },
                );
            }
        })
        .ok();
    tx
}

fn backend_id(backend: TranscriptionBackend) -> &'static str {
    match backend {
        TranscriptionBackend::Whisper => "whisper",
        TranscriptionBackend::SherpaStreaming => "sherpa-streaming",
    }
}

fn location_id(location: TranscriptionLocation) -> &'static str {
    match location {
        TranscriptionLocation::Local => "local",
        TranscriptionLocation::RemoteHost => "remote-host",
    }
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
        .on_window_event(|window, event| {
            // The home window is a real app window: closing it should hide it
            // (keeping it reopenable from the pill), not tear it down.
            if window.label() == "home" {
                if let WindowEvent::CloseRequested { api, .. } = event {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .manage(AppServices {
            audio: Mutex::new(AudioService::default()),
            clipboard: ClipboardService::default(),
            models: ModelService::default(),
            model_prepare_running: Arc::new(AtomicBool::new(false)),
            transcription_cancel_requested: Arc::new(AtomicBool::new(false)),
            local_transcription_cancel: Mutex::new(None),
            settings: Mutex::new(SettingsService::default()),
            transcript_history: Mutex::new(TranscriptHistoryService::default()),
            notes: Mutex::new(NotesService::default()),
            usage_stats: Mutex::new(UsageStatsService::default()),
            speed_test: Mutex::new(SpeedTestService::default()),
            transcription: Mutex::new(TranscriptionService::default()),
            remote_transcription: Mutex::new(None),
            transcript_shelf_positioned: AtomicBool::new(false),
        })
        .invoke_handler(tauri::generate_handler![
            get_app_status,
            get_settings,
            save_settings,
            save_dictionary,
            get_model_status,
            prepare_model,
            get_transcription_model_status,
            prepare_transcription_model,
            begin_prepare_transcription_model,
            test_remote_transcription_host,
            open_home_window,
            show_transcript_shelf_window,
            hide_transcript_shelf_window,
            get_usage_stats,
            get_notes,
            update_note,
            set_note_pinned,
            delete_note,
            copy_note,
            get_transcript_history,
            clear_transcript_history,
            copy_transcript_history_item,
            delete_transcript_history_item,
            start_recording,
            stop_and_transcribe,
            cancel_transcription,
            stop_speed_test_capture,
            stop_voice_test_capture,
            save_speed_test_result,
            get_speed_test_summary,
            set_measured_typing_wpm,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
