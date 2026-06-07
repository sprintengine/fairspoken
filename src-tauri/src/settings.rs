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

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum RecordingShortcutMode {
    Toggle,
    PushToTalk,
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
    #[serde(default)]
    pub vocabulary_hints: Vec<String>,
    #[serde(default)]
    pub transcript_corrections: Vec<TranscriptCorrection>,
    #[serde(default)]
    pub snippets: Vec<Snippet>,
    pub always_on_top: bool,
    pub max_recording_seconds: u16,
    #[serde(default = "default_whisper_chunk_seconds")]
    pub whisper_chunk_seconds: u16,
    #[serde(default = "default_recording_shortcut")]
    pub recording_shortcut: String,
    #[serde(default)]
    pub recording_shortcut_mode: RecordingShortcutMode,
    #[serde(default = "default_transcript_stack_shortcut")]
    pub transcript_stack_shortcut: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptCorrection {
    #[serde(default = "default_correction_enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub from: String,
    #[serde(default)]
    pub to: String,
    #[serde(default)]
    pub case_sensitive: bool,
    #[serde(default = "default_correction_whole_phrase")]
    pub whole_phrase: bool,
}

/// A text-expansion shortcut: when `trigger` is dictated, it is replaced with
/// `expansion` during post-processing (whole-phrase, case-insensitive).
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Snippet {
    #[serde(default = "default_correction_enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub trigger: String,
    #[serde(default)]
    pub expansion: String,
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
            vocabulary_hints: Vec::new(),
            transcript_corrections: Vec::new(),
            snippets: Vec::new(),
            always_on_top: true,
            max_recording_seconds: 120,
            whisper_chunk_seconds: default_whisper_chunk_seconds(),
            recording_shortcut: default_recording_shortcut(),
            recording_shortcut_mode: RecordingShortcutMode::Toggle,
            transcript_stack_shortcut: default_transcript_stack_shortcut(),
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

impl Default for RecordingShortcutMode {
    fn default() -> Self {
        Self::Toggle
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

    pub fn whisper_initial_prompt(&self) -> Option<String> {
        let mut terms = self.vocabulary_hints.clone();
        terms.extend(
            self.transcript_corrections
                .iter()
                .filter(|correction| correction.enabled)
                .filter_map(|correction| {
                    let term = correction.to.trim();
                    (!term.is_empty()).then(|| term.to_string())
                }),
        );
        let terms = normalize_vocabulary_hints(terms);
        if terms.is_empty() {
            return None;
        }

        Some(format!("Relevant names and terms: {}.", terms.join(", ")))
    }
}

fn normalize(settings: Settings) -> Settings {
    Settings {
        input_gain: settings.input_gain.clamp(1, 6),
        max_recording_seconds: settings.max_recording_seconds.clamp(10, 300),
        whisper_chunk_seconds: settings.whisper_chunk_seconds.clamp(5, 60),
        remote_url: normalize_remote_url(&settings.remote_url),
        remote_timeout_seconds: settings.remote_timeout_seconds.clamp(5, 300),
        vocabulary_hints: normalize_vocabulary_hints(settings.vocabulary_hints),
        transcript_corrections: normalize_transcript_corrections(settings.transcript_corrections),
        snippets: normalize_snippets(settings.snippets),
        recording_shortcut: normalize_shortcut(
            &settings.recording_shortcut,
            &default_recording_shortcut(),
        ),
        transcript_stack_shortcut: normalize_shortcut(
            &settings.transcript_stack_shortcut,
            &default_transcript_stack_shortcut(),
        ),
        ..settings
    }
}

fn default_whisper_chunk_seconds() -> u16 {
    20
}

fn default_remote_timeout_seconds() -> u16 {
    60
}

fn default_recording_shortcut() -> String {
    "CommandOrControl+Shift+Digit1".to_string()
}

fn default_transcript_stack_shortcut() -> String {
    "CommandOrControl+Shift+Digit2".to_string()
}

fn default_correction_enabled() -> bool {
    true
}

fn default_correction_whole_phrase() -> bool {
    true
}

fn normalize_vocabulary_hints(hints: Vec<String>) -> Vec<String> {
    let mut normalized = Vec::new();
    for hint in hints {
        let hint = clean_text_setting(&hint, 100);
        if hint.is_empty() || normalized.iter().any(|value| value == &hint) {
            continue;
        }
        normalized.push(hint);
        if normalized.len() >= 50 {
            break;
        }
    }
    normalized
}

fn normalize_transcript_corrections(
    corrections: Vec<TranscriptCorrection>,
) -> Vec<TranscriptCorrection> {
    let mut normalized = Vec::new();
    for correction in corrections {
        let from = clean_text_setting(&correction.from, 120);
        let to = clean_text_setting(&correction.to, 120);
        if from.is_empty() || to.is_empty() {
            continue;
        }
        normalized.push(TranscriptCorrection {
            enabled: correction.enabled,
            from,
            to,
            case_sensitive: correction.case_sensitive,
            whole_phrase: correction.whole_phrase,
        });
        if normalized.len() >= 100 {
            break;
        }
    }
    normalized
}

fn normalize_snippets(snippets: Vec<Snippet>) -> Vec<Snippet> {
    let mut normalized = Vec::new();
    for snippet in snippets {
        let trigger = clean_text_setting(&snippet.trigger, 120);
        let expansion = clean_text_setting(&snippet.expansion, 500);
        if trigger.is_empty() || expansion.is_empty() {
            continue;
        }
        normalized.push(Snippet {
            enabled: snippet.enabled,
            trigger,
            expansion,
        });
        if normalized.len() >= 100 {
            break;
        }
    }
    normalized
}

fn clean_text_setting(value: &str, max_chars: usize) -> String {
    value
        .replace('\0', "")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(max_chars)
        .collect()
}

fn normalize_remote_url(url: &str) -> String {
    url.trim().trim_end_matches('/').to_string()
}

fn normalize_shortcut(shortcut: &str, fallback: &str) -> String {
    let normalized = shortcut
        .split('+')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("+");
    if normalized.is_empty() {
        fallback.to_string()
    } else {
        normalized.chars().take(80).collect()
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_shortcuts_are_persisted_settings() {
        let settings = Settings::default();

        assert_eq!(settings.recording_shortcut, "CommandOrControl+Shift+Digit1");
        assert_eq!(
            settings.recording_shortcut_mode,
            RecordingShortcutMode::Toggle
        );
        assert_eq!(
            settings.transcript_stack_shortcut,
            "CommandOrControl+Shift+Digit2"
        );
    }

    #[test]
    fn normalize_shortcuts_trims_and_falls_back_when_empty() {
        let mut settings = Settings::default();
        settings.recording_shortcut = "  CommandOrControl + Shift + A  ".to_string();
        settings.transcript_stack_shortcut = "   ".to_string();

        let normalized = normalize(settings);

        assert_eq!(normalized.recording_shortcut, "CommandOrControl+Shift+A");
        assert_eq!(
            normalized.transcript_stack_shortcut,
            "CommandOrControl+Shift+Digit2"
        );
    }
}
