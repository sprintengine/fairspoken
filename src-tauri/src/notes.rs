use serde::{Deserialize, Serialize};
use std::env;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

// The durable library. Unlike the 50-item transcript shelf this is not
// de-duplicated (saying the same thing twice is legitimate) and is only
// bounded by a generous cap; pinned notes are never evicted.
const MAX_NOTES: usize = 2000;

static NOTE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Note {
    pub id: String,
    pub created_at: u64,
    pub updated_at: u64,
    pub text: String,
    #[serde(default)]
    pub pinned: bool,
    #[serde(default)]
    pub duration_seconds: f32,
}

#[derive(Clone, Debug)]
pub struct NewNote {
    pub text: String,
    pub duration_seconds: f32,
}

pub struct NotesService {
    notes: Vec<Note>,
    path: PathBuf,
}

impl Default for NotesService {
    fn default() -> Self {
        let path = default_notes_path();
        let notes = fs::read_to_string(&path)
            .ok()
            .and_then(|raw| serde_json::from_str::<Vec<Note>>(&raw).ok())
            .unwrap_or_default();
        Self { notes, path }
    }
}

impl NotesService {
    /// Pinned notes first, then newest-first within each group.
    pub fn list(&self) -> Vec<Note> {
        let mut ordered = self.notes.clone();
        // Stable sort keeps the stored newest-first order inside each group.
        ordered.sort_by(|a, b| b.pinned.cmp(&a.pinned));
        ordered
    }

    pub fn add(&mut self, note: NewNote) -> Result<Note, String> {
        let text = note.text.trim().to_string();
        if text.is_empty() {
            return Err("Note cannot be empty".to_string());
        }

        let now = current_epoch_millis();
        let sequence = NOTE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let stored = Note {
            id: format!("note-{now}-{sequence}"),
            created_at: now,
            updated_at: now,
            text,
            pinned: false,
            duration_seconds: note.duration_seconds,
        };
        self.notes.insert(0, stored.clone());
        self.enforce_cap();
        self.save()?;
        Ok(stored)
    }

    pub fn update_text(&mut self, id: &str, text: &str) -> Result<Note, String> {
        let trimmed = text.trim().to_string();
        if trimmed.is_empty() {
            return Err("Note cannot be empty".to_string());
        }

        let note = self
            .notes
            .iter_mut()
            .find(|note| note.id == id)
            .ok_or_else(|| "Note was not found".to_string())?;
        note.text = trimmed;
        note.updated_at = current_epoch_millis();
        let updated = note.clone();
        self.save()?;
        Ok(updated)
    }

    pub fn set_pinned(&mut self, id: &str, pinned: bool) -> Result<Note, String> {
        let note = self
            .notes
            .iter_mut()
            .find(|note| note.id == id)
            .ok_or_else(|| "Note was not found".to_string())?;
        note.pinned = pinned;
        let updated = note.clone();
        self.save()?;
        Ok(updated)
    }

    pub fn find(&self, id: &str) -> Option<Note> {
        self.notes.iter().find(|note| note.id == id).cloned()
    }

    pub fn delete(&mut self, id: &str) -> Result<(), String> {
        let original_len = self.notes.len();
        self.notes.retain(|note| note.id != id);
        if self.notes.len() == original_len {
            return Err("Note was not found".to_string());
        }
        self.save()
    }

    /// Drop the oldest unpinned notes once over the cap; pinned notes stay.
    fn enforce_cap(&mut self) {
        if self.notes.len() <= MAX_NOTES {
            return;
        }
        let mut index = self.notes.len();
        while self.notes.len() > MAX_NOTES && index > 0 {
            index -= 1;
            if !self.notes[index].pinned {
                self.notes.remove(index);
            }
        }
    }

    fn save(&self) -> Result<(), String> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)
                .map_err(|err| format!("Failed to create notes directory: {err}"))?;
        }
        let payload = serde_json::to_string_pretty(&self.notes)
            .map_err(|err| format!("Failed to serialize notes: {err}"))?;
        fs::write(&self.path, payload).map_err(|err| format!("Failed to write notes: {err}"))
    }
}

fn current_epoch_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or_default()
}

fn default_notes_path() -> PathBuf {
    if let Some(path) = env::var_os("MULTIVOICE_TAURI_NOTES_PATH") {
        return PathBuf::from(path);
    }

    if let Some(local_app_data) = env::var_os("LOCALAPPDATA") {
        return PathBuf::from(local_app_data)
            .join("Multivoice Tauri")
            .join("notes.json");
    }

    if let Some(home) = env::var_os("HOME") {
        return PathBuf::from(home)
            .join(".config")
            .join("multivoice-tauri")
            .join("notes.json");
    }

    PathBuf::from("notes.json")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn service() -> NotesService {
        NotesService {
            notes: Vec::new(),
            path: std::env::temp_dir().join("multivoice-notes-test-unused.json"),
        }
    }

    #[test]
    fn add_rejects_empty_text() {
        let mut notes = service();
        assert!(notes
            .add(NewNote {
                text: "   ".to_string(),
                duration_seconds: 1.0,
            })
            .is_err());
        assert!(notes.notes.is_empty());
    }

    #[test]
    fn list_puts_pinned_first_then_newest() {
        let mut notes = service();
        let first = notes.add(new("first")).unwrap();
        let _second = notes.add(new("second")).unwrap();
        let third = notes.add(new("third")).unwrap();

        // Pin the oldest note; it should jump to the top.
        notes.set_pinned(&first.id, true).unwrap();
        let listed = notes.list();

        assert_eq!(listed[0].id, first.id, "pinned note first");
        assert_eq!(listed[1].id, third.id, "then newest unpinned");
        assert_eq!(listed[2].text, "second");
    }

    #[test]
    fn update_text_changes_content_and_rejects_empty() {
        let mut notes = service();
        let note = notes.add(new("before")).unwrap();

        let updated = notes.update_text(&note.id, "  after  ").unwrap();
        assert_eq!(updated.text, "after");
        assert!(updated.updated_at >= note.created_at);
        assert!(notes.update_text(&note.id, "  ").is_err());
        assert!(notes.update_text("missing", "x").is_err());
    }

    #[test]
    fn delete_removes_only_the_target() {
        let mut notes = service();
        let a = notes.add(new("a")).unwrap();
        let b = notes.add(new("b")).unwrap();

        notes.delete(&a.id).unwrap();
        assert!(notes.find(&a.id).is_none());
        assert!(notes.find(&b.id).is_some());
        assert!(notes.delete("missing").is_err());
    }

    #[test]
    fn cap_evicts_oldest_unpinned_but_keeps_pinned() {
        let mut notes = service();
        // One pinned note at the very bottom (oldest).
        let pinned = notes.add(new("keep me")).unwrap();
        notes.set_pinned(&pinned.id, true).unwrap();
        for index in 0..MAX_NOTES + 5 {
            notes.add(new(&format!("note {index}"))).unwrap();
        }

        assert_eq!(notes.notes.len(), MAX_NOTES);
        assert!(notes.find(&pinned.id).is_some(), "pinned note survives cap");
    }

    fn new(text: &str) -> NewNote {
        NewNote {
            text: text.to_string(),
            duration_seconds: 1.0,
        }
    }
}
