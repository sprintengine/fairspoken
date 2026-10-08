//! Deterministic transcript helpers that run around the polish model.
//!
//! A small cleanup model is good at punctuation and bad at restraint, so the
//! jobs that have a mechanical answer are done here instead of in the prompt:
//! * filler and non-speech noises ("uh", a trailing "Mm-hmm.") are stripped
//!   with word lists rather than asked for;
//! * dictionary terms reach the model only when something in the transcript
//!   sounds like them, and a term that shows up in the output without having
//!   been spoken rejects the pass;
//! * the marks speech leaves in a transcript — a repeated word, a restarted
//!   phrase, "you know", the period and capital an ASR model puts around a
//!   mid-sentence pause — are repaired by `tidy`, which needs no model at all;
//! * streaming polish works on fragments, so the capital the model puts on a
//!   mid-sentence tail and the period it puts on an unfinished one are undone.

use std::collections::HashSet;

/// Noises that are not words in any language the ASR models cover.
const UNIVERSAL_FILLERS: &[&str] = &[
    "uh", "uhh", "uhm", "umm", "hm", "hmm", "mmm", "mhm", "mmhmm", "mm-hmm", "uh-huh",
];

/// Fillers that are real words somewhere else ("um" in Portuguese and German),
/// so they are only removed when the dictation language is English.
const ENGLISH_FILLERS: &[&str] = &["um", "er", "erm"];

/// Two-token spellings of the same noises.
const FILLER_PAIRS: &[(&str, &str)] = &[("mm", "hmm"), ("uh", "huh")];

/// Words a sentence cannot end on. A raw tail that stops on one of these was
/// cut mid-sentence no matter what punctuation the model added.
const DANGLING_WORDS: &[&str] = &[
    "a", "an", "the", "to", "of", "for", "and", "but", "or", "with", "in", "on", "at", "that",
    "is", "are", "was", "were", "my", "our", "your", "their", "this", "these", "those", "from",
    "by", "as", "if", "so", "because", "than", "into", "about", "which", "who", "when", "while",
];

/// Words that open a continuation far more often than they are proper nouns.
const CONTINUATION_WORDS: &[&str] = &[
    "a", "an", "the", "to", "of", "for", "and", "but", "or", "with", "in", "on", "at", "that",
    "is", "are", "was", "were", "it", "we", "you", "they", "he", "she", "this", "these", "those",
    "from", "by", "as", "if", "so", "because", "than", "into", "about", "which", "who", "when",
    "while", "then", "also", "not", "just", "like", "there", "here", "what", "how", "why", "where",
];

fn is_sentence_end(ch: char) -> bool {
    matches!(ch, '.' | '?' | '!' | '…')
}

/// A token's word content: lowercase, outer punctuation removed, inner
/// hyphens kept so "Mm-hmm." reads as "mm-hmm".
fn token_core(token: &str) -> String {
    token
        .trim_matches(|c: char| !c.is_alphanumeric())
        .to_lowercase()
}

fn is_filler(core: &str, english: bool) -> bool {
    UNIVERSAL_FILLERS.contains(&core) || (english && ENGLISH_FILLERS.contains(&core))
}

fn capitalize_first(token: &str) -> String {
    let mut chars = token.chars();
    let mut out = String::with_capacity(token.len());
    let mut done = false;
    for ch in chars.by_ref() {
        if !done && ch.is_alphabetic() {
            out.extend(ch.to_uppercase());
            done = true;
        } else {
            out.push(ch);
        }
    }
    out
}

/// Removes filler noises. A filler that carried the sentence's closing
/// punctuation hands it to the word before it, and one that opened a sentence
/// hands its capital to the word after it.
pub fn strip_fillers(text: &str, language: &str) -> String {
    let english = language.trim().to_ascii_lowercase().starts_with("en");
    text.split('\n')
        .map(|line| strip_fillers_line(line, english))
        .collect::<Vec<_>>()
        .join("\n")
}

fn strip_fillers_line(line: &str, english: bool) -> String {
    let tokens: Vec<&str> = line.split_whitespace().collect();
    let mut kept: Vec<String> = Vec::with_capacity(tokens.len());
    let mut capitalize_next = false;
    let mut removed_any = false;
    let mut index = 0;
    while index < tokens.len() {
        let token = tokens[index];
        let core = token_core(token);
        let pair = tokens.get(index + 1).is_some_and(|next| {
            FILLER_PAIRS
                .iter()
                .any(|(a, b)| core == *a && token_core(next) == *b)
        });
        if !pair && !is_filler(&core, english) {
            kept.push(if capitalize_next {
                capitalize_first(token)
            } else {
                token.to_string()
            });
            capitalize_next = false;
            index += 1;
            continue;
        }
        removed_any = true;
        let last = if pair { tokens[index + 1] } else { token };
        let at_sentence_start = kept
            .last()
            .map(|previous| previous.chars().last().is_some_and(is_sentence_end))
            .unwrap_or(true);
        if at_sentence_start {
            capitalize_next = true;
        } else if let Some(end) = last.chars().last().filter(|c| is_sentence_end(*c)) {
            if let Some(previous) = kept.last_mut() {
                while previous.ends_with(',') {
                    previous.pop();
                }
                previous.push(end);
            }
        }
        index += if pair { 2 } else { 1 };
    }
    if !removed_any {
        return line.to_string();
    }
    kept.join(" ")
}

/// Words before which "you know" is the verb it looks like ("do you know").
const YOU_KNOW_VERB_CUES: &[&str] = &[
    "do", "did", "don't", "didn't", "if", "as", "what", "would", "could", "should", "might",
    "may", "can", "when", "once", "until", "unless", "how", "sure", "think",
];

/// What follows "I d" when the ASR split "I'd".
const I_WOULD_VERBS: &[&str] = &[
    "like", "love", "rather", "say", "prefer", "be", "have", "want", "need", "also", "just",
    "probably", "really", "suggest", "recommend", "expect", "imagine", "guess", "hope",
];

/// The cleanup that needs no model: filler noises and phrases, stuttered words
/// and restarted phrases, and the stray period and capital an ASR model leaves
/// around a pause in the middle of a sentence. Every rule here was taken from
/// what a polish model usefully did to real dictations; none of them can add a
/// word, which is more than could be said for the model.
///
/// `capitalised_terms` are the user's dictionary terms and the pack terms the
/// text writes out (`vocabulary_packs::capitalised_terms`): a word of one that
/// is written with a capital keeps it.
pub fn tidy(text: &str, language: &str, capitalised_terms: &[String]) -> String {
    let english = language.trim().to_ascii_lowercase().starts_with("en");
    // A capitalized word that the same dictation also wrote in lowercase is an
    // ordinary word, not a name — unless it is also a common given name
    // ("Ask Will whether he will come").
    let lowercase_words: HashSet<String> = text
        .split_whitespace()
        .filter(|t| t.chars().any(char::is_alphabetic) && !t.chars().any(char::is_uppercase))
        .map(token_core)
        .filter(|core| !NAME_WORDS.contains(&core.as_str()))
        .collect();
    let mut words = StrayCapitalWords {
        lowercase: lowercase_words,
        kept: HashSet::new(),
    };
    for term in capitalised_terms {
        if term.chars().find(|c| c.is_alphabetic()).is_some_and(char::is_uppercase) {
            words.kept.extend(term.split_whitespace().map(token_core));
        }
    }
    strip_fillers(text, language)
        .split('\n')
        .map(|line| tidy_line(line, english, &words))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Given names that are also everyday words. Seeing the word in lowercase
/// elsewhere in a dictation is no evidence that its capitalised use is not
/// the name.
const NAME_WORDS: &[&str] = &[
    "will", "bill", "mark", "may", "grace", "hope", "faith", "joy", "rose", "jack", "frank",
    "pat", "bob", "sue", "dawn", "art", "rob", "drew", "chase", "ray", "rich", "summer", "iris",
    "ivy", "amber", "ruby", "pearl", "penny", "lily", "holly", "june", "april", "august", "gene",
    "grant", "lance", "miles", "pierce", "reed", "sandy", "victor", "wade", "carol", "cliff",
    "dale", "dean", "don", "earl", "glen", "hazel", "heather", "jade", "jay", "matt", "max",
    "nick", "norm", "olive", "page", "rod", "sage", "skip", "stone", "sterling", "win",
];

/// What `lower_stray_capitals` needs to know about the whole dictation.
struct StrayCapitalWords {
    /// Words the dictation also wrote in lowercase.
    lowercase: HashSet<String>,
    /// Words of capitalised dictionary and pack terms, never lowered.
    kept: HashSet<String>,
}

fn ends_clause(token: &str) -> bool {
    token.chars().last().is_some_and(|c| is_sentence_end(c) || c == ':')
}

fn tidy_line(line: &str, english: bool, words: &StrayCapitalWords) -> String {
    let original: Vec<&str> = line.split_whitespace().collect();
    let mut tokens: Vec<String> = original.iter().map(|t| t.to_string()).collect();
    if english {
        tokens = drop_you_know(tokens);
        tokens = join_split_contraction(tokens);
    }
    tokens = collapse_repeats(tokens);
    tokens = drop_stray_periods(tokens);
    tokens = lower_stray_capitals(tokens, words);
    if tokens.len() == original.len() && tokens.iter().zip(&original).all(|(a, b)| a == b) {
        return line.to_string();
    }
    tokens.join(" ")
}

/// "you know" as a filler: set off by a comma, or not where a verb could be.
fn drop_you_know(tokens: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(tokens.len());
    let mut index = 0;
    while index < tokens.len() {
        let is_pair = token_core(&tokens[index]) == "you"
            && !tokens[index].ends_with(',')
            && tokens.get(index + 1).is_some_and(|next| token_core(next) == "know");
        if is_pair {
            let after_comma = out.last().is_some_and(|previous| previous.ends_with(','));
            let before_comma = tokens[index + 1].ends_with(',');
            let verb = out
                .last()
                .is_some_and(|previous| YOU_KNOW_VERB_CUES.contains(&token_core(previous).as_str()));
            let closes = ends_clause(&tokens[index + 1]);
            let opens = out.last().is_none_or(|previous| ends_clause(previous));
            if (after_comma || before_comma) && !verb && !closes && !opens {
                index += 2;
                continue;
            }
        }
        out.push(tokens[index].clone());
        index += 1;
    }
    out
}

/// "I d like" is how some ASR models write "I'd like".
fn join_split_contraction(tokens: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(tokens.len());
    let mut index = 0;
    while index < tokens.len() {
        let split = tokens[index] == "I"
            && tokens.get(index + 1).is_some_and(|t| t == "d")
            && tokens
                .get(index + 2)
                .is_some_and(|t| I_WOULD_VERBS.contains(&token_core(t).as_str()));
        if split {
            out.push("I'd".to_string());
            index += 2;
        } else {
            out.push(tokens[index].clone());
            index += 1;
        }
    }
    out
}

/// The longest restarted phrase looked for. The longer an exact repeat, the
/// surer it is a restart, so this is a cost bound rather than a safety one.
const MAX_RESTART_WORDS: usize = 12;

/// A phrase said twice in a row is a restart: the first attempt goes. A single
/// word needs to be a function word ("the the") or said three times, because
/// "very very" and "no, no" are things people mean.
fn collapse_repeats(tokens: Vec<String>) -> Vec<String> {
    let cores: Vec<String> = tokens.iter().map(|t| token_core(t)).collect();
    let mut keep = vec![true; tokens.len()];
    let mut index = 0;
    'scan: while index < tokens.len() {
        for n in (1..=MAX_RESTART_WORDS).rev() {
            if index + 2 * n > tokens.len() {
                continue;
            }
            let first = index..index + n;
            let same = (0..n).all(|k| !cores[index + k].is_empty() && cores[index + k] == cores[index + n + k]);
            // A sentence end inside the first attempt means two sentences.
            let broken = tokens[first.clone()].iter().any(|t| ends_clause(t));
            if !same || broken {
                continue;
            }
            if n == 1 {
                let triple = cores.get(index + 2) == Some(&cores[index]);
                if !triple && !CONTINUATION_WORDS.contains(&cores[index].as_str()) {
                    continue;
                }
            }
            for slot in first {
                keep[slot] = false;
            }
            index += n;
            continue 'scan;
        }
        index += 1;
    }
    tokens
        .into_iter()
        .zip(keep)
        .filter_map(|(token, kept)| kept.then_some(token))
        .collect()
}

/// A period before a lowercase word is a pause, not a sentence end.
fn drop_stray_periods(mut tokens: Vec<String>) -> Vec<String> {
    for index in 0..tokens.len().saturating_sub(1) {
        let next_is_lower = tokens[index + 1].chars().next().is_some_and(char::is_lowercase);
        let token = &tokens[index];
        let core = token_core(token);
        let plain_word = !core.is_empty()
            && !core.contains('.')
            && !core.chars().all(|c| c.is_ascii_digit());
        if next_is_lower && plain_word && token.ends_with('.') && !token.ends_with("..") {
            tokens[index].pop();
        }
    }
    tokens
}

/// A capital in the middle of a sentence, on a word that is plainly not a name.
fn lower_stray_capitals(mut tokens: Vec<String>, words: &StrayCapitalWords) -> Vec<String> {
    for index in 1..tokens.len() {
        if ends_clause(&tokens[index - 1]) {
            continue;
        }
        let token = &tokens[index];
        let core = token_core(token);
        let mut letters = token.chars().filter(|c| c.is_alphabetic());
        let plain_capital = letters.next().is_some_and(char::is_uppercase)
            && letters.all(char::is_lowercase)
            && core.chars().count() > 1
            && core != "i"
            && !core.starts_with("i'")
            && !core.starts_with("i’");
        let ordinary = CONTINUATION_WORDS.contains(&core.as_str())
            || CONTINUATION_WORDS.contains(&core.split(['\'', '’']).next().unwrap_or(""))
            || words.lowercase.contains(&core);
        if plain_capital && ordinary && !words.kept.contains(&core) {
            tokens[index] = token
                .chars()
                .map(|c| if c.is_uppercase() { c.to_lowercase().next().unwrap_or(c) } else { c })
                .collect();
        }
    }
    tokens
}

fn normalized_words(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(|w| w.to_lowercase())
        .collect()
}

fn normalize_term(term: &str) -> String {
    term.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn levenshtein_within(a: &[char], b: &[char], budget: usize) -> bool {
    if a.len().abs_diff(b.len()) > budget {
        return false;
    }
    let mut previous: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut current = vec![i + 1];
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            current.push((previous[j + 1] + 1).min(current[j] + 1).min(previous[j] + cost));
        }
        previous = current;
    }
    previous[b.len()] <= budget
}

/// Whether one to three consecutive words of `words` spell `term` within
/// `budget` edits ("rail way" and "railway" both reach "Railway").
fn sounds_like(words: &[String], term: &str, budget: usize) -> bool {
    let term: Vec<char> = term.chars().collect();
    if term.is_empty() {
        return false;
    }
    for start in 0..words.len() {
        let mut joined = String::new();
        for word in words.iter().skip(start).take(3) {
            joined.push_str(word);
            if joined.chars().count() > term.len() + budget {
                break;
            }
            let candidate: Vec<char> = joined.chars().collect();
            if levenshtein_within(&candidate, &term, budget) {
                return true;
            }
        }
    }
    false
}

/// Whether `word` (lowercase, alphanumeric) is a plausible rendering of
/// something in `spoken_words` — the same word, a respelling of it, or a run
/// of words joined up ("rocket deck" for "rocketdeck").
pub fn was_spoken(spoken_words: &[String], word: &str) -> bool {
    sounds_like(spoken_words, word, mishearing_budget(word))
}

/// Short terms must be spoken exactly; longer ones may be misheard by about
/// one letter in five.
fn mishearing_budget(term: &str) -> usize {
    let len = term.chars().count();
    if len <= 4 {
        0
    } else {
        (len / 5).max(1)
    }
}

/// Whether `intended` could be what the speaker said when the model wrote
/// `heard` ("cloud" for "Claude", "rocket deck" for "RocketDeck"). Spacing,
/// case and punctuation are ignored, and the user's fix gets one letter more
/// leeway than `mishearing_budget`, because a person confirmed it.
pub fn misheard_as(heard: &str, intended: &str) -> bool {
    let heard: Vec<char> = normalize_term(heard).chars().collect();
    let intended_key = normalize_term(intended);
    let intended: Vec<char> = intended_key.chars().collect();
    if heard.is_empty() || intended.is_empty() {
        return false;
    }
    levenshtein_within(&heard, &intended, mishearing_budget(&intended_key) + 1)
}

/// The dictionary terms that something in `raw` plausibly refers to. A small
/// model treats every term it is shown as a candidate word, so terms nobody
/// said are never shown to it.
pub fn relevant_vocabulary(raw: &str, vocabulary: &[String]) -> Vec<String> {
    let words = normalized_words(raw);
    vocabulary
        .iter()
        .filter(|term| {
            let key = normalize_term(term);
            sounds_like(&words, &key, mishearing_budget(&key))
        })
        .cloned()
        .collect()
}

/// A dictionary term that appears in `edited` although nothing in `raw`
/// sounded like it — the model made it up.
pub fn ungrounded_vocabulary<'a>(
    raw: &str,
    edited: &str,
    vocabulary: &'a [String],
) -> Option<&'a str> {
    let raw_words = normalized_words(raw);
    let edited_words = normalized_words(edited);
    vocabulary.iter().map(String::as_str).find(|term| {
        let key = normalize_term(term);
        sounds_like(&edited_words, &key, 0)
            && !sounds_like(&raw_words, &key, mishearing_budget(&key))
    })
}

fn last_word(text: &str) -> Option<String> {
    normalized_words(text).pop()
}

/// True when `raw` was cut off before its sentence ended.
fn ends_unfinished(raw: &str) -> bool {
    let trimmed = raw.trim_end_matches(|c: char| c.is_whitespace() || matches!(c, '"' | '”' | '\''));
    if trimmed.chars().last().is_none_or(is_sentence_end) {
        return false;
    }
    // ASR that punctuates at all would have closed a finished sentence; ASR
    // that never punctuates gives no such evidence, so only a dangling last
    // word counts there.
    let punctuates = trimmed.chars().any(is_sentence_end);
    punctuates || last_word(trimmed).is_some_and(|w| DANGLING_WORDS.contains(&w.as_str()))
}

fn continues_sentence(lead_in: &str) -> bool {
    let trimmed =
        lead_in.trim_end_matches(|c: char| c.is_whitespace() || matches!(c, '"' | '”' | '\'' | ')'));
    // An email greeting laid out on its own line ends with a comma, but what
    // follows it opens a new paragraph.
    if trimmed.lines().last().is_some_and(is_salutation_line) {
        return false;
    }
    trimmed
        .chars()
        .last()
        .is_some_and(|c| !is_sentence_end(c) && c != ':')
}

/// A greeting or sign-off line that a format layout set on its own ("Hi
/// Mary,", "Thanks,", "Best regards,"): a short capitalised line ending in a
/// comma. Plain cleanup never produces one, because it never breaks a line
/// after a comma.
pub fn is_salutation_line(line: &str) -> bool {
    let line = line.trim();
    line.ends_with(',')
        && line.split_whitespace().count() <= 4
        && line.chars().next().is_some_and(char::is_uppercase)
}

/// Undoes what a model does to the edges of a fragment: the capital on a tail
/// that continues `lead_in` mid-sentence, and — while the speaker is still
/// talking — the period on a tail that stops mid-sentence.
pub fn repair_fragment_edges(raw: &str, polished: &str, lead_in: &str, is_final: bool) -> String {
    let mut text = polished.trim().to_string();
    if continues_sentence(lead_in) {
        let first: String = text.chars().take_while(|c| !c.is_whitespace()).collect();
        let word = token_core(&first);
        let plain_capital = first.chars().next().is_some_and(char::is_uppercase)
            && first.chars().skip(1).all(|c| !c.is_uppercase())
            && word != "i"
            && !word.starts_with("i'")
            && !word.starts_with("i’");
        let raw_was_lower = raw
            .trim_start()
            .chars()
            .find(|c| c.is_alphabetic())
            .is_some_and(char::is_lowercase);
        if plain_capital && (raw_was_lower || CONTINUATION_WORDS.contains(&word.as_str())) {
            let mut chars = text.chars();
            if let Some(head) = chars.next() {
                text = head.to_lowercase().chain(chars).collect();
            }
        }
    }
    if !is_final && ends_unfinished(raw) && text.ends_with('.') && !text.ends_with("..") {
        text.pop();
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vocabulary() -> Vec<String> {
        ["Hypercube", "RocketDeck", "Railway", "Claude", "AI", "MCP"]
            .iter()
            .map(|s| s.to_string())
            .collect()
    }

    #[test]
    fn strips_trailing_non_speech_sentences() {
        assert_eq!(
            strip_fillers(
                "And review it for its soundness, architectural Robustness. Mm-hmm.",
                "en"
            ),
            "And review it for its soundness, architectural Robustness."
        );
        assert_eq!(strip_fillers("at least for now. Mm hmm.", "en"), "at least for now.");
    }

    #[test]
    fn filler_hands_its_capital_and_period_on() {
        assert_eq!(strip_fillers("Um, so I think we ship.", "en"), "So I think we ship.");
        assert_eq!(strip_fillers("we could try, uh. Then we stop", "en"), "we could try. Then we stop");
        assert_eq!(
            strip_fillers("I was thinking uh that we could er ship", "en"),
            "I was thinking that we could ship"
        );
    }

    #[test]
    fn language_gated_fillers_survive_other_languages() {
        assert_eq!(strip_fillers("um dia de sol uh", "pt"), "um dia de sol");
        assert_eq!(strip_fillers("um dia de sol", "auto"), "um dia de sol");
    }

    #[test]
    fn text_without_fillers_is_returned_untouched() {
        let text = "We have two goals:\n1. Fix the  login bug\n2. Write docs";
        assert_eq!(strip_fillers(text, "en"), text);
    }

    #[test]
    fn tidy_repairs_what_speech_leaves_behind() {
        for (raw, tidied) in [
            // Stray periods and capitals around a pause.
            (
                "how long the Transcription takes, how long the transcription model takes. and the rest",
                "how long the transcription takes, how long the transcription model takes and the rest",
            ),
            // "polished" is no evidence that "Polish" is an ordinary word.
            (
                "Find out what the Polish got wrong. and what had to be polished.",
                "Find out what the Polish got wrong and what had to be polished.",
            ),
            (
                "we give them some sentences and then we can train the model. based on that. Could that work?",
                "we give them some sentences and then we can train the model based on that. Could that work?",
            ),
            (
                "only in the text box because you know, the the text box Where it polishes.",
                "only in the text box because the text box where it polishes.",
            ),
            // Restarted phrases; the second attempt is the one that continues.
            (
                "So we don't need to worry about the So we don't need to worry about the ads yet.",
                "So we don't need to worry about the ads yet.",
            ),
            ("show it as a new a new conversation", "show it as a new conversation"),
            ("I have been using it for a For a time.", "I have been using it for a time."),
            // Split contraction.
            ("I d like to know if that works.", "I'd like to know if that works."),
        ] {
            assert_eq!(tidy(raw, "en", &[]), tidied);
        }
    }

    #[test]
    fn tidy_leaves_meaning_alone() {
        for text in [
            "Do you know, I think it works.",
            "No. No. That is very very wrong.",
            "Ask Claude about Railway. Then tell Sam.",
            "We need version 2.4 of settings.json e.g. the new one.",
            "We have two goals:\n1. Fix the  login bug\n2. Write docs",
            "I think I'll ask Hypercube Labs about Canvas MCP today.",
        ] {
            assert_eq!(tidy(text, "en", &[]), text, "{text}");
        }
        // Word rules are English; structure rules are not.
        assert_eq!(tidy("sabes, you know, que si", "es", &[]), "sabes, you know, que si");
    }

    #[test]
    fn names_that_are_also_words_keep_their_capital() {
        for text in [
            "Ask Will whether he will come.",
            "Tell Bill the bill is paid.",
            "I asked Mark to mark it and Grace said grace.",
            "Maybe May may come too.",
        ] {
            assert_eq!(tidy(text, "en", &[]), text, "{text}");
        }
    }

    #[test]
    fn dictionary_and_pack_terms_keep_their_capital() {
        // "Transcription" seen lowercase is lowered, unless it is a term.
        let text = "how the Transcription takes, how the transcription model takes";
        assert_eq!(
            tidy(text, "en", &[]),
            "how the transcription takes, how the transcription model takes"
        );
        assert_eq!(tidy(text, "en", &["Transcription".to_string()]), text);
        // Continuation words that make up a multi-word term stay too.
        let text = "we shipped it with The Who playing";
        assert_eq!(tidy(text, "en", &[]), "we shipped it with the who playing");
        assert_eq!(tidy(text, "en", &["The Who".to_string()]), text);
        // A lowercase dictionary term gives no capital to keep.
        assert_eq!(tidy("ask Will and will", "en", &["will".to_string()]), "ask Will and will");
    }

    #[test]
    fn only_spoken_vocabulary_is_relevant() {
        let vocabulary = vocabulary();
        assert_eq!(
            relevant_vocabulary("i deployed the hyper cube backend to rail way with claud", &vocabulary),
            vec!["Hypercube", "Railway", "Claude"]
        );
        assert!(relevant_vocabulary("So we don't need to worry about the", &vocabulary).is_empty());
        // Short terms need an exact hit: "a" and "air" are not "AI".
        assert!(relevant_vocabulary("a breath of fresh air", &vocabulary).is_empty());
        assert_eq!(relevant_vocabulary("the ai agrees", &vocabulary), vec!["AI"]);
    }

    #[test]
    fn invented_vocabulary_is_caught() {
        let vocabulary = vocabulary();
        assert_eq!(
            ungrounded_vocabulary(
                "So we don't need to worry about the",
                "So we don't need to worry about the rocket deck.",
                &vocabulary
            ),
            Some("RocketDeck")
        );
        assert_eq!(
            ungrounded_vocabulary(
                "we moved it to rail way last week",
                "We moved it to Railway last week.",
                &vocabulary
            ),
            None
        );
    }

    #[test]
    fn continuation_tail_loses_its_capital() {
        assert_eq!(
            repair_fragment_edges(
                "interpreting if the user is going to quote.",
                "Interpreting if the user is going to quote.",
                "Whisperflow has functionality for",
                false
            ),
            "interpreting if the user is going to quote."
        );
        // A new sentence, a proper noun and "I" all keep their capital.
        for (raw, polished, lead_in) in [
            ("the second one", "The second one.", "That is the first problem."),
            ("John agreed.", "John agreed.", "and then"),
            ("I think so.", "I think so.", "and then"),
            ("MCP servers work.", "MCP servers work.", "and then"),
        ] {
            assert_eq!(repair_fragment_edges(raw, polished, lead_in, true), polished);
        }
    }

    #[test]
    fn unfinished_tail_loses_the_models_period_until_release() {
        let raw = "Okay, this is great. I think this is a perfect";
        let polished = "Okay, this is great. I think this is a perfect.";
        assert_eq!(
            repair_fragment_edges(raw, polished, "", false),
            "Okay, this is great. I think this is a perfect"
        );
        assert_eq!(repair_fragment_edges(raw, polished, "", true), polished);
        // Unpunctuated ASR: only a dangling last word proves a cut.
        assert_eq!(
            repair_fragment_edges("send it tomorrow", "Send it tomorrow.", "", false),
            "Send it tomorrow."
        );
        assert_eq!(
            repair_fragment_edges("send it to the", "Send it to the.", "", false),
            "Send it to the"
        );
    }

    #[test]
    fn misheard_as_accepts_close_respellings_only() {
        assert!(misheard_as("cloud code", "Claude Code"));
        assert!(misheard_as("rocket deck", "RocketDeck"));
        assert!(misheard_as("jon", "John"));
        assert!(misheard_as("siobhan", "Siobhán"));
        assert!(!misheard_as("board", "committee"));
        assert!(!misheard_as("dog", "cat"));
        assert!(!misheard_as("", "anything"));
    }

    #[test]
    fn a_greeting_line_lead_in_opens_a_new_sentence() {
        // An email greeting ends with a comma, but the paragraph after it keeps
        // its capital.
        assert_eq!(
            repair_fragment_edges("thanks for the slides", "Thanks for the slides.", "Hi Mary,", true),
            "Thanks for the slides."
        );
        // A comma mid-sentence still continues it.
        assert_eq!(
            repair_fragment_edges(
                "thanks for the slides",
                "Thanks for the slides.",
                "So I wanted to say,",
                true
            ),
            "thanks for the slides."
        );
        assert!(is_salutation_line("Best regards,"));
        assert!(!is_salutation_line("Hi Mary."));
        assert!(!is_salutation_line("so we went there and then,"));
    }
}
