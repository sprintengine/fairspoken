//! Platform-neutral half of context awareness:
//! caret-aware casing/spacing, on-screen vocabulary extraction, and the
//! budgeted tree walk. All pure logic — the macOS Accessibility reads live in
//! `macos_ax.rs` and feed these functions, which keeps every rule unit-testable
//! off-macOS.
//!
//! Privacy rules enforced by the callers and re-checked here where possible:
//! reads are local and session-only unless optional development metadata capture
//! is enabled; secure fields and password managers are excluded before any text
//! reaches this module.

use std::time::{Duration, Instant};

/// Budgets for the AX tree walk. AX calls are cross-process IPC and can hang
/// or explode in size (web areas); on breach the walk returns partial results.
pub struct WalkBudget {
    pub deadline: Instant,
    pub max_elements: usize,
    pub max_text_bytes: usize,
}

impl WalkBudget {
    pub fn standard() -> Self {
        Self {
            deadline: Instant::now() + Duration::from_millis(150),
            max_elements: 200,
            max_text_bytes: 8 * 1024,
        }
    }
}

/// One node of an accessibility tree — implemented by the real AX element on
/// macOS and by fixtures in tests.
pub trait ContextNode {
    /// AX role, e.g. "AXTextField". Secure fields are excluded by the
    /// implementation before the walk, but the walker re-checks.
    fn role(&self) -> Option<String>;
    /// AX subrole. `NSSecureTextField` and browser password inputs report
    /// role `AXTextField` with subrole `AXSecureTextField`, so the walker
    /// checks both.
    fn subrole(&self) -> Option<String>;
    /// Visible text carried by this node (value/title/description).
    fn texts(&self) -> Vec<String>;
    fn children(&self) -> Vec<Self>
    where
        Self: Sized;
}

pub const SECURE_FIELD_ROLE: &str = "AXSecureTextField";

/// Whether a role/subrole pair marks a secure (password) field.
pub fn is_secure_role(role: Option<&str>, subrole: Option<&str>) -> bool {
    role == Some(SECURE_FIELD_ROLE) || subrole == Some(SECURE_FIELD_ROLE)
}

/// Breadth-first collection of on-screen text under the budgets. The
/// traversal order (roots first, then their children level by level) is what
/// gives "terms nearest the focused element win" — callers pass the focused
/// element as the first root.
pub fn collect_context_text<N: ContextNode>(roots: Vec<N>, budget: &WalkBudget) -> Vec<String> {
    let mut queue: std::collections::VecDeque<N> = roots.into();
    let mut texts = Vec::new();
    let mut visited = 0;
    let mut text_bytes = 0;

    while let Some(node) = queue.pop_front() {
        if visited >= budget.max_elements
            || text_bytes >= budget.max_text_bytes
            || Instant::now() >= budget.deadline
        {
            break;
        }
        visited += 1;

        if is_secure_role(node.role().as_deref(), node.subrole().as_deref()) {
            continue; // never read, never descend
        }

        for text in node.texts() {
            let trimmed = text.trim();
            if trimmed.is_empty() {
                continue;
            }
            text_bytes += trimmed.len();
            texts.push(trimmed.to_string());
            if text_bytes >= budget.max_text_bytes {
                break;
            }
        }

        for child in node.children() {
            queue.push_back(child);
        }
    }

    texts
}

/// Words too common to be useful biasing terms even when capitalized
/// (sentence starts slip through the crude sentence detection).
const STOPLIST: &[&str] = &[
    "the", "a", "an", "and", "or", "but", "if", "then", "this", "that", "these", "those", "i",
    "you", "he", "she", "it", "we", "they", "is", "are", "was", "were", "be", "been", "to", "of",
    "in", "on", "at", "by", "for", "with", "from", "as", "not", "no", "yes", "ok", "okay", "new",
    "all", "any", "can", "will", "just", "so", "up", "down", "out", "about", "into", "over",
    "after", "before", "when", "where", "what", "who", "how", "why", "there", "here", "your",
    "my", "our", "their", "his", "her", "its", "me", "him", "them", "us", "do", "does", "did",
    "have", "has", "had", "get", "got", "go", "going", "one", "two", "today", "yesterday",
    "tomorrow", "now", "am", "pm", "monday", "tuesday", "wednesday", "thursday", "friday",
    "saturday", "sunday",
];

pub const HARVEST_TERM_CAP: usize = 40;

/// Extract candidate vocabulary terms from on-screen text: proper nouns,
/// code identifiers, file paths, versioned tokens. Order-preserving and
/// deduplicating, capped at `HARVEST_TERM_CAP` — earlier texts (nearer the
/// focused element) win the cap.
pub fn extract_candidate_terms(texts: &[String]) -> Vec<String> {
    let mut terms: Vec<String> = Vec::new();
    let mut seen = std::collections::HashSet::new();

    for text in texts {
        let tokens: Vec<&str> = text.split_whitespace().collect();
        for (index, raw_token) in tokens.iter().enumerate() {
            let token = raw_token.trim_matches(|c: char| {
                c.is_ascii_punctuation() && !matches!(c, '/' | '_' | '.' | '-')
            });
            let token = token.trim_end_matches(['.', ',', ':', ';']);
            if token.chars().count() < 2 || token.chars().count() > 60 {
                continue;
            }

            let sentence_initial = index == 0
                || tokens
                    .get(index - 1)
                    .is_some_and(|prev| prev.ends_with(['.', '!', '?']));

            if !is_candidate_term(token, sentence_initial) {
                continue;
            }
            if STOPLIST.contains(&token.to_lowercase().as_str()) {
                continue;
            }
            if seen.insert(token.to_string()) {
                terms.push(token.to_string());
                if terms.len() >= HARVEST_TERM_CAP {
                    return terms;
                }
            }
        }
    }
    terms
}

fn is_candidate_term(token: &str, sentence_initial: bool) -> bool {
    let chars: Vec<char> = token.chars().collect();
    let first_upper = chars[0].is_uppercase();
    let interior_upper = chars.iter().skip(1).any(|c| c.is_uppercase());
    let has_lower = chars.iter().any(|c| c.is_lowercase());
    let interior_digit = chars.iter().skip(1).any(|c| c.is_ascii_digit());
    let all_alpha = chars.iter().all(|c| c.is_alphabetic());

    // camelCase / PascalCase identifiers.
    if interior_upper && has_lower {
        return true;
    }
    // snake_case identifiers.
    if token.contains('_') && chars.iter().any(|c| c.is_alphanumeric()) {
        return true;
    }
    // Paths and files.
    if token.contains('/') && token.len() > 2 {
        return true;
    }
    if has_file_extension(token) {
        return true;
    }
    // Versioned tokens: v3, oauth2, sha256.
    if interior_digit && chars.iter().any(|c| c.is_alphabetic()) {
        return true;
    }
    // Capitalized words that are not just sentence-initial capitalization.
    if first_upper && all_alpha && !sentence_initial && chars.len() >= 3 {
        return true;
    }
    false
}

fn has_file_extension(token: &str) -> bool {
    let Some((stem, ext)) = token.rsplit_once('.') else {
        return false;
    };
    !stem.is_empty()
        && (2..=5).contains(&ext.len())
        && ext.chars().all(|c| c.is_ascii_alphanumeric())
        && ext.chars().any(|c| c.is_ascii_alphabetic())
        && !stem.ends_with('.')
}

/// The text situation at the insertion point, read from the focused element.
#[derive(Clone, Debug, Default)]
pub struct CaretContext {
    /// Text before the caret (already truncated by the reader).
    pub before: String,
    /// The character immediately after the caret, if any.
    pub after_char: Option<char>,
}

/// Whether the caret sits at a sentence start: no text before it, a newline
/// before it (ignoring spaces), or `.`/`!`/`?` as the previous
/// non-space-character.
fn at_sentence_start(before: &str) -> bool {
    for c in before.chars().rev() {
        if c == '\n' {
            return true;
        }
        if c == ' ' || c == '\t' {
            continue;
        }
        return matches!(c, '.' | '!' | '?');
    }
    true // no text at all
}

fn first_word(transcript: &str) -> &str {
    transcript
        .split_whitespace()
        .next()
        .unwrap_or("")
        .trim_matches(|c: char| c.is_ascii_punctuation())
}

fn keep_capital(word: &str, vocabulary: &[String]) -> bool {
    if matches!(word, "I" | "I'm" | "I'll" | "I've" | "I'd") {
        return true;
    }
    // Acronyms: two or more capitals.
    if word.chars().filter(|c| c.is_uppercase()).count() >= 2 {
        return true;
    }
    vocabulary
        .iter()
        .any(|term| term.eq_ignore_ascii_case(word) && term.chars().next().is_some_and(char::is_uppercase))
}

/// Phase A: adapt the transcript's leading capitalization and leading/trailing
/// spacing to the caret context. Mirrors `transcript_clipboard_text`'s
/// contract (trim + single trailing space) when no adjustment applies, so a
/// failed AX read falls back to byte-identical behavior.
pub fn adjust_for_caret(
    transcript: &str,
    context: &CaretContext,
    vocabulary: &[String],
) -> String {
    let trimmed = transcript.trim();
    if trimmed.is_empty() {
        return String::new();
    }

    let mut text = trimmed.to_string();

    // Casing: mid-sentence insertions lose their leading capital unless the
    // first word deserves it (dictionary term, acronym, or the pronoun I).
    if !at_sentence_start(&context.before) && !keep_capital(first_word(trimmed), vocabulary) {
        let mut chars = text.chars();
        if let Some(first) = chars.next() {
            if first.is_alphabetic() && first.is_uppercase() {
                text = first.to_lowercase().collect::<String>() + chars.as_str();
            }
        }
    }

    // Leading space: needed unless the caret already follows whitespace, an
    // opening bracket/quote, or sits at the very start of the field.
    let leading_space = match context.before.chars().last() {
        None => false,
        Some(c) if c.is_whitespace() => false,
        Some('(' | '[' | '{' | '"' | '\'' | '“' | '‘') => false,
        Some(_) => true,
    };
    if leading_space {
        text.insert(0, ' ');
    }

    // Trailing space: today's rule, unless the caret sits immediately before
    // a non-space character (then a trailing space would double up oddly).
    let trailing_space = !matches!(context.after_char, Some(c) if !c.is_whitespace());
    if trailing_space {
        text.push(' ');
    }

    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone)]
    struct FakeNode {
        role: &'static str,
        subrole: Option<&'static str>,
        text: Vec<&'static str>,
        children: Vec<FakeNode>,
    }

    impl ContextNode for FakeNode {
        fn role(&self) -> Option<String> {
            Some(self.role.to_string())
        }
        fn subrole(&self) -> Option<String> {
            self.subrole.map(str::to_string)
        }
        fn texts(&self) -> Vec<String> {
            self.text.iter().map(|t| t.to_string()).collect()
        }
        fn children(&self) -> Vec<FakeNode> {
            self.children.clone()
        }
    }

    fn node(text: Vec<&'static str>, children: Vec<FakeNode>) -> FakeNode {
        FakeNode {
            role: "AXStaticText",
            subrole: None,
            text,
            children,
        }
    }

    fn generous_budget() -> WalkBudget {
        WalkBudget {
            deadline: Instant::now() + Duration::from_secs(5),
            max_elements: 200,
            max_text_bytes: 8 * 1024,
        }
    }

    #[test]
    fn walker_collects_breadth_first_and_skips_secure_fields() {
        let tree = node(
            vec!["root text"],
            vec![
                FakeNode {
                    role: SECURE_FIELD_ROLE,
                    subrole: None,
                    text: vec!["hunter2"],
                    children: vec![node(vec!["child of secure"], vec![])],
                },
                node(vec!["visible child"], vec![]),
            ],
        );

        let texts = collect_context_text(vec![tree], &generous_budget());

        assert_eq!(texts, vec!["root text", "visible child"]);
    }

    #[test]
    fn walker_skips_text_fields_with_the_secure_subrole() {
        // NSSecureTextField and browser password inputs: role AXTextField,
        // subrole AXSecureTextField.
        let tree = node(
            vec!["root text"],
            vec![
                FakeNode {
                    role: "AXTextField",
                    subrole: Some(SECURE_FIELD_ROLE),
                    text: vec!["hunter2"],
                    children: vec![node(vec!["child of secure"], vec![])],
                },
                node(vec!["visible child"], vec![]),
            ],
        );

        let texts = collect_context_text(vec![tree], &generous_budget());

        assert_eq!(texts, vec!["root text", "visible child"]);
        assert!(is_secure_role(Some("AXTextField"), Some(SECURE_FIELD_ROLE)));
        assert!(!is_secure_role(Some("AXTextField"), None));
    }

    #[test]
    fn walker_stops_at_the_element_budget() {
        // A deep chain of 300 nodes against a 200-element budget.
        let mut tree = node(vec!["n"], vec![]);
        for _ in 0..300 {
            tree = node(vec!["n"], vec![tree]);
        }
        let budget = WalkBudget {
            max_elements: 200,
            ..generous_budget()
        };

        let texts = collect_context_text(vec![tree], &budget);

        assert_eq!(texts.len(), 200);
    }

    #[test]
    fn walker_stops_at_the_text_budget() {
        let big = "x".repeat(3000);
        let leaked: &'static str = Box::leak(big.into_boxed_str());
        let tree = node(
            vec![leaked],
            vec![
                node(vec![leaked], vec![]),
                node(vec![leaked], vec![]),
                node(vec![leaked], vec![]),
                node(vec![leaked], vec![]),
            ],
        );

        let texts = collect_context_text(vec![tree], &generous_budget());

        // 8 KiB budget → three 3000-byte texts at most.
        assert!(texts.len() <= 3, "collected {}", texts.len());
    }

    #[test]
    fn walker_respects_an_expired_deadline() {
        let tree = node(vec!["anything"], vec![]);
        let budget = WalkBudget {
            deadline: Instant::now() - Duration::from_millis(1),
            ..generous_budget()
        };

        assert!(collect_context_text(vec![tree], &budget).is_empty());
    }

    #[test]
    fn extractor_finds_identifiers_paths_and_proper_nouns() {
        let texts = vec![
            "fn start_remote_streaming_session builds the RemoteStreamingSession".to_string(),
            "See src-tauri/src/remote_transcription.rs and settings.json".to_string(),
            "Talked to Priya about Parakeet v3 and oauth2 flows".to_string(),
        ];

        let terms = extract_candidate_terms(&texts);

        assert!(terms.contains(&"start_remote_streaming_session".to_string()));
        assert!(terms.contains(&"RemoteStreamingSession".to_string()));
        assert!(terms.contains(&"src-tauri/src/remote_transcription.rs".to_string()));
        assert!(terms.contains(&"settings.json".to_string()));
        assert!(terms.contains(&"Priya".to_string()));
        assert!(terms.contains(&"Parakeet".to_string()));
        assert!(terms.contains(&"v3".to_string()));
        assert!(terms.contains(&"oauth2".to_string()));
    }

    #[test]
    fn extractor_skips_sentence_initial_capitals_and_stopwords() {
        let texts = vec!["The meeting went well. Overall we agreed. Ask Sarah".to_string()];

        let terms = extract_candidate_terms(&texts);

        assert!(!terms.contains(&"The".to_string()));
        assert!(!terms.contains(&"Overall".to_string())); // sentence-initial
        assert!(!terms.contains(&"Ask".to_string())); // sentence-initial
        assert!(terms.contains(&"Sarah".to_string()));
    }

    #[test]
    fn extractor_dedups_and_caps_at_forty_preferring_earlier_texts() {
        let mut texts = vec!["NearestTerm NearestTerm".to_string()];
        for i in 0..60 {
            texts.push(format!("FarTerm{i}Xy"));
        }

        let terms = extract_candidate_terms(&texts);

        assert_eq!(terms.len(), HARVEST_TERM_CAP);
        assert_eq!(terms[0], "NearestTerm");
        assert_eq!(terms.iter().filter(|t| *t == "NearestTerm").count(), 1);
    }

    fn ctx(before: &str, after: Option<char>) -> CaretContext {
        CaretContext {
            before: before.to_string(),
            after_char: after,
        }
    }

    #[test]
    fn caret_casing_table() {
        let vocab = vec!["Tauri".to_string()];
        let cases: Vec<(&str, CaretContext, &str)> = vec![
            // Empty field → sentence start, keep capital, no leading space.
            ("Hello there", ctx("", None), "Hello there "),
            // After a period → sentence start.
            ("Hello there", ctx("Done.", None), " Hello there "),
            ("Hello there", ctx("Done. ", None), "Hello there "),
            // After a newline (even with trailing spaces) → sentence start.
            ("Hello there", ctx("line one\n", None), "Hello there "),
            ("Hello there", ctx("line one\n  ", None), "Hello there "),
            // Mid-sentence → lowercase the leading capital.
            ("Hello there", ctx("I think", None), " hello there "),
            ("Hello there", ctx("I think ", None), "hello there "),
            // Mid-sentence but the first word must keep its capital.
            ("I think so", ctx("and then", None), " I think so "),
            ("I'm ready", ctx("and then", None), " I'm ready "),
            ("USB is broken", ctx("the", None), " USB is broken "),
            ("Tauri is fast", ctx("using", None), " Tauri is fast "),
            // Opening bracket/quote → no leading space.
            ("Hello", ctx("(", None), "hello "),
            ("Hello", ctx("\"", None), "hello "),
            // Caret immediately before a non-space char → no trailing space.
            ("Hello", ctx("say ", Some('w')), "hello"),
            ("Hello", ctx("say ", Some(' ')), "hello "),
            // Question mark ends a sentence.
            ("Hello", ctx("Really?", None), " Hello "),
        ];

        for (transcript, context, expected) in cases {
            assert_eq!(
                adjust_for_caret(transcript, &context, &vocab),
                expected,
                "transcript={transcript:?} before={:?} after={:?}",
                context.before,
                context.after_char,
            );
        }
    }

    #[test]
    fn caret_adjustment_of_empty_transcript_stays_empty() {
        assert_eq!(adjust_for_caret("   ", &ctx("abc", None), &[]), "");
    }
}
