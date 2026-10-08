//! Learned dictionary entries: what the edit watcher saw the user fix, and
//! the rules that turn those fixes into vocabulary hints and corrections.
//!
//! Each `heard → intended` pair is kept in `learned.json` with a sighting
//! count and the speech models that produced it. Only the word pair is
//! stored, never the sentence around it. Promotion:
//! - an uncommon capitalised name or term is added to the vocabulary the first
//!   time it is fixed;
//! - a mapping becomes a correction, scoped to those speech models, after it
//!   is seen twice;
//! - a mapping between two medicine-like words, a look-alike/sound-alike
//!   medicine, or a single short word is never applied automatically: it
//!   waits in Suggestions for the user to accept or dismiss.
//!
//! Applied entries are marked `origin: learned` in Settings. Once an item has
//! been applied, accepted or dismissed it is never promoted again, so a
//! learned entry the user deletes stays deleted.

use crate::edit_diff::Mishearing;
use crate::settings::{EntryOrigin, Settings, TranscriptCorrection};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;
use tauri::{AppHandle, Emitter, Manager};

/// Sightings before a word mapping becomes a correction.
const CORRECTION_SIGHTINGS: u32 = 2;
/// Oldest items are forgotten beyond this many.
const MAX_ITEMS: usize = 500;
/// Mirror of the Settings caps, so a full dictionary is left alone.
const MAX_VOCABULARY_HINTS: usize = 50;
const MAX_CORRECTIONS: usize = 100;

const MEDICINE_REASON: &str =
    "Medicine names are never changed automatically. Check this before accepting it.";
const SHORT_WORD_REASON: &str = "Short words are never replaced automatically.";

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum LearnedStatus {
    /// Seen, not (yet) enough to act on.
    #[default]
    Observed,
    /// Added to Settings as a learned correction.
    Applied,
    /// Waiting for the user on the Dictionary screen.
    Suggested,
    Accepted,
    Dismissed,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LearnedItem {
    pub id: String,
    pub heard: String,
    pub intended: String,
    pub count: u32,
    /// Epoch seconds.
    pub first_seen: u64,
    pub last_seen: u64,
    /// `Settings::speech_model_id` of each model that produced `heard`.
    #[serde(default)]
    pub models: Vec<String>,
    /// The `heard → intended` correction's state.
    #[serde(default)]
    pub status: LearnedStatus,
    /// `intended` has been offered to the vocabulary (added, or the
    /// dictionary was full); never offered twice.
    #[serde(default)]
    pub vocabulary: bool,
    /// Why a suggestion was not applied automatically.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// A change to make in Settings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Promotion {
    Vocabulary(String),
    Correction {
        from: String,
        to: String,
        models: Vec<String>,
    },
}

#[derive(Default, Deserialize, Serialize)]
struct LearnedFile {
    #[serde(default)]
    items: Vec<LearnedItem>,
}

pub struct LearnedStore {
    path: PathBuf,
    items: Vec<LearnedItem>,
}

impl LearnedStore {
    pub fn load(path: PathBuf) -> Self {
        let items = fs::read_to_string(&path)
            .ok()
            .and_then(|raw| serde_json::from_str::<LearnedFile>(&raw).ok())
            .map(|file| file.items)
            .unwrap_or_default();
        Self { path, items }
    }

    pub fn save(&self) -> Result<(), String> {
        let payload = serde_json::to_string_pretty(&LearnedFile {
            items: self.items.clone(),
        })
        .map_err(|err| format!("Failed to serialize learned words: {err}"))?;
        crate::app_dirs::write_atomic(&self.path, payload.as_bytes())
            .map_err(|err| format!("Failed to write learned words: {err}"))
    }

    pub fn suggestions(&self) -> Vec<LearnedItem> {
        self.items
            .iter()
            .filter(|item| item.status == LearnedStatus::Suggested)
            .cloned()
            .collect()
    }

    /// Counts one sighting of `fix` from `model` and returns what it now
    /// earns.
    pub fn observe(&mut self, fix: &Mishearing, model: &str, now: u64) -> Vec<Promotion> {
        let heard_key = fix.heard.to_lowercase();
        let index = match self
            .items
            .iter()
            .position(|item| item.heard.to_lowercase() == heard_key && item.intended == fix.intended)
        {
            Some(index) => {
                let item = &mut self.items[index];
                item.count += 1;
                item.last_seen = now;
                index
            }
            None => {
                self.items.push(LearnedItem {
                    id: uuid::Uuid::new_v4().to_string(),
                    heard: fix.heard.clone(),
                    intended: fix.intended.clone(),
                    count: 1,
                    first_seen: now,
                    last_seen: now,
                    models: Vec::new(),
                    status: LearnedStatus::Observed,
                    vocabulary: false,
                    reason: None,
                });
                self.items.len() - 1
            }
        };
        let item = &mut self.items[index];
        if !model.is_empty() && !item.models.iter().any(|known| known == model) {
            item.models.push(model.to_string());
        }
        let promotions = review(item);
        self.forget_oldest();
        promotions
    }

    /// Marks a suggestion accepted and returns the correction to apply.
    pub fn accept(&mut self, id: &str) -> Option<Promotion> {
        let item = self
            .items
            .iter_mut()
            .find(|item| item.id == id && item.status == LearnedStatus::Suggested)?;
        item.status = LearnedStatus::Accepted;
        Some(Promotion::Correction {
            from: item.heard.clone(),
            to: item.intended.clone(),
            models: item.models.clone(),
        })
    }

    pub fn dismiss(&mut self, id: &str) -> bool {
        match self.items.iter_mut().find(|item| item.id == id) {
            Some(item) => {
                item.status = LearnedStatus::Dismissed;
                true
            }
            None => false,
        }
    }

    fn forget_oldest(&mut self) {
        while self.items.len() > MAX_ITEMS {
            // Suggestions wait for the user, so only settled items are dropped.
            let oldest = self
                .items
                .iter()
                .enumerate()
                .filter(|(_, item)| item.status != LearnedStatus::Suggested)
                .min_by_key(|(_, item)| item.last_seen)
                .map(|(index, _)| index);
            match oldest {
                Some(index) => {
                    self.items.remove(index);
                }
                None => break,
            }
        }
    }
}

/// Applies the promotion rules to an item that was just seen again.
fn review(item: &mut LearnedItem) -> Vec<Promotion> {
    let mut promotions = Vec::new();
    let medicine = involves_medicine(&item.heard, &item.intended);
    if !item.vocabulary && !medicine && is_uncommon_term(&item.intended) {
        item.vocabulary = true;
        promotions.push(Promotion::Vocabulary(item.intended.clone()));
    }

    // A case-only fix ("niamh" → "Niamh") is a vocabulary matter: corrections
    // match case-insensitively, so one would rewrite every occurrence.
    if item.status != LearnedStatus::Observed
        || item.heard.to_lowercase() == item.intended.to_lowercase()
    {
        return promotions;
    }
    if medicine {
        item.status = LearnedStatus::Suggested;
        item.reason = Some(MEDICINE_REASON.to_string());
    } else if item.count >= CORRECTION_SIGHTINGS {
        if is_short_single_word(&item.heard) {
            item.status = LearnedStatus::Suggested;
            item.reason = Some(SHORT_WORD_REASON.to_string());
        } else {
            item.status = LearnedStatus::Applied;
            promotions.push(Promotion::Correction {
                from: item.heard.clone(),
                to: item.intended.clone(),
                models: item.models.clone(),
            });
        }
    }
    promotions
}

/// Applies promotions to the dictionary, leaving anything the user already
/// has alone. Returns a short description of each change made.
pub fn apply_promotions(settings: &mut Settings, promotions: &[Promotion]) -> Vec<String> {
    let mut changes = Vec::new();
    for promotion in promotions {
        match promotion {
            Promotion::Vocabulary(term) => {
                let known = settings
                    .vocabulary_hints
                    .iter()
                    .any(|hint| hint.to_lowercase() == term.to_lowercase());
                if known || settings.vocabulary_hints.len() >= MAX_VOCABULARY_HINTS {
                    continue;
                }
                settings.vocabulary_hints.push(term.clone());
                settings.learned_vocabulary_hints.push(term.clone());
                changes.push(format!("added \"{term}\" to your vocabulary"));
            }
            Promotion::Correction { from, to, models } => {
                let known = settings
                    .transcript_corrections
                    .iter()
                    .any(|correction| correction.from.to_lowercase() == from.to_lowercase());
                if known || settings.transcript_corrections.len() >= MAX_CORRECTIONS {
                    continue;
                }
                settings.transcript_corrections.push(TranscriptCorrection {
                    from: from.clone(),
                    to: to.clone(),
                    origin: EntryOrigin::Learned,
                    models: models.clone(),
                    ..TranscriptCorrection::default()
                });
                changes.push(format!("now corrects \"{from}\" to \"{to}\""));
            }
        }
    }
    changes
}

fn is_short_single_word(phrase: &str) -> bool {
    !phrase.contains(char::is_whitespace) && phrase.chars().count() <= 3
}

/// A name, place, product or identifier worth biasing towards: some word is
/// capitalised, mixed-case or identifier-like, and is not an everyday word
/// that is merely capitalised at a sentence start.
fn is_uncommon_term(phrase: &str) -> bool {
    phrase.split_whitespace().any(|word| {
        let word = word.trim_matches(|c: char| !c.is_alphanumeric());
        let capitalised = word.chars().any(char::is_uppercase);
        let identifier = word.contains(['_', '.'])
            || (word.chars().any(|c| c.is_ascii_digit()) && word.chars().any(char::is_alphabetic));
        word.chars().count() >= 2
            && (capitalised || identifier)
            && !is_common_word(&word.to_lowercase())
    })
}

/// Everyday English words, including those that start sentences. A short
/// list, not a frequency dictionary: it only has to stop sentence-initial
/// capitals from looking like names.
const COMMON_WORDS: &str = "a about above after again against all also am an and any are aren't as \
    at back be because been before being below between both but by can can't cannot could couldn't \
    did didn't do does doesn't doing don't down during each even every few first for from further \
    get gets go goes going good got great had hadn't has hasn't have haven't having he he'd he'll \
    he's her here here's hers herself hi him himself his how how's however i i'd i'll i'm i've if in \
    into is isn't it it's its itself just know last let let's like look made make many may me might \
    more most much must mustn't my myself need never new next no nor not now of off ok okay on once \
    one only or other ought our ours ourselves out over own please really right said same say see \
    shall shan't she she'd she'll she's should shouldn't so some still such sure take than thank \
    thanks that that's the their theirs them themselves then there there's these they they'd \
    they'll they're they've thing things think this those though through time to today tomorrow \
    too two under until up us very want was wasn't way we we'd we'll we're we've well were weren't \
    what what's when when's where where's which while who who's whom why why's will with won't \
    work would wouldn't yes yesterday yet you you'd you'll you're you've your yours yourself \
    yourselves monday tuesday wednesday thursday friday saturday sunday january february march \
    april june july august september october november december dear hello hey sorry maybe also \
    actually anyway great cheers best regards morning afternoon evening night week year month day";

fn is_common_word(word: &str) -> bool {
    COMMON_WORDS.split_whitespace().any(|common| common == word)
}

/// Medicines whose names are easily confused with another medicine's
/// (look-alike/sound-alike lists). A mapping touching one is never automatic.
const LOOK_ALIKE_MEDICINES: &[&str] = &[
    "hydroxyzine", "hydralazine", "carbamazepine", "carbimazole", "lamotrigine", "lamivudine",
    "prednisone", "prednisolone", "tramadol", "trazodone", "chlorpromazine", "chlorpropamide",
    "clonidine", "clonazepam", "risperidone", "ropinirole", "amlodipine", "amiloride",
    "metformin", "metronidazole", "sertraline", "cetirizine", "zolpidem", "zopiclone",
];

/// Common medicines, mostly the HSE PCRS top prescribing list, for names the
/// suffix rules miss.
const COMMON_MEDICINES: &[&str] = &[
    "levothyroxine", "colecalciferol", "aspirin", "paracetamol", "salbutamol", "co-codamol",
    "folic", "escitalopram", "amoxicillin", "mirtazapine", "pregabalin", "venlafaxine",
    "etofenamate", "macrogol", "quetiapine", "furosemide", "diazepam", "warfarin", "tolterodine",
    "ibuprofen", "insulin", "codeine", "morphine", "citalopram", "tamsulosin", "beclometasone",
    "formoterol", "dapagliflozin", "losartan", "candesartan", "apixaban", "rivaroxaban",
];

/// Endings that mark a word as a medicine (`-prazole`, `-statin`, `-olol`…)
/// or a clinical term (`-aemia`, `-itis`…).
const MEDICAL_SUFFIXES: &[&str] = &[
    "prazole", "azole", "statin", "olol", "pril", "sartan", "dipine", "apine", "mycin", "cillin",
    "oxacin", "cycline", "tidine", "azepam", "zolam", "triptyline", "oxetine", "pramine",
    "gliptin", "gliflozin", "formin", "semide", "thiazide", "codone", "olone", "parin", "xaban",
    "gatran", "ciclovir", "navir", "tinib", "umab", "ximab", "aemia", "emia", "itis", "osis",
    "ectomy", "otomy", "algia", "opathy",
];

fn phrase_words(phrase: &str) -> impl Iterator<Item = String> + '_ {
    phrase
        .split(|c: char| !c.is_alphanumeric() && c != '-')
        .filter(|word| !word.is_empty())
        .map(str::to_lowercase)
}

/// The built-in heuristic plus every medicine in the bundled packs
/// (`vocabulary_packs::is_drug_term`), enabled or not.
fn looks_medical(phrase: &str) -> bool {
    crate::vocabulary_packs::is_drug_term(phrase)
        || phrase_words(phrase).any(|word| {
            crate::vocabulary_packs::is_drug_term(&word)
                || COMMON_MEDICINES.contains(&word.as_str())
                || LOOK_ALIKE_MEDICINES.contains(&word.as_str())
                || (word.chars().count() >= 6
                    && MEDICAL_SUFFIXES.iter().any(|suffix| word.ends_with(suffix)))
                || (word.chars().count() >= 8 && (word.starts_with("hyper") || word.starts_with("hypo")))
        })
}

fn is_look_alike_medicine(phrase: &str) -> bool {
    phrase_words(phrase).any(|word| LOOK_ALIKE_MEDICINES.contains(&word.as_str()))
}

/// The clinical safety rule: silently rewriting one medicine into another is
/// a patient-safety hazard.
fn involves_medicine(heard: &str, intended: &str) -> bool {
    (looks_medical(heard) && looks_medical(intended))
        || is_look_alike_medicine(heard)
        || is_look_alike_medicine(intended)
}

fn default_path() -> PathBuf {
    if let Some(path) = crate::app_dirs::env_var_os("FAIRSPOKEN_LEARNED_PATH") {
        return PathBuf::from(path);
    }
    crate::app_dirs::config_dir()
        .map(|dir| dir.join("learned.json"))
        .unwrap_or_else(|| PathBuf::from("learned.json"))
}

static STORE: Mutex<Option<LearnedStore>> = Mutex::new(None);

fn with_store<R>(update: impl FnOnce(&mut LearnedStore) -> R) -> Result<R, String> {
    let mut store = STORE
        .lock()
        .map_err(|_| "Learned words lock failed".to_string())?;
    Ok(update(store.get_or_insert_with(|| LearnedStore::load(default_path()))))
}

fn now_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0)
}

/// Records fixes the edit watcher saw and applies whatever they earn.
pub fn learn_from_fixes(app: &AppHandle, fixes: &[Mishearing], model: &str) {
    if fixes.is_empty() {
        return;
    }
    let now = now_seconds();
    let promotions = with_store(|store| {
        let promotions: Vec<Promotion> = fixes
            .iter()
            .flat_map(|fix| store.observe(fix, model, now))
            .collect();
        store.save().map(|_| promotions)
    });
    match promotions {
        Ok(Ok(promotions)) => apply_to_settings(app, &promotions),
        Ok(Err(err)) | Err(err) => crate::emit_backend_event(app, "warning", err),
    }
    let _ = app.emit("learned-updated", ());
}

fn apply_to_settings(app: &AppHandle, promotions: &[Promotion]) {
    if promotions.is_empty() {
        return;
    }
    let services = app.state::<crate::AppServices>();
    let saved = services
        .settings
        .lock()
        .map_err(|_| "Settings service lock failed".to_string())
        .and_then(|mut service| {
            let mut settings = service.current();
            let changes = apply_promotions(&mut settings, promotions);
            if changes.is_empty() {
                return Ok(None);
            }
            service.save(settings)?;
            Ok(Some((changes, service.current())))
        });
    match saved {
        Ok(Some((changes, settings))) => {
            let _ = app.emit("settings-updated", &settings);
            crate::emit_backend_event(
                app,
                "info",
                format!("Learned from your edit: {}", changes.join("; ")),
            );
        }
        Ok(None) => {}
        Err(err) => crate::emit_backend_event(app, "warning", err),
    }
}

#[tauri::command]
pub fn get_learned_suggestions() -> Result<Vec<LearnedItem>, String> {
    with_store(|store| store.suggestions())
}

/// Saves the store (and the settings an accept changes) off the main thread.
#[tauri::command]
pub async fn accept_learned_suggestion(app: AppHandle, id: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let promotion = with_store(|store| {
            let promotion = store.accept(&id);
            store.save().map(|_| promotion)
        })??;
        if let Some(promotion) = promotion {
            apply_to_settings(&app, &[promotion]);
        }
        let _ = app.emit("learned-updated", ());
        Ok(())
    })
    .await
    .map_err(|err| err.to_string())?
}

#[tauri::command]
pub async fn dismiss_learned_suggestion(app: AppHandle, id: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        with_store(|store| {
            store.dismiss(&id);
            store.save()
        })??;
        let _ = app.emit("learned-updated", ());
        Ok(())
    })
    .await
    .map_err(|err| err.to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> LearnedStore {
        LearnedStore {
            path: std::env::temp_dir().join(format!("learned-{}.json", uuid::Uuid::new_v4())),
            items: Vec::new(),
        }
    }

    fn fix(heard: &str, intended: &str) -> Mishearing {
        Mishearing {
            heard: heard.to_string(),
            intended: intended.to_string(),
        }
    }

    const MODEL: &str = "parakeet-tdt-0.6b-v3";

    #[test]
    fn uncommon_name_joins_the_vocabulary_on_the_first_fix() {
        let mut store = store();
        assert_eq!(
            store.observe(&fix("Neve", "Niamh"), MODEL, 1),
            [Promotion::Vocabulary("Niamh".into())]
        );
        // A second sighting adds the scoped correction, never the hint again.
        assert_eq!(
            store.observe(&fix("neve", "Niamh"), MODEL, 2),
            [Promotion::Correction {
                from: "Neve".into(),
                to: "Niamh".into(),
                models: vec![MODEL.into()]
            }]
        );
        assert!(store.observe(&fix("Neve", "Niamh"), MODEL, 3).is_empty());
    }

    #[test]
    fn common_word_mapping_needs_two_sightings_and_records_its_models() {
        let mut store = store();
        assert!(store.observe(&fix("their", "there"), MODEL, 1).is_empty());
        assert_eq!(
            store.observe(&fix("their", "there"), "large-v3-turbo", 2),
            [Promotion::Correction {
                from: "their".into(),
                to: "there".into(),
                models: vec![MODEL.into(), "large-v3-turbo".into()]
            }]
        );
    }

    #[test]
    fn sentence_initial_capitals_and_case_only_fixes_are_not_corrections() {
        let mut store = store();
        assert!(store.observe(&fix("Their", "They're"), MODEL, 1).is_empty());
        // "niamh" → "Niamh" earns the hint but never a correction.
        assert_eq!(
            store.observe(&fix("niamh", "Niamh"), MODEL, 1),
            [Promotion::Vocabulary("Niamh".into())]
        );
        assert!(store.observe(&fix("niamh", "Niamh"), MODEL, 2).is_empty());
    }

    #[test]
    fn medicine_pairs_and_look_alikes_are_only_suggested() {
        let mut store = store();
        // Both sides look like medicines (suffix rule).
        assert!(store.observe(&fix("omeprazole", "esomeprazole"), MODEL, 1).is_empty());
        // One side is on the look-alike/sound-alike list.
        assert!(store.observe(&fix("hydroxyzine", "hydralazine"), MODEL, 1).is_empty());
        assert!(store.observe(&fix("hydroxyzine", "hydralazine"), MODEL, 2).is_empty());
        let suggestions = store.suggestions();
        assert_eq!(suggestions.len(), 2);
        assert!(suggestions.iter().all(|item| item.reason.as_deref() == Some(MEDICINE_REASON)));

        // Accepting applies the correction; it is then no longer suggested.
        let id = suggestions[1].id.clone();
        assert_eq!(
            store.accept(&id),
            Some(Promotion::Correction {
                from: "hydroxyzine".into(),
                to: "hydralazine".into(),
                models: vec![MODEL.into()]
            })
        );
        assert!(store.dismiss(&suggestions[0].id));
        assert!(store.suggestions().is_empty());
        assert!(store.observe(&fix("omeprazole", "esomeprazole"), MODEL, 3).is_empty());
    }

    #[test]
    fn a_common_word_heard_for_a_medicine_still_learns() {
        let mut store = store();
        // Only the intended side is a medicine, and not a look-alike one.
        assert!(store.observe(&fix("ram a pill", "ramipril"), MODEL, 1).is_empty());
        assert_eq!(
            store.observe(&fix("ram a pill", "ramipril"), MODEL, 2),
            [Promotion::Correction {
                from: "ram a pill".into(),
                to: "ramipril".into(),
                models: vec![MODEL.into()]
            }]
        );
        assert!(store.suggestions().is_empty());
    }

    #[test]
    fn bundled_pack_medicines_count_as_medical() {
        assert!(looks_medical("Zyloric"));
        assert!(looks_medical("allopurinol"));
    }

    #[test]
    fn short_single_words_wait_for_the_user() {
        let mut store = store();
        store.observe(&fix("an", "and"), MODEL, 1);
        assert!(store.observe(&fix("an", "and"), MODEL, 2).is_empty());
        assert_eq!(store.suggestions()[0].reason.as_deref(), Some(SHORT_WORD_REASON));
    }

    #[test]
    fn promotions_mark_learned_entries_and_respect_the_users_own() {
        let mut settings = Settings {
            transcript_corrections: vec![TranscriptCorrection {
                from: "fair spoken".into(),
                to: "FairSpoken".into(),
                ..TranscriptCorrection::default()
            }],
            ..Settings::default()
        };
        let changes = apply_promotions(
            &mut settings,
            &[
                Promotion::Vocabulary("Niamh".into()),
                Promotion::Vocabulary("niamh".into()),
                Promotion::Correction {
                    from: "Fair Spoken".into(),
                    to: "Fairspoken".into(),
                    models: Vec::new(),
                },
                Promotion::Correction {
                    from: "cloud code".into(),
                    to: "Claude Code".into(),
                    models: vec![MODEL.into()],
                },
            ],
        );
        assert_eq!(changes.len(), 2);
        assert_eq!(settings.vocabulary_hints, ["Niamh"]);
        assert_eq!(settings.learned_vocabulary_hints, ["Niamh"]);
        assert_eq!(settings.transcript_corrections.len(), 2);
        assert_eq!(settings.transcript_corrections[0].origin, EntryOrigin::Manual);
        let learned = &settings.transcript_corrections[1];
        assert_eq!(learned.origin, EntryOrigin::Learned);
        assert_eq!(learned.models, [MODEL]);
    }

    #[test]
    fn store_round_trips_through_its_file() {
        let mut first = store();
        first.observe(&fix("cloud", "Claude"), MODEL, 1);
        first.save().unwrap();
        let reloaded = LearnedStore::load(first.path.clone());
        assert_eq!(reloaded.items.len(), 1);
        assert_eq!(reloaded.items[0].intended, "Claude");
        assert!(reloaded.items[0].vocabulary);
        fs::remove_file(&first.path).unwrap();
        // A missing or unreadable file is an empty store.
        assert!(LearnedStore::load(first.path.clone()).items.is_empty());
    }
}
