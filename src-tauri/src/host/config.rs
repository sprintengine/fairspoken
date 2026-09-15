use crate::models::SttModel;
use serde::{Deserialize, Serialize};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Mutex, MutexGuard};

pub(super) const DEFAULT_HOST_WORKERS: usize = 1;
pub(super) const DEFAULT_HOST_QUEUE_CAPACITY: usize = 8;
pub(super) const DEFAULT_HOST_MAX_ACTIVE_STREAMS: u32 = 4;
// Matches the longest client max-recording option so the client setting is
// the effective limit; this also sizes the host's upload-buffer bounds.
pub(super) const DEFAULT_HOST_MAX_RECORDING_SECONDS: u16 = 600;
pub(super) const MIN_HOST_MAX_ACTIVE_STREAMS: u32 = 1;
pub(super) const MAX_HOST_MAX_ACTIVE_STREAMS: u32 = 32;
pub(super) const MIN_HOST_MAX_RECORDING_SECONDS: u16 = 10;
pub(super) const MAX_HOST_MAX_RECORDING_SECONDS: u16 = 600;
pub(super) const DEFAULT_HOST_MODEL: SttModel = SttModel::Parakeet;

#[derive(Clone)]
pub(super) struct HostRuntimeConfig {
    pub(super) worker_count: usize,
    pub(super) queue_capacity: usize,
    pub(super) max_active_streams: u32,
    pub(super) max_recording_seconds: u16,
    pub(super) use_gpu: bool,
    pub(super) worker_models: Vec<SttModel>,
}

impl HostRuntimeConfig {
    pub(super) fn from_env() -> Result<Self, String> {
        let worker_count = env_usize("MULTIVOICE_HOST_WORKERS", DEFAULT_HOST_WORKERS).clamp(1, 4);
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
pub(super) fn parse_worker_models(
    raw: Option<&str>,
    worker_count: usize,
) -> Result<Vec<SttModel>, String> {
    let Some(raw) = raw.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(vec![DEFAULT_HOST_MODEL; worker_count]);
    };
    let models = raw
        .split(',')
        .map(|id| {
            let id = id.trim();
            SttModel::from_model_id(id)
                .ok_or_else(|| format!("MULTIVOICE_HOST_MODEL has an unsupported model: {id}"))
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
pub(super) struct PersistedHostConfig {
    pub(super) max_active_streams: u32,
    pub(super) max_recording_seconds: u16,
    pub(super) use_gpu: bool,
    pub(super) worker_models: Vec<SttModel>,
}

pub(super) fn default_host_config_path() -> PathBuf {
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
pub(super) fn load_persisted_config(path: &Path) -> Result<Option<PersistedHostConfig>, String> {
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
pub(super) fn overlay_persisted_config(
    config: &mut HostRuntimeConfig,
    persisted: PersistedHostConfig,
) {
    config.max_active_streams = persisted
        .max_active_streams
        .clamp(MIN_HOST_MAX_ACTIVE_STREAMS, MAX_HOST_MAX_ACTIVE_STREAMS);
    config.max_recording_seconds = persisted.max_recording_seconds.clamp(
        MIN_HOST_MAX_RECORDING_SECONDS,
        MAX_HOST_MAX_RECORDING_SECONDS,
    );
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

pub(super) fn persist_live_config(path: &Path, live: &HostLiveConfig) -> Result<(), String> {
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
pub(super) struct HostLiveConfig {
    pub(super) max_active_streams: AtomicU32,
    pub(super) max_recording_seconds: AtomicU32,
    pub(super) use_gpu: AtomicBool,
    worker_models: Mutex<Vec<SttModel>>,
}

impl HostLiveConfig {
    pub(super) fn new(config: &HostRuntimeConfig) -> Self {
        Self {
            max_active_streams: AtomicU32::new(config.max_active_streams),
            max_recording_seconds: AtomicU32::new(u32::from(config.max_recording_seconds)),
            use_gpu: AtomicBool::new(config.use_gpu),
            worker_models: Mutex::new(config.worker_models.clone()),
        }
    }

    pub(super) fn max_active_streams(&self) -> u32 {
        self.max_active_streams.load(Ordering::Relaxed)
    }

    pub(super) fn max_recording_seconds(&self) -> u16 {
        self.max_recording_seconds.load(Ordering::Relaxed) as u16
    }

    pub(super) fn use_gpu(&self) -> bool {
        self.use_gpu.load(Ordering::Relaxed)
    }

    pub(super) fn worker_models_lock(&self) -> MutexGuard<'_, Vec<SttModel>> {
        // The critical sections only read or swap the Vec, so a poisoned lock
        // still holds a usable value.
        self.worker_models
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub(super) fn worker_model(&self, worker_index: usize) -> SttModel {
        self.worker_models_lock()
            .get(worker_index)
            .copied()
            .unwrap_or(DEFAULT_HOST_MODEL)
    }

    pub(super) fn worker_models(&self) -> Vec<SttModel> {
        self.worker_models_lock().clone()
    }

    pub(super) fn set_worker_models(&self, next: Vec<SttModel>) {
        *self.worker_models_lock() = next;
    }

    /// One model id when every worker serves the same model, otherwise
    /// "mixed" — used where a job cannot know which worker will run it.
    pub(super) fn model_summary(&self) -> String {
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
pub(super) struct HostConfigUpdate {
    pub(super) max_active_streams: Option<u32>,
    pub(super) max_recording_seconds: Option<u16>,
    pub(super) use_gpu: Option<bool>,
    /// One model id applied to every worker.
    pub(super) model: Option<String>,
    /// One model id per worker; length must match the worker count.
    pub(super) worker_models: Option<Vec<String>>,
}

/// Validates every supplied field before applying any of them, so a rejected
/// update never partially changes the host configuration.
pub(super) fn apply_config_update(
    update: &HostConfigUpdate,
    live: &HostLiveConfig,
) -> Result<(), String> {
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
            let model =
                SttModel::from_model_id(id).ok_or_else(|| format!("Unsupported model: {id}"))?;
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
                        SttModel::from_model_id(id)
                            .ok_or_else(|| format!("Unsupported model: {id}"))
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

pub(super) fn parse_bool(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" => Some(true),
        "0" | "false" => Some(false),
        _ => None,
    }
}
