mod app_categories;
mod app_dirs;
mod audio;
// Consumed by the macOS-only AX integration; compiled everywhere so the pure
// logic and its tests stay platform-neutral.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
mod ax_context;
mod clipboard;
mod cursor_preview;
mod host;
mod hugging_face;
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
mod insertion;
mod live_preview;
mod local_models;
#[cfg(target_os = "macos")]
mod macos_ax;
#[cfg(target_os = "macos")]
mod macos_input;
mod models;
mod note_debug;
mod notes;
mod polish;
mod polish_stream;
mod post_processing;
mod preview;
mod remote_transcription;
mod settings;
mod sounds;
mod speed_test;
mod transcript_cleanup;
mod transcript_history;
mod transcription;
mod updates;
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
/// How long the finished preview stays up so the last edits can be read.
const CURSOR_PREVIEW_LINGER: Duration = Duration::from_millis(1200);

struct AppServices {
    debug_capture_enabled: AtomicBool,
    debug_trace: Mutex<Option<note_debug::Trace>>,
    operation: Mutex<()>,
    preview_generation: AtomicU64,
    preview_gate: Mutex<()>,
    cursor_snapshot: Mutex<live_preview::Snapshot>,
    preview_cancel: Mutex<Option<Arc<AtomicBool>>>,
    /// Sealed-prefix polish state for the active dictation. Live passes and
    /// the commit path share it, so release only has to polish the tail.
    polish_stream: Mutex<polish_stream::PolishStream>,
    local_models: local_models::LocalModels,
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
    /// Screen-harvested vocabulary for the CURRENT recording session only
    /// (context awareness Phase B). Set at recording start, read at finish,
    /// never persisted.
    session_vocabulary: Mutex<Vec<String>>,
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
fn get_update_status(app: AppHandle) -> updates::UpdateStatus {
    updates::status(&app)
}
#[tauri::command]
async fn check_for_updates(app: AppHandle) -> Result<updates::UpdateStatus, String> {
    updates::check(&app).await
}
#[tauri::command]
fn install_update(app: AppHandle) -> Result<(), String> {
    updates::install_and_restart(&app)
}

#[derive(Clone, Serialize)]
struct NoteDebugStatus {
    available: bool,
    enabled: bool,
}
#[tauri::command]
fn get_note_debug_status(services: State<'_, AppServices>) -> NoteDebugStatus {
    NoteDebugStatus {
        available: note_debug::AVAILABLE,
        enabled: note_debug::AVAILABLE && services.debug_capture_enabled.load(Ordering::SeqCst),
    }
}
#[tauri::command]
fn set_note_debug_capture(
    app: AppHandle,
    enabled: bool,
    services: State<'_, AppServices>,
) -> Result<(), String> {
    if !note_debug::AVAILABLE {
        return Err("Note metadata is development-only".into());
    }
    services
        .debug_capture_enabled
        .store(enabled, Ordering::SeqCst);
    let _ = app.emit(
        "note-debug-status",
        NoteDebugStatus {
            available: true,
            enabled,
        },
    );
    Ok(())
}
#[tauri::command]
async fn get_note_metadata(
    app: AppHandle,
    id: String,
) -> Result<Option<note_debug::NoteMetadata>, String> {
    if !note_debug::AVAILABLE {
        return Err("Note metadata is development-only".into());
    }
    tauri::async_runtime::spawn_blocking(move || {
        app.state::<AppServices>()
            .notes
            .lock()
            .map_err(|e| e.to_string())?
            .metadata(&id)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
fn get_settings(services: State<'_, AppServices>) -> Result<Settings, String> {
    services
        .settings
        .lock()
        .map_err(|_| "Settings service lock failed".to_string())
        .map(|service| service.current())
}

/// Whether this build has a Fairspoken Cloud endpoint; without one the UI
/// hides the Cloud location and cloud polish.
#[tauri::command]
fn get_cloud_available() -> bool {
    settings::cloud_url().is_some()
}

fn save_settings_inner(
    app: AppHandle,
    settings: Option<Settings>,
    shortcut_patch: Option<ShortcutPatch>,
    services: State<'_, AppServices>,
) -> Result<(), String> {
    let _operation = services
        .operation
        .try_lock()
        .map_err(|_| "Wait for the current dictation to finish".to_string())?;
    let current_settings = services
        .settings
        .lock()
        .map_err(|_| "Settings service lock failed".to_string())?
        .current();

    let mut settings = merge_settings_shortcuts(settings, shortcut_patch, &current_settings);

    // The dictionary (vocabulary, corrections, snippets) is owned by the
    // Dictionary screen via save_dictionary; the Settings screen must not be
    // able to clear it, so preserve those fields regardless of the payload.
    settings.vocabulary_hints = current_settings.vocabulary_hints.clone();
    settings.transcript_corrections = current_settings.transcript_corrections.clone();
    settings.snippets = current_settings.snippets.clone();

    // Only newly chosen cloud options are refused, so a stale Cloud setting
    // never blocks saving unrelated changes.
    if settings::cloud_url().is_none() {
        let picks_cloud_location = settings.transcription_location == TranscriptionLocation::Cloud
            && current_settings.transcription_location != TranscriptionLocation::Cloud;
        let picks_cloud_polish = settings.polish_enabled
            && settings.polish_provider == settings::PolishProvider::Cloud
            && !(current_settings.polish_enabled
                && current_settings.polish_provider == settings::PolishProvider::Cloud);
        if picks_cloud_location || picks_cloud_polish {
            return Err(settings::CLOUD_UNAVAILABLE.to_string());
        }
    }

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

    if settings.polish_enabled && settings.polish_provider == settings::PolishProvider::Local {
        local_models::spec(&settings.polish_model)?;
        if !services.local_models.installed(&settings.polish_model) {
            return Err("Download the selected polish model first".into());
        }
    }
    if !settings.polish_enabled
        || settings.polish_provider != current_settings.polish_provider
        || settings.polish_model != current_settings.polish_model
    {
        services.local_models.unload();
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

#[derive(Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct ShortcutPatch {
    recording_shortcut: Option<String>,
    transcript_stack_shortcut: Option<String>,
}

// Chords have a separate native-registration transaction. Ordinary preference
// saves cannot overwrite them using a stale snapshot from another webview.
fn merge_settings_shortcuts(
    settings: Option<Settings>,
    patch: Option<ShortcutPatch>,
    current: &Settings,
) -> Settings {
    if let Some(patch) = patch {
        let mut next = current.clone();
        if let Some(shortcut) = patch.recording_shortcut {
            next.recording_shortcut = shortcut;
        }
        if let Some(shortcut) = patch.transcript_stack_shortcut {
            next.transcript_stack_shortcut = shortcut;
        }
        next
    } else {
        let mut next = settings.unwrap_or_else(|| current.clone());
        next.recording_shortcut = current.recording_shortcut.clone();
        next.transcript_stack_shortcut = current.transcript_stack_shortcut.clone();
        next
    }
}

#[tauri::command]
async fn save_shortcut_settings(app: AppHandle, patch: ShortcutPatch) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        save_settings_inner(app.clone(), None, Some(patch), app.state::<AppServices>())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn save_settings(app: AppHandle, settings: Settings) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        save_settings_inner(
            app.clone(),
            Some(settings),
            None,
            app.state::<AppServices>(),
        )
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn get_local_model_catalog(
    app: AppHandle,
    refresh: bool,
) -> Result<local_models::Catalog, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let services = app.state::<AppServices>();
        let settings = services
            .settings
            .lock()
            .map_err(|e| e.to_string())?
            .current();
        Ok(services.local_models.catalog(&settings, refresh))
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn download_local_model(app: AppHandle, model: String) -> Result<(), String> {
    local_models::spec(&model)?;
    app.state::<AppServices>().local_models.begin_download()?;
    tauri::async_runtime::spawn_blocking(move || {
        app.state::<AppServices>()
            .local_models
            .download(&app, &model)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
fn cancel_local_model_download(services: State<'_, AppServices>) {
    services.local_models.cancel_download();
}

#[tauri::command]
async fn remove_local_model(app: AppHandle, model: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let services = app.state::<AppServices>();
        let _operation = services
            .operation
            .try_lock()
            .map_err(|_| "Wait for dictation to finish".to_string())?;
        if services
            .audio
            .lock()
            .map_err(|e| e.to_string())?
            .is_recording()
        {
            return Err("Stop recording before removing a model".into());
        }
        services.local_models.remove(&model)?;
        let mut store = services.settings.lock().map_err(|e| e.to_string())?;
        let mut settings = store.current();
        if settings.polish_provider == settings::PolishProvider::Local
            && settings.polish_model == model
        {
            settings.polish_enabled = false;
            store.save(settings)?;
            let _ = app.emit("settings-updated", store.current());
        }
        Ok(())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
fn get_dictation_models(services: State<'_, AppServices>) -> Vec<ModelStatus> {
    let ids = [
        "parakeet-tdt-0.6b-v3",
        "parakeet-ultra",
        "parakeet-tdt-0.6b-v2",
        "tiny",
        "base",
        "small",
        "medium",
        "large-v2",
        "large-v3",
        "large-v3-turbo",
    ];
    ids.iter()
        .filter_map(|id| SttModel::from_model_id(id))
        .map(|m| ModelStatus {
            model: m,
            cached: services.models.files_present(m),
            model_path: services.models.path_for(m).display().to_string(),
            message: String::new(),
        })
        .collect()
}

#[tauri::command]
async fn search_hugging_face_models(query: String) -> Result<hugging_face::SearchResults, String> {
    tauri::async_runtime::spawn_blocking(move || hugging_face::search(&query))
        .await
        .map_err(|_| "Model search stopped unexpectedly".to_string())?
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
    let _operation = services
        .operation
        .try_lock()
        .map_err(|_| "Wait for the current settings operation to finish".to_string())?;
    let mut settings = services
        .settings
        .lock()
        .map_err(|_| "Settings service lock failed".to_string())?
        .current();
    // Zero-edit metric signal: adding a correction or dictionary term right
    // after a dictation means the user just taught the app a fix for it.
    let taught_a_fix = update.vocabulary_hints.len() > settings.vocabulary_hints.len()
        || update.transcript_corrections.len() > settings.transcript_corrections.len();
    settings.vocabulary_hints = update.vocabulary_hints;
    settings.transcript_corrections = update.transcript_corrections;
    settings.snippets = update.snippets;
    services
        .settings
        .lock()
        .map_err(|_| "Settings service lock failed".to_string())?
        .save(settings)?;
    if taught_a_fix {
        if let Ok(mut usage_stats) = services.usage_stats.lock() {
            let _ = usage_stats.mark_recent_edited(current_epoch_seconds());
        }
    }
    Ok(())
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
    session_id: u64,
    revision: u64,
    polished: bool,
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
fn prepare_model(model: SttModel, services: State<'_, AppServices>) -> Result<ModelStatus, String> {
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
        "idle" => Some((68.0, 24.0)),
        "hover" => Some((186.0, 46.0)),
        "starting" | "recording" | "transcribing" => Some((186.0, 46.0)),
        "copied" => Some((128.0, 46.0)),
        "polished" => Some((204.0, 46.0)),
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
    let _ = window.unminimize();
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
/// rejected as fragile.
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
    mark_dictation_edited(&services, &item.id);
    emit_backend_event(
        &app,
        "info",
        "Original transcript copied — paste to replace",
    );
    Ok(item)
}

/// Zero-edit metric signal — best-effort, never fails the calling action.
fn mark_dictation_edited(services: &AppServices, history_id: &str) {
    if let Ok(mut usage_stats) = services.usage_stats.lock() {
        let _ = usage_stats.mark_edited(history_id, current_epoch_seconds());
    }
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
        .delete(&id)?;
    mark_dictation_edited(&services, &id);
    Ok(())
}

#[tauri::command]
async fn start_recording(app: AppHandle) -> Result<u16, String> {
    tauri::async_runtime::spawn_blocking(move || {
        start_recording_inner(app.clone(), app.state::<AppServices>())
    })
    .await
    .map_err(|e| e.to_string())?
}
fn start_recording_inner(app: AppHandle, services: State<'_, AppServices>) -> Result<u16, String> {
    let _operation = services
        .operation
        .try_lock()
        .map_err(|_| "Dictation is still finishing".to_string())?;
    let recording_active = services
        .audio
        .lock()
        .map_err(|_| "Audio service lock failed".to_string())?
        .is_recording();
    if recording_active {
        return Err("Recording already in progress".to_string());
    }

    services
        .transcription_cancel_requested
        .store(false, Ordering::SeqCst);
    let settings = services
        .settings
        .lock()
        .map_err(|_| "Settings service lock failed".to_string())?
        .current();

    let trace = note_debug::Trace::new(services.debug_capture_enabled.load(Ordering::SeqCst));
    *services.debug_trace.lock().map_err(|e| e.to_string())? = trace.clone();
    if let Some(trace) = &trace {
        trace.event("recording-settings", serde_json::json!({"speechModel":settings.model,"location":settings.transcription_location,"polishEnabled":settings.polish_enabled,"polishProvider":settings.polish_provider,"polishModel":settings.polish_model,"contextAwareness":settings.context_awareness,"dictionaryVocabulary":settings.vocabulary_hints,"note":"Accessibility collection is unchanged by debug capture. Configured authentication secrets and audio samples are not captured."}));
    }
    // Context awareness Phase B: harvest on-screen terms into this session's
    // vocabulary hints (session-only, except optional dev metadata). Budgeted and
    // thread-timed so a wedged app cannot delay recording start beyond the
    // AX timeout. The terms are kept in service state so the finish flow
    // (which re-reads settings) sees the same hints.
    #[cfg_attr(not(target_os = "macos"), allow(unused_mut))]
    let mut harvested_terms: Vec<String> = Vec::new();
    #[cfg(target_os = "macos")]
    if settings.context_awareness {
        let harvest = with_ax_timeout(macos_ax::harvest_screen_context);
        if let Some(trace) = &trace {
            trace.event("accessibility-harvest", serde_json::json!({"status":match &harvest { Some(Some(_)) => "read", Some(None) => "excluded-or-unavailable", None => "timeout" },"windowTexts":harvest.as_ref().and_then(|v|v.as_ref()).map(|v|&v.0),"extractedTerms":harvest.as_ref().and_then(|v|v.as_ref()).map(|v|&v.1),"sentToPolish":"Only extracted terms, not the windowTexts, are added to vocabulary."}));
        }
        match harvest {
            Some(Some((_, terms))) if !terms.is_empty() => {
                emit_backend_event(
                    &app,
                    "info",
                    format!(
                        "Context harvest: {} on-screen terms added as hints",
                        terms.len()
                    ),
                );
                harvested_terms = terms;
            }
            Some(None) => emit_backend_event(
                &app,
                "info",
                "Context harvest skipped (password manager or no frontmost app)",
            ),
            _ => {}
        }
    }
    #[cfg(not(target_os = "macos"))]
    if settings.context_awareness {
        if let Some(trace) = &trace {
            trace.event(
                "accessibility-harvest",
                serde_json::json!({"status":"platform-unavailable"}),
            );
        }
    }
    if !settings.context_awareness {
        if let Some(trace) = &trace {
            trace.event(
                "accessibility-harvest",
                serde_json::json!({"status":"disabled"}),
            );
        }
    }
    if let Ok(mut session_vocabulary) = services.session_vocabulary.lock() {
        *session_vocabulary = harvested_terms.clone();
    }
    let settings = settings.with_session_vocabulary(harvested_terms);

    // If a previous start failed after opening the transcription worker but before
    // audio capture became active, clear that stale worker before the next attempt.
    cancel_transcription_for_location(&services, settings.transcription_location)?;
    invalidate_previews(&services);
    let cursor_session = services.preview_generation.load(Ordering::SeqCst);
    // Remote transcription streams no previews, so bind the polish stream here
    // rather than in the forwarder: the commit path must never mistake a
    // previous dictation's sealed text for this one's.
    if let Ok(mut stream) = services.polish_stream.lock() {
        stream.begin(cursor_session);
    }
    update_cursor_snapshot(&app, |s| {
        s.begin(
            cursor_session,
            settings.transcription_location != TranscriptionLocation::Local,
        );
        true
    });
    let mut cursor_guard = CursorSessionGuard::new(&app, cursor_session);
    let _ = app.emit("transcript-session-started", cursor_session);

    let stream_sink = match settings.transcription_location {
        TranscriptionLocation::Local => {
            let preview_tx =
                start_transcript_preview_forwarder(app.clone(), settings.clone(), trace.clone());
            let start = services
                .transcription
                .lock()
                .map_err(|_| "Transcription service lock failed".to_string())?
                .start_session_traced(&settings, &services.models, Some(preview_tx), trace.clone());
            let start = match start {
                Ok(start) => start,
                Err(err) => {
                    invalidate_previews(&services);
                    return Err(err);
                }
            };
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
                    "Starting Fairspoken Cloud streaming transcription session"
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
        hide_cursor_session(&stream_error_app, cursor_session);
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
        invalidate_previews(&services);
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

    // Zero-edit metric signal: a recording starting right after a short
    // dictation is treated as the user scrapping it and re-dictating. Fired
    // only now that capture is confirmed live — a failed start (mic missing,
    // host down) is not a re-dictation.
    if let Ok(mut usage_stats) = services.usage_stats.lock() {
        let _ = usage_stats.note_recording_started(current_epoch_seconds());
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
    if let Ok(snapshot) = services.cursor_snapshot.lock() {
        if snapshot.session_id == cursor_session && snapshot.phase == live_preview::Phase::Recording
        {
            cursor_preview::show(&app, cursor_session);
        }
    }
    cursor_guard.keep_visible = true;
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
            let _ = app.emit(
                "audio-level",
                AudioLevelEvent {
                    peak: 0.0,
                    rms: 0.0,
                },
            );
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
async fn stop_and_transcribe(app: AppHandle) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || {
        perform_stop_and_transcribe(&app, &app.state::<AppServices>())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// The full dictation commit pipeline. Free of the command layer so both the
/// `stop_and_transcribe` command and the max-duration watchdog can run it.
fn perform_stop_and_transcribe(app: &AppHandle, services: &AppServices) -> Result<String, String> {
    let _operation = services
        .operation
        .try_lock()
        .map_err(|_| "Dictation is already finishing".to_string())?;
    invalidate_previews(services);
    let cursor_session = {
        let mut snapshot = services.cursor_snapshot.lock().map_err(|e| e.to_string())?;
        if snapshot.phase == live_preview::Phase::Recording {
            snapshot.finishing();
            let _ = app.emit("cursor-preview-state", &*snapshot);
        }
        snapshot.session_id
    };
    let mut cursor_guard = CursorSessionGuard::new(app, cursor_session);

    let trace = services.debug_trace.lock().ok().and_then(|mut t| t.take());
    let settings = services
        .settings
        .lock()
        .map_err(|_| "Settings service lock failed".to_string())?
        .current();
    // Re-apply this session's screen-harvested vocabulary (set at recording
    // start) so the finish path sees the same hints the session started with.
    let settings = settings.with_session_vocabulary(
        services
            .session_vocabulary
            .lock()
            .map(|terms| terms.clone())
            .unwrap_or_default(),
    );
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
    let released_at = std::time::Instant::now();
    let raw_transcription = finish_transcription_raw(app, services, &recording, &settings)?;
    let transcribe_ms = released_at.elapsed().as_millis() as u64;
    let raw_transcript = raw_transcription.text;
    update_cursor_snapshot(app, |s| {
        s.full_text(cursor_session, &raw_transcript, false, false)
    });
    if let Some(trace) = &trace {
        trace.event("full-transcription", serde_json::json!({"text":raw_transcript,"droppedStreamFrames":stats.dropped_stream_frames,"backend":raw_transcription.backend}));
    }

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

    // Context awareness Phases A/C: one budgeted read of the focused
    // element's caret situation, used for the polish surrounding text and
    // the paste-time casing/spacing adjustment. Any failure means "no
    // context" and today's exact behavior.
    #[cfg(target_os = "macos")]
    let caret_context = if settings.context_awareness {
        with_ax_timeout(macos_ax::focused_caret_context).flatten()
    } else {
        None
    };
    #[cfg(not(target_os = "macos"))]
    let caret_context: Option<ax_context::CaretContext> = None;

    if let Some(trace) = &trace {
        trace.event("accessibility-caret", serde_json::json!({"enabled":settings.context_awareness,"status":if !settings.context_awareness { "disabled" } else if caret_context.is_some() { "read" } else { "unavailable-excluded-or-timeout" },"before":caret_context.as_ref().map(|c|&c.before),"afterChar":caret_context.as_ref().and_then(|c|c.after_char),"sentToPolish":"Only before is supplied to the final polish pass; afterChar is used for paste spacing."}));
    }
    let caret_before = caret_context.as_ref().map(|c| c.before.as_str());
    // Streaming polish freezes older chunks (silence gaps or a three-chunk
    // window), so the commit pass only covers the volatile tail. A stream that
    // cannot describe the transcript falls back to a whole-text pass.
    let tail_job = services
        .polish_stream
        .lock()
        .ok()
        .and_then(|mut stream| stream.finish_job(cursor_session, &raw_transcript))
        .map(|mut job| {
            // Mid-dictation the sealed prefix is the right lead-in; with
            // nothing sealed yet, fall back to the text around the caret.
            if job.context.is_empty() {
                job.context = caret_before.unwrap_or_default().to_string();
            }
            job
        });
    let polish_input = tail_job
        .as_ref()
        .map(|job| job.raw.clone())
        .unwrap_or_else(|| raw_transcript.clone());
    let final_span = trace
        .as_ref()
        .map(|t| t.span("final", &polish_input, frontmost_app.as_ref()));
    if let Some(trace) = &trace {
        trace.event("final-polish-scope", serde_json::json!({"mode":if tail_job.as_ref().is_some_and(|job| !job.raw.is_empty()) { "tail" } else if tail_job.is_some() { "already-sealed" } else { "whole-transcript" },"tailChars":polish_input.len(),"transcriptChars":raw_transcript.len(),"note":"Streaming polish seals at the last sentence end behind the ASR freeze point (the last silence gap, or everything except the last three ASR chunks)."}));
    }
    set_polish_activity(app, cursor_session, true);
    let polish_started = std::time::Instant::now();
    let decision = match &tail_job {
        Some(job) if job.raw.is_empty() => polish::PolishDecision::Disabled,
        Some(job) => run_polish_pass(
            services,
            job,
            &settings,
            frontmost_app.as_ref(),
            &services.transcription_cancel_requested,
            final_span.as_ref(),
        ),
        None if settings.polish_provider == settings::PolishProvider::Local => {
            services.local_models.polish_traced(
                &raw_transcript,
                &settings,
                frontmost_app.as_ref(),
                caret_before,
                &services.transcription_cancel_requested,
                final_span.as_ref(),
            )
        }
        None => polish::maybe_polish_traced(
            &raw_transcript,
            &settings,
            frontmost_app.as_ref(),
            caret_before,
            final_span.as_ref(),
        ),
    };
    set_polish_activity(app, cursor_session, false);
    let polish_ms = match &decision {
        polish::PolishDecision::Disabled | polish::PolishDecision::Skipped(_) => 0,
        _ => polish_started.elapsed().as_millis() as u64,
    };
    if services
        .transcription_cancel_requested
        .load(Ordering::SeqCst)
    {
        return Err("Transcription was cancelled".into());
    }
    match &decision {
        polish::PolishDecision::Polished(outcome) => emit_backend_event(
            app,
            "info",
            format!(
                "Transcript polished ({}ms, {})",
                outcome.duration_ms, outcome.model
            ),
        ),
        polish::PolishDecision::Unchanged { duration_ms } => emit_backend_event(
            app,
            "info",
            format!("Transcript polish made no changes ({duration_ms}ms)"),
        ),
        polish::PolishDecision::Skipped(reason) => {
            emit_backend_event(app, "info", format!("Polish skipped: {reason}"))
        }
        polish::PolishDecision::Failed(reason) => {
            emit_backend_event(app, "warning", format!("Polish skipped: {reason}"))
        }
        polish::PolishDecision::Disabled => {}
    }
    let (transcript_for_rules, polished) = match &tail_job {
        // Sealing the commit pass yields the whole transcript: the polished
        // prefix plus this tail. The dictation counts as polished when any
        // pass in the session actually rewrote something, not just this one.
        Some(job) => {
            let text = pass_text(&decision, job, true);
            services
                .polish_stream
                .lock()
                .ok()
                .and_then(|mut stream| {
                    stream
                        .apply(cursor_session, job, &text)
                        .then(|| (stream.composed(), stream.changed()))
                })
                .unwrap_or_else(|| (raw_transcript.clone(), false))
        }
        None => match decision {
            polish::PolishDecision::Polished(outcome) => (outcome.text, true),
            _ => (raw_transcript.clone(), false),
        },
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

    if services
        .transcription_cancel_requested
        .load(Ordering::SeqCst)
    {
        return Err("Transcription was cancelled".into());
    }
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
            // "Undo AI edit" must yield what the user would have gotten with
            // polish OFF — the raw transcript WITH their deterministic rules
            // applied — not the bare ASR output (which would silently undo
            // corrections the AI never made).
            raw_text: polished
                .then(|| apply_transcript_post_processing(&raw_transcript, &settings).text),
            timings: Some(transcript_history::DictationTimings {
                transcribe_ms,
                // Only the local chunk worker keeps this counter.
                speech_model_ms: if settings.transcription_location == TranscriptionLocation::Local {
                    transcription::speech_model_busy_ms()
                } else {
                    0
                },
                polish_ms,
                total_ms: released_at.elapsed().as_millis() as u64,
            }),
        })?;

    #[cfg(target_os = "macos")]
    let will_insert_at_cursor = settings.insert_at_cursor && !transcript.is_empty();

    // Context awareness Phase A: when we are about to paste and the caret
    // situation is known, adapt leading capitalization and spacing to it.
    // Otherwise the clipboard text is byte-identical to today's behavior.
    #[cfg(target_os = "macos")]
    let clipboard_text = match caret_context.as_ref() {
        Some(context) if will_insert_at_cursor => {
            ax_context::adjust_for_caret(&transcript, context, &settings.vocabulary_hints)
        }
        _ => transcript_clipboard_text(&transcript),
    };
    #[cfg(not(target_os = "macos"))]
    let clipboard_text = transcript_clipboard_text(&transcript);

    if services
        .transcription_cancel_requested
        .load(Ordering::SeqCst)
    {
        return Err("Transcription was cancelled".into());
    }
    services.clipboard.write_text(&clipboard_text)?;
    let _ = app.emit(
        "transcript-history-updated",
        TranscriptHistoryUpdatedEvent {
            item: stored_item.clone(),
        },
    );

    #[cfg(target_os = "macos")]
    {
        if will_insert_at_cursor && !cursor_preview::is_claimed(cursor_session) {
            deliver_transcript_at_cursor(app, &clipboard_text, settings.accessibility_insert);
        } else {
            emit_backend_event(
                app,
                "info",
                if cursor_preview::is_claimed(cursor_session) {
                    "Insert skipped because the preview was claimed for editing; transcript is on the clipboard"
                        .to_string()
                } else {
                    "Transcript copied to clipboard".to_string()
                },
            );
        }
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
    // Zero-edit metric: register the completion so later edit signals (undo,
    // delete, new correction, quick re-dictation) can attribute to it.
    if let Ok(mut usage_stats) = services.usage_stats.lock() {
        let _ = usage_stats.record_dictation_completed(
            &stored_item.id,
            polished,
            transcript.chars().count() as u32,
            current_epoch_seconds(),
        );
    }

    // History notification precedes insertion and the stats write. Refresh
    // dashboards only after this dictation is included in the durable totals.
    let _ = app.emit("usage-stats-updated", ());

    // Save to the durable notes library (also best-effort), then notify the
    // home window so the Notes screen refreshes live.
    match save_note(services, transcript.clone(), stats.duration_seconds) {
        Ok(note) => {
            if let Some(trace) = &trace {
                let metadata = trace.finish(
                    &raw_transcript,
                    &transcript_for_rules,
                    &transcript,
                    &clipboard_text,
                );
                let saved = services
                    .notes
                    .lock()
                    .map_err(|e| e.to_string())
                    .and_then(|notes| notes.attach_metadata(&note.id, &metadata));
                if let Err(err) = saved {
                    emit_backend_event(
                        app,
                        "warning",
                        format!("Note saved but debug metadata could not be saved: {err}"),
                    );
                }
            }
            let _ = app.emit("notes-updated", &note);
        }
        Err(err) => emit_backend_event(app, "warning", format!("Could not save note: {err}")),
    }

    update_cursor_snapshot(app, |s| {
        s.full_text(cursor_session, &transcript, true, polished)
    });
    cursor_guard.keep_visible = true;
    let cursor_app = app.clone();
    thread::spawn(move || {
        // A short glance, then hide unless the reader is hovering or has
        // claimed the box to edit. Claim has no deadline; hover lasts until
        // they leave. Accidental hover at complete is ignored by the frontend.
        thread::sleep(CURSOR_PREVIEW_LINGER);
        while cursor_preview::is_claimed(cursor_session)
            || cursor_preview::is_interacting(cursor_session)
        {
            thread::sleep(Duration::from_millis(200));
        }
        hide_cursor_session(&cursor_app, cursor_session);
    });
    Ok(transcript)
}

/// Delivers the freshly copied transcript into whichever app the user is
/// dictating into, via the three-tier pattern (AX insert → ⌘V →
/// AppleScript; see `insertion.rs` for why tiers are chosen by
/// pre-condition and never verified-and-retried). AX insert is only
/// considered when the user opted into `accessibility_insert`. Skipped when one of our
/// own windows is focused (the paste would land in Fairspoken itself);
/// every other failure is surfaced as a backend event because the user is
/// otherwise left wondering why no text appeared.
///
/// The transcript always stays on the clipboard afterwards — paste or no
/// paste — so the user can paste the same dictation into multiple targets.
#[cfg(target_os = "macos")]
fn deliver_transcript_at_cursor(app: &AppHandle, text: &str, accessibility_insert: bool) {
    use insertion::InsertionTier;

    let our_window_focused = app
        .webview_windows()
        .values()
        .any(|window| window.is_focused().unwrap_or(false));
    if our_window_focused {
        emit_backend_event(
            app,
            "info",
            "Insert at cursor skipped while a Fairspoken window is focused; transcript is on the clipboard",
        );
        return;
    }

    let bundle_id = macos_input::frontmost_app().map(|frontmost| frontmost.bundle_id);
    // Without a focused element the tier choice never picks AX insertion.
    let element = if accessibility_insert {
        with_ax_timeout(macos_ax::focused_element_info).flatten()
    } else {
        None
    };
    let tier = insertion::choose_insertion_tier(bundle_id.as_deref(), element.as_ref());

    if tier == InsertionTier::AxInsert {
        let text_for_ax = text.to_string();
        match with_ax_timeout(move || macos_ax::ax_insert_text(&text_for_ax)) {
            Some(Ok(())) => {
                // Success is trusted: no fallback, no verification read-back.
                emit_backend_event(
                    app,
                    "info",
                    format!(
                        "Transcript inserted via {} and kept on the clipboard",
                        InsertionTier::AxInsert.label()
                    ),
                );
                return;
            }
            Some(Err(code)) => {
                // A returned AX error means nothing was inserted — falling
                // through to ⌘V cannot double-insert.
                emit_backend_event(
                    app,
                    "info",
                    format!("AX insertion declined (AXError {code}); using ⌘V"),
                );
            }
            None => {
                // Timeout = unknown state. Falling back could double-insert,
                // so we stop here; the transcript is on the clipboard.
                emit_backend_event(
                    app,
                    "warning",
                    "AX insertion timed out; not retrying to avoid a double paste — transcript is on the clipboard",
                );
                return;
            }
        }
    }

    // Both remaining tiers synthesize ⌘V, which a still-held shortcut
    // modifier would turn into a different keystroke.
    if let Err(err) = macos_input::wait_for_modifier_release(Duration::from_millis(1500)) {
        emit_backend_event(
            app,
            "warning",
            format!("Insert at cursor skipped: {err}; transcript is on the clipboard"),
        );
        return;
    }

    if tier == InsertionTier::AppleScript {
        match applescript_paste_keystroke() {
            Ok(()) => emit_backend_event(
                app,
                "info",
                format!(
                    "Transcript inserted via {} and kept on the clipboard",
                    InsertionTier::AppleScript.label()
                ),
            ),
            Err(err) => emit_backend_event(
                app,
                "warning",
                format!("AppleScript insertion failed ({err}); transcript is on the clipboard"),
            ),
        }
        return;
    }

    match macos_input::paste_clipboard_at_cursor() {
        Ok(()) => emit_backend_event(
            app,
            "info",
            format!(
                "Transcript inserted via {} and kept on the clipboard",
                InsertionTier::CmdV.label()
            ),
        ),
        Err(err) => emit_backend_event(
            app,
            "warning",
            format!("Insert at cursor failed ({err}); transcript is on the clipboard"),
        ),
    }
}

/// Tier 3: a System Events keystroke for apps that ignore HID-posted
/// CGEvents. First use per machine triggers the macOS Automation permission
/// prompt — acceptable because this tier only fires for bundles in
/// `insertion::PREFER_APPLESCRIPT`, which starts empty.
#[cfg(target_os = "macos")]
fn applescript_paste_keystroke() -> Result<(), String> {
    let output = std::process::Command::new("osascript")
        .arg("-e")
        .arg(r#"tell application "System Events" to keystroke "v" using command down"#)
        .output()
        .map_err(|err| format!("could not run osascript: {err}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
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
                "{} audio frames were dropped from live streaming; {}",
                stats.dropped_stream_frames,
                if settings.transcription_location == TranscriptionLocation::Local {
                    "the final pass will use the complete recording"
                } else {
                    "the remote transcript may be missing words"
                }
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
    let _operation = services
        .operation
        .try_lock()
        .map_err(|_| "Dictation is already finishing".to_string())?;
    invalidate_previews(&services);
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

fn save_note(services: &AppServices, text: String, duration_seconds: f32) -> Result<Note, String> {
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

/// Dev-only spike command (context-awareness-ax Phase 0): dump what the AX
/// tree exposes for the currently focused app into the backend event log, so
/// per-app coverage (native vs Electron vs web areas vs terminals) can be
/// recorded without a debugger. Reads are budgeted like the real feature.
#[tauri::command]
fn debug_dump_ax_context(app: AppHandle) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        match with_ax_timeout(macos_ax::focused_element_debug).flatten() {
            Some(debug) => {
                emit_backend_event(
                    &app,
                    "info",
                    format!(
                        "AX spike [{}]: role={:?}, value={} chars, selectedRange={:?}",
                        debug.bundle_id,
                        debug.role.as_deref().unwrap_or("(none)"),
                        debug
                            .value_chars
                            .map(|chars| chars.to_string())
                            .unwrap_or_else(|| "(none)".to_string()),
                        debug.selected_range,
                    ),
                );
            }
            None => emit_backend_event(&app, "warning", "AX spike: no focused element readable"),
        }

        match with_ax_timeout(macos_ax::focused_caret_context).flatten() {
            Some(context) => emit_backend_event(
                &app,
                "info",
                format!(
                    "AX spike caret: {} chars before caret, after={:?}",
                    context.before.chars().count(),
                    context.after_char,
                ),
            ),
            None => emit_backend_event(&app, "info", "AX spike caret: no caret context"),
        }

        let terms = with_ax_timeout(macos_ax::harvest_screen_vocabulary)
            .flatten()
            .unwrap_or_default();
        emit_backend_event(
            &app,
            "info",
            format!(
                "AX spike harvest ({} terms): {}",
                terms.len(),
                terms.join(", ")
            ),
        );
        Ok(())
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = app;
        Err("Context awareness is macOS-only".to_string())
    }
}

#[tauri::command]
fn cancel_transcription(app: AppHandle, services: State<'_, AppServices>) -> Result<(), String> {
    services
        .transcription_cancel_requested
        .store(true, Ordering::SeqCst);
    invalidate_previews(&services);
    let cursor_session = services
        .cursor_snapshot
        .lock()
        .map(|s| s.session_id)
        .unwrap_or(0);
    hide_cursor_session(&app, cursor_session);
    services.local_models.unload();
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

/// Run an Accessibility read on an abandoned thread with a hard timeout: AX
/// calls are cross-process IPC and can hang; recording start and paste must
/// never wait on a wedged app. A timed-out thread is left to finish (or hang)
/// on its own and its result is dropped.
#[cfg(target_os = "macos")]
fn with_ax_timeout<T: Send + 'static>(read: impl FnOnce() -> T + Send + 'static) -> Option<T> {
    let (tx, rx) = mpsc::channel();
    thread::Builder::new()
        .name("ax-context-read".to_string())
        .spawn(move || {
            let _ = tx.send(read());
        })
        .ok()?;
    rx.recv_timeout(Duration::from_millis(200)).ok()
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

#[tauri::command]
fn set_cursor_preview_size(
    app: AppHandle,
    session_id: u64,
    height: f64,
    dark: Option<bool>,
    services: State<'_, AppServices>,
) -> Result<(), String> {
    if !height.is_finite() {
        return Err("Invalid preview height".into());
    }
    let snapshot = services
        .cursor_snapshot
        .lock()
        .map_err(|_| "Cursor preview lock failed".to_string())?;
    if snapshot.session_id == session_id && snapshot.phase != live_preview::Phase::Idle {
        cursor_preview::resize(&app, session_id, height, dark.unwrap_or(false));
    }
    Ok(())
}

#[tauri::command]
fn set_cursor_preview_interacting(
    session_id: u64,
    active: bool,
    services: State<'_, AppServices>,
) -> Result<(), String> {
    let snapshot = services
        .cursor_snapshot
        .lock()
        .map_err(|_| "Cursor preview lock failed".to_string())?;
    if snapshot.session_id == session_id && snapshot.phase != live_preview::Phase::Idle {
        cursor_preview::set_interacting(session_id, active);
    }
    Ok(())
}

#[tauri::command]
fn set_cursor_preview_claimed(
    app: AppHandle,
    session_id: u64,
    claimed: bool,
    services: State<'_, AppServices>,
) -> Result<(), String> {
    let snapshot = services
        .cursor_snapshot
        .lock()
        .map_err(|_| "Cursor preview lock failed".to_string())?;
    if snapshot.session_id == session_id && snapshot.phase != live_preview::Phase::Idle {
        cursor_preview::set_claimed(&app, session_id, claimed);
        if claimed {
            cursor_preview::set_interacting(session_id, true);
        }
    }
    Ok(())
}

#[tauri::command]
fn copy_cursor_preview_text(text: String, services: State<'_, AppServices>) -> Result<(), String> {
    services.clipboard.write_text(&text)
}

#[tauri::command]
fn get_cursor_preview_state(
    services: State<'_, AppServices>,
) -> Result<live_preview::Snapshot, String> {
    services
        .cursor_snapshot
        .lock()
        .map(|s| s.clone())
        .map_err(|e| e.to_string())
}
fn update_cursor_snapshot(
    app: &AppHandle,
    update: impl FnOnce(&mut live_preview::Snapshot) -> bool,
) {
    if let Ok(mut snapshot) = app.state::<AppServices>().cursor_snapshot.lock() {
        if update(&mut snapshot) {
            let _ = app.emit("cursor-preview-state", &*snapshot);
        }
    }
}
fn hide_cursor_session(app: &AppHandle, session: u64) {
    if let Ok(mut snapshot) = app.state::<AppServices>().cursor_snapshot.lock() {
        if snapshot.hide(session) {
            let _ = app.emit("cursor-preview-state", &*snapshot);
            cursor_preview::hide(app);
        }
    }
}
fn emit_transcript_preview(app: &AppHandle, preview: TranscriptPreviewEvent) {
    update_cursor_snapshot(app, |s| {
        s.preview(
            preview.session_id,
            preview.revision,
            &preview.text,
            preview.polished,
        )
    });
    let _ = app.emit("transcript-preview", preview);
}
struct CursorSessionGuard {
    app: AppHandle,
    session: u64,
    keep_visible: bool,
}
impl CursorSessionGuard {
    fn new(app: &AppHandle, session: u64) -> Self {
        Self {
            app: app.clone(),
            session,
            keep_visible: false,
        }
    }
}
impl Drop for CursorSessionGuard {
    fn drop(&mut self) {
        if !self.keep_visible {
            hide_cursor_session(&self.app, self.session);
        }
    }
}

fn invalidate_previews(services: &AppServices) {
    if let Ok(_gate) = services.preview_gate.lock() {
        services.preview_generation.fetch_add(1, Ordering::SeqCst);
        if let Ok(mut cancel) = services.preview_cancel.lock() {
            if let Some(cancel) = cancel.take() {
                cancel.store(true, Ordering::SeqCst);
            }
        }
    }
}

/// Runs one polish pass over a tail, through whichever provider is configured.
/// The sealed prefix is supplied as surrounding context so the model continues
/// the sentence instead of re-opening one.
fn run_polish_pass(
    services: &AppServices,
    job: &polish_stream::TailJob,
    settings: &Settings,
    target: Option<&polish::PolishTargetApp>,
    cancel: &AtomicBool,
    span: Option<&note_debug::Span>,
) -> polish::PolishDecision {
    let context = (!job.context.is_empty()).then_some(job.context.as_str());
    if settings.polish_provider == settings::PolishProvider::Local {
        services
            .local_models
            .polish_traced(&job.raw, settings, target, context, cancel, span)
    } else {
        polish::maybe_polish_traced(&job.raw, settings, target, context, span)
    }
}

/// What a pass contributes to the stream. Anything short of polished output —
/// a guardrail trip, a skip, a timeout — contributes the raw tail, so the
/// composed transcript always covers every word and polish failure stays
/// invisible to the dictation, exactly as the one-shot pass behaved.
///
/// A tail is a fragment: it may continue the sealed text mid-sentence and,
/// until release (`is_final`), may stop mid-sentence too. The model polishes
/// it as if it were whole, so its edges are repaired here.
fn pass_text(decision: &polish::PolishDecision, job: &polish_stream::TailJob, is_final: bool) -> String {
    match decision {
        polish::PolishDecision::Polished(outcome) => transcript_cleanup::repair_fragment_edges(
            &job.raw,
            &outcome.text,
            &job.context,
            is_final,
        ),
        _ => job.raw.clone(),
    }
}

/// Reports polish activity on the preview so the box can show that its text is
/// still moving.
fn set_polish_activity(app: &AppHandle, session: u64, active: bool) {
    update_cursor_snapshot(app, |s| s.set_polishing(session, active));
}

fn start_transcript_preview_forwarder(
    app: AppHandle,
    settings: Settings,
    trace: Option<note_debug::Trace>,
) -> TranscriptionPreviewSender {
    let generation = app
        .state::<AppServices>()
        .preview_generation
        .load(Ordering::SeqCst);
    let preview_cancel = Arc::new(AtomicBool::new(false));
    if let Ok(mut current) = app.state::<AppServices>().preview_cancel.lock() {
        *current = Some(preview_cancel.clone());
    }
    let (tx, rx) = mpsc::channel::<TranscriptPreview>();
    // One current inference plus one replaceable pending revision, never a backlog.
    let pending = Arc::new(preview::LatestPreview::default());
    // Both providers stream now: there is no whole-transcript pass at release
    // to fall back on, so whatever polish the user gets is earned here.
    if settings.polish_enabled {
        let app = app.clone();
        let pending = pending.clone();
        let trace = trace.clone();
        thread::spawn(move || {
            #[cfg(target_os = "macos")]
            let target = macos_input::frontmost_app().map(|a| polish::PolishTargetApp {
                bundle_id: a.bundle_id,
                name: a.name,
            });
            #[cfg(not(target_os = "macos"))]
            let target: Option<polish::PolishTargetApp> = None;
            if settings.polish_provider == settings::PolishProvider::Local {
                // Load cleanup weights alongside capture and ASR initialization.
                let _ = app
                    .state::<AppServices>()
                    .local_models
                    .warm(&settings.polish_model, &preview_cancel);
            }
            loop {
                let services = app.state::<AppServices>();
                if services.preview_generation.load(Ordering::SeqCst) != generation {
                    break;
                }
                let Some((rev, preview)) = pending.take() else {
                    set_polish_activity(&app, generation, false);
                    thread::sleep(Duration::from_millis(100));
                    continue;
                };
                // Seal newly frozen chunks first, then polish the volatile
                // window — at most two bounded passes per preview.
                loop {
                    if services.preview_generation.load(Ordering::SeqCst) != generation {
                        break;
                    }
                    let job = services.polish_stream.lock().ok().and_then(|mut stream| {
                        stream.next_job(generation, &preview.text, preview.sealed_len)
                    });
                    let Some(job) = job else {
                        set_polish_activity(&app, generation, false);
                        break;
                    };
                    set_polish_activity(&app, generation, true);
                    let span = trace
                        .as_ref()
                        .map(|t| t.span(&format!("preview-{rev}"), &job.raw, target.as_ref()));
                    let result = run_polish_pass(
                        &services,
                        &job,
                        &settings,
                        target.as_ref(),
                        &preview_cancel,
                        span.as_ref(),
                    );
                    if let Some(trace) = &trace {
                        trace.event("preview-disposition", serde_json::json!({"revision":rev,"asrChunkIndex":preview.index,"tailChars":job.raw.len(),"sealedAfter":job.seal,"current":services.preview_generation.load(Ordering::SeqCst) == generation && pending.is_current(rev),"hasPolishedOutput":matches!(&result,polish::PolishDecision::Polished(_))}));
                    }
                    if matches!(result, polish::PolishDecision::Disabled) {
                        set_polish_activity(&app, generation, false);
                        break;
                    }
                    let text = pass_text(&result, &job, false);
                    let sealed = job.seal;
                    if let Ok(_gate) = services.preview_gate.lock() {
                        if services.preview_generation.load(Ordering::SeqCst) != generation {
                            break;
                        }
                        // A superseded revision still seals its work — that is
                        // what keeps the tail short — but only the newest pass is
                        // allowed to repaint the box.
                        let composed = services
                            .polish_stream
                            .lock()
                            .ok()
                            .and_then(|mut stream| {
                                stream
                                    .apply(generation, &job, &text)
                                    .then(|| stream.composed())
                            })
                            .filter(|_| pending.is_current(rev));
                        if let Some(composed) = composed {
                            emit_transcript_preview(
                                &app,
                                TranscriptPreviewEvent {
                                    session_id: generation,
                                    revision: rev,
                                    polished: true,
                                    index: preview.index,
                                    text: composed,
                                    final_preview: false,
                                },
                            );
                        }
                    };
                    if !sealed || !pending.is_current(rev) {
                        break;
                    }
                }
            }
            set_polish_activity(&app, generation, false);
        });
    }

    thread::spawn(move || {
        for preview in rx {
            let services = app.state::<AppServices>();
            let Ok(_gate) = services.preview_gate.lock() else {
                break;
            };
            if services.preview_generation.load(Ordering::SeqCst) != generation {
                break;
            }
            let rev = pending.submit(preview.clone());
            if let Ok(mut stream) = services.polish_stream.lock() {
                stream.observe(generation, preview.sealed_len);
            }
            if let Some(trace) = &trace {
                trace.event("transcript-preview", serde_json::json!({"revision":rev,"asrChunkIndex":preview.index,"text":preview.text,"sealedLen":preview.sealed_len,"finalPreview":preview.final_preview,"note":"Cumulative merged raw text; sealedLen is the frozen prefix (silence gap or all but the last three chunks). A preview without a matching polish-start was coalesced or finalization began before it ran."}));
            }
            emit_transcript_preview(
                &app,
                TranscriptPreviewEvent {
                    session_id: generation,
                    revision: rev,
                    polished: false,
                    index: preview.index,
                    text: preview.text.clone(),
                    final_preview: preview.final_preview,
                },
            );
        }
    });
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
            services.local_models.unload_if_idle();
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
    // Before any service below opens its files.
    app_dirs::migrate_legacy_dirs();
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _, _| {
            let _ = open_home_window(app.clone(), None);
        }))
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
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
            debug_capture_enabled: AtomicBool::new(false),
            debug_trace: Mutex::new(None),
            operation: Mutex::new(()),
            preview_generation: AtomicU64::new(0),
            preview_gate: Mutex::new(()),
            cursor_snapshot: Mutex::new(live_preview::Snapshot::default()),
            preview_cancel: Mutex::new(None),
            local_models: local_models::LocalModels::default(),
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
            polish_stream: Mutex::new(polish_stream::PolishStream::default()),
            session_vocabulary: Mutex::new(Vec::new()),
            transcript_shelf_positioned: AtomicBool::new(false),
            #[cfg(target_os = "macos")]
            fn_push_to_talk_enabled: Arc::new(AtomicBool::new(false)),
            #[cfg(target_os = "macos")]
            fn_push_to_talk_tap_started: AtomicBool::new(false),
        })
        .manage(updates::Updates::default())
        .setup(|app| {
            let handle = app.handle();
            if let Err(err) = layout_pill_window(handle.clone(), "idle".to_string()) {
                emit_backend_event(handle, "warning", format!("Could not position pill: {err}"));
            }
            start_note_retention_sweeper(handle.clone());
            updates::start(handle.clone());
            let _ = open_home_window(handle.clone(), None);
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
            get_cursor_preview_state,
            set_cursor_preview_interacting,
            set_cursor_preview_claimed,
            copy_cursor_preview_text,
            get_note_debug_status,
            set_note_debug_capture,
            get_note_metadata,
            get_local_model_catalog,
            get_dictation_models,
            search_hugging_face_models,
            set_cursor_preview_size,
            download_local_model,
            cancel_local_model_download,
            remove_local_model,
            get_app_status,
            get_settings,
            get_cloud_available,
            save_settings,
            save_shortcut_settings,
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
            debug_dump_ax_context,
            preview_interaction_sound,
            stop_speed_test_capture,
            stop_voice_test_capture,
            save_speed_test_result,
            get_speed_test_summary,
            set_measured_typing_wpm,
            get_update_status,
            check_for_updates,
            install_update,
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app, event| match event {
            #[cfg(target_os = "macos")]
            tauri::RunEvent::Reopen { .. } => {
                let _ = open_home_window(app.clone(), None);
            }
            tauri::RunEvent::Exit => {
                let services = app.state::<AppServices>();
                services.local_models.cancel_download();
                services
                    .transcription_cancel_requested
                    .store(true, Ordering::SeqCst);
                invalidate_previews(&services);
                services.local_models.unload();
            }
            _ => {}
        });
}

#[cfg(test)]
mod shortcut_patch_tests {
    use super::*;
    #[test]
    fn shortcut_patch_preserves_latest_unrelated_preferences() {
        let current = Settings {
            interaction_sounds: false,
            recording_shortcut: "Control+K".into(),
            ..Settings::default()
        };
        let patched = merge_settings_shortcuts(
            None,
            Some(ShortcutPatch {
                recording_shortcut: None,
                transcript_stack_shortcut: Some("Control+L".into()),
            }),
            &current,
        );
        assert!(!patched.interaction_sounds);
        assert_eq!(patched.recording_shortcut, "Control+K");
        assert_eq!(patched.transcript_stack_shortcut, "Control+L");
    }
    #[test]
    fn ordinary_stale_save_cannot_revert_registered_chords() {
        let current = Settings {
            recording_shortcut: "Control+K".into(),
            transcript_stack_shortcut: "Control+L".into(),
            ..Settings::default()
        };
        let stale = Settings {
            interaction_sounds: false,
            ..Settings::default()
        };
        let merged = merge_settings_shortcuts(Some(stale), None, &current);
        assert_eq!(merged.recording_shortcut, current.recording_shortcut);
        assert_eq!(
            merged.transcript_stack_shortcut,
            current.transcript_stack_shortcut
        );
        assert!(!merged.interaction_sounds);
    }
}
