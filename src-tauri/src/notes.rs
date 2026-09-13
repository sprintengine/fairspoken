use serde::{Deserialize, Serialize};
use std::cmp::Reverse;
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
    pub fn attach_metadata(
        &self,
        id: &str,
        metadata: &crate::note_debug::NoteMetadata,
    ) -> Result<(), String> {
        if !crate::note_debug::AVAILABLE {
            return Err("Note metadata is development-only".into());
        }
        if self.find(id).is_none() {
            return Err("Note was not found".into());
        }
        let path = self.metadata_path(id)?;
        fs::create_dir_all(path.parent().ok_or("Invalid metadata path")?)
            .map_err(|e| e.to_string())?;
        let partial = path.with_extension("partial");
        let payload = serde_json::to_vec_pretty(metadata).map_err(|e| e.to_string())?;
        use std::io::Write;
        let mut options = fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&partial).map_err(|e| e.to_string())?;
        file.write_all(&payload).map_err(|e| e.to_string())?;
        file.sync_all().map_err(|e| e.to_string())?;
        drop(file);
        fs::rename(partial, path).map_err(|e| e.to_string())
    }
    pub fn metadata(&self, id: &str) -> Result<Option<crate::note_debug::NoteMetadata>, String> {
        if !crate::note_debug::AVAILABLE {
            return Err("Note metadata is development-only".into());
        }
        if self.find(id).is_none() {
            return Err("Note was not found".into());
        }
        let path = self.metadata_path(id)?;
        if !path.exists() {
            return Ok(None);
        }
        let file = fs::File::open(path).map_err(|e| e.to_string())?;
        serde_json::from_reader(file)
            .map(Some)
            .map_err(|e| format!("Could not read note metadata: {e}"))
    }
    fn metadata_path(&self, id: &str) -> Result<PathBuf, String> {
        if id.is_empty() || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
            return Err("Invalid note id".into());
        }
        Ok(self.path.with_extension("debug").join(format!("{id}.json")))
    }
    fn remove_metadata(&self, id: &str) {
        if let Ok(path) = self.metadata_path(id) {
            let _ = fs::remove_file(path);
        }
    }
    /// Pinned notes first, then newest-first within each group.
    pub fn list(&self) -> Vec<Note> {
        let mut ordered = self.notes.clone();
        // Stable sort keeps the stored newest-first order inside each group.
        ordered.sort_by_key(|note| Reverse(note.pinned));
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
        self.remove_metadata(id);
        self.save()
    }

    /// Delete unpinned notes whose last edit is older than the retention
    /// window; 0 keeps notes forever. Returns whether anything was removed.
    pub fn sweep_expired(&mut self, retention_minutes: u32) -> Result<bool, String> {
        if retention_minutes == 0 {
            return Ok(false);
        }
        let cutoff = current_epoch_millis().saturating_sub(u64::from(retention_minutes) * 60_000);
        let original_len = self.notes.len();
        for note in &self.notes {
            if !note.pinned && note.updated_at < cutoff {
                self.remove_metadata(&note.id);
            }
        }
        self.notes
            .retain(|note| note.pinned || note.updated_at >= cutoff);
        if self.notes.len() == original_len {
            return Ok(false);
        }
        self.save()?;
        Ok(true)
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
                self.remove_metadata(&self.notes[index].id);
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
    #[cfg(debug_assertions)]
    fn metadata_is_immutable_recording_evidence_and_follows_note_lifecycle() {
        let dir = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
        let mut notes = NotesService {
            notes: Vec::new(),
            path: dir.join("notes.json"),
        };
        let note = notes.add(new("polished note")).unwrap();
        assert!(notes.metadata(&note.id).unwrap().is_none());
        let trace = crate::note_debug::Trace::new(true).unwrap();
        trace.event(
            "accessibility-harvest",
            serde_json::json!({"windowTexts":["project context"],"extractedTerms":["Project"]}),
        );
        let metadata = trace.finish(
            "raw dictation",
            "polished note",
            "polished note",
            "polished note ",
        );
        notes.attach_metadata(&note.id, &metadata).unwrap();
        notes.update_text(&note.id, "edited later").unwrap();
        assert_eq!(
            notes
                .metadata(&note.id)
                .unwrap()
                .unwrap()
                .saved_text
                .as_deref(),
            Some("polished note")
        );
        let reloaded = NotesService {
            notes: serde_json::from_str(&fs::read_to_string(&notes.path).unwrap()).unwrap(),
            path: notes.path.clone(),
        };
        assert_eq!(
            reloaded
                .metadata(&note.id)
                .unwrap()
                .unwrap()
                .original_transcript
                .as_deref(),
            Some("raw dictation")
        );
        let path = notes.metadata_path(&note.id).unwrap();
        notes.delete(&note.id).unwrap();
        assert!(!path.exists());
        assert!(notes.metadata_path("../outside").is_err());
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    #[cfg(debug_assertions)]
    fn expiry_removes_metadata_and_pinning_retains_it() {
        let dir = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
        let mut notes = NotesService {
            notes: Vec::new(),
            path: dir.join("notes.json"),
        };
        let a = notes.add(new("expired")).unwrap();
        let b = notes.add(new("pinned")).unwrap();
        let metadata = crate::note_debug::Trace::new(true).unwrap().finish(
            "raw",
            "polished",
            "saved",
            "clipboard",
        );
        notes.attach_metadata(&a.id, &metadata).unwrap();
        notes.attach_metadata(&b.id, &metadata).unwrap();
        notes.set_pinned(&b.id, true).unwrap();
        for note in &mut notes.notes {
            note.updated_at = 0;
        }
        notes.sweep_expired(1).unwrap();
        assert!(!notes.metadata_path(&a.id).unwrap().exists());
        assert!(notes.metadata_path(&b.id).unwrap().exists());
        fs::remove_dir_all(dir).unwrap();
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

    #[test]
    fn sweep_expired_removes_old_unpinned_and_keeps_pinned() {
        let mut notes = service();
        let old = notes.add(new("old")).unwrap();
        let pinned = notes.add(new("pinned and old")).unwrap();
        notes.set_pinned(&pinned.id, true).unwrap();
        let fresh = notes.add(new("fresh")).unwrap();

        // Age everything except the fresh note beyond a 10-minute window.
        let stale = current_epoch_millis() - 11 * 60_000;
        for note in &mut notes.notes {
            if note.id != fresh.id {
                note.updated_at = stale;
            }
        }

        assert!(notes.sweep_expired(10).unwrap(), "reports a removal");
        assert!(notes.find(&old.id).is_none(), "stale unpinned note removed");
        assert!(notes.find(&pinned.id).is_some(), "pinned note survives");
        assert!(notes.find(&fresh.id).is_some(), "fresh note survives");
        assert!(!notes.sweep_expired(10).unwrap(), "second sweep is a no-op");
    }

    #[test]
    fn sweep_expired_zero_retention_keeps_everything() {
        let mut notes = service();
        let note = notes.add(new("keep forever")).unwrap();
        notes.notes[0].updated_at = 0;

        assert!(!notes.sweep_expired(0).unwrap());
        assert!(notes.find(&note.id).is_some());
    }

    fn new(text: &str) -> NewNote {
        NewNote {
            text: text.to_string(),
            duration_seconds: 1.0,
        }
    }
}
