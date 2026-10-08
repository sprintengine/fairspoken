//! Super mode's merge: one transcript from two speech models' hypotheses of
//! the same chunk of audio.
//!
//! The primary (Parakeet) is fast and rarely invents words, but it cannot be
//! prompted. The secondary (Whisper) can be steered towards the user's
//! dictionary through its initial prompt, and it hallucinates on quiet audio.
//! So the merge keeps the primary's words, punctuation and casing wherever
//! the two agree, and the secondary wins a disagreement only for a reason it
//! can name, tried in this order:
//!
//! 1. its side of the span spells a dictionary term that the primary's side
//!    only sounds like (and the reverse keeps the primary);
//! 2. both sides carry confidences and the secondary's is clearly higher;
//! 3. otherwise the primary wins.
//!
//! Words only the secondary heard are first screened for Whisper's classic
//! hallucinations: stock closing phrases, loops, words over silence and words
//! Whisper itself gave a low probability. Every disagreement is reported with
//! the choice and the reason, for debugging and history.
//!
//! Everything here is a pure function of its inputs.

use serde::{Deserialize, Serialize};

/// One recognised word. `start`/`end` are seconds from the start of the
/// chunk when the engine reports timings.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Word {
    pub text: String,
    pub confidence: Option<f32>,
    pub start: Option<f32>,
    pub end: Option<f32>,
}

impl Word {
    pub fn new(text: &str) -> Self {
        Self {
            text: text.to_string(),
            ..Self::default()
        }
    }
}

/// An engine's transcript of one chunk, word by word.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Hypothesis {
    pub words: Vec<Word>,
}

impl Hypothesis {
    /// A hypothesis without confidences or timings.
    pub fn from_text(text: &str) -> Self {
        Self {
            words: text.split_whitespace().map(Word::new).collect(),
        }
    }

    pub fn text(&self) -> String {
        self.words
            .iter()
            .map(|word| word.text.as_str())
            .collect::<Vec<_>>()
            .join(" ")
    }

    pub fn is_empty(&self) -> bool {
        self.words.is_empty()
    }
}

/// Which 30 ms windows of the chunk held speech-level energy.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct EnergyProfile {
    pub window_seconds: f32,
    pub active: Vec<bool>,
}

impl EnergyProfile {
    pub fn duration(&self) -> f32 {
        self.window_seconds * self.active.len() as f32
    }

    /// Share of the windows overlapping `start..end` that held speech, or
    /// `None` when the interval covers no window.
    fn active_fraction(&self, start: f32, end: f32) -> Option<f32> {
        if self.window_seconds <= 0.0 || end.partial_cmp(&start) != Some(std::cmp::Ordering::Greater) {
            return None;
        }
        let first = (start.max(0.0) / self.window_seconds).floor() as usize;
        let last = ((end / self.window_seconds).ceil() as usize).min(self.active.len());
        if first >= last {
            return None;
        }
        let active = self.active[first..last].iter().filter(|a| **a).count();
        Some(active as f32 / (last - first) as f32)
    }
}

/// The merge's thresholds.
///
/// The confidence rule only runs when both engines report confidences.
/// Parakeet through parakeet-rs 0.3 reports none (its greedy TDT decoder
/// discards the logits), so with today's engines rule 2 is dormant and the
/// confidence values below are conservative placeholders, not calibrated
/// against Parakeet scores.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MergeConfig {
    /// Alignment cost of substituting a word that sounds like the other one
    /// (an unrelated word costs 1, as do insertions and deletions).
    pub sounds_alike_cost: f32,
    /// How much higher the secondary's mean confidence must be to win.
    pub confidence_margin: f32,
    /// The secondary never wins on confidence below this.
    pub min_secondary_confidence: f32,
    /// Secondary-only words below this mean token probability are dropped.
    pub min_insertion_probability: f32,
    /// Secondary-only words over audio with less voiced share than this are
    /// dropped as heard in silence.
    pub min_active_fraction: f32,
}

impl Default for MergeConfig {
    fn default() -> Self {
        Self {
            sounds_alike_cost: 0.4,
            confidence_margin: 0.2,
            min_secondary_confidence: 0.85,
            min_insertion_probability: 0.4,
            min_active_fraction: 0.2,
        }
    }
}

pub struct MergeInput<'a> {
    pub primary: &'a Hypothesis,
    pub secondary: &'a Hypothesis,
    /// The user's dictionary terms (vocabulary hints and correction targets).
    pub dictionary: &'a [String],
    pub energy: Option<&'a EnergyProfile>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Side {
    Primary,
    Secondary,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Reason {
    /// The chosen side spells a dictionary term the other side only sounds like.
    DictionaryTerm,
    /// Both sides had confidences and the chosen one was clearly higher.
    Confidence,
    /// Nothing argued for the secondary.
    PrimaryDefault,
    /// Secondary-only stock phrase ("Thank you.") of the kind Whisper
    /// produces from silence.
    PhantomPhrase,
    /// Secondary-only words repeating what came just before (a decode loop).
    RepeatedPhrase,
    /// Secondary-only words over near-silent audio.
    Silence,
    /// Secondary-only words Whisper itself gave a low probability.
    LowProbability,
}

/// One span where the hypotheses differ.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Disagreement {
    pub primary: String,
    pub secondary: String,
    pub chosen: Side,
    pub reason: Reason,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub term: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub primary_confidence: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub secondary_confidence: Option<f32>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MergeReport {
    pub agreed_words: usize,
    pub disagreements: Vec<Disagreement>,
}

impl MergeReport {
    pub fn secondary_wins(&self) -> usize {
        self.disagreements
            .iter()
            .filter(|d| d.chosen == Side::Secondary)
            .count()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Merged {
    pub text: String,
    pub report: MergeReport,
}

/// The dictionary terms `text` plausibly refers to.
///
/// The one seam through which super mode retrieves vocabulary, so a better
/// retrieval (the vocabulary packs' phonetic index) can replace it in one
/// place.
pub fn vocabulary_terms(text: &str, dictionary: &[String]) -> Vec<String> {
    crate::transcript_cleanup::relevant_vocabulary(text, dictionary)
}

/// Whisper's prompt window is 224 tokens; this leaves room for the prefix.
pub const PROMPT_TOKEN_BUDGET: usize = 200;
const PROMPT_PREFIX: &str = "Relevant names and terms: ";

/// A per-chunk Whisper prompt: the dictionary terms the primary's transcript
/// of this chunk points at first, then the rest of the dictionary in the
/// user's order, cut at `token_budget` (estimated, about three characters a
/// token for these mostly-rare words).
pub fn targeted_prompt(
    primary_text: &str,
    dictionary: &[String],
    token_budget: usize,
) -> Option<String> {
    let mut ordered: Vec<String> = Vec::new();
    let push = |term: &str, ordered: &mut Vec<String>| {
        let term: String = term
            .chars()
            .map(|c| if c.is_control() { ' ' } else { c })
            .collect::<String>()
            .trim()
            .to_string();
        if !term.is_empty() && !ordered.iter().any(|t| t.eq_ignore_ascii_case(&term)) {
            ordered.push(term);
        }
    };
    for term in vocabulary_terms(primary_text, dictionary) {
        push(&term, &mut ordered);
    }
    for term in dictionary {
        push(term, &mut ordered);
    }

    let mut used = estimate_tokens(PROMPT_PREFIX) + 1;
    let mut kept = Vec::new();
    for term in ordered {
        // The term plus its ", " separator.
        let cost = estimate_tokens(&term) + 1;
        if used + cost > token_budget {
            break;
        }
        used += cost;
        kept.push(term);
    }
    (!kept.is_empty()).then(|| format!("{PROMPT_PREFIX}{}.", kept.join(", ")))
}

fn estimate_tokens(text: &str) -> usize {
    text.chars().count().div_ceil(3).max(1)
}

/// What speech models decode from a breath or room noise.
pub(crate) const PHANTOM_PHRASES: &[&str] = &[
    "thank you",
    "thank you very much",
    "thank you so much",
    "thanks for watching",
    "thank you for watching",
    "please subscribe",
    "bye",
    "bye bye",
    "you",
    "mm hmm",
    "mhm",
    "uh huh",
    "hmm",
];

pub fn merge(input: &MergeInput<'_>, config: &MergeConfig) -> Merged {
    let primary = &input.primary.words;
    let secondary = &input.secondary.words;
    let p_norm: Vec<String> = primary.iter().map(|w| normalize(&w.text)).collect();
    let s_norm: Vec<String> = secondary.iter().map(|w| normalize(&w.text)).collect();
    let ops = align(&p_norm, &s_norm, config);

    let mut out: Vec<String> = Vec::new();
    let mut report = MergeReport::default();
    let mut span = Span::default();
    let mut last_primary: Option<usize> = None;

    for op in ops {
        match op {
            Op::Match(i, _) => {
                flush_span(
                    &mut span,
                    input,
                    &p_norm,
                    &s_norm,
                    config,
                    &mut out,
                    &mut report,
                    last_primary,
                    Some(i),
                );
                report.agreed_words += 1;
                out.push(primary[i].text.clone());
                last_primary = Some(i);
            }
            Op::Sub(i, j) => {
                span.primary.push(i);
                span.secondary.push(j);
            }
            Op::Delete(i) => {
                if p_norm[i].is_empty() && span.is_empty() {
                    // Punctuation-only primary token between agreed words.
                    out.push(primary[i].text.clone());
                    last_primary = Some(i);
                } else {
                    span.primary.push(i);
                }
            }
            Op::Insert(j) => {
                if !s_norm[j].is_empty() {
                    span.secondary.push(j);
                }
            }
        }
    }
    flush_span(
        &mut span,
        input,
        &p_norm,
        &s_norm,
        config,
        &mut out,
        &mut report,
        last_primary,
        None,
    );

    Merged {
        text: out.join(" "),
        report,
    }
}

#[derive(Default)]
struct Span {
    primary: Vec<usize>,
    secondary: Vec<usize>,
}

impl Span {
    fn is_empty(&self) -> bool {
        self.primary.is_empty() && self.secondary.is_empty()
    }
}

#[allow(clippy::too_many_arguments)]
fn flush_span(
    span: &mut Span,
    input: &MergeInput<'_>,
    p_norm: &[String],
    s_norm: &[String],
    config: &MergeConfig,
    out: &mut Vec<String>,
    report: &mut MergeReport,
    previous_primary: Option<usize>,
    next_primary: Option<usize>,
) {
    if span.is_empty() {
        return;
    }
    let span = std::mem::take(span);
    let primary = &input.primary.words;
    let secondary = &input.secondary.words;
    let previous_primary = span
        .primary
        .first()
        .map(|first| first.checked_sub(1))
        .unwrap_or(previous_primary);
    let next_primary = span
        .primary
        .last()
        .map(|last| (last + 1 < primary.len()).then_some(last + 1))
        .unwrap_or(next_primary);

    let p_words: Vec<&Word> = span.primary.iter().map(|&i| &primary[i]).collect();
    let s_words: Vec<&Word> = span.secondary.iter().map(|&j| &secondary[j]).collect();
    let p_keys: Vec<&str> = span
        .primary
        .iter()
        .map(|&i| p_norm[i].as_str())
        .filter(|k| !k.is_empty())
        .collect();
    let s_keys: Vec<&str> = span.secondary.iter().map(|&j| s_norm[j].as_str()).collect();

    let decision = if s_keys.is_empty() {
        // The secondary simply missed words the primary heard.
        Decision::primary(Reason::PrimaryDefault)
    } else if p_keys.is_empty() {
        resolve_insertion(
            input,
            config,
            &s_words,
            &s_keys,
            out,
            previous_primary,
            next_primary,
        )
    } else {
        resolve_substitution(input, config, &p_words, &p_keys, &s_words, &s_keys)
    };

    let primary_text = join_words(&p_words);
    let secondary_text = join_words(&s_words);
    match decision.side {
        Side::Primary => out.extend(p_words.iter().map(|w| w.text.clone())),
        Side::Secondary => {
            let mut replacement =
                secondary_replacement(&p_words, &s_words, &s_keys, decision.term.as_deref(), out);
            // An insertion that moved a sentence end: Whisper put the
            // primary's closing punctuation after the inserted words.
            if p_words.is_empty() && !replacement.is_empty() {
                if let Some(tail) = take_moved_punctuation(out, input, &span) {
                    if let Some(last) = replacement.last_mut() {
                        last.push_str(&tail);
                    }
                }
            }
            out.extend(replacement);
        }
    }
    if !(p_keys.is_empty() && s_keys.is_empty()) {
        report.disagreements.push(Disagreement {
            primary: primary_text,
            secondary: secondary_text,
            chosen: decision.side,
            reason: decision.reason,
            term: decision.term,
            primary_confidence: decision.primary_confidence,
            secondary_confidence: decision.secondary_confidence,
        });
    }
}

struct Decision {
    side: Side,
    reason: Reason,
    term: Option<String>,
    primary_confidence: Option<f32>,
    secondary_confidence: Option<f32>,
}

impl Decision {
    fn primary(reason: Reason) -> Self {
        Self {
            side: Side::Primary,
            reason,
            term: None,
            primary_confidence: None,
            secondary_confidence: None,
        }
    }

    fn with_confidences(mut self, primary: Option<f32>, secondary: Option<f32>) -> Self {
        self.primary_confidence = primary;
        self.secondary_confidence = secondary;
        self
    }
}

fn resolve_insertion(
    input: &MergeInput<'_>,
    config: &MergeConfig,
    s_words: &[&Word],
    s_keys: &[&str],
    out: &[String],
    previous_primary: Option<usize>,
    next_primary: Option<usize>,
) -> Decision {
    let secondary_confidence = mean_confidence(s_words);
    let primary = &input.primary.words;
    let neighbours: Vec<&Word> = [previous_primary, next_primary]
        .into_iter()
        .flatten()
        .map(|i| &primary[i])
        .collect();
    let primary_confidence = mean_confidence(&neighbours);
    let rejected = |reason| {
        Decision::primary(reason).with_confidences(primary_confidence, secondary_confidence)
    };

    let phrase = s_keys.join(" ");
    if PHANTOM_PHRASES.contains(&phrase.as_str()) {
        return rejected(Reason::PhantomPhrase);
    }
    // The alignment may place a doubled phrase before its first copy rather
    // than after it, so the words that follow count as well.
    let following: Vec<String> = next_primary
        .map(|next| {
            primary[next..]
                .iter()
                .take(s_keys.len())
                .map(|w| normalize(&w.text))
                .collect()
        })
        .unwrap_or_default();
    let repeats_following =
        following.len() == s_keys.len() && following.iter().zip(s_keys).all(|(a, b)| a == b);
    if repeats_preceding(s_keys, out) || repeats_following || is_loop(s_keys) {
        return rejected(Reason::RepeatedPhrase);
    }
    if let (Some(energy), Some((start, end))) = (
        input.energy,
        insertion_region(
            s_words,
            primary,
            previous_primary,
            next_primary,
            input.energy,
        ),
    ) {
        if energy
            .active_fraction(start, end)
            .is_some_and(|fraction| fraction < config.min_active_fraction)
        {
            return rejected(Reason::Silence);
        }
    }
    if secondary_confidence.is_some_and(|c| c < config.min_insertion_probability) {
        return rejected(Reason::LowProbability);
    }
    // A dictionary term the primary did not hear at all is exactly what a
    // prompt makes Whisper invent, so insertions never win on the dictionary.
    confidence_decision(primary_confidence, secondary_confidence, config)
}

fn resolve_substitution(
    input: &MergeInput<'_>,
    config: &MergeConfig,
    p_words: &[&Word],
    p_keys: &[&str],
    s_words: &[&Word],
    s_keys: &[&str],
) -> Decision {
    let primary_confidence = mean_confidence(p_words);
    let secondary_confidence = mean_confidence(s_words);
    if let Some(decision) = dictionary_decision(input.dictionary, p_words, p_keys, s_words, s_keys)
    {
        return decision.with_confidences(primary_confidence, secondary_confidence);
    }
    confidence_decision(primary_confidence, secondary_confidence, config)
}

fn dictionary_decision(
    dictionary: &[String],
    p_words: &[&Word],
    p_keys: &[&str],
    s_words: &[&Word],
    s_keys: &[&str],
) -> Option<Decision> {
    if dictionary.is_empty() {
        return None;
    }
    let mut candidates = vocabulary_terms(&join_words(s_words), dictionary);
    for term in vocabulary_terms(&join_words(p_words), dictionary) {
        if !candidates.contains(&term) {
            candidates.push(term);
        }
    }
    let mut secondary_term = None;
    let mut primary_term = None;
    for term in candidates {
        let key = normalize(&term);
        if key.is_empty() {
            continue;
        }
        let s_spells = spells(s_keys, &term);
        let p_spells = spells(p_keys, &term);
        if s_spells && !p_spells && sounds_like_term(p_keys, &key) {
            secondary_term.get_or_insert(term);
        } else if p_spells && !s_spells {
            primary_term.get_or_insert(term);
        }
    }
    match (primary_term, secondary_term) {
        (Some(term), None) => Some(Decision {
            side: Side::Primary,
            reason: Reason::DictionaryTerm,
            term: Some(term),
            primary_confidence: None,
            secondary_confidence: None,
        }),
        (None, Some(term)) => Some(Decision {
            side: Side::Secondary,
            reason: Reason::DictionaryTerm,
            term: Some(term),
            primary_confidence: None,
            secondary_confidence: None,
        }),
        // Each side spells a different term: no evidence either way.
        _ => None,
    }
}

fn confidence_decision(
    primary: Option<f32>,
    secondary: Option<f32>,
    config: &MergeConfig,
) -> Decision {
    let decision = match (primary, secondary) {
        (Some(p), Some(s))
            if s >= config.min_secondary_confidence && s - p >= config.confidence_margin =>
        {
            Decision {
                side: Side::Secondary,
                reason: Reason::Confidence,
                term: None,
                primary_confidence: None,
                secondary_confidence: None,
            }
        }
        (Some(p), Some(s)) if p - s >= config.confidence_margin => {
            Decision::primary(Reason::Confidence)
        }
        _ => Decision::primary(Reason::PrimaryDefault),
    };
    decision.with_confidences(primary, secondary)
}

/// Mean confidence of the words, when every one of them has one.
fn mean_confidence(words: &[&Word]) -> Option<f32> {
    if words.is_empty() {
        return None;
    }
    let mut sum = 0.0;
    for word in words {
        sum += word.confidence.filter(|c| c.is_finite())?;
    }
    Some(sum / words.len() as f32)
}

/// Where in the chunk the inserted words were spoken: Whisper's own timings
/// when it reported them, otherwise the audio before the primary's first
/// word or after its last one. `None` when neither is known.
fn insertion_region(
    s_words: &[&Word],
    primary: &[Word],
    previous_primary: Option<usize>,
    next_primary: Option<usize>,
    energy: Option<&EnergyProfile>,
) -> Option<(f32, f32)> {
    let starts = s_words
        .iter()
        .filter_map(|w| w.start)
        .fold(f32::INFINITY, f32::min);
    let ends = s_words
        .iter()
        .filter_map(|w| w.end)
        .fold(f32::NEG_INFINITY, f32::max);
    if starts.is_finite() && ends.is_finite() && ends > starts {
        return Some((starts, ends));
    }
    let duration = energy?.duration();
    match (previous_primary, next_primary) {
        (None, Some(next)) => primary[next].start.map(|start| (0.0, start)),
        // A word's reported end is where the next token began; the word
        // itself is still sounding for a moment after that.
        (Some(previous), None) => primary[previous]
            .end
            .or(primary[previous].start)
            .map(|end| (end + 0.3, duration)),
        _ => None,
    }
}

/// The inserted words repeat the words just before them.
fn repeats_preceding(s_keys: &[&str], out: &[String]) -> bool {
    let n = s_keys.len();
    if n == 0 || out.len() < n {
        return false;
    }
    out[out.len() - n..]
        .iter()
        .zip(s_keys)
        .all(|(previous, key)| normalize(previous) == *key)
}

/// The inserted words are one short phrase said over and over.
fn is_loop(s_keys: &[&str]) -> bool {
    let n = s_keys.len();
    (1..=n / 2).any(|period| {
        n.is_multiple_of(period)
            && n / period >= if period == 1 { 3 } else { 2 }
            && s_keys
                .iter()
                .enumerate()
                .all(|(i, key)| *key == s_keys[i % period])
    })
}

/// Whether consecutive words spell the term word for word: "rail way" does
/// not spell "Railway", it only sounds like it.
fn spells(keys: &[&str], term: &str) -> bool {
    let term_words: Vec<String> = term
        .split_whitespace()
        .map(normalize)
        .filter(|w| !w.is_empty())
        .collect();
    !term_words.is_empty()
        && keys
            .windows(term_words.len())
            .any(|window| window.iter().zip(&term_words).all(|(a, b)| *a == b))
}

/// Whether the primary's words could be a mishearing of the term: a near
/// spelling (also joined up), or nearly the same consonant skeleton.
fn sounds_like_term(keys: &[&str], key: &str) -> bool {
    let term_code = phonetic_key(key);
    let allowed = match term_code.len() {
        0..=2 => 0,
        3..=6 => 1,
        _ => 2,
    };
    windows(keys).any(|joined| {
        sounds_alike(&joined, key) || {
            let code = phonetic_key(&joined);
            code.len() >= 2 && edit_distance(&code, &term_code) <= allowed
        }
    })
}

fn windows<'a>(keys: &'a [&'a str]) -> impl Iterator<Item = String> + 'a {
    (0..keys.len()).flat_map(move |start| {
        (start + 1..=(start + 3).min(keys.len())).map(move |end| keys[start..end].concat())
    })
}

/// The secondary's words for a span it won, dressed in the primary's
/// punctuation and sentence casing; a dictionary term is written the way the
/// dictionary spells it.
fn secondary_replacement(
    p_words: &[&Word],
    s_words: &[&Word],
    s_keys: &[&str],
    term: Option<&str>,
    out: &[String],
) -> Vec<String> {
    let mut cores: Vec<String> = s_words.iter().map(|w| core(&w.text).to_string()).collect();
    if let Some(term) = term {
        let key = normalize(term);
        'search: for start in 0..s_keys.len() {
            for end in start + 1..=(start + 3).min(s_keys.len()) {
                if s_keys[start..end].concat() == key {
                    cores.splice(start..end, [term.to_string()]);
                    break 'search;
                }
            }
        }
    }
    cores.retain(|c| !c.is_empty());
    if cores.is_empty() {
        return Vec::new();
    }
    if let Some(first) = p_words.first() {
        let lead = leading_punctuation(&first.text);
        let primary_capital = core(&first.text)
            .chars()
            .next()
            .is_some_and(char::is_uppercase);
        let sentence_start = out
            .last()
            .is_none_or(|previous| previous.ends_with(['.', '?', '!']));
        // A dictionary term keeps the dictionary's casing ("iPhone",
        // "macOS"); an all-lowercase one ("metoprolol") is capitalized only
        // to start a sentence.
        let capitalize_first = if term.is_some_and(|term| cores[0] == term) {
            sentence_start && !cores[0].chars().any(char::is_uppercase)
        } else {
            primary_capital || sentence_start
        };
        if capitalize_first {
            cores[0] = capitalize(&cores[0]);
        }
        cores[0] = format!("{lead}{}", cores[0]);
    }
    if let Some(last) = p_words.last() {
        let tail = trailing_punctuation(&last.text);
        let end = cores.len() - 1;
        cores[end].push_str(tail);
    }
    cores
}

/// For an accepted insertion: when the secondary put the closing punctuation
/// of the word before the insertion after the inserted words instead, takes
/// it off that word so the caller can move it.
fn take_moved_punctuation(
    out: &mut [String],
    input: &MergeInput<'_>,
    span: &Span,
) -> Option<String> {
    let previous = out.last_mut()?;
    let (first, last) = (*span.secondary.first()?, *span.secondary.last()?);
    let tail = trailing_punctuation(previous).to_string();
    if tail.is_empty() {
        return None;
    }
    let secondary = &input.secondary.words;
    let unpunctuated_before = first
        .checked_sub(1)
        .is_some_and(|j| trailing_punctuation(&secondary[j].text).is_empty());
    if !unpunctuated_before || trailing_punctuation(&secondary[last].text).is_empty() {
        return None;
    }
    previous.truncate(previous.len() - tail.len());
    Some(tail)
}

fn join_words(words: &[&Word]) -> String {
    words
        .iter()
        .map(|w| w.text.as_str())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Lowercase letters and digits only, so "Don't," and "dont" agree.
pub fn normalize(word: &str) -> String {
    word.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn core(token: &str) -> &str {
    token.trim_matches(|c: char| !c.is_alphanumeric())
}

fn leading_punctuation(token: &str) -> &str {
    let end = token
        .find(|c: char| c.is_alphanumeric())
        .unwrap_or(token.len());
    &token[..end]
}

fn trailing_punctuation(token: &str) -> &str {
    match token.rfind(|c: char| c.is_alphanumeric()) {
        Some(index) => {
            let after = index + token[index..].chars().next().map_or(1, char::len_utf8);
            &token[after..]
        }
        None => "",
    }
}

fn capitalize(word: &str) -> String {
    let mut chars = word.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// A consonant skeleton: letters that sound alike share a class, vowels drop
/// out (a leading vowel is kept as `A`), repeats collapse. "coopernetties"
/// and "kubernetes" both give `KPRNTS`.
pub fn phonetic_key(normalized: &str) -> String {
    let chars: Vec<char> = normalized.chars().collect();
    let mut out = String::new();
    let mut previous: Option<char> = None;
    for (index, &ch) in chars.iter().enumerate() {
        let next = chars.get(index + 1).copied();
        let code = match ch {
            'a' | 'e' | 'i' | 'o' | 'u' | 'y' => {
                if index == 0 {
                    Some('A')
                } else {
                    None
                }
            }
            'h' | 'w' => None,
            'b' | 'p' | 'f' | 'v' => Some('P'),
            'c' if matches!(next, Some('e' | 'i' | 'y')) => Some('S'),
            'c' | 'k' | 'q' | 'g' | 'j' => Some('K'),
            'x' => {
                push_code(&mut out, &mut previous, 'K');
                Some('S')
            }
            's' | 'z' => Some('S'),
            'd' | 't' => Some('T'),
            'l' => Some('L'),
            'r' => Some('R'),
            'm' | 'n' => Some('N'),
            digit if digit.is_ascii_digit() => Some(digit),
            _ => None,
        };
        if let Some(code) = code {
            push_code(&mut out, &mut previous, code);
        }
    }
    out
}

fn push_code(out: &mut String, previous: &mut Option<char>, code: char) {
    if *previous != Some(code) {
        out.push(code);
        *previous = Some(code);
    }
}

/// Two normalized words that a listener could confuse: the same consonant
/// skeleton, or a spelling one letter in four away.
pub fn sounds_alike(a: &str, b: &str) -> bool {
    if a.is_empty() || b.is_empty() {
        return false;
    }
    if a == b {
        return true;
    }
    let shorter = a.chars().count().min(b.chars().count());
    let longer = a.chars().count().max(b.chars().count());
    if shorter >= 4 && edit_distance(a, b) <= (longer / 4).max(1) {
        return true;
    }
    let (ka, kb) = (phonetic_key(a), phonetic_key(b));
    ka.len() >= 2 && ka == kb
}

fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut previous: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut current = vec![i + 1];
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            current.push(
                (previous[j + 1] + 1)
                    .min(current[j] + 1)
                    .min(previous[j] + cost),
            );
        }
        previous = current;
    }
    previous[b.len()]
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Op {
    Match(usize, usize),
    Sub(usize, usize),
    /// A primary word the secondary does not have.
    Delete(usize),
    /// A secondary word the primary does not have.
    Insert(usize),
}

/// Word-level Levenshtein alignment of normalized tokens. A substitution
/// between words that sound alike is cheaper than between unrelated words,
/// so "cooper netties" lines up with "Kubernetes" rather than with whatever
/// sits next to it. Punctuation-only tokens (empty after normalizing) align
/// for free.
fn align(p: &[String], s: &[String], config: &MergeConfig) -> Vec<Op> {
    let (n, m) = (p.len(), s.len());
    let gap = |key: &String| if key.is_empty() { 0.0 } else { 1.0 };
    let sub = |a: &String, b: &String| -> f32 {
        if a.is_empty() || b.is_empty() {
            f32::INFINITY
        } else if a == b {
            0.0
        } else if sounds_alike(a, b) {
            config.sounds_alike_cost
        } else {
            1.0
        }
    };
    let mut cost = vec![vec![0.0_f32; m + 1]; n + 1];
    for i in 1..=n {
        cost[i][0] = cost[i - 1][0] + gap(&p[i - 1]);
    }
    for j in 1..=m {
        cost[0][j] = cost[0][j - 1] + gap(&s[j - 1]);
    }
    for i in 1..=n {
        for j in 1..=m {
            let diagonal = cost[i - 1][j - 1] + sub(&p[i - 1], &s[j - 1]);
            let delete = cost[i - 1][j] + gap(&p[i - 1]);
            let insert = cost[i][j - 1] + gap(&s[j - 1]);
            cost[i][j] = diagonal.min(delete).min(insert);
        }
    }

    let mut ops = Vec::with_capacity(n.max(m));
    let (mut i, mut j) = (n, m);
    const EPS: f32 = 1e-4;
    while i > 0 || j > 0 {
        if i > 0
            && j > 0
            && (cost[i][j] - (cost[i - 1][j - 1] + sub(&p[i - 1], &s[j - 1]))).abs() < EPS
        {
            ops.push(if p[i - 1] == s[j - 1] {
                Op::Match(i - 1, j - 1)
            } else {
                Op::Sub(i - 1, j - 1)
            });
            i -= 1;
            j -= 1;
        } else if i > 0 && (cost[i][j] - (cost[i - 1][j] + gap(&p[i - 1]))).abs() < EPS {
            ops.push(Op::Delete(i - 1));
            i -= 1;
        } else {
            ops.push(Op::Insert(j - 1));
            j -= 1;
        }
    }
    ops.reverse();
    ops
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dict(terms: &[&str]) -> Vec<String> {
        terms.iter().map(|t| t.to_string()).collect()
    }

    fn run(primary: &str, secondary: &str, dictionary: &[&str]) -> Merged {
        let dictionary = dict(dictionary);
        merge(
            &MergeInput {
                primary: &Hypothesis::from_text(primary),
                secondary: &Hypothesis::from_text(secondary),
                dictionary: &dictionary,
                energy: None,
            },
            &MergeConfig::default(),
        )
    }

    fn words(spec: &[(&str, f32)]) -> Hypothesis {
        Hypothesis {
            words: spec
                .iter()
                .map(|(text, confidence)| Word {
                    text: text.to_string(),
                    confidence: Some(*confidence),
                    ..Word::default()
                })
                .collect(),
        }
    }

    fn merge_plain(primary: &Hypothesis, secondary: &Hypothesis, dictionary: &[String]) -> Merged {
        merge(
            &MergeInput {
                primary,
                secondary,
                dictionary,
                energy: None,
            },
            &MergeConfig::default(),
        )
    }

    #[test]
    fn agreement_keeps_the_primary_text_byte_for_byte() {
        let merged = run("Hello, World! Don't stop.", "hello world dont stop", &[]);
        assert_eq!(merged.text, "Hello, World! Don't stop.");
        assert_eq!(merged.report.agreed_words, 4);
        assert!(merged.report.disagreements.is_empty());
    }

    #[test]
    fn without_a_dictionary_or_confidences_the_primary_wins() {
        let merged = run(
            "We shipped the build today.",
            "We shipped a bill today.",
            &[],
        );
        assert_eq!(merged.text, "We shipped the build today.");
        let d = &merged.report.disagreements;
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].primary, "the build");
        assert_eq!(d[0].secondary, "a bill");
        assert_eq!(d[0].chosen, Side::Primary);
        assert_eq!(d[0].reason, Reason::PrimaryDefault);
    }

    #[test]
    fn a_dictionary_term_the_primary_only_sounds_like_goes_to_the_secondary() {
        let merged = run(
            "Deploy it on cooper netties, then restart.",
            "Deploy it on Kubernetes then restart.",
            &["Kubernetes", "Grafana"],
        );
        assert_eq!(merged.text, "Deploy it on Kubernetes, then restart.");
        let d = &merged.report.disagreements[0];
        assert_eq!(d.chosen, Side::Secondary);
        assert_eq!(d.reason, Reason::DictionaryTerm);
        assert_eq!(d.term.as_deref(), Some("Kubernetes"));
    }

    #[test]
    fn split_and_joined_spellings_align_into_one_span() {
        let merged = run(
            "I host it on rail way.",
            "I host it on railway.",
            &["Railway"],
        );
        assert_eq!(merged.text, "I host it on Railway.");
        assert_eq!(merged.report.disagreements.len(), 1);
    }

    #[test]
    fn the_dictionary_spelling_wins_over_the_secondary_casing() {
        let merged = run(
            "Start metro pro lol tonight.",
            "Start metoprolol tonight.",
            &["Metoprolol"],
        );
        assert_eq!(merged.text, "Start Metoprolol tonight.");
    }

    #[test]
    fn a_term_the_primary_does_not_sound_like_is_prompt_bias_and_loses() {
        let merged = run(
            "Book the meeting room.",
            "Book the Kubernetes room.",
            &["Kubernetes"],
        );
        assert_eq!(merged.text, "Book the meeting room.");
        assert_eq!(
            merged.report.disagreements[0].reason,
            Reason::PrimaryDefault
        );
    }

    #[test]
    fn the_primary_keeps_a_term_it_spelled() {
        let merged = run("Open Grafana now.", "Open Grafanna now.", &["Grafana"]);
        assert_eq!(merged.text, "Open Grafana now.");
        let d = &merged.report.disagreements[0];
        assert_eq!(
            (d.chosen, d.reason),
            (Side::Primary, Reason::DictionaryTerm)
        );
    }

    #[test]
    fn a_replaced_span_at_a_sentence_start_is_capitalized() {
        let merged = run(
            "Done. Shiv bond called.",
            "Done. siobhan called.",
            &["Siobhan"],
        );
        assert_eq!(merged.text, "Done. Siobhan called.");
        let merged = run("shiv bond called.", "siobhan called.", &["siobhan"]);
        assert_eq!(merged.text, "Siobhan called.");
    }

    #[test]
    fn a_dictionary_term_keeps_its_own_casing_at_a_sentence_start() {
        let merged = run("Eye phone sales grew.", "iPhone sales grew.", &["iPhone"]);
        assert_eq!(merged.text, "iPhone sales grew.");
        let merged = run(
            "Done. Mac oh ass updated.",
            "Done. macOS updated.",
            &["macOS"],
        );
        assert_eq!(merged.text, "Done. macOS updated.");
        // An all-lowercase term takes a capital only to start a sentence,
        // not because the primary happened to capitalize its guess.
        let merged = run(
            "Start Metro pro lol tonight.",
            "Start metoprolol tonight.",
            &["metoprolol"],
        );
        assert_eq!(merged.text, "Start metoprolol tonight.");
    }

    #[test]
    fn leading_punctuation_of_the_primary_span_is_kept() {
        let merged = run(
            "He said \"cooper netties\" twice.",
            "He said Kubernetes twice.",
            &["Kubernetes"],
        );
        assert_eq!(merged.text, "He said \"Kubernetes\" twice.");
    }

    #[test]
    fn confidence_decides_only_with_a_clear_margin_and_a_high_floor() {
        let case = |p: f32, s: f32| {
            let primary = words(&[("send", 0.9), ("the", 0.9), ("file", p)]);
            let secondary = words(&[("send", 0.9), ("the", 0.9), ("files", s)]);
            merge_plain(&primary, &secondary, &[])
        };
        let won = case(0.5, 0.95);
        assert_eq!(won.text, "send the files");
        assert_eq!(won.report.disagreements[0].reason, Reason::Confidence);
        assert_eq!(won.report.disagreements[0].secondary_confidence, Some(0.95));
        // Too close.
        assert_eq!(case(0.8, 0.9).text, "send the file");
        // Clearly higher but below the floor.
        assert_eq!(case(0.3, 0.7).text, "send the file");
        let primary_won = case(0.95, 0.5);
        assert_eq!(
            primary_won.report.disagreements[0].reason,
            Reason::Confidence
        );
        assert_eq!(primary_won.text, "send the file");
    }

    #[test]
    fn the_dictionary_outranks_confidence() {
        let primary = words(&[("call", 0.9), ("shiv", 0.99), ("bond", 0.99)]);
        let secondary = words(&[("call", 0.9), ("Siobhan", 0.3)]);
        let merged = merge_plain(&primary, &secondary, &dict(&["Siobhan"]));
        assert_eq!(merged.text, "call Siobhan");
        assert_eq!(
            merged.report.disagreements[0].reason,
            Reason::DictionaryTerm
        );
    }

    #[test]
    fn thank_you_on_silence_is_dropped_as_a_phantom() {
        let merged = run("See you tomorrow.", "See you tomorrow. Thank you.", &[]);
        assert_eq!(merged.text, "See you tomorrow.");
        let d = &merged.report.disagreements[0];
        assert_eq!((d.chosen, d.reason), (Side::Primary, Reason::PhantomPhrase));
        assert_eq!(d.secondary, "Thank you.");
    }

    #[test]
    fn decode_loops_and_repeats_are_dropped() {
        let merged = run("and then we left", "and then we left and then we left", &[]);
        assert_eq!(merged.text, "and then we left");
        assert_eq!(
            merged.report.disagreements[0].reason,
            Reason::RepeatedPhrase
        );
        let merged = run("okay", "okay go go go go", &[]);
        assert_eq!(
            merged.report.disagreements[0].reason,
            Reason::RepeatedPhrase
        );
    }

    #[test]
    fn secondary_only_words_over_silence_are_dropped() {
        let primary = Hypothesis {
            words: vec![Word {
                text: "Hello.".into(),
                start: Some(0.1),
                end: Some(0.5),
                ..Word::default()
            }],
        };
        let secondary = Hypothesis {
            words: vec![
                Word {
                    text: "Hello.".into(),
                    start: Some(0.1),
                    end: Some(0.5),
                    confidence: Some(0.9),
                },
                Word {
                    text: "Goodbye".into(),
                    start: Some(1.2),
                    end: Some(1.8),
                    confidence: Some(0.9),
                },
            ],
        };
        // Speech for the first 0.6 s of a 2 s chunk, silence after.
        let energy = EnergyProfile {
            window_seconds: 0.03,
            active: (0..67).map(|w| w < 20).collect(),
        };
        let merged = merge(
            &MergeInput {
                primary: &primary,
                secondary: &secondary,
                dictionary: &[],
                energy: Some(&energy),
            },
            &MergeConfig::default(),
        );
        assert_eq!(merged.text, "Hello.");
        assert_eq!(merged.report.disagreements[0].reason, Reason::Silence);
    }

    #[test]
    fn an_insertion_after_the_last_primary_word_is_checked_against_the_tail_audio() {
        let primary = Hypothesis {
            words: vec![Word {
                text: "Yes.".into(),
                start: Some(0.2),
                end: Some(0.21),
                ..Word::default()
            }],
        };
        let secondary = Hypothesis::from_text("Yes. Absolutely right");
        let energy = EnergyProfile {
            window_seconds: 0.03,
            active: (0..100).map(|w| w < 20).collect(),
        };
        let merged = merge(
            &MergeInput {
                primary: &primary,
                secondary: &secondary,
                dictionary: &[],
                energy: Some(&energy),
            },
            &MergeConfig::default(),
        );
        assert_eq!(merged.report.disagreements[0].reason, Reason::Silence);
    }

    #[test]
    fn low_probability_insertions_are_dropped() {
        let primary = words(&[("ship", 0.9), ("it", 0.9)]);
        let secondary = words(&[("ship", 0.9), ("it", 0.9), ("now", 0.1)]);
        let merged = merge_plain(&primary, &secondary, &[]);
        assert_eq!(merged.text, "ship it");
        assert_eq!(
            merged.report.disagreements[0].reason,
            Reason::LowProbability
        );
    }

    #[test]
    fn a_confident_insertion_wins_only_against_doubtful_primary_neighbours() {
        let primary = words(&[("ship", 0.4), ("it", 0.5)]);
        let secondary = words(&[("ship", 0.9), ("it", 0.9), ("today", 0.97)]);
        let merged = merge_plain(&primary, &secondary, &[]);
        assert_eq!(merged.text, "ship it today");
        assert_eq!(merged.report.disagreements[0].reason, Reason::Confidence);
        let sure = words(&[("ship", 0.9), ("it", 0.95)]);
        assert_eq!(merge_plain(&sure, &secondary, &[]).text, "ship it");
    }

    #[test]
    fn an_accepted_insertion_takes_over_the_sentence_end() {
        let primary = words(&[("ship", 0.4), ("it.", 0.5)]);
        let secondary = words(&[("ship", 0.9), ("it", 0.9), ("today.", 0.97)]);
        assert_eq!(
            merge_plain(&primary, &secondary, &[]).text,
            "ship it today."
        );
    }

    #[test]
    fn insertions_never_win_on_the_dictionary_alone() {
        let merged = run("Deploy it.", "Deploy it to Kubernetes.", &["Kubernetes"]);
        assert_eq!(merged.text, "Deploy it.");
    }

    #[test]
    fn words_only_the_primary_heard_are_kept() {
        let merged = run("I really really mean it.", "I really mean it.", &[]);
        assert_eq!(merged.text, "I really really mean it.");
        assert_eq!(merged.report.disagreements[0].chosen, Side::Primary);
    }

    #[test]
    fn empty_sides() {
        assert_eq!(run("", "Thank you.", &[]).text, "");
        assert_eq!(run("Keep this.", "", &[]).text, "Keep this.");
        assert_eq!(run("", "", &[]).text, "");
    }

    #[test]
    fn punctuation_only_tokens_never_open_a_span() {
        let merged = run("Salt & pepper — please.", "salt and pepper please", &[]);
        assert_eq!(merged.text, "Salt & pepper — please.");
        assert_eq!(merged.report.disagreements.len(), 1);
        assert_eq!(merged.report.disagreements[0].secondary, "and");
    }

    #[test]
    fn report_serializes_for_history() {
        let merged = run("on cooper netties", "on Kubernetes", &["Kubernetes"]);
        let json = serde_json::to_value(&merged.report).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "agreedWords": 1,
                "disagreements": [{
                    "primary": "cooper netties",
                    "secondary": "Kubernetes",
                    "chosen": "secondary",
                    "reason": "dictionary-term",
                    "term": "Kubernetes"
                }]
            })
        );
        assert_eq!(merged.report.secondary_wins(), 1);
    }

    #[test]
    fn phonetic_keys_group_sounds_that_get_confused() {
        assert_eq!(phonetic_key("coopernetties"), phonetic_key("kubernetes"));
        assert_eq!(phonetic_key("metroprolol"), "NTRPRL");
        assert_eq!(phonetic_key("ozempic"), "ASNPK");
        assert!(sounds_alike("grafana", "grafanna"));
        assert!(sounds_alike("nguyen", "nguen"));
        assert!(!sounds_alike("meeting", "kubernetes"));
        assert!(!sounds_alike("a", "i"));
    }

    #[test]
    fn alignment_prefers_sound_alike_substitutions() {
        let p: Vec<String> = ["the", "grafanna", "board"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let s: Vec<String> = ["the", "grafana", "dashboard"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let ops = align(&p, &s, &MergeConfig::default());
        assert_eq!(ops, vec![Op::Match(0, 0), Op::Sub(1, 1), Op::Sub(2, 2)]);
    }

    #[test]
    fn targeted_prompt_puts_the_chunks_terms_first_and_respects_the_budget() {
        let dictionary = dict(&["Grafana", "Kubernetes", "Siobhan", "Ozempic"]);
        let prompt =
            targeted_prompt("check the ozempic dose", &dictionary, PROMPT_TOKEN_BUDGET).unwrap();
        assert_eq!(
            prompt,
            "Relevant names and terms: Ozempic, Grafana, Kubernetes, Siobhan."
        );
        let tight = targeted_prompt("check the ozempic dose", &dictionary, 14).unwrap();
        assert_eq!(tight, "Relevant names and terms: Ozempic.");
        assert_eq!(targeted_prompt("anything", &[], PROMPT_TOKEN_BUDGET), None);
        let many: Vec<String> = (0..200).map(|i| format!("Term{i:03}")).collect();
        let long = targeted_prompt("", &many, PROMPT_TOKEN_BUDGET).unwrap();
        assert!(
            estimate_tokens(&long) <= PROMPT_TOKEN_BUDGET + 2,
            "{}",
            long.len()
        );
        let nul = targeted_prompt("", &dict(&["bad\0term"]), PROMPT_TOKEN_BUDGET).unwrap();
        assert!(!nul.contains('\0'));
    }
}
