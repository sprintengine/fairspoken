mod audio;
mod clipboard;
mod host;
#[cfg(target_os = "macos")]
mod macos_input;
mod models;
mod notes;
mod polish;
mod post_processing;
mod remote_transcription;
mod settings;
mod sounds;
mod speed_test;
mod transcript_history;
mod transcription;
mod usage_stats;

pub use host::run_transcription_host;

use audio::{AudioService, LevelSample, Recording, StreamErrorCallback};
use clipboard::ClipboardService;
use models::{ModelPrepareProgress, ModelService, ModelStatus, SttModel};
use notes::{NewNote, Note, NotesService};
use post_processing::apply_transcript_post_processing;
use remote_transcription::{
    start_remote_streaming_session,
    test_remote_transcription_host as check_remote_transcription_host, RemoteHealth,
    RemoteStreamingSession,
};
use serde::{Deserialize, Serialize};
use settings::{Settings, SettingsService, Snippet, TranscriptCorrection, TranscriptionLocation};
use sounds::{play_interaction_sound, InteractionSound};
use speed_test::{
    build_capture_result, SpeedTestCapture, SpeedTestRecord, SpeedTestService, SpeedTestSummary,
};
use std::sync::atomic::AtomicU64;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc, Arc, Mutex,
};
use std::thread;
use std::time::Duration;
use tauri::{
    AppHandle, Emitter, LogicalPosition, LogicalSize, Manager, Position, Size, State, WindowEvent,
};
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
    /// Bumped on every recording start so the max-duration watchdog can tell
    /// whether the session it armed for is still the active one.
    recording_generation: AtomicU64,
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
    #[cfg(target_os = "macos")]
    fn_push_to_talk_enabled: Arc<AtomicBool>,
    #[cfg(target_os = "macos")]
    fn_push_to_talk_tap_started: AtomicBool,
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
    #[cfg(target_os = "macos")]
    sync_fn_push_to_talk(&app, services.inner(), normalized.fn_push_to_talk);
    Ok(())
}

/// Applies the hold-Fn setting: the event tap is installed at most once per
/// app run (installing prompts for Input Monitoring, so it only happens when
/// the user has the feature on), and the enabled flag gates event emission so
/// later toggles apply instantly without reinstalling.
#[cfg(target_os = "macos")]
fn sync_fn_push_to_talk(app: &AppHandle, services: &AppServices, enabled: bool) {
    services
        .fn_push_to_talk_enabled
        .store(enabled, Ordering::SeqCst);
    if enabled
        && !services
            .fn_push_to_talk_tap_started
            .swap(true, Ordering::SeqCst)
    {
        macos_input::spawn_fn_push_to_talk_tap(
            app.clone(),
            Arc::clone(&services.fn_push_to_talk_enabled),
        );
    }
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
    model: Option<SttModel>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ModelPrepareProgressEvent {
    model: String,
    stage: String,
    message: String,
    percentage: u8,
    done: bool,
    error: Option<String>,
    status: Option<ModelStatus>,
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
    model: SttModel,
    services: State<'_, AppServices>,
) -> Result<ModelStatus, String> {
    Ok(services.models.status(model))
}

#[tauri::command]
fn prepare_model(
    model: SttModel,
    services: State<'_, AppServices>,
) -> Result<ModelStatus, String> {
    services.models.prepare(model)
}

#[tauri::command]
fn get_transcription_model_status(
    request: TranscriptionModelRequest,
    services: State<'_, AppServices>,
) -> Result<ModelStatus, String> {
    let settings = services
        .settings
        .lock()
        .map_err(|_| "Settings service lock failed".to_string())?
        .current();
    Ok(services
        .models
        .status(request.model.unwrap_or(settings.model)))
}

#[tauri::command]
fn prepare_transcription_model(
    request: TranscriptionModelRequest,
    services: State<'_, AppServices>,
) -> Result<ModelStatus, String> {
    let settings = services
        .settings
        .lock()
        .map_err(|_| "Settings service lock failed".to_string())?
        .current();
    services
        .models
        .prepare(request.model.unwrap_or(settings.model))
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
    let model = request.model.unwrap_or(settings.model);
    let model_id = model.model_id().to_string();
    let models = services.models.clone();
    let running = Arc::clone(&services.model_prepare_running);

    thread::Builder::new()
        .name("model-preparation".to_string())
        .spawn(move || {
            emit_model_prepare_event(
                &app,
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

            let result = models.prepare_with_progress(model, |progress| {
                emit_model_prepare_event(&app, &model_id, progress, false, None, None);
            });

            match result {
                Ok(status) => emit_model_prepare_event(
                    &app,
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
    model: &str,
    progress: ModelPrepareProgress,
    done: bool,
    error: Option<String>,
    status: Option<ModelStatus>,
) {
    let _ = app.emit(
        "model-prepare-progress",
        ModelPrepareProgressEvent {
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

/// Native window bounds (logical points) for each mini-pill layout state.
/// Each is the visible capsule plus a 6 pt margin so the capsule's shadow is
/// not clipped; the margin is part of the always-on-top hit area, which is why
/// idle is kept as small as possible.
fn pill_window_bounds(state: &str) -> Option<(f64, f64)> {
    match state {
        "idle" => Some((76.0, 24.0)),
        "hover" => Some((212.0, 46.0)),
        "starting" | "recording" | "transcribing" => Some((212.0, 46.0)),
        "copied" => Some((140.0, 46.0)),
        "polished" => Some((220.0, 46.0)),
        "error" => Some((280.0, 46.0)),
        _ => None,
    }
}

/// Gap between the capsule and the bottom of the monitor work area, so the
/// pill floats just above the Dock like the competitors' recorders.
const PILL_BOTTOM_GAP: f64 = 12.0;

/// Resizes the pill window for a layout state and re-anchors it bottom-center
/// of the work area of whichever monitor it is currently on.
#[tauri::command]
fn layout_pill_window(app: AppHandle, state: String) -> Result<(), String> {
    let window = app
        .get_webview_window("main")
        .ok_or_else(|| "Pill window is not configured".to_string())?;
    let (width, height) =
        pill_window_bounds(&state).ok_or_else(|| format!("Unknown pill layout state: {state}"))?;
    let monitor = window
        .current_monitor()
        .map_err(|err| err.to_string())?
        .ok_or_else(|| "Pill window has no monitor".to_string())?;

    let scale = monitor.scale_factor();
    let area = monitor.work_area();
    let area_x = f64::from(area.position.x) / scale;
    let area_y = f64::from(area.position.y) / scale;
    let area_width = f64::from(area.size.width) / scale;
    let area_height = f64::from(area.size.height) / scale;

    let x = area_x + (area_width - width) / 2.0;
    let y = area_y + area_height - height - PILL_BOTTOM_GAP;

    window
        .set_size(Size::Logical(LogicalSize { width, height }))
        .map_err(|err| err.to_string())?;
    window
        .set_position(Position::Logical(LogicalPosition { x, y }))
        .map_err(|err| err.to_string())?;
    Ok(())
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
    let retention_minutes = services
        .settings
        .lock()
        .map_err(|_| "Settings service lock failed".to_string())?
        .current()
        .note_retention_minutes;
    let mut notes = services
        .notes
        .lock()
        .map_err(|_| "Notes service lock failed".to_string())?;
    notes.sweep_expired(retention_minutes)?;
    Ok(notes.list())
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

    services
        .clipboard
        .write_text(&transcript_clipboard_text(&item.text))?;
    let _ = app.emit("transcript-copied", &item);
    Ok(item)
}

/// Copies the pre-polish transcript of a polished dictation to the clipboard
/// (the pill's "Undo AI edit" and history's "Copy original"). Undo is a copy,
/// not an in-place replacement: synthetic ⌘Z+⌘V and AX replacement were both
/// rejected as fragile (see backlog/client-polish-undo.md).
#[tauri::command]
fn copy_original_transcript(
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
    let raw_text = item
        .raw_text
        .clone()
        .ok_or_else(|| "No original transcript is stored for this dictation".to_string())?;

    services
        .clipboard
        .write_text(&transcript_clipboard_text(&raw_text))?;
    emit_backend_event(
        &app,
        "info",
        "Original transcript copied — paste to replace",
    );
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
        TranscriptionLocation::RemoteHost | TranscriptionLocation::Cloud => {
            emit_backend_event(
                &app,
                "info",
                if settings.transcription_location == TranscriptionLocation::Cloud {
                    "Starting MultiVoice Cloud streaming transcription session"
                } else {
                    "Starting remote streaming transcription session"
                },
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

    let level_sink = start_audio_level_forwarder(app.clone());
    let stream_error_app = app.clone();
    let on_stream_error: StreamErrorCallback = Box::new(move |message: String| {
        emit_backend_event(
            &stream_error_app,
            "error",
            format!("Microphone stream failed: {message}"),
        );
        let _ = stream_error_app.emit("recording-stream-error", message);
    });

    if let Err(err) = services
        .audio
        .lock()
        .map_err(|_| "Audio service lock failed".to_string())?
        .start(
            settings.max_recording_seconds,
            settings.input_gain,
            stream_sink,
            Some(level_sink),
            Some(on_stream_error),
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

    // The start click plays only once native capture is confirmed, so the
    // click never lies about recording being live.
    if settings.interaction_sounds {
        play_interaction_sound(InteractionSound::RecordingStart);
    }

    let generation = services.recording_generation.fetch_add(1, Ordering::SeqCst) + 1;
    start_max_duration_watchdog(app.clone(), generation, settings.max_recording_seconds);

    emit_backend_event(
        &app,
        "info",
        format!(
            "Recording started: location={:?}, model={:?}, gain={}x",
            settings.transcription_location, settings.model, settings.input_gain
        ),
    );
    Ok(settings.max_recording_seconds)
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AudioLevelEvent {
    peak: f32,
    rms: f32,
}

const AUDIO_LEVEL_EMIT_INTERVAL: Duration = Duration::from_millis(40);

/// Receives per-callback capture levels and forwards them to the pill as a
/// throttled `audio-level` event stream (~25 Hz). Ends when capture stops and
/// the audio stream drops its sender, parking the meter at zero.
fn start_audio_level_forwarder(app: AppHandle) -> mpsc::SyncSender<LevelSample> {
    let (tx, rx) = mpsc::sync_channel::<LevelSample>(32);
    thread::Builder::new()
        .name("audio-level-forwarder".to_string())
        .spawn(move || {
            let mut window_peak = 0.0_f32;
            let mut window_rms = 0.0_f32;
            let mut last_emit = std::time::Instant::now();
            while let Ok(sample) = rx.recv() {
                window_peak = window_peak.max(sample.peak);
                window_rms = window_rms.max(sample.rms);
                if last_emit.elapsed() >= AUDIO_LEVEL_EMIT_INTERVAL {
                    let _ = app.emit(
                        "audio-level",
                        AudioLevelEvent {
                            peak: window_peak,
                            rms: window_rms,
                        },
                    );
                    window_peak = 0.0;
                    window_rms = 0.0;
                    last_emit = std::time::Instant::now();
                }
            }
            let _ = app.emit("audio-level", AudioLevelEvent { peak: 0.0, rms: 0.0 });
        })
        .ok();
    tx
}

/// Grace past the frontend's own max-duration timer, so the watchdog only
/// ever acts when the webview failed to stop the recording itself.
const MAX_DURATION_WATCHDOG_GRACE: Duration = Duration::from_secs(2);

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RecordingAutoStoppedEvent {
    committed: bool,
    message: String,
}

/// Backend enforcement of the recording cap. The frontend stops recordings on
/// its own timer; if the webview is wedged, this thread commits the dictation
/// through the real stop pipeline so capture can never silently run forever.
fn start_max_duration_watchdog(app: AppHandle, generation: u64, max_recording_seconds: u16) {
    thread::Builder::new()
        .name("max-recording-watchdog".to_string())
        .spawn(move || {
            thread::sleep(
                Duration::from_secs(u64::from(max_recording_seconds)) + MAX_DURATION_WATCHDOG_GRACE,
            );
            let services = app.state::<AppServices>();
            if services.recording_generation.load(Ordering::SeqCst) != generation {
                return;
            }
            let still_recording = services
                .audio
                .lock()
                .map(|audio| audio.is_recording())
                .unwrap_or(false);
            if !still_recording {
                return;
            }

            emit_backend_event(
                &app,
                "warning",
                "Max recording duration reached; committing the dictation from the backend",
            );
            let event = match perform_stop_and_transcribe(&app, &services) {
                Ok(_) => RecordingAutoStoppedEvent {
                    committed: true,
                    message: "Recording hit the maximum duration and was transcribed".to_string(),
                },
                Err(err) => RecordingAutoStoppedEvent {
                    committed: false,
                    message: format!(
                        "Recording hit the maximum duration but could not be transcribed: {err}"
                    ),
                },
            };
            let _ = app.emit("recording-auto-stopped", event);
        })
        .ok();
}

#[tauri::command]
fn stop_and_transcribe(app: AppHandle, services: State<'_, AppServices>) -> Result<String, String> {
    perform_stop_and_transcribe(&app, &services)
}

/// The full dictation commit pipeline. Free of the command layer so both the
/// `stop_and_transcribe` command and the max-duration watchdog can run it.
fn perform_stop_and_transcribe(app: &AppHandle, services: &AppServices) -> Result<String, String> {
    services
        .transcription_cancel_requested
        .store(false, Ordering::SeqCst);

    let settings = services
        .settings
        .lock()
        .map_err(|_| "Settings service lock failed".to_string())?
        .current();
    let recording = stop_and_validate_recording(app, services, &settings)?;
    let stats = recording.stats();

    emit_backend_event(
        app,
        "info",
        format!(
            "Finishing transcription with location={:?}, model={:?}",
            settings.transcription_location, settings.model
        ),
    );
    let raw_transcription = finish_transcription_raw(app, services, &recording, &settings)?;
    let raw_transcript = raw_transcription.text;

    // AI polish sits between the raw transcript and the user's deterministic
    // rules — user-authored corrections/snippets stay authoritative and run
    // last. Any polish failure falls back to the raw transcript silently
    // (event-logged): polish must never turn a successful dictation into a
    // failure.
    #[cfg(target_os = "macos")]
    let frontmost_app = macos_input::frontmost_app().map(|app| polish::PolishTargetApp {
        bundle_id: app.bundle_id,
        name: app.name,
    });
    #[cfg(not(target_os = "macos"))]
    let frontmost_app: Option<polish::PolishTargetApp> = None;

    let (transcript_for_rules, polished) =
        match polish::maybe_polish(&raw_transcript, &settings, frontmost_app.as_ref()) {
            polish::PolishDecision::Polished(outcome) => {
                emit_backend_event(
                    app,
                    "info",
                    format!(
                        "Transcript polished ({}ms, {})",
                        outcome.duration_ms, outcome.model
                    ),
                );
                (outcome.text, true)
            }
            polish::PolishDecision::Unchanged { duration_ms } => {
                emit_backend_event(
                    app,
                    "info",
                    format!("Transcript polish made no changes ({duration_ms}ms)"),
                );
                (raw_transcript.clone(), false)
            }
            polish::PolishDecision::Skipped(reason) => {
                emit_backend_event(app, "info", format!("Polish skipped: {reason}"));
                (raw_transcript.clone(), false)
            }
            polish::PolishDecision::Failed(reason) => {
                emit_backend_event(app, "warning", format!("Polish skipped: {reason}"));
                (raw_transcript.clone(), false)
            }
            polish::PolishDecision::Disabled => (raw_transcript.clone(), false),
        };

    let processed = apply_transcript_post_processing(&transcript_for_rules, &settings);
    if processed.corrections_applied > 0 {
        emit_backend_event(
            app,
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
            backend: raw_transcription.backend,
            location: location_id(settings.transcription_location).to_string(),
            duration_seconds: stats.duration_seconds,
            polished,
            raw_text: polished.then(|| raw_transcript.clone()),
        })?;

    let clipboard_text = transcript_clipboard_text(&transcript);
    #[cfg(target_os = "macos")]
    let will_insert_at_cursor = settings.insert_at_cursor && !transcript.is_empty();

    services.clipboard.write_text(&clipboard_text)?;
    let _ = app.emit(
        "transcript-history-updated",
        TranscriptHistoryUpdatedEvent {
            item: stored_item.clone(),
        },
    );

    #[cfg(target_os = "macos")]
    if will_insert_at_cursor {
        deliver_transcript_at_cursor(app);
    } else {
        emit_backend_event(app, "info", "Transcript copied to clipboard");
    }
    #[cfg(not(target_os = "macos"))]
    emit_backend_event(app, "info", "Transcript copied to clipboard");

    // Usage stats are best-effort: a stats write must never fail the dictation
    // the user just completed.
    let word_count = transcript.split_whitespace().count() as u64;
    if let Err(err) = record_usage_stats(services, word_count, stats.duration_seconds) {
        emit_backend_event(
            app,
            "warning",
            format!("Could not update usage stats: {err}"),
        );
    }

    // Save to the durable notes library (also best-effort), then notify the
    // home window so the Notes screen refreshes live.
    match save_note(services, transcript.clone(), stats.duration_seconds) {
        Ok(note) => {
            let _ = app.emit("notes-updated", &note);
        }
        Err(err) => emit_backend_event(app, "warning", format!("Could not save note: {err}")),
    }

    Ok(transcript)
}

/// Pastes the freshly copied transcript into whichever app the user is
/// dictating into. Skipped when one of our own windows is focused (the paste
/// would land in Multivoice itself); every other failure is surfaced as a
/// backend event because the user is otherwise left wondering why no text
/// appeared.
///
/// The transcript always stays on the clipboard afterwards — paste or no
/// paste — so the user can paste the same dictation into multiple targets.
#[cfg(target_os = "macos")]
fn deliver_transcript_at_cursor(app: &AppHandle) {
    let our_window_focused = app
        .webview_windows()
        .values()
        .any(|window| window.is_focused().unwrap_or(false));
    if our_window_focused {
        emit_backend_event(
            app,
            "info",
            "Insert at cursor skipped while a Multivoice window is focused; transcript is on the clipboard",
        );
        return;
    }

    match macos_input::paste_clipboard_at_cursor() {
        Ok(()) => emit_backend_event(
            app,
            "info",
            "Transcript inserted at cursor and kept on the clipboard",
        ),
        Err(err) => emit_backend_event(
            app,
            "warning",
            format!("Insert at cursor failed ({err}); transcript is on the clipboard"),
        ),
    }
}

fn transcript_clipboard_text(transcript: &str) -> String {
    let trimmed = transcript.trim_end();
    if trimmed.is_empty() {
        String::new()
    } else {
        format!("{trimmed} ")
    }
}

#[cfg(test)]
mod tests {
    use super::transcript_clipboard_text;

    #[test]
    fn transcript_clipboard_text_adds_single_trailing_space() {
        assert_eq!(
            transcript_clipboard_text("First recording."),
            "First recording. "
        );
        assert_eq!(
            transcript_clipboard_text("First recording.   "),
            "First recording. "
        );
    }

    #[test]
    fn transcript_clipboard_text_preserves_empty_transcript() {
        assert_eq!(transcript_clipboard_text("   "), "");
    }
}

/// Stop audio capture and reject recordings that are too short or silent.
/// Shared by the dictation commit path and the speed test so neither forks the
/// validation. On rejection the in-flight transcription session is cancelled.
fn stop_and_validate_recording(
    app: &AppHandle,
    services: &AppServices,
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

    // Played on confirmed capture stop — before validation, because the user
    // action being acknowledged is "recording ended", not "transcript OK".
    if settings.interaction_sounds {
        play_interaction_sound(InteractionSound::RecordingStop);
    }

    let stats = recording.stats();
    emit_backend_event(
        app,
        "info",
        format!(
            "Recording stopped: {:.2}s, peak {:.4}, rms {:.4}, dropped stream frames {}",
            stats.duration_seconds, stats.peak, stats.rms, stats.dropped_stream_frames
        ),
    );

    if stats.dropped_stream_frames > 0 {
        emit_backend_event(
            app,
            "warning",
            format!(
                "{} audio frames were dropped while streaming to the transcriber; the transcript may be missing words",
                stats.dropped_stream_frames
            ),
        );
    }

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

/// A finished raw transcription plus the engine that actually produced it —
/// the backend id is what history stores, so it must be truthful per
/// location (local Parakeet vs whatever the remote/cloud host reports).
struct RawTranscription {
    text: String,
    backend: String,
}

/// Finish the active transcription session and return the raw transcript (no
/// post-processing). Shared by the dictation commit path and the speed test.
fn finish_transcription_raw(
    app: &AppHandle,
    services: &AppServices,
    recording: &Recording,
    settings: &Settings,
) -> Result<RawTranscription, String> {
    let raw_transcription = match settings.transcription_location {
        TranscriptionLocation::Local => {
            let result = services
                .transcription
                .lock()
                .map_err(|_| "Transcription service lock failed".to_string())?
                .finish_session(recording, settings, &services.models);
            clear_local_transcription_cancel(services)?;
            match result {
                Ok(transcript) => RawTranscription {
                    text: transcript,
                    backend: remote_transcription::BACKEND_ID.to_string(),
                },
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
        TranscriptionLocation::RemoteHost | TranscriptionLocation::Cloud => {
            let response = services
                .remote_transcription
                .lock()
                .map_err(|_| "Remote transcription service lock failed".to_string())?
                .take()
                .ok_or_else(|| "No remote transcription session is active".to_string())?
                .finish()?;
            RawTranscription {
                text: response.text,
                backend: response.backend,
            }
        }
    };

    if services
        .transcription_cancel_requested
        .load(Ordering::SeqCst)
    {
        emit_backend_event(app, "info", "Transcription cancelled");
        return Err("Transcription was cancelled".to_string());
    }

    Ok(raw_transcription)
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
    let transcript = finish_transcription_raw(&app, &services, &recording, &settings)?.text;
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
    services: &AppServices,
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
    services: &AppServices,
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

/// Plays an interaction sound on demand (the Settings screen's preview when
/// the toggle is switched on). Deliberately ignores the interaction-sounds
/// setting: the user just asked to hear it.
#[tauri::command]
fn preview_interaction_sound(sound: String) -> Result<(), String> {
    match sound.as_str() {
        "recording-start" => play_interaction_sound(InteractionSound::RecordingStart),
        "recording-stop" => play_interaction_sound(InteractionSound::RecordingStop),
        other => return Err(format!("Unknown interaction sound: {other}")),
    }
    Ok(())
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

pub(crate) fn emit_backend_event(app: &AppHandle, level: &'static str, message: impl Into<String>) {
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

/// Apply the note-retention setting while the app sits idle, so expired
/// notes disappear without waiting for the notes screen to reload them.
fn start_note_retention_sweeper(app: AppHandle) {
    thread::Builder::new()
        .name("note-retention-sweeper".to_string())
        .spawn(move || loop {
            thread::sleep(Duration::from_secs(60));
            let services = app.state::<AppServices>();
            let retention_minutes = services
                .settings
                .lock()
                .map(|service| service.current().note_retention_minutes)
                .unwrap_or(0);
            if retention_minutes == 0 {
                continue;
            }
            let removed = services
                .notes
                .lock()
                .ok()
                .and_then(|mut notes| notes.sweep_expired(retention_minutes).ok())
                .unwrap_or(false);
            if removed {
                let _ = app.emit("notes-updated", ());
            }
        })
        .ok();
}

fn location_id(location: TranscriptionLocation) -> &'static str {
    match location {
        TranscriptionLocation::Local => "local",
        TranscriptionLocation::RemoteHost => "remote-host",
        TranscriptionLocation::Cloud => "cloud",
    }
}

fn cancel_transcription_for_location(
    services: &AppServices,
    location: TranscriptionLocation,
) -> Result<(), String> {
    match location {
        TranscriptionLocation::Local => cancel_local_transcription_if_needed(services),
        TranscriptionLocation::RemoteHost | TranscriptionLocation::Cloud => {
            cancel_remote_transcription_if_needed(services)
        }
    }
}

fn cancel_local_transcription_if_needed(services: &AppServices) -> Result<(), String> {
    services
        .transcription
        .lock()
        .map_err(|_| "Transcription service lock failed".to_string())?
        .cancel_session();
    clear_local_transcription_cancel(services)?;
    Ok(())
}

fn clear_local_transcription_cancel(services: &AppServices) -> Result<(), String> {
    *services
        .local_transcription_cancel
        .lock()
        .map_err(|_| "Transcription cancellation lock failed".to_string())? = None;
    Ok(())
}

fn cancel_remote_transcription_if_needed(services: &AppServices) -> Result<(), String> {
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
            clipboard: ClipboardService,
            models: ModelService::default(),
            recording_generation: AtomicU64::new(0),
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
            #[cfg(target_os = "macos")]
            fn_push_to_talk_enabled: Arc::new(AtomicBool::new(false)),
            #[cfg(target_os = "macos")]
            fn_push_to_talk_tap_started: AtomicBool::new(false),
        })
        .setup(|app| {
            let handle = app.handle();
            if let Err(err) = layout_pill_window(handle.clone(), "idle".to_string()) {
                emit_backend_event(handle, "warning", format!("Could not position pill: {err}"));
            }
            start_note_retention_sweeper(handle.clone());
            #[cfg(target_os = "macos")]
            {
                let services = app.state::<AppServices>();
                let fn_enabled = services
                    .settings
                    .lock()
                    .map(|service| service.current().fn_push_to_talk)
                    .unwrap_or(false);
                sync_fn_push_to_talk(handle, services.inner(), fn_enabled);
            }
            Ok(())
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
            layout_pill_window,
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
            copy_original_transcript,
            delete_transcript_history_item,
            start_recording,
            stop_and_transcribe,
            cancel_transcription,
            preview_interaction_sound,
            stop_speed_test_capture,
            stop_voice_test_capture,
            save_speed_test_result,
            get_speed_test_summary,
            set_measured_typing_wpm,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
