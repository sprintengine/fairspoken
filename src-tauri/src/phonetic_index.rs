//! Phonetic retrieval over a large term list.
//!
//! A vocabulary pack can hold thousands of terms, far more than a speech or
//! polish model can be shown, so only the few that something in a transcript
//! plausibly refers to are handed on. ASR mangles an unfamiliar word into
//! ordinary ones ("met forming" for metformin, "a tour of a statin" for
//! atorvastatin), so a transcript is scanned in windows of one to a few words
//! and each window is looked up three ways:
//! * exactly, against every written and spoken form of every term;
//! * by Double Metaphone key, allowing one key edit (the index stores each key
//!   and its one-deletion variants, so a lookup is a few hash probes);
//! * by spelling, within a small edit distance, as a fallback for windows the
//!   phonetic lookup found nothing for.
//!
//! Every non-exact candidate is confirmed by a bounded edit distance on the
//! letters, so a shared phonetic key alone never retrieves a term. The index is
//! built once per set of packs; a lookup allocates per window, never per term.

use rphonetic::DoubleMetaphone;
use std::collections::{HashMap, HashSet};

/// The longest window looked at, in words. Spoken forms such as
/// "h b a one c" need five.
const MAX_WINDOW_WORDS: usize = 5;

/// Double Metaphone keys are cut at this length: long enough to tell ten
/// thousand terms apart, short enough that a mangled ending still matches.
const KEY_LENGTH: usize = 10;

/// A single word that is not a term is an ordinary word the recogniser
/// heard correctly far more often than a misheard term, so only long terms
/// ("esomeprazol") are matched to one inexactly: "option" is not Optison.
const SINGLE_WORD_MIN_LETTERS: usize = 8;

/// Below this score a candidate is not worth showing to a model.
const MIN_SCORE: f32 = 0.45;

/// Words that cannot open or close a misheard term. "the met" is not a
/// rendering of anything; "a tour of a statin" is, so single letters stay.
const EDGE_STOPWORDS: &[&str] = &[
    "the", "and", "to", "of", "in", "on", "at", "is", "it", "for", "with", "that", "this", "was",
    "are", "be", "as", "by", "or", "an", "we", "he", "she", "his", "her", "you", "they", "them",
    "our", "your", "their", "my", "me", "us", "so", "if", "but", "not", "no", "do", "did", "has",
    "had", "have", "will", "would", "can", "could", "should", "from", "then", "than", "there",
    "here", "what", "when", "which", "who", "also", "just", "very", "been", "were", "its", "into",
    "about", "up", "out", "all", "any", "some", "one", "two",
];

const NUMBER_WORDS: &[(&str, &str)] = &[
    ("zero", "0"),
    ("oh", "0"),
    ("one", "1"),
    ("two", "2"),
    ("three", "3"),
    ("four", "4"),
    ("five", "5"),
    ("six", "6"),
    ("seven", "7"),
    ("eight", "8"),
    ("nine", "9"),
    ("ten", "10"),
];

/// One term as the index sees it.
#[derive(Clone, Debug)]
pub struct IndexEntry {
    /// The written form handed to callers.
    pub term: String,
    /// The forms that count as the term being written (the term and its
    /// accepted alternative spellings).
    pub written: Vec<String>,
    /// How the term tends to come out of speech recognition.
    pub spoken: Vec<String>,
    /// 0.0 to 1.0; how much the term deserves a slot when space is short.
    pub weight: f32,
    /// The term is also an everyday word ("Rust", "Swift"), so it is only
    /// retrieved where the transcript capitalises it.
    pub ambiguous: bool,
}

/// A retrieved term and how sure the index is about it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Hit {
    pub entry: usize,
    pub score: f32,
}

struct Surface {
    entry: u32,
    /// Lowercase alphanumerics with diacritics folded, words joined.
    letters: String,
    /// How many words the form has: one heard word is never matched
    /// inexactly to a form of several ("engineer" is not "engine ex").
    words: usize,
    written: bool,
}

/// How a phonetic probe met a surface's key.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum KeyDistance {
    Same,
    Near,
}

pub struct PhoneticIndex {
    entries: Vec<IndexEntry>,
    surfaces: Vec<Surface>,
    /// Joined letters (and the number-word variant) → surfaces.
    exact: HashMap<String, Vec<u32>>,
    /// Double Metaphone key → surfaces whose key it is.
    keys: HashMap<String, Vec<u32>>,
    /// One-deletion variant of a key → surfaces it was derived from.
    key_variants: HashMap<String, Vec<u32>>,
    /// (first three letters, length) and (last three letters, length) →
    /// surfaces, for the spelling fallback.
    prefixes: HashMap<(String, usize), Vec<u32>>,
    suffixes: HashMap<(String, usize), Vec<u32>>,
    max_words: usize,
    encoder: DoubleMetaphone,
}

struct Token {
    norm: String,
    capitalized: bool,
}

impl PhoneticIndex {
    pub fn build(entries: Vec<IndexEntry>) -> Self {
        let encoder = DoubleMetaphone::new(Some(KEY_LENGTH));
        let mut index = PhoneticIndex {
            entries: Vec::new(),
            surfaces: Vec::new(),
            exact: HashMap::new(),
            keys: HashMap::new(),
            key_variants: HashMap::new(),
            prefixes: HashMap::new(),
            suffixes: HashMap::new(),
            max_words: 1,
            encoder,
        };
        for (entry_id, entry) in entries.iter().enumerate() {
            let mut seen = HashSet::new();
            let forms = entry
                .written
                .iter()
                .map(|form| (form, true))
                .chain(entry.spoken.iter().map(|form| (form, false)));
            for (form, written) in forms {
                let tokens = tokenize(form);
                let letters: String = tokens.iter().map(|t| t.norm.as_str()).collect();
                if letters.is_empty() || !seen.insert((letters.clone(), written)) {
                    continue;
                }
                index.max_words = index.max_words.max(tokens.len().min(MAX_WINDOW_WORDS));
                let surface_id = index.surfaces.len() as u32;
                let mapped = number_mapped(&tokens);
                push(&mut index.exact, letters.clone(), surface_id);
                if mapped != letters {
                    push(&mut index.exact, mapped, surface_id);
                }
                let alphabetic: String = letters.chars().filter(|c| c.is_alphabetic()).collect();
                if alphabetic.chars().count() >= 4 {
                    for key in index.phonetic_keys(&alphabetic) {
                        for variant in deletions(&key) {
                            push(&mut index.key_variants, variant, surface_id);
                        }
                        push(&mut index.keys, key, surface_id);
                    }
                }
                let length = letters.chars().count();
                if length >= 6 {
                    push(&mut index.prefixes, (head(&letters), length), surface_id);
                    push(&mut index.suffixes, (tail(&letters), length), surface_id);
                }
                index.surfaces.push(Surface {
                    entry: entry_id as u32,
                    letters,
                    words: tokens.len(),
                    written,
                });
            }
        }
        index.entries = entries;
        index
    }

    pub fn entries(&self) -> &[IndexEntry] {
        &self.entries
    }

    /// The `limit` terms something in `text` most plausibly refers to, best
    /// first.
    pub fn retrieve(&self, text: &str, limit: usize) -> Vec<Hit> {
        let mut best: HashMap<usize, f32> = HashMap::new();
        self.scan(text, true, |entry, score| {
            let slot = best.entry(entry).or_insert(score);
            if score > *slot {
                *slot = score;
            }
        });
        let mut hits: Vec<Hit> = best
            .into_iter()
            .map(|(entry, score)| Hit { entry, score })
            .collect();
        hits.sort_by(|a, b| {
            b.score
                .total_cmp(&a.score)
                .then_with(|| {
                    self.entries[b.entry]
                        .weight
                        .total_cmp(&self.entries[a.entry].weight)
                })
                .then_with(|| self.entries[a.entry].term.cmp(&self.entries[b.entry].term))
        });
        hits.truncate(limit);
        hits
    }

    /// Every term something in `text` sounds like, with no limit and no
    /// capitalisation requirement: the set a term in edited text must belong
    /// to for it to count as spoken.
    pub fn sounded(&self, text: &str) -> HashSet<usize> {
        let mut found = HashSet::new();
        self.scan(text, false, |entry, _| {
            found.insert(entry);
        });
        found
    }

    /// The terms `text` writes out exactly, in a written or accepted form.
    pub fn written_in(&self, text: &str) -> Vec<usize> {
        let tokens = tokenize(text);
        let mut found = Vec::new();
        for start in 0..tokens.len() {
            let mut joined = String::new();
            for token in tokens.iter().skip(start).take(self.max_words) {
                joined.push_str(&token.norm);
                for &surface in self.exact.get(&joined).into_iter().flatten() {
                    let surface = &self.surfaces[surface as usize];
                    // Only the plain spelling: "1" for "one" is not writing it.
                    if surface.written && surface.letters == joined {
                        let entry = surface.entry as usize;
                        if !found.contains(&entry) {
                            found.push(entry);
                        }
                    }
                }
            }
        }
        found
    }

    fn phonetic_keys(&self, letters: &str) -> Vec<String> {
        let result = self.encoder.double_metaphone(letters);
        let primary = result.primary();
        let alternate = result.alternate();
        let mut keys = Vec::with_capacity(2);
        if primary.len() >= 2 {
            keys.push(primary.clone());
        }
        if alternate.len() >= 2 && alternate != primary {
            keys.push(alternate);
        }
        keys
    }

    fn scan(&self, text: &str, honour_ambiguity: bool, mut visit: impl FnMut(usize, f32)) {
        let tokens = tokenize(text);
        let mut candidates: Vec<(u32, KeyDistance)> = Vec::new();
        for start in 0..tokens.len() {
            let mut joined = String::new();
            for words in 1..=self.max_words.min(tokens.len() - start) {
                let window = &tokens[start..start + words];
                joined.push_str(&window[words - 1].norm);
                let capitalized = window[0].capitalized;
                let accept = |entry: usize| -> bool {
                    !honour_ambiguity || !self.entries[entry].ambiguous || capitalized
                };

                let mapped = number_mapped(window);
                let mut matched_exactly = false;
                for key in [&joined, &mapped] {
                    for &surface in self.exact.get(key.as_str()).into_iter().flatten() {
                        let entry = self.surfaces[surface as usize].entry as usize;
                        if accept(entry) {
                            visit(entry, 1.0 + 0.2 * self.entries[entry].weight);
                            matched_exactly = true;
                        }
                    }
                }
                if matched_exactly || !self.fuzzy_window(window, &joined) {
                    continue;
                }

                let length = joined.chars().count();
                let alphabetic: String = joined.chars().filter(|c| c.is_alphabetic()).collect();
                candidates.clear();
                for key in self.phonetic_keys(&alphabetic) {
                    for &surface in self.keys.get(&key).into_iter().flatten() {
                        candidates.push((surface, KeyDistance::Same));
                    }
                    for &surface in self.key_variants.get(&key).into_iter().flatten() {
                        candidates.push((surface, KeyDistance::Near));
                    }
                    for variant in deletions(&key) {
                        let lookups = [self.keys.get(&variant), self.key_variants.get(&variant)];
                        for &surface in lookups.into_iter().flatten().flatten() {
                            candidates.push((surface, KeyDistance::Near));
                        }
                    }
                }
                let mut any = false;
                candidates.sort_unstable();
                candidates.dedup_by_key(|(surface, _)| *surface);
                for &(surface_id, distance) in &candidates {
                    let surface = &self.surfaces[surface_id as usize];
                    let entry = surface.entry as usize;
                    let surface_length = surface.letters.chars().count();
                    if words == 1 && (surface_length < SINGLE_WORD_MIN_LETTERS || surface.words > 1)
                    {
                        continue;
                    }
                    let budget = phonetic_budget(surface_length, words);
                    let Some(edits) = bounded_levenshtein(&joined, &surface.letters, budget) else {
                        continue;
                    };
                    if !accept(entry) {
                        continue;
                    }
                    let base = match distance {
                        KeyDistance::Same => 0.8,
                        KeyDistance::Near => 0.7,
                    };
                    let score = self.score(base, edits, surface_length, entry);
                    if score >= MIN_SCORE {
                        visit(entry, score);
                        any = true;
                    }
                }
                let shortest = if words == 1 {
                    SINGLE_WORD_MIN_LETTERS
                } else {
                    6
                };
                if any || length < shortest {
                    continue;
                }

                // Spelling fallback: same first or last three letters, close in
                // length, within about one edit in five.
                let budget = length / 5;
                let (prefix, suffix) = (head(&joined), tail(&joined));
                for candidate_length in length.saturating_sub(budget)..=length + budget {
                    let probes = [
                        self.prefixes.get(&(prefix.clone(), candidate_length)),
                        self.suffixes.get(&(suffix.clone(), candidate_length)),
                    ];
                    for &surface_id in probes.into_iter().flatten().flatten() {
                        let surface = &self.surfaces[surface_id as usize];
                        let entry = surface.entry as usize;
                        if words == 1 && surface.words > 1 {
                            continue;
                        }
                        let Some(edits) = bounded_levenshtein(&joined, &surface.letters, budget)
                        else {
                            continue;
                        };
                        if !accept(entry) {
                            continue;
                        }
                        let score = self.score(0.6, edits, candidate_length, entry);
                        if score >= MIN_SCORE {
                            visit(entry, score);
                        }
                    }
                }
            }
        }
    }

    fn score(&self, base: f32, edits: usize, length: usize, entry: usize) -> f32 {
        base - 0.6 * edits as f32 / length.max(1) as f32 + 0.2 * self.entries[entry].weight
    }

    /// Whether a window may be matched other than exactly: not an edge
    /// stopword, and long enough that a near miss means something.
    fn fuzzy_window(&self, window: &[Token], joined: &str) -> bool {
        let stop = |token: &Token| EDGE_STOPWORDS.contains(&token.norm.as_str());
        let letters = joined.chars().filter(|c| c.is_alphabetic()).count();
        if window.len() == 1 {
            return letters >= 5 && !stop(&window[0]);
        }
        // A run may open on a lone letter ("a tour of a statin") but not
        // close on one ("means a" is not mesna).
        let last = &window[window.len() - 1];
        letters >= 5 && !stop(&window[0]) && !stop(last) && last.norm.chars().count() > 1
    }
}

/// How many letter edits a phonetic match may need. A single word is already
/// a word the recogniser knows, so it may be off by about one letter in five;
/// a run of words a term was split into ("a tour of a statin") by about one in
/// four. Short terms get one edit at most: "old man" is not "Olumiant".
fn phonetic_budget(length: usize, words: usize) -> usize {
    if length < 5 {
        return 0;
    }
    if words == 1 || length < 8 {
        (length / 5).max(1)
    } else {
        (length as f32 * 0.25).round() as usize
    }
}

fn push<K: std::hash::Hash + Eq>(map: &mut HashMap<K, Vec<u32>>, key: K, value: u32) {
    let values = map.entry(key).or_default();
    if !values.contains(&value) {
        values.push(value);
    }
}

fn head(letters: &str) -> String {
    letters.chars().take(3).collect()
}

fn tail(letters: &str) -> String {
    let chars: Vec<char> = letters.chars().collect();
    chars[chars.len().saturating_sub(3)..].iter().collect()
}

/// Every string `key` becomes with one character removed.
fn deletions(key: &str) -> Vec<String> {
    let chars: Vec<char> = key.chars().collect();
    if chars.len() < 3 {
        return Vec::new();
    }
    let mut variants: Vec<String> = (0..chars.len())
        .map(|skip| {
            chars
                .iter()
                .enumerate()
                .filter(|(index, _)| *index != skip)
                .map(|(_, c)| *c)
                .collect()
        })
        .collect();
    variants.dedup();
    variants
}

/// The joined window with whole number words written as digits, so "h b a one
/// c" meets "HbA1c".
fn number_mapped(tokens: &[Token]) -> String {
    tokens
        .iter()
        .map(|token| {
            NUMBER_WORDS
                .iter()
                .find(|(word, _)| *word == token.norm)
                .map(|(_, digit)| *digit)
                .unwrap_or(token.norm.as_str())
        })
        .collect()
}

/// Lowercase alphanumeric words with Latin diacritics folded ("Dún" → "dun").
/// The symbols that are part of a name are spelled the way they are said, so
/// "C++", "C#" and ".NET" do not collapse into "c", "c" and "net":
/// a `+` or `#` closing a word is "plus" or "sharp", and a `.` opening one is
/// "dot". Any other symbol separates words.
fn tokenize(text: &str) -> Vec<Token> {
    let chars: Vec<char> = text.chars().collect();
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut capitalized = false;
    for (index, &ch) in chars.iter().enumerate() {
        let previous = index.checked_sub(1).map(|i| chars[i]);
        let next = chars.get(index + 1).copied();
        let closes_word = |symbol: char| {
            let run_end = chars[index..]
                .iter()
                .position(|c| *c != symbol)
                .map(|n| index + n);
            !current.is_empty() && run_end.is_none_or(|end| !chars[end].is_alphanumeric())
        };
        if (ch == '+' || ch == '#') && closes_word(ch) {
            current.push_str(if ch == '+' { "plus" } else { "sharp" });
            continue;
        }
        let opens_word = current.is_empty()
            && previous.is_none_or(|c| c.is_whitespace() || c == '/' || c == '(')
            && next.is_some_and(char::is_alphanumeric);
        if ch == '.' && opens_word {
            capitalized = next.is_some_and(char::is_uppercase);
            current.push_str("dot");
            continue;
        }
        if ch.is_alphanumeric() {
            if current.is_empty() {
                capitalized = ch.is_uppercase();
            }
            for lower in ch.to_lowercase() {
                current.push(fold(lower));
            }
        } else if !current.is_empty() {
            tokens.push(Token {
                norm: std::mem::take(&mut current),
                capitalized,
            });
        }
    }
    if !current.is_empty() {
        tokens.push(Token {
            norm: current,
            capitalized,
        });
    }
    tokens
}

/// The joined, normalised spelling the index compares terms by.
pub fn normalize(text: &str) -> String {
    tokenize(text).into_iter().map(|t| t.norm).collect()
}

fn fold(ch: char) -> char {
    match ch {
        'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' | 'ā' => 'a',
        'ç' | 'ć' | 'č' => 'c',
        'è' | 'é' | 'ê' | 'ë' | 'ē' | 'ę' => 'e',
        'ì' | 'í' | 'î' | 'ï' | 'ī' => 'i',
        'ñ' | 'ń' => 'n',
        'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' | 'ō' => 'o',
        'ù' | 'ú' | 'û' | 'ü' | 'ū' => 'u',
        'ý' | 'ÿ' => 'y',
        'š' | 'ś' => 's',
        'ž' | 'ź' | 'ż' => 'z',
        'ł' => 'l',
        other => other,
    }
}

/// Levenshtein distance between `a` and `b` if it is at most `budget`.
fn bounded_levenshtein(a: &str, b: &str, budget: usize) -> Option<usize> {
    let a = a.as_bytes();
    let b = b.as_bytes();
    if a.len().abs_diff(b.len()) > budget {
        return None;
    }
    let mut previous: Vec<usize> = (0..=b.len()).collect();
    let mut current = vec![0; b.len() + 1];
    for (i, ca) in a.iter().enumerate() {
        current[0] = i + 1;
        let mut row_min = current[0];
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            current[j + 1] = (previous[j + 1] + 1)
                .min(current[j] + 1)
                .min(previous[j] + cost);
            row_min = row_min.min(current[j + 1]);
        }
        if row_min > budget {
            return None;
        }
        std::mem::swap(&mut previous, &mut current);
    }
    (previous[b.len()] <= budget).then_some(previous[b.len()])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(term: &str, spoken: &[&str]) -> IndexEntry {
        IndexEntry {
            term: term.to_string(),
            written: vec![term.to_string()],
            spoken: spoken.iter().map(|s| s.to_string()).collect(),
            weight: 0.5,
            ambiguous: false,
        }
    }

    fn terms(index: &PhoneticIndex, text: &str) -> Vec<String> {
        index
            .retrieve(text, 15)
            .into_iter()
            .map(|hit| index.entries()[hit.entry].term.clone())
            .collect()
    }

    /// Double Metaphone codes: Lawrence Philips' and Apache commons-codec's
    /// examples, then terms this index's tests depend on, so a dependency
    /// update cannot silently change matching.
    #[test]
    fn double_metaphone_vectors() {
        let encoder = DoubleMetaphone::new(Some(KEY_LENGTH));
        for (word, primary, alternate) in [
            ("Smith", "SM0", "XMT"),
            ("Schmidt", "XMT", "SMT"),
            ("Jose", "HS", "HS"),
            ("Xavier", "SF", "SFR"),
            ("knight", "NT", "NT"),
            ("Thumb", "0M", "TM"),
            ("Caesar", "SSR", "SSR"),
            ("Chianti", "KNT", "KNT"),
            ("Gough", "KF", "KF"),
            ("Tichner", "TXNR", "TKNR"),
            ("metformin", "MTFRMN", "MTFRMN"),
            ("atorvastatin", "ATRFSTTN", "ATRFSTTN"),
            ("Siobhan", "SPN", "XPN"),
        ] {
            let result = encoder.double_metaphone(word);
            assert_eq!(
                (result.primary().as_str(), result.alternate().as_str()),
                (primary, alternate),
                "{word}"
            );
        }
    }

    #[test]
    fn bounded_levenshtein_stops_at_the_budget() {
        assert_eq!(bounded_levenshtein("metforming", "metformin", 2), Some(1));
        assert_eq!(bounded_levenshtein("kitten", "sitting", 3), Some(3));
        assert_eq!(bounded_levenshtein("kitten", "sitting", 2), None);
        assert_eq!(bounded_levenshtein("a", "abcdef", 2), None);
    }

    #[test]
    fn retrieves_misheard_terms_from_word_windows() {
        let index = PhoneticIndex::build(vec![
            entry("metformin", &[]),
            entry("atorvastatin", &[]),
            entry("kubectl", &["cube control", "kube cuttle", "kube c t l"]),
            entry("PostgreSQL", &["postgres", "post gres", "post gress q l"]),
            entry("HbA1c", &["h b a one c"]),
            entry("Dún Laoghaire", &["dun leery", "dun leary"]),
        ]);
        assert_eq!(
            terms(&index, "the patient is on met forming 500"),
            vec!["metformin"]
        );
        assert_eq!(
            terms(&index, "started a tour of a statin 20 mg"),
            vec!["atorvastatin"]
        );
        assert_eq!(terms(&index, "run cube control apply"), vec!["kubectl"]);
        assert_eq!(terms(&index, "then kube cuttle get pods"), vec!["kubectl"]);
        assert_eq!(
            terms(&index, "migrate it to post gres tonight"),
            vec!["PostgreSQL"]
        );
        assert_eq!(terms(&index, "her h b a one c is 52"), vec!["HbA1c"]);
        assert_eq!(
            terms(&index, "a man from dun leery with AF"),
            vec!["Dún Laoghaire"]
        );
        assert_eq!(terms(&index, "Dun Laoghaire"), vec!["Dún Laoghaire"]);
        assert!(terms(&index, "the weather is fine and we should go home").is_empty());
        assert!(terms(&index, "it was performing well and the format is mine").is_empty());
    }

    #[test]
    fn ambiguous_terms_need_a_capital_to_be_retrieved_but_not_to_be_grounded() {
        let mut rust = entry("Rust", &[]);
        rust.ambiguous = true;
        let index = PhoneticIndex::build(vec![rust]);
        assert!(terms(&index, "there is rust on the gate").is_empty());
        assert_eq!(terms(&index, "rewrite it in Rust"), vec!["Rust"]);
        assert!(index.sounded("there is rust on the gate").contains(&0));
    }

    #[test]
    fn written_forms_are_found_exactly() {
        let index = PhoneticIndex::build(vec![
            entry("co-codamol", &["co codamol"]),
            entry("folic acid", &[]),
        ]);
        assert_eq!(
            index.written_in("Take co-codamol and folic acid."),
            vec![0, 1]
        );
        assert!(index.written_in("take co codamill").is_empty());
    }
}
