use crate::models::{SherpaModel, WhisperModel};
use serde::{Deserialize, Serialize};
use std::env;
use std::fs;
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum TranscriptionBackend {
    Whisper,
    SherpaStreaming,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum TranscriptionLocation {
    Local,
    RemoteHost,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    #[serde(default)]
    pub transcription_location: TranscriptionLocation,
    #[serde(default)]
    pub transcription_backend: TranscriptionBackend,
    pub model: WhisperModel,
    #[serde(default)]
    pub sherpa_model: SherpaModel,
    #[serde(default)]
    pub remote_url: String,
    #[serde(default)]
    pub remote_auth_token: String,
    #[serde(default = "default_remote_timeout_seconds")]
    pub remote_timeout_seconds: u16,
    pub language: String,
    pub audio_device: String,
    pub noise_suppression: bool,
    pub echo_cancellation: bool,
    pub input_gain: u8,
    pub post_process: bool,
    pub always_on_top: bool,
    pub max_recording_seconds: u16,
    #[serde(default = "default_whisper_chunk_seconds")]
    pub whisper_chunk_seconds: u16,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            transcription_location: TranscriptionLocation::Local,
            transcription_backend: TranscriptionBackend::Whisper,
            model: WhisperModel::Base,
            sherpa_model: SherpaModel::StreamingZipformerEn20230626Int8,
            remote_url: String::new(),
            remote_auth_token: String::new(),
            remote_timeout_seconds: default_remote_timeout_seconds(),
            language: "en".to_string(),
            audio_device: String::new(),
            noise_suppression: true,
            echo_cancellation: true,
            input_gain: 2,
            post_process: true,
            always_on_top: true,
            max_recording_seconds: 120,
            whisper_chunk_seconds: default_whisper_chunk_seconds(),
        }
    }
}

impl Default for TranscriptionBackend {
    fn default() -> Self {
        Self::Whisper
    }
}

impl Default for TranscriptionLocation {
    fn default() -> Self {
        Self::Local
    }
}

impl Default for SherpaModel {
    fn default() -> Self {
        Self::StreamingZipformerEn20230626Int8
    }
}

pub struct SettingsService {
    current: Settings,
    path: PathBuf,
}

impl Default for SettingsService {
    fn default() -> Self {
        let path = default_settings_path();
        let current = fs::read_to_string(&path)
            .ok()
            .and_then(|raw| serde_json::from_str::<Settings>(&raw).ok())
            .map(normalize)
            .unwrap_or_default();

        Self { current, path }
    }
}

impl SettingsService {
    pub fn current(&self) -> Settings {
        self.current.clone()
    }

    pub fn save(&mut self, settings: Settings) -> Result<(), String> {
        self.current = normalize(settings);
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)
                .map_err(|err| format!("Failed to create settings directory: {err}"))?;
        }

        let payload = serde_json::to_string_pretty(&self.current)
            .map_err(|err| format!("Failed to serialize settings: {err}"))?;
        fs::write(&self.path, payload).map_err(|err| format!("Failed to write settings: {err}"))?;
        Ok(())
    }
}

impl Settings {
    pub fn requires_transcription_unload(&self, next: &Self) -> bool {
        self.transcription_location != next.transcription_location
            || self.transcription_backend != next.transcription_backend
            || self.model != next.model
            || self.sherpa_model != next.sherpa_model
    }
}

fn normalize(settings: Settings) -> Settings {
    Settings {
        input_gain: settings.input_gain.clamp(1, 6),
        max_recording_seconds: settings.max_recording_seconds.clamp(10, 300),
        whisper_chunk_seconds: settings.whisper_chunk_seconds.clamp(5, 60),
        remote_url: normalize_remote_url(&settings.remote_url),
        remote_timeout_seconds: settings.remote_timeout_seconds.clamp(5, 300),
        ..settings
    }
}

fn default_whisper_chunk_seconds() -> u16 {
    20
}

fn default_remote_timeout_seconds() -> u16 {
    60
}

fn normalize_remote_url(url: &str) -> String {
    url.trim().trim_end_matches('/').to_string()
}

fn default_settings_path() -> PathBuf {
    if let Some(path) = env::var_os("MULTIVOICE_TAURI_SETTINGS_PATH") {
        return PathBuf::from(path);
    }

    if let Some(local_app_data) = env::var_os("LOCALAPPDATA") {
        return PathBuf::from(local_app_data)
            .join("Multivoice Tauri")
            .join("settings.json");
    }

    if let Some(home) = env::var_os("HOME") {
        return PathBuf::from(home)
            .join(".config")
            .join("multivoice-tauri")
            .join("settings.json");
    }

    PathBuf::from("settings.json")
}
