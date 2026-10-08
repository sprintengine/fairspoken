//! Opt-in training-data capture ("Keep my dictations to improve models"),
//! off by default. Each dictation keeps its audio and every stage of its
//! text on this device, so it can later be exported to fine-tune a speech
//! model or train a polish LoRA. Nothing here is ever uploaded.
//!
//! Layout, under the app data dir (`FAIRSPOKEN_TRAINING_DATA_DIR` overrides):
//!
//! ```text
//! training-data/
//!   index.jsonl          one CaptureEntry per line
//!   <id>/audio.wav       16 kHz mono 16-bit PCM
//!   <id>/meta.json       the same CaptureEntry
//! ```
//!
//! Only the app category is kept about the target app, never its name or
//! window contents. The export format is documented in docs/training-data.md.

use crate::audio::Recording;
use crate::settings::Settings;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;
use tauri::{AppHandle, Manager};

const SAMPLE_RATE: u32 = 16_000;
const INDEX_FILE: &str = "index.jsonl";
const AUDIO_FILE: &str = "audio.wav";
const META_FILE: &str = "meta.json";
const SECONDS_PER_DAY: u64 = 24 * 60 * 60;

/// One kept dictation.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureEntry {
    pub id: String,
    /// Epoch seconds.
    pub created_at: u64,
    pub duration_seconds: f32,
    /// `Settings::speech_model_id`.
    pub speech_model: String,
    /// `AppCategory::id` of the app dictated into.
    pub app_category: String,
    /// The speech model's text.
    pub raw_text: String,
    /// The polish model's text, when polish changed anything.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub polished_text: Option<String>,
    /// What was inserted, after corrections and snippets.
    pub final_text: String,
    /// The dictation as the user left it, when the edit watcher saw a change.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edited_text: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureSummary {
    pub count: usize,
    pub bytes: u64,
}

/// The texts of one dictation, borrowed from the commit path.
pub struct DictationTexts<'a> {
    pub raw: &'a str,
    pub polished: Option<&'a str>,
    pub final_text: &'a str,
}

pub struct CaptureStore {
    root: PathBuf,
}

impl CaptureStore {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    fn index_path(&self) -> PathBuf {
        self.root.join(INDEX_FILE)
    }

    /// Writes the dictation's folder, then appends it to the index.
    pub fn record(&self, entry: &CaptureEntry, samples: &[i16]) -> Result<(), String> {
        let folder = self.root.join(&entry.id);
        create_private_dir(&folder).map_err(|err| format!("Could not create {folder:?}: {err}"))?;
        write_wav(&folder.join(AUDIO_FILE), samples)?;
        write_json(&folder.join(META_FILE), entry)?;
        let mut line = serde_json::to_string(entry).map_err(|err| err.to_string())?;
        line.push('\n');
        fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.index_path())
            .and_then(|mut index| index.write_all(line.as_bytes()))
            .map_err(|err| format!("Could not update the training-data index: {err}"))
    }

    /// Every readable entry; a damaged line is skipped, not fatal.
    pub fn entries(&self) -> Vec<CaptureEntry> {
        let Ok(file) = fs::File::open(self.index_path()) else {
            return Vec::new();
        };
        BufReader::new(file)
            .lines()
            .map_while(Result::ok)
            .filter_map(|line| serde_json::from_str(&line).ok())
            .collect()
    }

    fn write_index(&self, entries: &[CaptureEntry]) -> Result<(), String> {
        let mut payload = String::new();
        for entry in entries {
            payload.push_str(&serde_json::to_string(entry).map_err(|err| err.to_string())?);
            payload.push('\n');
        }
        let temporary = self.root.join(format!("{INDEX_FILE}.{}.tmp", uuid::Uuid::new_v4()));
        fs::write(&temporary, payload)
            .and_then(|_| fs::rename(&temporary, self.index_path()))
            .map_err(|err| {
                let _ = fs::remove_file(&temporary);
                format!("Could not rewrite the training-data index: {err}")
            })
    }

    /// Records the user's edited text for a kept dictation.
    pub fn attach_edit(&self, id: &str, edited: &str) -> Result<bool, String> {
        let mut entries = self.entries();
        let Some(entry) = entries.iter_mut().find(|entry| entry.id == id) else {
            return Ok(false);
        };
        if entry.final_text.trim() == edited.trim() {
            return Ok(false);
        }
        entry.edited_text = Some(edited.trim().to_string());
        write_json(&self.root.join(id).join(META_FILE), &*entry)?;
        self.write_index(&entries)?;
        Ok(true)
    }

    /// Deletes dictations older than `retention_days` (0 keeps everything).
    pub fn prune(&self, retention_days: u16, now: u64) -> Result<usize, String> {
        if retention_days == 0 {
            return Ok(0);
        }
        let cutoff = now.saturating_sub(u64::from(retention_days) * SECONDS_PER_DAY);
        let (kept, expired): (Vec<_>, Vec<_>) = self
            .entries()
            .into_iter()
            .partition(|entry| entry.created_at >= cutoff);
        if expired.is_empty() {
            return Ok(0);
        }
        for entry in &expired {
            let folder = self.root.join(&entry.id);
            if folder.is_dir() {
                fs::remove_dir_all(&folder).map_err(|err| format!("Could not delete {folder:?}: {err}"))?;
            }
        }
        self.write_index(&kept)?;
        Ok(expired.len())
    }

    pub fn delete_all(&self) -> Result<(), String> {
        match fs::remove_dir_all(&self.root) {
            Err(err) if err.kind() != std::io::ErrorKind::NotFound => {
                Err(format!("Could not delete the kept dictations: {err}"))
            }
            _ => Ok(()),
        }
    }

    pub fn summary(&self) -> CaptureSummary {
        CaptureSummary {
            count: self.entries().len(),
            bytes: directory_size(&self.root),
        }
    }

    /// Writes the export zip (see docs/training-data.md) and returns how many
    /// dictations it holds.
    pub fn export(&self, destination: &Path) -> Result<usize, String> {
        use zip::write::SimpleFileOptions;
        let entries: Vec<CaptureEntry> = self
            .entries()
            .into_iter()
            .filter(|entry| self.root.join(&entry.id).join(AUDIO_FILE).is_file())
            .collect();
        let file = fs::File::create(destination)
            .map_err(|err| format!("Could not create {destination:?}: {err}"))?;
        let mut zip = zip::ZipWriter::new(file);
        let text = SimpleFileOptions::default();
        // WAV barely compresses; storing it keeps exports fast.
        let audio = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
        let fail = |err: &dyn std::fmt::Display| format!("Could not write the export: {err}");

        let mut manifest = String::new();
        let mut polish = String::new();
        for entry in &entries {
            let target = entry.edited_text.as_deref().unwrap_or(&entry.final_text);
            let audio_path = format!("audio/{}.wav", entry.id);
            manifest.push_str(
                &serde_json::json!({
                    "audio_filepath": audio_path,
                    "duration": entry.duration_seconds,
                    "text": target,
                    "id": entry.id,
                    "created_at": entry.created_at,
                    "speech_model": entry.speech_model,
                    "app_category": entry.app_category,
                    "raw_text": entry.raw_text,
                    "polished_text": entry.polished_text,
                    "final_text": entry.final_text,
                    "edited_text": entry.edited_text,
                })
                .to_string(),
            );
            manifest.push('\n');
            polish.push_str(
                &serde_json::json!({ "id": entry.id, "prompt": entry.raw_text, "completion": target })
                    .to_string(),
            );
            polish.push('\n');

            zip.start_file(audio_path, audio).map_err(|err| fail(&err))?;
            let bytes = fs::read(self.root.join(&entry.id).join(AUDIO_FILE)).map_err(|err| fail(&err))?;
            zip.write_all(&bytes).map_err(|err| fail(&err))?;
        }
        for (name, contents) in [
            ("manifest.jsonl", manifest.as_str()),
            ("polish.jsonl", polish.as_str()),
            ("README.txt", EXPORT_README),
        ] {
            zip.start_file(name, text).map_err(|err| fail(&err))?;
            zip.write_all(contents.as_bytes()).map_err(|err| fail(&err))?;
        }
        zip.finish().map_err(|err| fail(&err))?;
        Ok(entries.len())
    }
}

const EXPORT_README: &str = "Fairspoken training data export (format version 1).\n\
\n\
manifest.jsonl  one dictation per line, NeMo-style: audio_filepath, duration, text\n\
                (the user's edited text, else the inserted text), plus every\n\
                stage of the text and the speech model that heard it.\n\
polish.jsonl    prompt/completion pairs (speech model text -> final text) for\n\
                polish LoRA training with MLX-LM.\n\
audio/          16 kHz mono 16-bit PCM WAV, one per dictation.\n\
\n\
These recordings may contain sensitive or patient information. Keep them on\n\
machines you control. See docs/training-data.md in the Fairspoken repository.\n";

fn create_private_dir(path: &Path) -> std::io::Result<()> {
    fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn write_wav(path: &Path, samples: &[i16]) -> Result<(), String> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: SAMPLE_RATE,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer =
        hound::WavWriter::create(path, spec).map_err(|err| format!("Could not write audio: {err}"))?;
    for sample in samples {
        writer
            .write_sample(*sample)
            .map_err(|err| format!("Could not write audio: {err}"))?;
    }
    writer
        .finalize()
        .map_err(|err| format!("Could not write audio: {err}"))
}

fn write_json(path: &Path, entry: &CaptureEntry) -> Result<(), String> {
    let payload = serde_json::to_string_pretty(entry).map_err(|err| err.to_string())?;
    fs::write(path, payload).map_err(|err| format!("Could not write {path:?}: {err}"))
}

fn directory_size(path: &Path) -> u64 {
    let Ok(entries) = fs::read_dir(path) else {
        return 0;
    };
    entries
        .flatten()
        .map(|entry| match entry.metadata() {
            Ok(metadata) if metadata.is_dir() => directory_size(&entry.path()),
            Ok(metadata) => metadata.len(),
            Err(_) => 0,
        })
        .sum()
}

fn to_16khz(recording: &Recording) -> Vec<i16> {
    crate::transcription::resample_i16_to_16khz_f32(&recording.pcm_i16, recording.sample_rate)
        .into_iter()
        .map(|sample| (sample * 32768.0).round().clamp(-32768.0, 32767.0) as i16)
        .collect()
}

fn default_root() -> Option<PathBuf> {
    if let Some(path) = crate::app_dirs::env_var_os("FAIRSPOKEN_TRAINING_DATA_DIR") {
        return Some(PathBuf::from(path));
    }
    crate::app_dirs::data_dir().map(|dir| dir.join("training-data"))
}

/// Serialises file access between the commit path, the edit watcher, the
/// sweeper and the Settings commands.
static LOCK: Mutex<()> = Mutex::new(());

fn with_store<R>(use_store: impl FnOnce(&CaptureStore) -> R) -> Result<R, String> {
    let root = default_root().ok_or("No data directory for training data")?;
    let _guard = LOCK.lock().map_err(|_| "Training data lock failed".to_string())?;
    Ok(use_store(&CaptureStore::new(root)))
}

fn now_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0)
}

/// Keeps this dictation when capture is on, writing it in the background.
/// Returns its id so the edit watcher can attach the user's edit later.
pub fn capture_dictation(
    app: &AppHandle,
    settings: &Settings,
    recording: &Recording,
    texts: DictationTexts<'_>,
    app_category: &str,
) -> Option<String> {
    if !settings.training_capture {
        return None;
    }
    let mut entry = CaptureEntry {
        id: uuid::Uuid::new_v4().to_string(),
        created_at: now_seconds(),
        duration_seconds: 0.0,
        speech_model: settings.speech_model_id(),
        app_category: app_category.to_string(),
        raw_text: texts.raw.to_string(),
        polished_text: texts.polished.map(str::to_string),
        final_text: texts.final_text.to_string(),
        edited_text: None,
    };
    let id = entry.id.clone();
    let retention_days = settings.training_retention_days;
    let app = app.clone();
    // The copy is cheap; the resample runs on the writer thread so it never
    // delays the paste.
    let recording = recording.clone();
    let _ = std::thread::Builder::new()
        .name("training-capture".to_string())
        .spawn(move || {
            let samples = to_16khz(&recording);
            drop(recording);
            entry.duration_seconds = samples.len() as f32 / SAMPLE_RATE as f32;
            let result = with_store(|store| {
                store.record(&entry, &samples)?;
                store.prune(retention_days, entry.created_at)
            });
            if let Err(err) | Ok(Err(err)) = result {
                crate::emit_backend_event(&app, "warning", format!("Could not keep dictation: {err}"));
            }
        });
    Some(id)
}

/// Records the user's edit of a kept dictation, if it is still kept.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn attach_edit(id: &str, edited: &str) {
    let _ = with_store(|store| store.attach_edit(id, edited));
}

fn retention_days(app: &AppHandle) -> u16 {
    app.state::<crate::AppServices>()
        .settings
        .lock()
        .map(|service| service.current().training_retention_days)
        .unwrap_or(0)
}

/// Applies retention while the app runs, so old dictations go even when no
/// new ones are kept.
pub fn start_retention_sweeper(app: AppHandle) {
    let _ = std::thread::Builder::new()
        .name("training-retention-sweeper".to_string())
        .spawn(move || loop {
            let days = retention_days(&app);
            let _ = with_store(|store| store.prune(days, now_seconds()));
            std::thread::sleep(Duration::from_secs(60 * 60));
        });
}

/// Prunes and sizes the folder off the main thread.
#[tauri::command]
pub async fn get_training_data_summary(app: AppHandle) -> Result<CaptureSummary, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let days = retention_days(&app);
        with_store(|store| {
            store.prune(days, now_seconds())?;
            Ok(store.summary())
        })?
    })
    .await
    .map_err(|err| err.to_string())?
}

#[tauri::command]
pub async fn delete_training_data() -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(|| with_store(CaptureStore::delete_all)?)
        .await
        .map_err(|err| err.to_string())?
}

/// Exports to the Downloads folder and reveals the zip. Returns its path.
#[tauri::command]
pub async fn export_training_data() -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(|| {
        let folder = std::env::var_os("HOME")
            .map(|home| PathBuf::from(home).join("Downloads"))
            .filter(|downloads| downloads.is_dir())
            .or_else(crate::app_dirs::data_dir)
            .ok_or("No folder to export to")?;
        let destination = folder.join(format!("fairspoken-training-data-{}.zip", now_seconds()));
        let count = with_store(|store| store.export(&destination))??;
        if count == 0 {
            let _ = fs::remove_file(&destination);
            return Err("There are no kept dictations to export".to_string());
        }
        let _ = tauri_plugin_opener::reveal_item_in_dir(&destination);
        Ok(destination.to_string_lossy().into_owned())
    })
    .await
    .map_err(|err| err.to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: &str, created_at: u64) -> CaptureEntry {
        CaptureEntry {
            id: id.to_string(),
            created_at,
            duration_seconds: 0.5,
            speech_model: "parakeet-tdt-0.6b-v3".to_string(),
            app_category: "docs".to_string(),
            raw_text: "ask cloud code".to_string(),
            polished_text: Some("Ask cloud code.".to_string()),
            final_text: "Ask cloud code.".to_string(),
            edited_text: None,
        }
    }

    #[test]
    fn index_round_trips_and_records_edits() {
        let directory = tempfile::tempdir().unwrap();
        let store = CaptureStore::new(directory.path().join("training-data"));
        store.record(&entry("a", 100), &[0, 1000, -1000]).unwrap();
        store.record(&entry("b", 200), &[0; 8000]).unwrap();

        assert_eq!(store.entries().iter().map(|e| e.id.as_str()).collect::<Vec<_>>(), ["a", "b"]);
        let audio = hound::WavReader::open(directory.path().join("training-data/b/audio.wav")).unwrap();
        assert_eq!(audio.spec().sample_rate, 16_000);
        assert_eq!(audio.spec().channels, 1);
        assert_eq!(audio.len(), 8000);

        // An unchanged edit is not recorded; a real one updates index and meta.
        assert!(!store.attach_edit("a", " Ask cloud code. ").unwrap());
        assert!(store.attach_edit("a", "Ask Claude Code.").unwrap());
        assert!(!store.attach_edit("missing", "anything").unwrap());
        assert_eq!(store.entries()[0].edited_text.as_deref(), Some("Ask Claude Code."));
        let meta: CaptureEntry =
            serde_json::from_str(&fs::read_to_string(directory.path().join("training-data/a/meta.json")).unwrap())
                .unwrap();
        assert_eq!(meta.edited_text.as_deref(), Some("Ask Claude Code."));

        // A damaged line does not lose the rest of the index.
        let mut index = fs::OpenOptions::new()
            .append(true)
            .open(directory.path().join("training-data/index.jsonl"))
            .unwrap();
        index.write_all(b"{not json\n").unwrap();
        assert_eq!(store.entries().len(), 2);
        assert_eq!(store.summary().count, 2);
        assert!(store.summary().bytes > 16_000);
    }

    #[test]
    fn retention_prunes_only_expired_dictations() {
        let directory = tempfile::tempdir().unwrap();
        let store = CaptureStore::new(directory.path().to_path_buf());
        let now = 100 * SECONDS_PER_DAY;
        store.record(&entry("old", now - 31 * SECONDS_PER_DAY), &[0; 10]).unwrap();
        store.record(&entry("recent", now - 29 * SECONDS_PER_DAY), &[0; 10]).unwrap();

        assert_eq!(store.prune(0, now).unwrap(), 0);
        assert_eq!(store.entries().len(), 2);
        assert_eq!(store.prune(90, now).unwrap(), 0);
        assert_eq!(store.prune(30, now).unwrap(), 1);

        assert_eq!(store.entries().iter().map(|e| e.id.as_str()).collect::<Vec<_>>(), ["recent"]);
        assert!(!directory.path().join("old").exists());
        assert!(directory.path().join("recent/audio.wav").exists());
    }

    #[test]
    fn export_writes_manifest_polish_pairs_and_audio() {
        use std::io::Read;
        let directory = tempfile::tempdir().unwrap();
        let store = CaptureStore::new(directory.path().join("data"));
        store.record(&entry("a", 1), &[0; 160]).unwrap();
        store.attach_edit("a", "Ask Claude Code.").unwrap();
        let destination = directory.path().join("export.zip");

        assert_eq!(store.export(&destination).unwrap(), 1);

        let mut zip = zip::ZipArchive::new(fs::File::open(&destination).unwrap()).unwrap();
        let mut manifest = String::new();
        zip.by_name("manifest.jsonl").unwrap().read_to_string(&mut manifest).unwrap();
        let line: serde_json::Value = serde_json::from_str(manifest.trim()).unwrap();
        assert_eq!(line["audio_filepath"], "audio/a.wav");
        assert_eq!(line["text"], "Ask Claude Code.");
        assert_eq!(line["raw_text"], "ask cloud code");
        let mut polish = String::new();
        zip.by_name("polish.jsonl").unwrap().read_to_string(&mut polish).unwrap();
        let pair: serde_json::Value = serde_json::from_str(polish.trim()).unwrap();
        assert_eq!(pair["prompt"], "ask cloud code");
        assert_eq!(pair["completion"], "Ask Claude Code.");
        assert!(zip.by_name("audio/a.wav").unwrap().size() > 44);

        store.delete_all().unwrap();
        assert_eq!(store.summary().count, 0);
        store.delete_all().unwrap();
    }

    #[test]
    fn audio_is_resampled_to_16khz() {
        let recording = Recording {
            pcm_i16: vec![1000; 48_000],
            sample_rate: 48_000,
            dropped_stream_frames: 0,
        };
        let samples = to_16khz(&recording);
        assert_eq!(samples.len(), 16_000);
        assert!(samples.iter().all(|sample| (*sample - 1000).abs() <= 1));
    }
}
