use super::pairing::{deserialize_password_change, validate_pairing_password, HostAuth};
use super::lock_unpoisoned;
use super::update::UpdatePrefs;
use crate::model_source::ModelSource;
use crate::models::SttModel;
use serde::{Deserialize, Serialize};
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
/// Where the host downloads models from (`model_source`), below
/// `--model-source` and above the config file's `modelSource`.
pub(super) const MODEL_SOURCE_ENV: &str = "FAIRSPOKEN_MODEL_SOURCE";

#[derive(Clone)]
pub(super) struct HostRuntimeConfig {
    pub(super) worker_count: usize,
    pub(super) queue_capacity: usize,
    pub(super) max_active_streams: u32,
    pub(super) max_recording_seconds: u16,
    pub(super) use_gpu: bool,
    pub(super) worker_models: Vec<SttModel>,
    pub(super) super_mode: super::super_mode::SuperModePolicy,
}

impl HostRuntimeConfig {
    pub(super) fn from_env() -> Result<Self, String> {
        let worker_count = env_usize("FAIRSPOKEN_HOST_WORKERS", DEFAULT_HOST_WORKERS).clamp(1, 4);
        let worker_models = parse_worker_models(
            crate::app_dirs::env_var("FAIRSPOKEN_HOST_MODEL").as_deref(),
            worker_count,
        )?;
        Ok(Self {
            worker_count,
            queue_capacity: env_usize(
                "FAIRSPOKEN_HOST_QUEUE_CAPACITY",
                DEFAULT_HOST_QUEUE_CAPACITY,
            )
            .clamp(1, 64),
            max_active_streams: env_u32(
                "FAIRSPOKEN_HOST_MAX_ACTIVE_STREAMS",
                DEFAULT_HOST_MAX_ACTIVE_STREAMS,
            )
            .clamp(MIN_HOST_MAX_ACTIVE_STREAMS, MAX_HOST_MAX_ACTIVE_STREAMS),
            max_recording_seconds: env_u16(
                "FAIRSPOKEN_HOST_MAX_RECORDING_SECONDS",
                DEFAULT_HOST_MAX_RECORDING_SECONDS,
            )
            .clamp(
                MIN_HOST_MAX_RECORDING_SECONDS,
                MAX_HOST_MAX_RECORDING_SECONDS,
            ),
            use_gpu: env_bool("FAIRSPOKEN_HOST_USE_GPU", true),
            worker_models,
            super_mode: super::super_mode::SuperModePolicy::from_env()?,
        })
    }
}

/// Parses `FAIRSPOKEN_HOST_MODEL`: a single model id serves on every worker,
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
                .ok_or_else(|| format!("FAIRSPOKEN_HOST_MODEL has an unsupported model: {id}"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    if models.len() == 1 {
        return Ok(vec![models[0]; worker_count]);
    }
    if models.len() != worker_count {
        return Err(format!(
            "FAIRSPOKEN_HOST_MODEL lists {} models for {worker_count} workers; provide one model or exactly one per worker",
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
    /// `updateChannel` / `autoUpdate`, edited from the dashboard's Updates
    /// panel or `--set-update-channel`.
    #[serde(flatten)]
    pub(super) update: UpdatePrefs,
    /// The host token when it lives in this file (generated for pairing, or
    /// written by the Swift host, which shares the format). An env token is
    /// never saved here. Empty means none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) token: Option<String>,
    /// Edited from the dashboard or `--set-pairing-password`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) pairing_password: Option<String>,
    /// Display name in `/v1/hello`; `FAIRSPOKEN_HOST_NAME` overrides it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) name: Option<String>,
    /// `"allow"` or `"off"`; absent in files written before super mode, which
    /// then keep the environment's value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) super_mode: Option<super::super_mode::SuperModePolicy>,
    /// Where models download from (`model_source`); absent is Hugging Face.
    /// `--model-source` and `FAIRSPOKEN_MODEL_SOURCE` override it. The same
    /// key the Swift host reads.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) model_source: Option<String>,
}

impl PersistedHostConfig {
    /// The file a first write creates: live settings seeded from the
    /// environment, as the host would on its first boot.
    pub(super) fn seeded(config: HostRuntimeConfig) -> Self {
        Self {
            max_active_streams: config.max_active_streams,
            max_recording_seconds: config.max_recording_seconds,
            use_gpu: config.use_gpu,
            worker_models: config.worker_models,
            update: UpdatePrefs::default(),
            token: None,
            pairing_password: None,
            name: None,
            super_mode: None,
            model_source: None,
        }
    }
}

/// Where the host's models come from, and which setting chose it: the
/// command line, then the environment, then the config file, then Hugging
/// Face. A blank environment variable counts as unset; an explicit
/// `--model-source ""` chooses Hugging Face. Invalid values fail startup,
/// like any other operator configuration, naming where they came from.
pub(super) fn resolve_model_source(
    cli: Option<&str>,
    env: Option<&str>,
    file: Option<&str>,
    config_path: &Path,
) -> Result<(ModelSource, String), String> {
    let (raw, origin) = if let Some(cli) = cli {
        (cli, "--model-source".to_string())
    } else if let Some(env) = env.filter(|value| !value.trim().is_empty()) {
        (env, MODEL_SOURCE_ENV.to_string())
    } else if let Some(file) = file.filter(|value| !value.trim().is_empty()) {
        (file, format!("modelSource in {}", config_path.display()))
    } else {
        return Ok((ModelSource::HuggingFace, "default".to_string()));
    };
    ModelSource::parse(raw)
        .map(|source| (source, origin.clone()))
        .map_err(|err| format!("{origin}: {err}"))
}

pub(super) fn default_host_config_path() -> PathBuf {
    if let Some(path) = crate::app_dirs::env_var_os("FAIRSPOKEN_HOST_CONFIG_PATH") {
        return PathBuf::from(path);
    }

    crate::app_dirs::config_dir()
        .map(|dir| dir.join("host-config.json"))
        .unwrap_or_else(|| PathBuf::from("host-config.json"))
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
    if let Some(policy) = persisted.super_mode {
        config.super_mode = policy;
    }
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

/// Serializes this process's config writes (the request handlers, the
/// loopback listener's, the updater's settings and the CLI): each builds its
/// snapshot and writes it under this lock, so the file always ends up
/// holding the latest snapshot, never an older one written last.
static CONFIG_WRITE: Mutex<()> = Mutex::new(());

pub(super) fn persist_live_config(path: &Path, live: &HostLiveConfig) -> Result<(), String> {
    let _writing = lock_unpoisoned(&CONFIG_WRITE);
    let auth = live.auth();
    write_config_file(
        path,
        &PersistedHostConfig {
            max_active_streams: live.max_active_streams(),
            max_recording_seconds: live.max_recording_seconds(),
            use_gpu: live.use_gpu(),
            worker_models: live.worker_models(),
            update: live.update_prefs(),
            token: auth.saved_token,
            pairing_password: auth.saved_pairing_password,
            name: live.saved_name(),
            super_mode: Some(live.super_mode),
            model_source: live.saved_model_source(),
        },
    )
}

pub(super) fn write_persisted_config(
    path: &Path,
    persisted: &PersistedHostConfig,
) -> Result<(), String> {
    let _writing = lock_unpoisoned(&CONFIG_WRITE);
    write_config_file(path, persisted)
}

/// Callers hold `CONFIG_WRITE`.
fn write_config_file(path: &Path, persisted: &PersistedHostConfig) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|err| format!("Failed to create host config directory: {err}"))?;
    }
    let payload = serde_json::to_string_pretty(persisted)
        .map_err(|err| format!("Failed to serialize host config: {err}"))?;
    write_private_file(path, payload.as_bytes())
        .map_err(|err| format!("Failed to write host config: {err}"))
}

/// Replaces the file atomically: the contents go to a temporary file in the
/// same directory, reach the disk, and are renamed over the old file, so a
/// crash or a concurrent reader (the host starting, the Swift host) never
/// sees a truncated or half-written config. The file can hold the token and
/// pairing password, so it is readable by its owner only where the platform
/// supports that.
fn write_private_file(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let file_name = path.file_name().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "config path has no file name")
    })?;
    let mut temp_name = std::ffi::OsString::from(".");
    temp_name.push(file_name);
    temp_name.push(format!(".{}.tmp", std::process::id()));
    let temp = path.with_file_name(temp_name);
    // Left over from a crash, or planted: never write through it.
    let _ = fs::remove_file(&temp);
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let written = options.open(&temp).and_then(|mut file| {
        file.write_all(contents)?;
        file.sync_all()
    });
    if let Err(err) = written.and_then(|()| fs::rename(&temp, path)) {
        let _ = fs::remove_file(&temp);
        return Err(err);
    }
    // Make the rename itself durable; best effort.
    #[cfg(unix)]
    if let Some(dir) = path.parent().filter(|dir| !dir.as_os_str().is_empty()) {
        let _ = fs::File::open(dir).and_then(|dir| dir.sync_all());
    }
    Ok(())
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
    update_prefs: Mutex<UpdatePrefs>,
    auth: Mutex<HostAuth>,
    /// The display name in `/v1/hello`, and the config file's `name` (kept
    /// as found so a rewrite of the file doesn't drop or bake in a default).
    name: Mutex<(String, Option<String>)>,
    /// The config file's `modelSource`, kept as found so rewriting the file
    /// never drops it or bakes in a command-line or environment value.
    saved_model_source: Mutex<Option<String>>,
    /// Restart-only: whether clients may get super mode at all.
    pub(super) super_mode: super::super_mode::SuperModePolicy,
}

impl HostLiveConfig {
    pub(super) fn new(config: &HostRuntimeConfig) -> Self {
        Self {
            max_active_streams: AtomicU32::new(config.max_active_streams),
            max_recording_seconds: AtomicU32::new(u32::from(config.max_recording_seconds)),
            use_gpu: AtomicBool::new(config.use_gpu),
            worker_models: Mutex::new(config.worker_models.clone()),
            update_prefs: Mutex::new(UpdatePrefs::default()),
            auth: Mutex::new(HostAuth::default()),
            name: Mutex::new((String::new(), None)),
            saved_model_source: Mutex::new(None),
            super_mode: config.super_mode,
        }
    }

    pub(super) fn auth(&self) -> HostAuth {
        lock_unpoisoned(&self.auth).clone()
    }

    pub(super) fn set_auth(&self, auth: HostAuth) {
        *lock_unpoisoned(&self.auth) = auth;
    }

    pub(super) fn token(&self) -> Option<String> {
        lock_unpoisoned(&self.auth).token.clone()
    }

    pub(super) fn pairing_enabled(&self) -> bool {
        lock_unpoisoned(&self.auth).pairing_enabled()
    }

    pub(super) fn name(&self) -> String {
        lock_unpoisoned(&self.name).0.clone()
    }

    fn saved_name(&self) -> Option<String> {
        lock_unpoisoned(&self.name).1.clone()
    }

    pub(super) fn set_name(&self, display: String, saved: Option<String>) {
        *lock_unpoisoned(&self.name) = (display, saved);
    }

    fn saved_model_source(&self) -> Option<String> {
        lock_unpoisoned(&self.saved_model_source).clone()
    }

    pub(super) fn set_saved_model_source(&self, saved: Option<String>) {
        *lock_unpoisoned(&self.saved_model_source) = saved;
    }

    pub(super) fn update_prefs(&self) -> UpdatePrefs {
        lock_unpoisoned(&self.update_prefs).clone()
    }

    pub(super) fn set_update_prefs(&self, prefs: UpdatePrefs) {
        *lock_unpoisoned(&self.update_prefs) = prefs;
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
        lock_unpoisoned(&self.worker_models)
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
    /// Absent leaves pairing alone; `null` or `""` turns it off.
    #[serde(default, deserialize_with = "deserialize_password_change")]
    pub(super) pairing_password: Option<Option<String>>,
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
    // Generating a token is fallible, so it happens before anything applies.
    let next_auth = match &update.pairing_password {
        Some(password) => {
            let password = password.clone().filter(|password| !password.is_empty());
            if let Some(password) = &password {
                validate_pairing_password(password)?;
            }
            let mut auth = live.auth();
            auth.set_pairing_password(password)?;
            Some(auth)
        }
        None => None,
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
    if let Some(auth) = next_auth {
        live.set_auth(auth);
    }
    Ok(())
}

fn env_usize(name: &str, default: usize) -> usize {
    crate::app_dirs::env_var(name)
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(default)
}

fn env_u32(name: &str, default: u32) -> u32 {
    crate::app_dirs::env_var(name)
        .and_then(|value| value.parse::<u32>().ok())
        .unwrap_or(default)
}

fn env_u16(name: &str, default: u16) -> u16 {
    crate::app_dirs::env_var(name)
        .and_then(|value| value.parse::<u16>().ok())
        .unwrap_or(default)
}

fn env_bool(name: &str, default: bool) -> bool {
    crate::app_dirs::env_var(name)
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

#[cfg(test)]
mod model_source_tests {
    use super::*;

    fn resolve(
        cli: Option<&str>,
        env: Option<&str>,
        file: Option<&str>,
    ) -> Result<(ModelSource, String), String> {
        resolve_model_source(cli, env, file, Path::new("host-config.json"))
    }

    fn mirror(base: &str) -> ModelSource {
        ModelSource::Mirror(base.to_string())
    }

    #[test]
    fn command_line_beats_environment_beats_config_file_beats_default() {
        let (cli, env, file) = (
            Some("https://cli.example"),
            Some("https://env.example/"),
            Some("https://file.example"),
        );
        assert_eq!(
            resolve(cli, env, file).unwrap(),
            (mirror("https://cli.example"), "--model-source".into())
        );
        assert_eq!(
            resolve(None, env, file).unwrap(),
            (mirror("https://env.example"), MODEL_SOURCE_ENV.into())
        );
        assert_eq!(
            resolve(None, None, file).unwrap(),
            (
                mirror("https://file.example"),
                "modelSource in host-config.json".into()
            )
        );
        assert_eq!(
            resolve(None, None, None).unwrap(),
            (ModelSource::HuggingFace, "default".into())
        );
        // A blank variable or file value is unset; a blank flag is a choice.
        assert_eq!(resolve(None, Some(" "), file).unwrap().0, mirror("https://file.example"));
        assert_eq!(resolve(None, None, Some("")).unwrap().0, ModelSource::HuggingFace);
        assert_eq!(resolve(Some(""), env, file).unwrap().0, ModelSource::HuggingFace);
    }

    #[test]
    fn an_invalid_source_fails_naming_where_it_came_from() {
        let err = resolve(None, Some("ftp://env.example"), None).unwrap_err();
        assert!(err.starts_with("FAIRSPOKEN_MODEL_SOURCE: "), "{err}");
        let err = resolve(None, None, Some("relative/models")).unwrap_err();
        assert!(err.starts_with("modelSource in host-config.json: "), "{err}");
        // A valid higher-precedence value wins over an invalid lower one.
        assert!(resolve(Some("https://cli.example"), Some("ftp://x"), None).is_ok());
    }

    #[test]
    fn the_config_file_keeps_its_model_source_when_rewritten() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("host-config.json");
        let model = SttModel::Parakeet.model_id();
        fs::write(
            &path,
            format!(r#"{{"maxActiveStreams":4,"maxRecordingSeconds":600,"useGpu":true,"workerModels":["{model}"],"modelSource":"https://mirror.example"}}"#),
        )
        .unwrap();
        let persisted = load_persisted_config(&path).unwrap().unwrap();
        assert_eq!(persisted.model_source.as_deref(), Some("https://mirror.example"));

        let live = HostLiveConfig::new(&HostRuntimeConfig {
            worker_count: 1,
            queue_capacity: 1,
            max_active_streams: 4,
            max_recording_seconds: 600,
            use_gpu: true,
            worker_models: vec![SttModel::Parakeet],
            super_mode: super::super::super_mode::SuperModePolicy::Allow,
        });
        live.set_saved_model_source(persisted.model_source);
        persist_live_config(&path, &live).unwrap();
        let rewritten = load_persisted_config(&path).unwrap().unwrap();
        assert_eq!(rewritten.model_source.as_deref(), Some("https://mirror.example"));

        // Without one, the key stays out of the file.
        live.set_saved_model_source(None);
        persist_live_config(&path, &live).unwrap();
        assert!(!fs::read_to_string(&path).unwrap().contains("modelSource"));
    }
}
