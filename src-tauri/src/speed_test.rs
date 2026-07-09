use crate::settings::{Settings, TranscriptionLocation};
use serde::{Deserialize, Serialize};
use std::env;
use std::fs;
use std::path::PathBuf;

/// The result of the speaking leg, produced by transcribing a real recording
/// with no commit side effects — nothing is written to the clipboard, the
/// transcript history, the notes library, or the lifetime usage stats. The
/// test must not pollute the very numbers it sits beside.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SpeedTestCapture {
    pub transcript: String,
    pub recording_seconds: f32,
    pub transcribe_ms: u32,
    pub engine: String,
    pub model: String,
    pub preview_mode: CapturePreviewMode,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum CapturePreviewMode {
    Chunked,
    FinalOnly,
    // Part of the public capture contract for backends whose preview support
    // cannot be known from local settings alone.
    #[allow(dead_code)]
    Unknown,
}

/// One completed speed test, persisted so the intro can show a personal best
/// and results can flag a new record. Non-sensitive: it holds only metrics,
/// never transcript text.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SpeedTestRecord {
    pub typing_wpm: u32,
    pub speaking_wpm: u32,
    pub multiplier: f32,
    pub accuracy: u32,
    pub recorded_at: u64,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct SpeedTestData {
    #[serde(default)]
    last: Option<SpeedTestRecord>,
    #[serde(default)]
    best: Option<SpeedTestRecord>,
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SpeedTestSummary {
    pub last: Option<SpeedTestRecord>,
    pub best: Option<SpeedTestRecord>,
}

pub struct SpeedTestService {
    data: SpeedTestData,
    path: PathBuf,
}

pub fn build_capture_result(
    transcript: String,
    recording_seconds: f32,
    transcribe_ms: u32,
    settings: &Settings,
) -> SpeedTestCapture {
    SpeedTestCapture {
        transcript,
        recording_seconds,
        transcribe_ms,
        engine: if settings.model.is_whisper() {
            "whisper".to_string()
        } else {
            "parakeet".to_string()
        },
        model: settings.model.model_id().to_string(),
        preview_mode: capture_preview_mode(settings.transcription_location),
    }
}

pub fn capture_preview_mode(location: TranscriptionLocation) -> CapturePreviewMode {
    match location {
        TranscriptionLocation::Local => CapturePreviewMode::Chunked,
        // Remote targets (self-hosted or MultiVoice Cloud) return one final
        // transcript at stream end; there are no live partial previews.
        TranscriptionLocation::RemoteHost | TranscriptionLocation::Cloud => {
            CapturePreviewMode::FinalOnly
        }
    }
}

impl Default for SpeedTestService {
    fn default() -> Self {
        let path = default_speed_test_path();
        let data = fs::read_to_string(&path)
            .ok()
            .and_then(|raw| serde_json::from_str::<SpeedTestData>(&raw).ok())
            .unwrap_or_default();
        Self { data, path }
    }
}

impl SpeedTestService {
    pub fn summary(&self) -> SpeedTestSummary {
        SpeedTestSummary {
            last: self.data.last,
            best: self.data.best,
        }
    }

    /// Record a finished run as the latest, promoting it to best when its
    /// multiplier beats the stored best. Returns the new summary and whether
    /// this run set a new best.
    pub fn record(&mut self, record: SpeedTestRecord) -> Result<(SpeedTestSummary, bool), String> {
        let is_best = self
            .data
            .best
            .is_none_or(|best| record.multiplier > best.multiplier);
        self.data.last = Some(record);
        if is_best {
            self.data.best = Some(record);
        }
        self.save()?;
        Ok((self.summary(), is_best))
    }

    fn save(&self) -> Result<(), String> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)
                .map_err(|err| format!("Failed to create speed test directory: {err}"))?;
        }
        let payload = serde_json::to_string_pretty(&self.data)
            .map_err(|err| format!("Failed to serialize speed test results: {err}"))?;
        fs::write(&self.path, payload)
            .map_err(|err| format!("Failed to write speed test results: {err}"))
    }
}

fn default_speed_test_path() -> PathBuf {
    if let Some(path) = env::var_os("MULTIVOICE_TAURI_SPEED_TEST_PATH") {
        return PathBuf::from(path);
    }

    if let Some(local_app_data) = env::var_os("LOCALAPPDATA") {
        return PathBuf::from(local_app_data)
            .join("Multivoice Tauri")
            .join("speed-test.json");
    }

    if let Some(home) = env::var_os("HOME") {
        return PathBuf::from(home)
            .join(".config")
            .join("multivoice-tauri")
            .join("speed-test.json");
    }

    PathBuf::from("speed-test.json")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn service_at(name: &str) -> SpeedTestService {
        SpeedTestService {
            data: SpeedTestData::default(),
            path: std::env::temp_dir().join(name),
        }
    }

    fn record(multiplier: f32, at: u64) -> SpeedTestRecord {
        SpeedTestRecord {
            typing_wpm: 40,
            speaking_wpm: 150,
            multiplier,
            accuracy: 95,
            recorded_at: at,
        }
    }

    #[test]
    fn first_run_is_always_a_new_best() {
        let mut service = service_at("multivoice-speedtest-first.json");
        let (summary, is_best) = service.record(record(3.0, 1)).expect("record");
        assert!(is_best);
        assert_eq!(summary.best.unwrap().multiplier, 3.0);
        assert_eq!(summary.last.unwrap().multiplier, 3.0);
        let _ = std::fs::remove_file(&service.path);
    }

    #[test]
    fn best_only_advances_on_a_higher_multiplier() {
        let mut service = service_at("multivoice-speedtest-best.json");
        service.record(record(3.5, 1)).expect("first");
        let (summary, is_best) = service.record(record(2.9, 2)).expect("slower run");
        // Last reflects the most recent run; best holds the faster earlier one.
        assert!(!is_best);
        assert_eq!(summary.last.unwrap().multiplier, 2.9);
        assert_eq!(summary.best.unwrap().multiplier, 3.5);

        let (summary, is_best) = service.record(record(4.1, 3)).expect("faster run");
        assert!(is_best);
        assert_eq!(summary.best.unwrap().multiplier, 4.1);
        let _ = std::fs::remove_file(&service.path);
    }

    #[test]
    fn preview_mode_reports_real_backend_behavior() {
        assert_eq!(
            capture_preview_mode(TranscriptionLocation::Local),
            CapturePreviewMode::Chunked
        );
        assert_eq!(
            capture_preview_mode(TranscriptionLocation::RemoteHost),
            CapturePreviewMode::FinalOnly
        );
        assert_eq!(
            serde_json::to_string(&CapturePreviewMode::Unknown).expect("serialize"),
            "\"unknown\""
        );
    }

    #[test]
    fn capture_result_builder_only_projects_measurement_metadata() {
        let settings = Settings {
            transcription_location: TranscriptionLocation::Local,
            ..Settings::default()
        };

        let capture = build_capture_result("hello world".to_string(), 1.25, 42, &settings);

        assert_eq!(capture.transcript, "hello world");
        assert_eq!(capture.recording_seconds, 1.25);
        assert_eq!(capture.transcribe_ms, 42);
        assert_eq!(capture.engine, "parakeet");
        assert_eq!(capture.model, "parakeet-tdt-0.6b-v3");
        assert_eq!(capture.preview_mode, CapturePreviewMode::Chunked);
    }
}
