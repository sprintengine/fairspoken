use crate::models::SttModel;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum TranscriptionLocation {
    #[default]
    Local,
    RemoteHost,
    /// The hosted Fairspoken Cloud service (Cloudflare Worker). Unlike
    /// `RemoteHost`, the URL is ours (not user-editable) and the token is a
    /// multiauth-issued JWT.
    Cloud,
}

/// Fairspoken Cloud endpoint. Not user-editable: users configure only their
/// token. Builds bake it in by setting `FAIRSPOKEN_CLOUD_URL` at compile time
/// (the former `MULTIVOICE_CLOUD_URL` is still read); the same variable at
/// runtime overrides it again (dev builds). With neither, the build has no
/// cloud: the Cloud location and cloud polish are unavailable.
const BUILD_CLOUD_URL: Option<&str> = match option_env!("FAIRSPOKEN_CLOUD_URL") {
    Some(url) => Some(url),
    None => option_env!("MULTIVOICE_CLOUD_URL"),
};

pub const CLOUD_UNAVAILABLE: &str =
    "Fairspoken Cloud is not available in this build. Choose Local or My host in Settings.";

pub fn cloud_url() -> Option<String> {
    let normalize = |url: &str| Some(url.trim().trim_end_matches('/').to_string()).filter(|url| !url.is_empty());
    crate::app_dirs::env_var("FAIRSPOKEN_CLOUD_URL")
        .and_then(|url| normalize(&url))
        .or_else(|| BUILD_CLOUD_URL.and_then(normalize))
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum RecordingShortcutMode {
    #[default]
    Toggle,
    PushToTalk,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum PolishProvider {
    Local,
    #[default]
    Cloud,
}

fn default_polish_model() -> String {
    "speakoflow-mini".to_string()
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    #[serde(default)]
    pub transcription_location: TranscriptionLocation,
    #[serde(default)]
    pub model: SttModel,
    #[serde(default)]
    pub remote_url: String,
    #[serde(default)]
    pub remote_auth_token: String,
    #[serde(default = "default_remote_timeout_seconds")]
    pub remote_timeout_seconds: u16,
    /// multiauth-issued JWT for Fairspoken Cloud. Phase 1 stores it in
    /// settings JSON exactly like `remote_auth_token` (same acknowledged
    /// keychain debt; phase-1 tokens are short-lived which bounds exposure).
    #[serde(default)]
    pub cloud_auth_token: String,
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
    #[serde(default = "default_interaction_sounds")]
    pub interaction_sounds: bool,
    pub max_recording_seconds: u16,
    /// Auto-delete unpinned notes whose last edit is older than this many
    /// minutes; 0 keeps notes forever.
    #[serde(default)]
    pub note_retention_minutes: u32,
    /// Run Whisper inference on the GPU when a GPU backend is compiled in
    /// (Metal on macOS). Off forces CPU inference on the next model load.
    #[serde(default = "default_use_gpu")]
    pub use_gpu: bool,
    #[serde(default = "default_recording_shortcut")]
    pub recording_shortcut: String,
    #[serde(default)]
    pub recording_shortcut_mode: RecordingShortcutMode,
    #[serde(default = "default_transcript_stack_shortcut")]
    pub transcript_stack_shortcut: String,
    /// macOS only: deliver the transcript by pasting at the cursor in the
    /// focused app (the clipboard copy still happens first).
    #[serde(default = "default_insert_at_cursor")]
    pub insert_at_cursor: bool,
    /// macOS only: insert by writing the focused field's selected text via
    /// Accessibility instead of pasting. Off by default — Electron apps,
    /// terminals and web fields report success but ignore the write.
    #[serde(default)]
    pub accessibility_insert: bool,
    /// macOS only: hold the Fn/Globe key to record, release to transcribe.
    #[serde(default)]
    pub fn_push_to_talk: bool,
    /// Opt-in AI polish: send the raw transcript (text, never audio) to
    /// Fairspoken Cloud for filler/self-correction cleanup before the user's
    /// deterministic rules run. Requires a cloud token; failures always fall
    /// back to the raw transcript.
    #[serde(default)]
    pub polish_enabled: bool,
    #[serde(default)]
    pub polish_provider: PolishProvider,
    #[serde(default = "default_polish_model")]
    pub polish_model: String,
    /// Per-app-category tone for AI polish. Keys are category ids
    /// (`messaging | email | docs | code | other`), values are
    /// `default | casual | formal | off` (`off` disables polish for that
    /// category). Missing key = `default`. Terminals are implicitly always
    /// off and have no entry.
    #[serde(default)]
    pub polish_tones: HashMap<String, String>,
    /// Context awareness: read text near the cursor and on the active window
    /// via Accessibility to improve name/term accuracy (never password
    /// fields, never password managers, never persisted). Off-device only as
    /// part of AI polish, and only when that is also enabled.
    #[serde(default)]
    pub context_awareness: bool,
    /// The subset of `vocabulary_hints` the edit watcher added, shown with a
    /// "Learned" chip. A separate list keeps `vocabulary_hints` a plain
    /// string array for older builds and every other reader.
    #[serde(default)]
    pub learned_vocabulary_hints: Vec<String>,
    /// Learn from the user fixing a dictated word in place (macOS): misheard
    /// names join the vocabulary and repeated mishearings become corrections.
    /// Only `heard → intended` word pairs are kept, never the sentence.
    #[serde(default = "default_learn_from_edits")]
    pub learn_from_edits: bool,
    /// Opt-in: keep each dictation's audio and texts on this device so they
    /// can be exported to fine-tune speech or polish models.
    #[serde(default)]
    pub training_capture: bool,
    /// Days kept dictations live before they are deleted; 0 keeps them until
    /// the user deletes them.
    #[serde(default = "default_training_retention_days")]
    pub training_retention_days: u16,
    /// "Detect format with AI for unknown apps": when no rule knows the
    /// destination, ask the polish provider once per site or app which
    /// format it wants (`format_classifier`). Sends the app name, window
    /// title, site host and field labels to that provider.
    #[serde(default)]
    pub format_ai_detection: bool,
}

/// Who wrote a dictionary entry. Learned entries come from the edit watcher
/// and carry a chip on the Dictionary screen.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum EntryOrigin {
    #[default]
    Manual,
    Learned,
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
    #[serde(default)]
    pub origin: EntryOrigin,
    /// Speech models (`Settings::speech_model_id`) the correction applies
    /// to; empty means every model. Learned mappings are scoped to the models
    /// that actually produced the mishearing.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub models: Vec<String>,
}

impl Default for TranscriptCorrection {
    fn default() -> Self {
        Self {
            enabled: default_correction_enabled(),
            from: String::new(),
            to: String::new(),
            case_sensitive: false,
            whole_phrase: default_correction_whole_phrase(),
            origin: EntryOrigin::Manual,
            models: Vec::new(),
        }
    }
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
            model: SttModel::default(),
            remote_url: String::new(),
            remote_auth_token: String::new(),
            remote_timeout_seconds: default_remote_timeout_seconds(),
            cloud_auth_token: String::new(),
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
            interaction_sounds: true,
            max_recording_seconds: 120,
            note_retention_minutes: 0,
            use_gpu: default_use_gpu(),
            recording_shortcut: default_recording_shortcut(),
            recording_shortcut_mode: RecordingShortcutMode::Toggle,
            transcript_stack_shortcut: default_transcript_stack_shortcut(),
            insert_at_cursor: default_insert_at_cursor(),
            accessibility_insert: false,
            fn_push_to_talk: false,
            polish_enabled: false,
            polish_provider: PolishProvider::Cloud,
            polish_model: default_polish_model(),
            polish_tones: HashMap::new(),
            context_awareness: false,
            learned_vocabulary_hints: Vec::new(),
            learn_from_edits: default_learn_from_edits(),
            training_capture: false,
            training_retention_days: default_training_retention_days(),
            format_ai_detection: false,
        }
    }
}

const POLISH_TONE_CATEGORIES: &[&str] = &["messaging", "email", "docs", "code", "other"];
const POLISH_TONE_VALUES: &[&str] = &["default", "casual", "formal", "off"];

fn normalize_polish_tones(tones: HashMap<String, String>) -> HashMap<String, String> {
    tones
        .into_iter()
        .filter(|(category, tone)| {
            POLISH_TONE_CATEGORIES.contains(&category.as_str())
                && POLISH_TONE_VALUES.contains(&tone.as_str())
                && tone != "default"
        })
        .collect()
}

fn default_insert_at_cursor() -> bool {
    cfg!(target_os = "macos")
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
        let next = normalize(settings);
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)
                .map_err(|err| format!("Failed to create settings directory: {err}"))?;
        }

        let payload = serde_json::to_string_pretty(&next)
            .map_err(|err| format!("Failed to serialize settings: {err}"))?;
        let temporary = self.path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
        let write = || -> std::io::Result<()> {
            let mut options = fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                // Replacing the file must not broaden access to stored API tokens.
                options.mode(0o600);
            }
            let mut file = options.open(&temporary)?;
            file.write_all(payload.as_bytes())?;
            file.sync_all()?;
            drop(file);
            fs::rename(&temporary, &self.path)
        };
        if let Err(error) = write() {
            let _ = fs::remove_file(&temporary);
            return Err(format!("Failed to write settings: {error}"));
        }
        self.current = next;
        Ok(())
    }
}

impl Settings {
    pub fn requires_transcription_unload(&self, next: &Self) -> bool {
        self.transcription_location != next.transcription_location
            || self.model != next.model
            || self.use_gpu != next.use_gpu
    }

    /// A session-scoped copy of these settings with screen-harvested terms
    /// merged into the vocabulary hints. The harvest is never persisted —
    /// this clone lives only as long as the recording session; user-authored
    /// hints keep priority under the normalization cap.
    pub fn with_session_vocabulary(&self, harvested: Vec<String>) -> Settings {
        if harvested.is_empty() {
            return self.clone();
        }
        let mut merged = self.vocabulary_hints.clone();
        merged.extend(harvested);
        Settings {
            vocabulary_hints: normalize_vocabulary_hints(merged),
            ..self.clone()
        }
    }

    /// The id a correction's `models` scope matches: the local speech model,
    /// or the location for remote engines, which choose their own model.
    pub fn speech_model_id(&self) -> String {
        match self.transcription_location {
            TranscriptionLocation::Local => self.model.model_id().to_string(),
            TranscriptionLocation::RemoteHost => "remote-host".to_string(),
            TranscriptionLocation::Cloud => "cloud".to_string(),
        }
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
    let vocabulary_hints = normalize_vocabulary_hints(settings.vocabulary_hints);
    let learned_vocabulary_hints =
        normalize_learned_hints(settings.learned_vocabulary_hints, &vocabulary_hints);
    Settings {
        input_gain: settings.input_gain.clamp(1, 6),
        max_recording_seconds: settings.max_recording_seconds.clamp(10, 600),
        remote_url: normalize_remote_url(&settings.remote_url),
        remote_timeout_seconds: settings.remote_timeout_seconds.clamp(5, 300),
        cloud_auth_token: settings.cloud_auth_token.trim().to_string(),
        polish_tones: normalize_polish_tones(settings.polish_tones),
        vocabulary_hints,
        learned_vocabulary_hints,
        training_retention_days: normalize_training_retention_days(
            settings.training_retention_days,
        ),
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

fn default_use_gpu() -> bool {
    true
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

fn default_learn_from_edits() -> bool {
    true
}

/// Retention choices offered in Settings: 30 days, 90 days, or until deleted.
const TRAINING_RETENTION_DAYS: &[u16] = &[0, 30, 90];

fn default_training_retention_days() -> u16 {
    30
}

fn normalize_training_retention_days(days: u16) -> u16 {
    if TRAINING_RETENTION_DAYS.contains(&days) {
        days
    } else {
        default_training_retention_days()
    }
}

/// Learned markers only survive for hints that are still in the vocabulary,
/// so removing a word also removes its chip.
fn normalize_learned_hints(learned: Vec<String>, vocabulary: &[String]) -> Vec<String> {
    let mut normalized: Vec<String> = Vec::new();
    for hint in learned {
        let hint = clean_text_setting(&hint, 100);
        if vocabulary.contains(&hint) && !normalized.contains(&hint) {
            normalized.push(hint);
        }
    }
    normalized
}

fn default_interaction_sounds() -> bool {
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
        let mut models: Vec<String> = Vec::new();
        for model in correction.models {
            let model = clean_text_setting(&model, 80);
            if !model.is_empty() && !models.contains(&model) && models.len() < 8 {
                models.push(model);
            }
        }
        normalized.push(TranscriptCorrection {
            enabled: correction.enabled,
            from,
            to,
            case_sensitive: correction.case_sensitive,
            whole_phrase: correction.whole_phrase,
            origin: correction.origin,
            models,
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
    if let Some(path) = crate::app_dirs::env_var_os("FAIRSPOKEN_SETTINGS_PATH") {
        return PathBuf::from(path);
    }

    crate::app_dirs::config_dir()
        .map(|dir| dir.join("settings.json"))
        .unwrap_or_else(|| PathBuf::from("settings.json"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn failed_save_keeps_current_settings_and_previous_path() {
        let path = std::env::temp_dir().join(format!("settings-failure-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&path).unwrap();
        let current = Settings::default();
        let mut service = SettingsService { current: current.clone(), path: path.clone() };
        let mut changed = current.clone();
        changed.recording_shortcut = "Control+K".into();
        assert!(service.save(changed).is_err());
        assert_eq!(service.current().recording_shortcut, current.recording_shortcut);
        assert!(path.is_dir());
        std::fs::remove_dir(path).unwrap();
    }


    #[test]
    fn old_settings_keep_cloud_polish_and_new_local_choice_round_trips() {
        let mut old = serde_json::to_value(Settings::default()).unwrap();
        old.as_object_mut().unwrap().remove("polishProvider");
        old.as_object_mut().unwrap().remove("polishModel");
        old["polishEnabled"] = serde_json::json!(true);
        let restored: Settings = serde_json::from_value(old).unwrap();
        assert_eq!(restored.polish_provider, super::PolishProvider::Cloud);
        assert!(restored.polish_enabled);
        let local = Settings {
            polish_provider: super::PolishProvider::Local,
            ..restored
        };
        let restored: Settings =
            serde_json::from_value(serde_json::to_value(local).unwrap()).unwrap();
        assert_eq!(restored.polish_provider, super::PolishProvider::Local);
        assert!(restored.cloud_auth_token.is_empty());
    }

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
    fn gpu_acceleration_defaults_on_and_reloads_transcription_when_toggled() {
        let settings = Settings::default();
        assert!(settings.use_gpu);

        let mut cpu_settings = settings.clone();
        cpu_settings.use_gpu = false;

        assert!(settings.requires_transcription_unload(&cpu_settings));
        assert!(!settings.requires_transcription_unload(&settings.clone()));
    }

    #[test]
    fn transcription_location_serde_uses_kebab_case_values() {
        assert_eq!(
            serde_json::to_string(&TranscriptionLocation::Local).unwrap(),
            "\"local\""
        );
        assert_eq!(
            serde_json::to_string(&TranscriptionLocation::RemoteHost).unwrap(),
            "\"remote-host\""
        );
        assert_eq!(
            serde_json::to_string(&TranscriptionLocation::Cloud).unwrap(),
            "\"cloud\""
        );
        assert_eq!(
            serde_json::from_str::<TranscriptionLocation>("\"cloud\"").unwrap(),
            TranscriptionLocation::Cloud
        );
    }

    #[test]
    fn settings_without_cloud_fields_load_with_defaults() {
        // A settings JSON written before cloud mode existed.
        let legacy = serde_json::json!({
            "transcriptionLocation": "remote-host",
            "language": "en",
            "audioDevice": "",
            "noiseSuppression": true,
            "echoCancellation": true,
            "inputGain": 2,
            "postProcess": true,
            "alwaysOnTop": true,
            "maxRecordingSeconds": 120
        });

        let settings: Settings = serde_json::from_value(legacy).expect("legacy settings load");

        assert_eq!(
            settings.transcription_location,
            TranscriptionLocation::RemoteHost
        );
        assert_eq!(settings.cloud_auth_token, "");
        assert!(!settings.polish_enabled);
        assert!(settings.polish_tones.is_empty());
    }

    #[test]
    fn dictionary_written_before_learning_loads_as_manual_entries() {
        // A dictionary saved before learned entries existed: corrections have
        // no origin or models, and there is no learned-hints list.
        let legacy = serde_json::json!({
            "language": "en",
            "audioDevice": "",
            "noiseSuppression": true,
            "echoCancellation": true,
            "inputGain": 2,
            "postProcess": true,
            "alwaysOnTop": true,
            "maxRecordingSeconds": 120,
            "vocabularyHints": ["Fairspoken", "Siobhán"],
            "transcriptCorrections": [
                {"enabled": true, "from": "fair spoken", "to": "Fairspoken", "caseSensitive": false, "wholePhrase": true}
            ]
        });

        let settings = normalize(serde_json::from_value::<Settings>(legacy).unwrap());

        assert_eq!(settings.vocabulary_hints, ["Fairspoken", "Siobhán"]);
        assert!(settings.learned_vocabulary_hints.is_empty());
        let correction = &settings.transcript_corrections[0];
        assert_eq!(correction.origin, EntryOrigin::Manual);
        assert!(correction.models.is_empty());
        assert!(settings.learn_from_edits);
        assert!(!settings.training_capture);
        assert_eq!(settings.training_retention_days, 30);
    }

    #[test]
    fn learned_entries_round_trip_and_markers_follow_the_vocabulary() {
        let settings = normalize(Settings {
            vocabulary_hints: vec!["Niamh".into(), "Tauri".into()],
            learned_vocabulary_hints: vec!["Niamh".into(), "Removed".into(), "Niamh".into()],
            transcript_corrections: vec![TranscriptCorrection {
                from: "knee of".into(),
                to: "Niamh".into(),
                origin: EntryOrigin::Learned,
                models: vec!["parakeet-tdt-0.6b-v3".into(), " ".into()],
                ..TranscriptCorrection::default()
            }],
            training_retention_days: 45,
            ..Settings::default()
        });
        assert_eq!(settings.learned_vocabulary_hints, ["Niamh"]);
        assert_eq!(settings.training_retention_days, 30);

        let json = serde_json::to_value(&settings).unwrap();
        assert_eq!(json["transcriptCorrections"][0]["origin"], "learned");
        assert_eq!(json["transcriptCorrections"][0]["models"][0], "parakeet-tdt-0.6b-v3");
        let restored: Settings = serde_json::from_value(json).unwrap();
        assert_eq!(restored.transcript_corrections[0].origin, EntryOrigin::Learned);
        assert_eq!(restored.transcript_corrections[0].models, ["parakeet-tdt-0.6b-v3"]);

        // Unscoped manual corrections serialize without a models key, so the
        // file stays readable by builds that predate scoping.
        let manual = serde_json::to_value(TranscriptCorrection {
            from: "a".into(),
            to: "b".into(),
            ..TranscriptCorrection::default()
        })
        .unwrap();
        assert!(manual.get("models").is_none());
        assert_eq!(manual["origin"], "manual");
    }

    #[test]
    fn polish_tones_normalize_drops_invalid_and_default_entries() {
        let mut tones = HashMap::new();
        tones.insert("messaging".to_string(), "casual".to_string());
        tones.insert("docs".to_string(), "off".to_string());
        tones.insert("email".to_string(), "default".to_string()); // redundant
        tones.insert("terminal".to_string(), "formal".to_string()); // not a settable category
        tones.insert("messaging2".to_string(), "casual".to_string()); // unknown key
        tones.insert("code".to_string(), "sarcastic".to_string()); // unknown tone

        let normalized = normalize(Settings {
            polish_tones: tones,
            ..Settings::default()
        })
        .polish_tones;

        assert_eq!(normalized.len(), 2);
        assert_eq!(
            normalized.get("messaging").map(String::as_str),
            Some("casual")
        );
        assert_eq!(normalized.get("docs").map(String::as_str), Some("off"));
    }

    #[test]
    fn default_location_stays_local_and_cloud_token_normalizes() {
        assert_eq!(
            Settings::default().transcription_location,
            TranscriptionLocation::Local
        );

        let normalized = normalize(Settings {
            cloud_auth_token: "  token-with-spaces  ".to_string(),
            ..Default::default()
        });
        assert_eq!(normalized.cloud_auth_token, "token-with-spaces");
    }

    #[test]
    fn normalize_shortcuts_trims_and_falls_back_when_empty() {
        let settings = Settings {
            recording_shortcut: "  CommandOrControl + Shift + A  ".to_string(),
            transcript_stack_shortcut: "   ".to_string(),
            ..Default::default()
        };

        let normalized = normalize(settings);

        assert_eq!(normalized.recording_shortcut, "CommandOrControl+Shift+A");
        assert_eq!(
            normalized.transcript_stack_shortcut,
            "CommandOrControl+Shift+Digit2"
        );
    }

    #[cfg(unix)]
    #[test]
    fn atomic_save_keeps_settings_private_and_replaces_existing_file() {
        use std::os::unix::fs::PermissionsExt;
        let directory = std::env::temp_dir().join(format!("fairspoken-settings-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join("settings.json");
        let mut service = SettingsService { path: path.clone(), current: Settings::default() };
        service.save(Settings::default()).unwrap();
        let next = Settings { interaction_sounds: false, ..Settings::default() };
        service.save(next).unwrap();
        assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        let saved: Settings = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert!(!saved.interaction_sounds);
        assert_eq!(fs::read_dir(&directory).unwrap().count(), 1);
        fs::remove_dir_all(directory).unwrap();
    }
}
