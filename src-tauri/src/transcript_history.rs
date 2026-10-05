use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_TRANSCRIPTS: usize = 50;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptHistoryItem {
    pub id: String,
    pub created_at: u64,
    pub text: String,
    pub backend: String,
    pub location: String,
    pub duration_seconds: f32,
    /// True when the stored text went through the AI polish pass.
    #[serde(default)]
    pub polished: bool,
    /// The pre-polish transcript, kept only when polish changed the text so
    /// the original is always recoverable ("Copy original" / pill Undo).
    #[serde(default)]
    pub raw_text: Option<String>,
    /// Absent on dictations recorded before timings existed.
    #[serde(default)]
    pub timings: Option<DictationTimings>,
}

/// Where the wait between releasing the key and seeing text went.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DictationTimings {
    /// Wall time from release until the final raw transcript was ready.
    pub transcribe_ms: u64,
    /// Time the speech model spent computing over the whole dictation, most of
    /// it while the user was still talking.
    pub speech_model_ms: u64,
    /// Wall time of the polish pass at release; 0 when no pass ran.
    pub polish_ms: u64,
    /// Release to text ready for insertion.
    pub total_ms: u64,
}

#[derive(Clone, Debug)]
pub struct NewTranscriptHistoryItem {
    pub text: String,
    pub backend: String,
    pub location: String,
    pub duration_seconds: f32,
    pub polished: bool,
    pub raw_text: Option<String>,
    pub timings: Option<DictationTimings>,
}

pub struct TranscriptHistoryService {
    items: Vec<TranscriptHistoryItem>,
    path: PathBuf,
}

impl Default for TranscriptHistoryService {
    fn default() -> Self {
        let path = default_history_path();
        let items = fs::read_to_string(&path)
            .ok()
            .and_then(|raw| serde_json::from_str::<Vec<TranscriptHistoryItem>>(&raw).ok())
            .map(normalize_items)
            .unwrap_or_default();

        Self { items, path }
    }
}

impl TranscriptHistoryService {
    pub fn list(&self) -> Vec<TranscriptHistoryItem> {
        self.items.clone()
    }

    pub fn add(&mut self, item: NewTranscriptHistoryItem) -> Result<TranscriptHistoryItem, String> {
        let text = item.text.trim().to_string();
        if text.is_empty() {
            return Err("Transcript history item cannot be empty".to_string());
        }

        let normalized_text = normalize_text(&text);
        self.items
            .retain(|existing| normalize_text(&existing.text) != normalized_text);
        let created_at = current_epoch_millis();
        let stored = TranscriptHistoryItem {
            id: format!("transcript-{created_at}"),
            created_at,
            text,
            backend: item.backend,
            location: item.location,
            duration_seconds: item.duration_seconds,
            polished: item.polished,
            raw_text: item.raw_text.filter(|raw| !raw.trim().is_empty()),
            timings: item.timings,
        };
        self.items.insert(0, stored.clone());
        self.items.truncate(MAX_TRANSCRIPTS);
        self.save()?;
        Ok(stored)
    }

    pub fn find(&self, id: &str) -> Option<TranscriptHistoryItem> {
        self.items.iter().find(|item| item.id == id).cloned()
    }

    pub fn delete(&mut self, id: &str) -> Result<(), String> {
        let original_len = self.items.len();
        self.items.retain(|item| item.id != id);
        if self.items.len() == original_len {
            return Err("Transcript history item was not found".to_string());
        }
        self.save()
    }

    pub fn clear(&mut self) -> Result<(), String> {
        self.items.clear();
        self.save()
    }

    fn save(&self) -> Result<(), String> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)
                .map_err(|err| format!("Failed to create transcript history directory: {err}"))?;
        }

        let payload = serde_json::to_string_pretty(&self.items)
            .map_err(|err| format!("Failed to serialize transcript history: {err}"))?;
        fs::write(&self.path, payload)
            .map_err(|err| format!("Failed to write transcript history: {err}"))
    }
}

fn normalize_items(items: Vec<TranscriptHistoryItem>) -> Vec<TranscriptHistoryItem> {
    let mut normalized = Vec::new();
    for item in items {
        let text = item.text.trim();
        let normalized_text = normalize_text(text);
        if text.is_empty()
            || normalized.iter().any(|existing: &TranscriptHistoryItem| {
                normalize_text(&existing.text) == normalized_text
            })
        {
            continue;
        }
        normalized.push(TranscriptHistoryItem {
            text: text.to_string(),
            ..item
        });
        if normalized.len() >= MAX_TRANSCRIPTS {
            break;
        }
    }
    normalized
}

fn normalize_text(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn current_epoch_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or_default()
}

fn default_history_path() -> PathBuf {
    if let Some(path) = crate::app_dirs::env_var_os("FAIRSPOKEN_HISTORY_PATH") {
        return PathBuf::from(path);
    }

    crate::app_dirs::config_dir()
        .map(|dir| dir.join("transcript-history.json"))
        .unwrap_or_else(|| PathBuf::from("transcript-history.json"))
}

#[cfg(test)]
mod tests {
    use super::{normalize_items, TranscriptHistoryItem, MAX_TRANSCRIPTS};

    #[test]
    fn normalize_items_removes_empty_and_duplicate_text() {
        let items = vec![
            item("a", "First"),
            item("b", " "),
            item("c", "First"),
            item("d", "Second"),
        ];

        let normalized = normalize_items(items);

        assert_eq!(normalized.len(), 2);
        assert_eq!(normalized[0].id, "a");
        assert_eq!(normalized[1].id, "d");
    }

    #[test]
    fn normalize_items_caps_history() {
        let items = (0..MAX_TRANSCRIPTS + 5)
            .map(|index| item(&format!("{index}"), &format!("Transcript {index}")))
            .collect::<Vec<_>>();

        assert_eq!(normalize_items(items).len(), MAX_TRANSCRIPTS);
    }

    fn item(id: &str, text: &str) -> TranscriptHistoryItem {
        TranscriptHistoryItem {
            id: id.to_string(),
            created_at: 1,
            text: text.to_string(),
            backend: "whisper".to_string(),
            location: "local".to_string(),
            duration_seconds: 1.0,
            polished: false,
            raw_text: None,
            timings: None,
        }
    }

    #[test]
    fn legacy_history_json_loads_without_polish_fields() {
        let legacy = r#"[{
            "id": "transcript-1",
            "createdAt": 1,
            "text": "Hello world",
            "backend": "parakeet",
            "location": "local",
            "durationSeconds": 2.5
        }]"#;

        let items: Vec<TranscriptHistoryItem> =
            serde_json::from_str(legacy).expect("legacy history loads");

        assert!(!items[0].polished);
        assert!(items[0].raw_text.is_none());
    }
}
