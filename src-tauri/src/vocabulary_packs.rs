//! Vocabulary packs: bundled, versioned term lists for a domain (Irish general
//! practice, software engineering) that the user turns on as a whole.
//!
//! A pack is far larger than the user's own dictionary and none of it counts
//! toward that dictionary's cap, so its terms are never all shown to a model:
//! * polish gets the user's spoken terms plus the pack terms the transcript
//!   most plausibly refers to (`polish_vocabulary`, a phonetic lookup);
//! * Whisper's prompt gets the user's terms, then each pack's short
//!   `always_on` list, then terms retrieved from earlier chunks of the same
//!   dictation, inside a conservative token budget (`whisper_prompt`);
//! * a remote host gets the user's terms and the `always_on` lists, capped as
//!   before (`remote_hints`).
//!
//! The same index guards polish output: a pack term nothing in the transcript
//! sounded like was invented, and a medicine name the speaker said that comes
//! back as a different medicine name is never accepted (`check_polish`).

use crate::phonetic_index::{normalize, IndexEntry, PhoneticIndex};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock};

/// The pack file format this build reads.
pub const PACK_SCHEMA_VERSION: u32 = 1;

/// How many terms polish is shown, the user's own spoken terms included.
pub const DEFAULT_RETRIEVAL_LIMIT: usize = 15;

/// Whisper keeps only the last ~224 tokens of its prompt and silently drops
/// the rest — from the front, where the user's own terms are.
pub const WHISPER_PROMPT_TOKEN_BUDGET: usize = 200;

/// The remote host header carried at most this many terms before packs
/// existed; it still does.
pub const REMOTE_HINT_LIMIT: usize = 50;

/// Terms retrieved from earlier chunks that may join the Whisper prompt.
const EARLIER_CHUNK_TERMS: usize = 8;

/// Only the most recent part of a long dictation is searched for them.
const EARLIER_CHUNK_CHARS: usize = 4000;

const WHISPER_PROMPT_PREFIX: &str = "Relevant names and terms: ";

/// "Relevant names and terms:" and the closing period, rounded up.
const WHISPER_PREFIX_TOKENS: usize = 8;

/// The packs shipped in the binary. They are text and compress well, so
/// embedding them costs less than a resource directory and cannot go missing
/// from an install.
const BUNDLED: &[&str] = &[
    include_str!("../packs/ie-general-practice.json"),
    include_str!("../packs/software-engineering.json"),
];

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Category {
    Drug,
    Condition,
    System,
    Organisation,
    Person,
    Place,
    Product,
    Identifier,
    Abbreviation,
    /// Anything else, including categories a newer pack format adds.
    #[default]
    #[serde(other)]
    Term,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Pack {
    pub schema: u32,
    pub id: String,
    pub name: String,
    pub description: String,
    pub version: String,
    #[serde(default)]
    pub locale: String,
    /// The licence the pack file is distributed under.
    pub licence: String,
    /// The attribution its sources require, shown with the pack.
    #[serde(default)]
    pub attribution: String,
    #[serde(default)]
    pub sources: Vec<PackSource>,
    /// The few highest-value terms, sent to speech models up front.
    #[serde(default)]
    pub always_on: Vec<String>,
    pub terms: Vec<PackTerm>,
    #[serde(default)]
    pub corrections: Vec<PackCorrection>,
    #[serde(default)]
    pub snippets: Vec<PackSnippet>,
    /// Reserved for a polish adapter trained on this pack's domain.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub polish_adapter: Option<serde_json::Value>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PackSource {
    pub name: String,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub licence: String,
    #[serde(default)]
    pub used_for: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PackTerm {
    pub term: String,
    /// How the term tends to come out of speech recognition.
    #[serde(default)]
    pub spoken_forms: Vec<String>,
    /// Other written forms that are equally correct (never spoken spellings).
    #[serde(default)]
    pub accept: Vec<String>,
    #[serde(default)]
    pub category: Category,
    #[serde(default = "default_weight")]
    pub weight: f32,
    /// Also an everyday word; retrieved only where the transcript capitalises it.
    #[serde(default)]
    pub ambiguous: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PackCorrection {
    pub from: String,
    pub to: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PackSnippet {
    pub trigger: String,
    pub expansion: String,
}

fn default_weight() -> f32 {
    0.5
}

/// Parses and checks one pack file.
pub fn parse_pack(json: &str) -> Result<Pack, String> {
    let mut pack: Pack =
        serde_json::from_str(json).map_err(|err| format!("invalid pack file: {err}"))?;
    if pack.schema != PACK_SCHEMA_VERSION {
        return Err(format!(
            "pack {} uses format {}; this build reads format {PACK_SCHEMA_VERSION}",
            pack.id, pack.schema
        ));
    }
    let valid_id = !pack.id.is_empty()
        && pack.id.len() <= 64
        && pack
            .id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    if !valid_id {
        return Err(format!(
            "pack id \"{}\" is not lowercase-kebab-case",
            pack.id
        ));
    }
    if pack.name.trim().is_empty() || pack.licence.trim().is_empty() {
        return Err(format!("pack {} needs a name and a licence", pack.id));
    }
    pack.terms.retain(|term| {
        let length = term.term.trim().chars().count();
        length > 0 && length <= 100 && !normalize(&term.term).is_empty()
    });
    if pack.terms.is_empty() {
        return Err(format!("pack {} has no terms", pack.id));
    }
    for term in &mut pack.terms {
        term.term = term.term.trim().to_string();
        term.weight = if term.weight.is_finite() {
            term.weight.clamp(0.0, 1.0)
        } else {
            default_weight()
        };
        term.spoken_forms.retain(|form| !normalize(form).is_empty());
        term.accept.retain(|form| !normalize(form).is_empty());
    }
    let known: HashSet<String> = pack.terms.iter().map(|t| normalize(&t.term)).collect();
    pack.always_on
        .retain(|term| known.contains(&normalize(term)));
    Ok(pack)
}

/// Every pack shipped with the app. A pack that fails to load is left out
/// (and fails the tests) rather than taking the others with it.
pub fn bundled_packs() -> &'static [Pack] {
    static PACKS: OnceLock<Vec<Pack>> = OnceLock::new();
    PACKS.get_or_init(|| {
        BUNDLED
            .iter()
            .filter_map(|json| match parse_pack(json) {
                Ok(pack) => Some(pack),
                Err(err) => {
                    eprintln!("Vocabulary pack skipped: {err}");
                    None
                }
            })
            .collect()
    })
}

/// The enabled pack ids this build knows, deduplicated, in bundled order.
fn known_enabled(enabled: &[String]) -> Vec<String> {
    bundled_packs()
        .iter()
        .filter(|pack| enabled.iter().any(|id| id == &pack.id))
        .map(|pack| pack.id.clone())
        .collect()
}

/// The terms of the enabled packs, ready to search.
pub struct ActivePacks {
    ids: Vec<String>,
    index: PhoneticIndex,
    always_on: Vec<String>,
}

impl ActivePacks {
    fn build(packs: &[&Pack]) -> Self {
        let mut entries: Vec<IndexEntry> = Vec::new();
        let mut by_key: HashMap<String, usize> = HashMap::new();
        for pack in packs {
            for term in &pack.terms {
                let key = normalize(&term.term);
                if let Some(&existing) = by_key.get(&key) {
                    let entry: &mut IndexEntry = &mut entries[existing];
                    entry.weight = entry.weight.max(term.weight);
                    entry.written.extend(term.accept.iter().cloned());
                    entry.spoken.extend(term.spoken_forms.iter().cloned());
                    entry.ambiguous &= term.ambiguous;
                    continue;
                }
                by_key.insert(key, entries.len());
                let mut written = vec![term.term.clone()];
                written.extend(term.accept.iter().cloned());
                entries.push(IndexEntry {
                    term: term.term.clone(),
                    written,
                    spoken: term.spoken_forms.clone(),
                    weight: term.weight,
                    ambiguous: term.ambiguous,
                });
            }
        }
        let mut always_on: Vec<String> = Vec::new();
        for term in packs.iter().flat_map(|pack| &pack.always_on) {
            if !always_on.iter().any(|t| same_term(t, term)) {
                always_on.push(term.clone());
            }
        }
        ActivePacks {
            ids: packs.iter().map(|pack| pack.id.clone()).collect(),
            index: PhoneticIndex::build(entries),
            always_on,
        }
    }

    /// The pack terms `text` most plausibly refers to, best first.
    pub fn retrieve(&self, text: &str, limit: usize) -> Vec<String> {
        self.index
            .retrieve(text, limit)
            .into_iter()
            .map(|hit| self.index.entries()[hit.entry].term.clone())
            .collect()
    }
}

/// The most recently requested set of packs. Rebuilt only when the set
/// changes; the transcription worker, which has no settings, reads it to add
/// terms from earlier chunks.
static ACTIVE: Mutex<Option<Arc<ActivePacks>>> = Mutex::new(None);

/// The index for `enabled`, built on first use and reused until the set of
/// enabled packs changes. `None` when no known pack is enabled.
pub fn active_packs(enabled: &[String]) -> Option<Arc<ActivePacks>> {
    let ids = known_enabled(enabled);
    let mut active = ACTIVE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if ids.is_empty() {
        *active = None;
        return None;
    }
    if let Some(current) = active.as_ref().filter(|current| current.ids == ids) {
        return Some(Arc::clone(current));
    }
    let packs: Vec<&Pack> = bundled_packs()
        .iter()
        .filter(|pack| ids.contains(&pack.id))
        .collect();
    let built = Arc::new(ActivePacks::build(&packs));
    *active = Some(Arc::clone(&built));
    Some(built)
}

fn last_active_packs() -> Option<Arc<ActivePacks>> {
    ACTIVE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
}

fn same_term(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b) || normalize(a) == normalize(b)
}

/// The terms to show polish for `raw`: the user's dictionary terms that
/// something in it sounds like, then the best pack matches, `limit` in all.
/// The user's terms always come first and are never cut for a pack term.
pub fn retrieve_terms(
    raw: &str,
    user_terms: &[String],
    enabled_packs: &[String],
    limit: usize,
) -> Vec<String> {
    let mut terms = crate::transcript_cleanup::relevant_vocabulary(raw, user_terms);
    if terms.len() >= limit {
        return terms;
    }
    if let Some(active) = active_packs(enabled_packs) {
        for term in active.retrieve(raw, limit) {
            if terms.len() >= limit {
                break;
            }
            if !terms.iter().any(|existing| same_term(existing, &term)) {
                terms.push(term);
            }
        }
    }
    terms
}

/// The spelling list for one polish pass, local or cloud.
pub fn polish_vocabulary(raw: &str, settings: &crate::settings::Settings) -> Vec<String> {
    retrieve_terms(
        raw,
        &settings.vocabulary_hints,
        &settings.enabled_packs,
        DEFAULT_RETRIEVAL_LIMIT,
    )
}

/// Why a polished transcript must not replace `raw`, if it must not. Checks
/// the drug-name rule (always) and, for the enabled packs, that every pack
/// term in `edited` was sounded out in `raw`. The reason reads after "polish".
pub fn check_polish(raw: &str, edited: &str, enabled_packs: &[String]) -> Result<(), String> {
    if let Some((said, written)) = drug_substitution(raw, edited) {
        let reason = format!(
            "replaced the medicine \"{said}\" with a different medicine, \"{written}\", so the raw transcript was kept"
        );
        eprintln!("Polish rejected: {reason}");
        return Err(reason);
    }
    if let Some(term) = ungrounded_pack_term(raw, edited, enabled_packs) {
        return Err(format!(
            "inserted the pack term \"{term}\" that was not spoken"
        ));
    }
    Ok(())
}

/// A pack term written in `edited` that nothing in `raw` sounded like.
pub fn ungrounded_pack_term(raw: &str, edited: &str, enabled_packs: &[String]) -> Option<String> {
    let active = active_packs(enabled_packs)?;
    let written = active.index.written_in(edited);
    if written.is_empty() {
        return None;
    }
    let spoken = active.index.sounded(raw);
    written
        .into_iter()
        .find(|entry| !spoken.contains(entry))
        .map(|entry| active.index.entries()[entry].term.clone())
}

// ── Drug lexicon ────────────────────────────────────────────

/// Every medicine name in the bundled packs, whether or not a pack is
/// enabled: one medicine silently becoming another is a patient-safety hazard
/// for anyone who dictates one.
struct DrugLexicon {
    /// Normalised written or accepted form → canonical term.
    forms: HashMap<String, usize>,
    terms: Vec<String>,
    max_words: usize,
}

fn drug_lexicon() -> &'static DrugLexicon {
    static LEXICON: OnceLock<DrugLexicon> = OnceLock::new();
    LEXICON.get_or_init(|| {
        let mut lexicon = DrugLexicon {
            forms: HashMap::new(),
            terms: Vec::new(),
            max_words: 1,
        };
        let drugs = bundled_packs()
            .iter()
            .flat_map(|pack| &pack.terms)
            .filter(|term| term.category == Category::Drug);
        for term in drugs {
            let id = match lexicon.forms.get(&normalize(&term.term)) {
                Some(&id) => id,
                None => {
                    lexicon.terms.push(term.term.clone());
                    lexicon.terms.len() - 1
                }
            };
            for form in std::iter::once(&term.term).chain(&term.accept) {
                let words = form
                    .split(|c: char| !c.is_alphanumeric())
                    .filter(|w| !w.is_empty())
                    .count();
                lexicon.max_words = lexicon.max_words.max(words);
                lexicon.forms.entry(normalize(form)).or_insert(id);
            }
        }
        lexicon
    })
}

/// Whether `term` is a medicine name (a substance or brand) in the bundled
/// drug lexicon. Case, spacing, hyphens and accents do not matter. For callers
/// that must never auto-apply a drug-to-drug mapping (learned corrections).
#[allow(dead_code)]
pub fn is_drug_term(term: &str) -> bool {
    drug_lexicon().forms.contains_key(&normalize(term))
}

/// Every written and accepted form of every medicine in the bundled packs,
/// for a caller that keeps its own list (`learned::register_drug_lexicon`).
#[allow(dead_code)]
pub fn drug_lexicon_terms() -> Vec<String> {
    bundled_packs()
        .iter()
        .flat_map(|pack| &pack.terms)
        .filter(|term| term.category == Category::Drug)
        .flat_map(|term| std::iter::once(&term.term).chain(&term.accept))
        .cloned()
        .collect()
}

/// The medicines `text` names exactly, as their canonical terms, in order of
/// first mention.
#[allow(dead_code)]
pub fn drug_terms_in(text: &str) -> Vec<String> {
    drug_ids_in(text)
        .into_iter()
        .map(|id| drug_lexicon().terms[id].clone())
        .collect()
}

fn drug_ids_in(text: &str) -> Vec<usize> {
    let lexicon = drug_lexicon();
    let words: Vec<String> = text
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(normalize)
        .collect();
    let mut found = Vec::new();
    for start in 0..words.len() {
        let mut joined = String::new();
        for word in words.iter().skip(start).take(lexicon.max_words) {
            joined.push_str(word);
            if let Some(&id) = lexicon.forms.get(&joined) {
                if !found.contains(&id) {
                    found.push(id);
                }
            }
        }
    }
    found
}

/// A medicine named in `raw` that `edited` no longer names, while `edited`
/// names one `raw` did not: the polish turned one drug into another. A
/// medicine merely dropped (a self-correction) or a misheard word turned into
/// a medicine is not this.
fn drug_substitution(raw: &str, edited: &str) -> Option<(String, String)> {
    let said = drug_ids_in(raw);
    let written = drug_ids_in(edited);
    let removed = said.iter().find(|id| !written.contains(id))?;
    let added = written.iter().find(|id| !said.contains(id))?;
    let terms = &drug_lexicon().terms;
    Some((terms[*removed].clone(), terms[*added].clone()))
}

// ── Speech model prompts ────────────────────────────────────

/// A conservative token estimate for Whisper's tokenizer: rare words and
/// non-Latin scripts split into many pieces, so two tokens per five bytes,
/// plus one for the separator.
fn term_tokens(term: &str) -> usize {
    (term.len() * 2).div_ceil(5) + 1
}

fn render_whisper_prompt(terms: &[String]) -> Option<String> {
    (!terms.is_empty()).then(|| format!("{WHISPER_PROMPT_PREFIX}{}.", terms.join(", ")))
}

/// Appends `candidates` to `terms` in order while they fit the budget.
fn fill_budget<'a>(
    terms: &mut Vec<String>,
    used: &mut usize,
    candidates: impl IntoIterator<Item = &'a String>,
) {
    for term in candidates {
        let cost = term_tokens(term);
        if *used + cost > WHISPER_PROMPT_TOKEN_BUDGET || terms.iter().any(|t| same_term(t, term)) {
            continue;
        }
        *used += cost;
        terms.push(term.clone());
    }
}

/// Whisper's initial prompt: the user's terms first, then the enabled packs'
/// `always_on` terms, as many as fit the token budget.
pub fn whisper_prompt(user_terms: &[String], enabled_packs: &[String]) -> Option<String> {
    let mut terms = Vec::new();
    let mut used = WHISPER_PREFIX_TOKENS;
    fill_budget(&mut terms, &mut used, user_terms);
    if let Some(active) = active_packs(enabled_packs) {
        fill_budget(&mut terms, &mut used, &active.always_on);
    }
    render_whisper_prompt(&terms)
}

/// `base` (a `whisper_prompt`) plus the pack terms retrieved from what the
/// dictation has said so far, within the same budget.
pub fn extend_whisper_prompt(base: &str, earlier_text: &str) -> String {
    let Some(active) = last_active_packs() else {
        return base.to_string();
    };
    let Some(list) = base
        .strip_prefix(WHISPER_PROMPT_PREFIX)
        .and_then(|rest| rest.strip_suffix('.'))
    else {
        return base.to_string();
    };
    let start = earlier_text
        .char_indices()
        .rev()
        .nth(EARLIER_CHUNK_CHARS)
        .map_or(0, |(index, _)| index);
    let retrieved = active.retrieve(&earlier_text[start..], EARLIER_CHUNK_TERMS);
    if retrieved.is_empty() {
        return base.to_string();
    }
    let mut terms: Vec<String> = list.split(", ").map(str::to_string).collect();
    let mut used = WHISPER_PREFIX_TOKENS + terms.iter().map(|t| term_tokens(t)).sum::<usize>();
    fill_budget(&mut terms, &mut used, &retrieved);
    render_whisper_prompt(&terms).unwrap_or_else(|| base.to_string())
}

/// The terms sent to a remote host before anything has been said: the
/// user's own, then the packs' `always_on` lists.
pub fn remote_hints(user_terms: &[String], enabled_packs: &[String]) -> Vec<String> {
    let mut hints: Vec<String> = user_terms.iter().take(REMOTE_HINT_LIMIT).cloned().collect();
    if let Some(active) = active_packs(enabled_packs) {
        for term in &active.always_on {
            if hints.len() >= REMOTE_HINT_LIMIT {
                break;
            }
            if !hints.iter().any(|t| same_term(t, term)) {
                hints.push(term.clone());
            }
        }
    }
    hints
}

// ── Commands for the Dictionary screen ──────────────────────

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PackSummary {
    id: String,
    name: String,
    description: String,
    version: String,
    licence: String,
    attribution: String,
    sources: Vec<PackSource>,
    term_count: usize,
    always_on: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PackTermPreview {
    term: String,
    category: Category,
    spoken_forms: Vec<String>,
}

#[tauri::command]
pub fn list_vocabulary_packs() -> Vec<PackSummary> {
    bundled_packs()
        .iter()
        .map(|pack| PackSummary {
            id: pack.id.clone(),
            name: pack.name.clone(),
            description: pack.description.clone(),
            version: pack.version.clone(),
            licence: pack.licence.clone(),
            attribution: pack.attribution.clone(),
            sources: pack.sources.clone(),
            term_count: pack.terms.len(),
            always_on: pack.always_on.clone(),
        })
        .collect()
}

/// Up to `limit` of a pack's terms whose written or spoken form contains
/// `query` (all terms, in pack order, for an empty query).
#[tauri::command]
pub fn search_vocabulary_pack(
    id: String,
    query: Option<String>,
    limit: Option<usize>,
) -> Result<Vec<PackTermPreview>, String> {
    let pack = bundled_packs()
        .iter()
        .find(|pack| pack.id == id)
        .ok_or_else(|| format!("Unknown vocabulary pack: {id}"))?;
    let query = normalize(query.as_deref().unwrap_or(""));
    let limit = limit.unwrap_or(100).clamp(1, 500);
    Ok(pack
        .terms
        .iter()
        .filter(|term| {
            query.is_empty()
                || std::iter::once(&term.term)
                    .chain(&term.accept)
                    .chain(&term.spoken_forms)
                    .any(|form| normalize(form).contains(&query))
        })
        .take(limit)
        .map(|term| PackTermPreview {
            term: term.term.clone(),
            category: term.category,
            spoken_forms: term.spoken_forms.clone(),
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    const GP: &str = "ie-general-practice";
    const SE: &str = "software-engineering";

    fn ids(ids: &[&str]) -> Vec<String> {
        ids.iter().map(|id| id.to_string()).collect()
    }

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|v| v.to_string()).collect()
    }

    fn pack(id: &str) -> &'static Pack {
        bundled_packs().iter().find(|pack| pack.id == id).unwrap()
    }

    #[test]
    fn every_bundled_pack_loads_and_is_well_formed() {
        assert_eq!(bundled_packs().len(), BUNDLED.len());
        for raw in BUNDLED {
            let file: serde_json::Value = serde_json::from_str(raw).unwrap();
            let pack = parse_pack(raw).unwrap();
            // Nothing was dropped by the loader's clean-up.
            assert_eq!(
                pack.terms.len(),
                file["terms"].as_array().unwrap().len(),
                "{}",
                pack.id
            );
            assert_eq!(
                pack.always_on.len(),
                file["always_on"].as_array().unwrap().len(),
                "{}: an always_on entry is not a term",
                pack.id
            );
            assert!((20..=40).contains(&pack.always_on.len()), "{}", pack.id);
            assert!(
                !pack.attribution.is_empty() && !pack.sources.is_empty(),
                "{}",
                pack.id
            );
            let mut seen = HashSet::new();
            for term in &pack.terms {
                assert!(
                    seen.insert(normalize(&term.term)),
                    "{}: duplicate {}",
                    pack.id,
                    term.term
                );
                assert!(term.term.trim() == term.term && !term.term.is_empty());
            }
        }
        assert!(pack(GP).terms.len() > 1000);
        assert!((200..=1000).contains(&pack(SE).terms.len()));
    }

    #[test]
    fn loader_rejects_bad_packs_and_tolerates_new_fields() {
        let minimal = r#"{"schema":1,"id":"x","name":"X","description":"","version":"1","licence":"MIT","terms":[{"term":"Foo"}]}"#;
        let pack = parse_pack(minimal).unwrap();
        assert_eq!(pack.terms[0].weight, 0.5);
        assert_eq!(pack.terms[0].category, Category::Term);
        assert!(parse_pack(&minimal.replace("\"schema\":1", "\"schema\":2")).is_err());
        assert!(parse_pack(&minimal.replace("\"id\":\"x\"", "\"id\":\"Bad Id\"")).is_err());
        assert!(parse_pack(&minimal.replace("{\"term\":\"Foo\"}", "{\"term\":\"  \"}")).is_err());
        let newer = minimal.replace(
            "{\"term\":\"Foo\"}",
            "{\"term\":\"Foo\",\"category\":\"gadget\",\"weight\":7,\"future\":true}",
        );
        let pack =
            parse_pack(&newer.replace("\"terms\"", "\"polish_adapter\":{\"id\":\"a\"},\"terms\""))
                .unwrap();
        assert_eq!(pack.terms[0].category, Category::Term);
        assert_eq!(pack.terms[0].weight, 1.0);
        assert!(pack.polish_adapter.is_some());
    }

    #[test]
    fn retrieval_finds_misheard_pack_terms_in_real_sentences() {
        let gp = ids(&[GP]);
        let se = ids(&[SE]);
        for (packs, raw, expected) in [
            (
                &gp,
                "the patient is on met forming 500 twice daily",
                "metformin",
            ),
            (
                &gp,
                "continue a tour of a statin 20 mg at night",
                "atorvastatin",
            ),
            (
                &gp,
                "started a pixa ban 5 mg twice daily for her AF",
                "apixaban",
            ),
            (&gp, "her h b a one c is down to 52", "HbA1c"),
            (
                &gp,
                "I'll send the referral through health link today",
                "Healthlink",
            ),
            (&gp, "uh can you send that by health mail", "Healthmail"),
            (&gp, "he's on es o meprazole 40 mg", "esomeprazole"),
            (
                &gp,
                "she's from dun leery and has a GP visit card",
                "Dún Laoghaire",
            ),
            (
                &se,
                "run cube control apply on the staging cluster",
                "kubectl",
            ),
            (&se, "move the cache to post gres", "PostgreSQL"),
            (&se, "put engine x in front of the api", "nginx"),
            (
                &se,
                "we log in with oh auth and then call the graph q l endpoint",
                "OAuth",
            ),
            (
                &se,
                "we log in with oh auth and then call the graph q l endpoint",
                "GraphQL",
            ),
            (&se, "the tower e app builds with vite", "Tauri"),
        ] {
            let terms = retrieve_terms(raw, &[], packs, DEFAULT_RETRIEVAL_LIMIT);
            assert!(
                terms.iter().any(|t| t == expected),
                "{raw}: wanted {expected}, got {terms:?}"
            );
            assert!(terms.len() <= DEFAULT_RETRIEVAL_LIMIT);
        }
    }

    /// Before/after spelling lists on the polish benchmark's real dictations:
    ///
    ///     python3 scripts/polish-bench.py --export-cases=/tmp/cases.json
    ///     FAIRSPOKEN_POLISH_BENCH_CASES=/tmp/cases.json \
    ///         cargo test --lib retrieval_on_benchmark_cases -- --ignored --nocapture
    #[test]
    #[ignore]
    fn retrieval_on_benchmark_cases() {
        let path = std::env::var("FAIRSPOKEN_POLISH_BENCH_CASES")
            .expect("set FAIRSPOKEN_POLISH_BENCH_CASES");
        let bench: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        let vocab: Vec<String> = serde_json::from_value(bench["vocab"].clone()).unwrap();
        let both = ids(&[GP, SE]);
        let (mut before_total, mut after_total, mut changed) = (0, 0, 0);
        let cases = bench["cases"].as_array().unwrap();
        for case in cases {
            let raw = case["raw"].as_str().unwrap();
            let before = crate::transcript_cleanup::relevant_vocabulary(raw, &vocab);
            let after = retrieve_terms(raw, &vocab, &both, DEFAULT_RETRIEVAL_LIMIT);
            before_total += before.len();
            after_total += after.len();
            if before != after {
                changed += 1;
                eprintln!("{}: {:?} -> {:?}", case["name"], before, after);
            }
        }
        eprintln!(
            "{} cases: spelling terms {before_total} -> {after_total}; {changed} lists changed",
            cases.len()
        );
    }

    #[test]
    fn retrieval_stays_quiet_on_ordinary_speech() {
        let both = ids(&[GP, SE]);
        for raw in [
            "So we don't need to worry about that until next week.",
            "Thanks everyone for coming, it really means a lot to the whole team.",
            "Let's meet on Wednesday at ten and go through the budget together.",
            "I think the second option is better because it is simpler and cheaper.",
        ] {
            let terms = retrieve_terms(raw, &[], &both, DEFAULT_RETRIEVAL_LIMIT);
            assert!(terms.len() <= 2, "{raw}: {terms:?}");
        }
    }

    #[test]
    fn user_terms_come_first_and_without_packs_nothing_changes() {
        let user = strings(&["Hypercube", "Railway", "Metformin"]);
        let raw = "deploy hyper cube to rail way and note the met forming dose";
        assert_eq!(
            retrieve_terms(raw, &user, &[], DEFAULT_RETRIEVAL_LIMIT),
            crate::transcript_cleanup::relevant_vocabulary(raw, &user)
        );
        let merged = retrieve_terms(raw, &user, &ids(&[GP]), DEFAULT_RETRIEVAL_LIMIT);
        assert_eq!(&merged[..3], &["Hypercube", "Railway", "Metformin"]);
        // The user's spelling wins; the pack's identical term is not repeated.
        assert!(!merged
            .iter()
            .skip(3)
            .any(|t| t.eq_ignore_ascii_case("metformin")));
        // Unknown pack ids are ignored, not an error.
        assert_eq!(
            retrieve_terms(raw, &user, &ids(&["not-a-pack"]), 15),
            crate::transcript_cleanup::relevant_vocabulary(raw, &user)
        );
    }

    #[test]
    fn invented_pack_terms_are_rejected() {
        let se = ids(&[SE]);
        assert!(check_polish("run cube control apply", "Run kubectl apply.", &se).is_ok());
        let err = check_polish(
            "then we need to update the",
            "Then we need to update the Kubernetes manifests.",
            &se,
        )
        .unwrap_err();
        assert!(err.contains("Kubernetes"), "{err}");
        // A pack that is not enabled does not police its terms.
        assert!(check_polish(
            "then we need to update the",
            "Then we need to update the Kubernetes manifests.",
            &[]
        )
        .is_ok());
        // Capitalising an everyday word the pack also lists is fine.
        assert!(check_polish("we rewrote it in rust", "We rewrote it in Rust.", &se).is_ok());
    }

    #[test]
    fn one_medicine_never_silently_becomes_another() {
        for (raw, edited) in [
            (
                "start hydroxyzine 25 mg at night",
                "Start hydralazine 25 mg at night.",
            ),
            ("continue carbamazepine", "Continue carbimazole."),
            ("he takes amlodipine 5 mg", "He takes amitriptyline 5 mg."),
        ] {
            let err = check_polish(raw, edited, &[]).unwrap_err();
            assert!(err.contains("different medicine"), "{err}");
        }
        for (raw, edited) in [
            // Formatting the same medicine.
            (
                "co codamol 30 500 two tablets",
                "Co-codamol 30/500, two tablets.",
            ),
            // A self-correction drops the first medicine without adding one.
            ("give amoxicillin no wait doxycycline", "Give doxycycline."),
            // A mishearing becomes the medicine that was said.
            (
                "the patient is on met forming 500",
                "The patient is on metformin 500.",
            ),
        ] {
            assert!(check_polish(raw, edited, &[]).is_ok(), "{edited}");
        }
    }

    #[test]
    fn drug_lexicon_lookup() {
        for drug in [
            "metformin",
            "Atorvastatin",
            "CO-CODAMOL",
            "apixaban",
            "Eliquis",
            "esomeprazole",
        ] {
            assert!(is_drug_term(drug), "{drug}");
        }
        for word in ["Healthlink", "kubectl", "patient", "HbA1c", "tablet"] {
            assert!(!is_drug_term(word), "{word}");
        }
        assert_eq!(
            drug_terms_in("Swap atorvastatin for rosuvastatin."),
            vec!["atorvastatin", "rosuvastatin"]
        );
        let terms = drug_lexicon_terms();
        assert!(terms.len() > 4000 && terms.iter().all(|t| is_drug_term(t)));
    }

    #[test]
    fn whisper_prompt_puts_user_terms_first_and_fits_the_budget() {
        assert_eq!(whisper_prompt(&[], &[]), None);
        assert_eq!(
            whisper_prompt(&strings(&["Hypercube"]), &[]).unwrap(),
            "Relevant names and terms: Hypercube."
        );
        let user: Vec<String> = (0..50).map(|i| format!("Projectname{i}")).collect();
        let prompt = whisper_prompt(&user, &ids(&[GP, SE])).unwrap();
        assert!(prompt.starts_with("Relevant names and terms: Projectname0, Projectname1,"));
        assert!(
            estimated_tokens(&prompt) <= WHISPER_PROMPT_TOKEN_BUDGET,
            "{prompt}"
        );
        // A short dictionary leaves room for the packs' always-on terms.
        let prompt = whisper_prompt(&strings(&["Hypercube"]), &ids(&[GP])).unwrap();
        assert!(prompt.starts_with("Relevant names and terms: Hypercube, "));
        assert!(prompt.contains("Healthlink"), "{prompt}");
        assert!(estimated_tokens(&prompt) <= WHISPER_PROMPT_TOKEN_BUDGET);
        // Earlier chunks add what they mentioned, still inside the budget.
        let extended = extend_whisper_prompt(&prompt, "she was started on a pixa ban last week");
        assert!(extended.contains("apixaban"), "{extended}");
        assert!(estimated_tokens(&extended) <= WHISPER_PROMPT_TOKEN_BUDGET);
        let full = whisper_prompt(&user, &ids(&[GP])).unwrap();
        let extended = extend_whisper_prompt(&full, "she was started on a pixa ban last week");
        assert!(estimated_tokens(&extended) <= WHISPER_PROMPT_TOKEN_BUDGET);
    }

    /// The prompt's cost as the budget counts it.
    fn estimated_tokens(prompt: &str) -> usize {
        let list = prompt
            .strip_prefix(WHISPER_PROMPT_PREFIX)
            .and_then(|rest| rest.strip_suffix('.'))
            .unwrap();
        WHISPER_PREFIX_TOKENS + list.split(", ").map(term_tokens).sum::<usize>()
    }

    #[test]
    fn whisper_budget_is_conservative_for_real_tokenizers() {
        // Whisper's GPT-2 BPE needs at most about one token per 2.5 bytes even
        // for rare medical words ("atorvastatin" is 4 tokens); non-Latin
        // scripts are 2-3 bytes per character.
        assert!(term_tokens("atorvastatin") >= 5);
        assert!(term_tokens("Dún Laoghaire") >= 6);
        assert!(term_tokens("東京") >= 3);
    }

    #[test]
    fn remote_hints_keep_the_cap_and_user_priority() {
        let user: Vec<String> = (0..45).map(|i| format!("term{i}")).collect();
        let hints = remote_hints(&user, &ids(&[GP]));
        assert_eq!(hints.len(), REMOTE_HINT_LIMIT);
        assert_eq!(&hints[..45], &user[..]);
        assert_eq!(remote_hints(&user, &[]), user);
    }

    #[test]
    fn pack_preview_search() {
        let found = search_vocabulary_pack(SE.into(), Some("cube control".into()), None).unwrap();
        assert!(found.iter().any(|t| t.term == "kubectl"));
        assert!(search_vocabulary_pack("nope".into(), None, None).is_err());
        assert_eq!(
            search_vocabulary_pack(GP.into(), None, Some(3))
                .unwrap()
                .len(),
            3
        );
        let summaries = list_vocabulary_packs();
        assert!(summaries.iter().any(|s| s.id == GP && s.term_count > 1000));
    }

    /// The retrieval cost on a 10,000-term index: a realistic dictation must
    /// be searched in a few milliseconds (optimised build), or polish and
    /// Whisper would wait on it.
    #[test]
    fn retrieval_over_ten_thousand_terms_is_fast() {
        let mut entries: Vec<IndexEntry> = pack(GP)
            .terms
            .iter()
            .chain(&pack(SE).terms)
            .map(|term| IndexEntry {
                term: term.term.clone(),
                written: vec![term.term.clone()],
                spoken: term.spoken_forms.clone(),
                weight: term.weight,
                ambiguous: term.ambiguous,
            })
            .collect();
        // Pad with pronounceable nonsense to 10,000 terms.
        let syllables = [
            "ka", "lo", "mir", "ta", "zen", "vo", "pra", "dol", "ix", "ne", "su", "rem",
        ];
        let mut seed: u32 = 7;
        while entries.len() < 10_000 {
            let mut term = String::new();
            for _ in 0..3 + seed % 3 {
                seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12_345);
                term.push_str(syllables[(seed >> 16) as usize % syllables.len()]);
            }
            entries.push(IndexEntry {
                term: term.clone(),
                written: vec![term],
                spoken: Vec::new(),
                weight: 0.3,
                ambiguous: false,
            });
        }
        let started = std::time::Instant::now();
        let index = PhoneticIndex::build(entries);
        let build = started.elapsed();
        let dictation = "Please see this 68 year old man from dun leery with new AF. He has been started on a pixa ban 5 mg twice daily and continues met forming 500 and a tour of a statin 20 at night. His h b a one c is 52 and his blood pressure is fine. I sent the referral through health link and copied the letter by health mail. Then run cube control apply on the staging cluster and check the post gres logs.";
        let rounds = 20;
        let started = std::time::Instant::now();
        for _ in 0..rounds {
            assert!(!index
                .retrieve(dictation, DEFAULT_RETRIEVAL_LIMIT)
                .is_empty());
        }
        let per_call = started.elapsed() / rounds;
        eprintln!(
            "phonetic index: {} terms built in {build:?}; {} words retrieved in {per_call:?} per call",
            index.entries().len(),
            dictation.split_whitespace().count()
        );
        // Debug builds are ~10x slower than release; the release figure is
        // what the report quotes.
        let bound = if cfg!(debug_assertions) { 100 } else { 10 };
        assert!(per_call.as_millis() < bound, "{per_call:?}");
    }
}
