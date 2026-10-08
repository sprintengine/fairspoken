//! The edit watcher (macOS): after a dictation lands in a text field, look at
//! that field again a little later and learn from what the user fixed.
//!
//! The field is read just before insertion (text and selection), which gives
//! the text either side of the dictation. It is read again when the next
//! dictation starts, when the frontmost app or focused element changes, or
//! after `WATCH_FOR`, whichever comes first. `edit_diff` finds the dictation
//! by that surrounding text and classifies the edits; fixes go to `learned`.
//!
//! When training capture is on, the edited region is also attached to the
//! kept dictation.
//!
//! Never watched: password managers, secure fields, terminals, fields with
//! no plain-text value, and very long values. Every AX read runs under
//! `with_ax_timeout`, off the main thread. The surrounding text stays in
//! memory until the watch is evaluated and is never persisted.
//!
//! Elsewhere the module compiles to no-ops.

use tauri::AppHandle;

/// The dictation whose field is being watched.
pub struct Dictation {
    /// Exactly what was inserted (after caret adjustment).
    pub inserted: String,
    /// The speech model's text before polish and corrections.
    pub raw: String,
    /// `Settings::speech_model_id` of the model that heard it.
    pub speech_model: String,
    /// The training-capture entry to attach the user's edit to.
    pub capture_id: Option<String>,
}

/// Evaluates the pending watch now, in the background: the user has started
/// another dictation, which may land in the same field.
pub fn flush(app: &AppHandle) {
    #[cfg(target_os = "macos")]
    platform::flush(app);
    #[cfg(not(target_os = "macos"))]
    let _ = app;
}

#[cfg(target_os = "macos")]
pub use platform::{arm, prepare, prepared};

/// Whether `phrase`'s words appear, in order, among `text`'s words.
fn contains_words(text: &str, phrase: &str) -> bool {
    let split = |value: &str| -> Vec<String> {
        value
            .split(|c: char| !c.is_alphanumeric())
            .filter(|word| !word.is_empty())
            .map(str::to_lowercase)
            .collect()
    };
    let (text, phrase) = (split(text), split(phrase));
    !phrase.is_empty() && text.windows(phrase.len()).any(|window| window == phrase)
}

#[cfg(target_os = "macos")]
mod platform {
    use super::{contains_words, Dictation};
    use crate::app_categories::{categorize, AppCategory};
    use crate::edit_diff::{classify, EditOutcome, InsertionAnchor};
    use crate::macos_ax::{self, WatchedElement, WatchedField};
    use crate::settings::Settings;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Condvar, Mutex};
    use std::thread;
    use std::time::{Duration, Instant};
    use tauri::{AppHandle, Manager};

    /// How long after insertion the field is read at the latest.
    const WATCH_FOR: Duration = Duration::from_secs(60);
    /// The poll only asks NSWorkspace for the frontmost app, which is cheap.
    const POLL_EVERY: Duration = Duration::from_secs(1);
    /// Every this many polls, also ask Accessibility whether focus moved.
    const FOCUS_CHECK_POLLS: u32 = 2;

    struct Watch {
        element: WatchedElement,
        pid: i32,
        anchor: InsertionAnchor,
        dictation: Dictation,
        armed_at: Instant,
    }

    static WATCH: Mutex<Option<Watch>> = Mutex::new(None);
    /// Signalled when a watch is armed, so the idle poller can sleep.
    static ARMED: Condvar = Condvar::new();
    static POLLER_STARTED: AtomicBool = AtomicBool::new(false);

    /// The focused field as read just before insertion.
    pub struct Prepared(WatchedField);

    /// Reads the focused field before the dictation is inserted, if learning
    /// or training capture wants the edit and the field may be watched.
    pub fn prepare(settings: &Settings) -> Option<Prepared> {
        if !settings.learn_from_edits && !settings.training_capture {
            return None;
        }
        prepared(crate::with_ax_timeout(macos_ax::snapshot_focused_field).and_then(Result::ok))
    }

    /// `prepare` for a field the commit path already read
    /// (`macos_ax::read_focused`, asked only when `prepare` would read).
    pub fn prepared(field: Option<WatchedField>) -> Option<Prepared> {
        field
            .filter(|field| categorize(&field.bundle_id) != AppCategory::Terminal)
            .map(Prepared)
    }

    /// Starts watching the field once the dictation was inserted into it.
    pub fn arm(app: &AppHandle, prepared: Option<Prepared>, dictation: Dictation) {
        let Some(Prepared(field)) = prepared else {
            return;
        };
        let anchor = InsertionAnchor::from_field(
            &field.value,
            field.selection_start,
            field.selection_length,
            &dictation.inserted,
        );
        let watch = Watch {
            element: field.element,
            pid: field.pid,
            anchor,
            dictation,
            armed_at: Instant::now(),
        };
        let previous = WATCH.lock().ok().and_then(|mut slot| slot.replace(watch));
        ARMED.notify_one();
        if let Some(previous) = previous {
            evaluate_in_background(app, previous);
        }
        start_poller(app);
    }

    pub fn flush(app: &AppHandle) {
        if let Some(watch) = take() {
            evaluate_in_background(app, watch);
        }
    }

    fn take() -> Option<Watch> {
        WATCH.lock().ok().and_then(|mut slot| slot.take())
    }

    fn evaluate_in_background(app: &AppHandle, watch: Watch) {
        let app = app.clone();
        let _ = thread::Builder::new()
            .name("edit-watch-evaluate".to_string())
            .spawn(move || evaluate(&app, watch));
    }

    fn start_poller(app: &AppHandle) {
        if POLLER_STARTED.swap(true, Ordering::SeqCst) {
            return;
        }
        let app = app.clone();
        let started = thread::Builder::new()
            .name("edit-watch-poller".to_string())
            .spawn(move || {
                let mut polls: u32 = 0;
                loop {
                    // Parked while nothing is watched; `arm` wakes it.
                    {
                        let mut slot = WATCH.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                        while slot.is_none() {
                            slot = ARMED.wait(slot).unwrap_or_else(|poisoned| poisoned.into_inner());
                        }
                    }
                    thread::sleep(POLL_EVERY);
                    polls = polls.wrapping_add(1);
                    let pending = WATCH.lock().ok().and_then(|slot| {
                        slot.as_ref()
                            .map(|watch| (watch.element.clone(), watch.pid, watch.armed_at))
                    });
                    let Some((element, pid, armed_at)) = pending else {
                        continue;
                    };
                    let due = armed_at.elapsed() >= WATCH_FOR
                        || macos_ax::frontmost_pid() != Some(pid)
                        || (polls.is_multiple_of(FOCUS_CHECK_POLLS)
                            && crate::with_ax_timeout(move || macos_ax::is_focused(&element))
                                .flatten()
                                == Some(false));
                    if due {
                        if let Some(watch) = take() {
                            evaluate(&app, watch);
                        }
                    }
                }
            });
        if started.is_err() {
            POLLER_STARTED.store(false, Ordering::SeqCst);
        }
    }

    fn evaluate(app: &AppHandle, watch: Watch) {
        let element = watch.element;
        let Some(value) =
            crate::with_ax_timeout(move || macos_ax::read_watched_value(&element)).flatten()
        else {
            return;
        };
        let Some(region) = watch.anchor.locate(&value) else {
            return;
        };
        let outcome = classify(&watch.anchor.inserted, &region);
        if outcome != EditOutcome::Unchanged {
            if let Some(id) = &watch.dictation.capture_id {
                crate::training_capture::attach_edit(id, &region);
            }
        }
        let EditOutcome::Edited { fixes } = outcome else {
            return;
        };
        let learn = app
            .state::<crate::AppServices>()
            .settings
            .lock()
            .map(|service| service.current().learn_from_edits)
            .unwrap_or(false);
        if !learn {
            return;
        }
        for fix in fixes {
            // A word the speech model never wrote came from polish, so the
            // correction is not specific to the speech model.
            let model = if contains_words(&watch.dictation.raw, &fix.heard) {
                watch.dictation.speech_model.as_str()
            } else {
                ""
            };
            crate::learned::learn_from_fixes(app, std::slice::from_ref(&fix), model);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::contains_words;

    #[test]
    fn contains_words_matches_whole_words_in_order() {
        assert!(contains_words("Ask cloud code, please.", "cloud code"));
        assert!(contains_words("ask Cloud Code", "cloud code"));
        assert!(!contains_words("ask cloudy code", "cloud code"));
        assert!(!contains_words("code cloud", "cloud code"));
        assert!(!contains_words("anything", ""));
    }
}
