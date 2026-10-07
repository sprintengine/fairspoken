//! Pure half of the edit watcher: find a dictation again in its field's
//! current text, diff it word by word against what was inserted, and pick out
//! the edits that look like the user fixing a misheard word. The macOS
//! Accessibility reads live in `edit_watch.rs` and `macos_ax.rs`; everything
//! here is platform-neutral so it can be unit-tested anywhere.
//!
//! Offsets are chars except where noted: Accessibility reports UTF-16 code
//! units, which are converted once in `InsertionAnchor::from_field`.

use crate::transcript_cleanup::misheard_as;

/// Text kept on each side of an insertion to find it again later. It lives
/// only in memory until the watch is evaluated, and is never persisted.
pub const CONTEXT_CHARS: usize = 80;
/// The shortest anchor tried when the user also edited the text next to the
/// dictation; shorter anchors match too many places.
const MIN_ANCHOR_CHARS: usize = 8;
/// A fix replaces at most this many words with at most this many words.
const MAX_FIX_WORDS: usize = 3;
/// Word diffs are quadratic; larger regions are treated as rewrites.
const MAX_DIFF_WORDS: usize = 2000;

/// Where a dictation landed: the text around it and what was inserted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InsertionAnchor {
    /// Up to `CONTEXT_CHARS` immediately before the insertion.
    pub before: String,
    pub inserted: String,
    /// Up to `CONTEXT_CHARS` immediately after the insertion.
    pub after: String,
    /// Char offset of the insertion's start when it was made.
    pub start: usize,
}

impl InsertionAnchor {
    /// Builds the anchor from the field's text and selection, read just before
    /// `inserted` replaced the selection. The selection is in UTF-16 code
    /// units, as `AXSelectedTextRange` reports it.
    pub fn from_field(
        value: &str,
        selection_start: usize,
        selection_length: usize,
        inserted: &str,
    ) -> Self {
        let utf16: Vec<u16> = value.encode_utf16().collect();
        let start = selection_start.min(utf16.len());
        let end = start.saturating_add(selection_length).min(utf16.len());
        let before = String::from_utf16_lossy(&utf16[..start]);
        let after = String::from_utf16_lossy(&utf16[end..]);
        let before_chars = before.chars().count();
        Self {
            before: before
                .chars()
                .skip(before_chars.saturating_sub(CONTEXT_CHARS))
                .collect(),
            inserted: inserted.to_string(),
            after: after.chars().take(CONTEXT_CHARS).collect(),
            start: before_chars,
        }
    }

    /// The dictated region in the field's current text: whatever now sits
    /// between the text that surrounded the insertion. Offsets shift when the
    /// user types elsewhere, so the context is searched for, nearest the old
    /// position first, and trimmed down when the user edited it too. `None`
    /// when the context is gone or the region is implausibly large.
    pub fn locate(&self, current: &str) -> Option<String> {
        let text: Vec<char> = current.chars().collect();
        let inserted_len = self.inserted.chars().count();

        let before: Vec<char> = self.before.chars().collect();
        let start = if before.is_empty() {
            0
        } else {
            anchor_lengths(before.len()).into_iter().find_map(|length| {
                let needle = &before[before.len() - length..];
                let expected = self.start.saturating_sub(length);
                nearest_occurrence(&text, needle, 0, expected).map(|index| index + length)
            })?
        };

        let after: Vec<char> = self.after.chars().collect();
        let end = if after.is_empty() {
            text.len()
        } else {
            anchor_lengths(after.len()).into_iter().find_map(|length| {
                nearest_occurrence(&text, &after[..length], start, start + inserted_len)
            })?
        };

        let region: String = text[start..end].iter().collect();
        let region = trim_adjacent_typing(&self.inserted, &region, before.is_empty(), after.is_empty());
        (region.chars().count() <= inserted_len * 3 + 200).then_some(region)
    }
}

/// Drops text the user typed next to the dictation rather than into it: a
/// run of new words before the first dictated word, or after the last one.
/// That run is only cut off at a line break (a new paragraph above or below)
/// or where the region is open-ended because the dictation started or ended
/// the field; otherwise it may be the user's edit and is kept.
fn trim_adjacent_typing(inserted: &str, region: &str, open_start: bool, open_end: bool) -> String {
    let old = words(inserted);
    let tokens = word_tokens(region);
    if old.len() > MAX_DIFF_WORDS || tokens.len() > MAX_DIFF_WORDS {
        return region.to_string();
    }
    let new: Vec<String> = tokens.iter().map(|(_, word)| word.clone()).collect();
    let hunks = diff_words(&old, &new);
    let ends_line = |cut: usize| region[..cut].trim_end_matches([' ', '\t']).ends_with('\n');

    let mut start = 0;
    if let Some(first) = hunks.first() {
        if first.removed.is_empty() && first.new_start == 0 && first.new_end < new.len() {
            let cut = tokens[first.new_end].0;
            if open_start || ends_line(cut) {
                start = cut;
            }
        }
    }
    let mut end = region.len();
    if let Some(last) = hunks.last() {
        if last.removed.is_empty() && last.new_end == new.len() && last.new_start > 0 {
            let cut = tokens[last.new_start].0;
            if cut > start && (open_end || ends_line(cut)) {
                end = cut;
            }
        }
    }
    region[start..end].to_string()
}

/// Anchor lengths to try, longest first: the whole context, then shorter
/// pieces next to the insertion.
fn anchor_lengths(available: usize) -> Vec<usize> {
    let mut lengths = vec![available];
    for length in [40, 20, MIN_ANCHOR_CHARS] {
        if length < available {
            lengths.push(length);
        }
    }
    lengths
}

/// The occurrence of `needle` at or after `from` that starts closest to
/// `expected`.
fn nearest_occurrence(text: &[char], needle: &[char], from: usize, expected: usize) -> Option<usize> {
    if needle.is_empty() || needle.len() > text.len() {
        return None;
    }
    (from..=text.len() - needle.len())
        .filter(|&index| text[index..index + needle.len()] == *needle)
        .min_by_key(|&index| index.abs_diff(expected))
}

/// A short substitution the user made that sounds like what was dictated.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mishearing {
    pub heard: String,
    pub intended: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EditOutcome {
    /// The words are as dictated (punctuation and spacing may differ).
    Unchanged,
    /// Some words changed. `fixes` holds the changes that look like a
    /// misheard word being corrected; other edits are ignored.
    Edited { fixes: Vec<Mishearing> },
    /// Too much changed to read word fixes out of it.
    Rewrite,
}

/// Diffs the dictation against its edited region word by word and classifies
/// each change. Only a 1–3-word substitution that sounds like the original is
/// a fix; insertions, deletions and unrelated replacements are not, and a
/// region where most words changed is a rewrite.
pub fn classify(inserted: &str, edited: &str) -> EditOutcome {
    let old = words(inserted);
    let new = words(edited);
    if old.len() > MAX_DIFF_WORDS || new.len() > MAX_DIFF_WORDS {
        return EditOutcome::Rewrite;
    }
    let hunks = diff_words(&old, &new);
    if hunks.is_empty() {
        return EditOutcome::Unchanged;
    }
    let removed: usize = hunks.iter().map(|hunk| hunk.removed.len()).sum();
    if old.is_empty() || (old.len() > MAX_FIX_WORDS && removed * 2 > old.len()) {
        return EditOutcome::Rewrite;
    }
    let fixes = hunks
        .into_iter()
        .filter(|hunk| {
            (1..=MAX_FIX_WORDS).contains(&hunk.removed.len())
                && (1..=MAX_FIX_WORDS).contains(&hunk.added.len())
        })
        .map(|hunk| Mishearing {
            heard: hunk.removed.join(" "),
            intended: hunk.added.join(" "),
        })
        .filter(|fix| fix.heard != fix.intended && misheard_as(&fix.heard, &fix.intended))
        .collect();
    EditOutcome::Edited { fixes }
}

/// Words with surrounding punctuation removed, so "Dublin," and "Dublin."
/// compare equal; case is kept because capitalising a name is a fix.
fn words(text: &str) -> Vec<String> {
    word_tokens(text).into_iter().map(|(_, word)| word).collect()
}

/// `words`, each with the byte offset of the whitespace-delimited token it
/// came from.
fn word_tokens(text: &str) -> Vec<(usize, String)> {
    let mut tokens = Vec::new();
    let mut token_start = None;
    for (index, ch) in text.char_indices().chain(std::iter::once((text.len(), ' '))) {
        match (ch.is_whitespace(), token_start) {
            (false, None) => token_start = Some(index),
            (true, Some(start)) => {
                let word = text[start..index].trim_matches(|c: char| !c.is_alphanumeric());
                if !word.is_empty() {
                    tokens.push((start, word.to_string()));
                }
                token_start = None;
            }
            _ => {}
        }
    }
    tokens
}

#[derive(Debug, Default)]
struct Hunk {
    removed: Vec<String>,
    added: Vec<String>,
    /// The added words' index range in the new word list.
    new_start: usize,
    new_end: usize,
}

/// The runs of words that differ between `old` and `new`, from a longest
/// common subsequence alignment.
fn diff_words(old: &[String], new: &[String]) -> Vec<Hunk> {
    let (n, m) = (old.len(), new.len());
    // lcs[i][j] = LCS length of old[i..] and new[j..].
    let mut lcs = vec![vec![0u32; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            lcs[i][j] = if old[i] == new[j] {
                lcs[i + 1][j + 1] + 1
            } else {
                lcs[i + 1][j].max(lcs[i][j + 1])
            };
        }
    }

    let mut hunks = Vec::new();
    let mut current = Hunk::default();
    let (mut i, mut j) = (0, 0);
    while i < n || j < m {
        if i < n && j < m && old[i] == new[j] {
            if !current.removed.is_empty() || !current.added.is_empty() {
                hunks.push(std::mem::take(&mut current));
            }
            i += 1;
            j += 1;
            current.new_start = j;
            current.new_end = j;
        } else if j < m && (i == n || lcs[i][j + 1] >= lcs[i + 1][j]) {
            current.added.push(new[j].clone());
            j += 1;
            current.new_end = j;
        } else {
            current.removed.push(old[i].clone());
            i += 1;
        }
    }
    if !current.removed.is_empty() || !current.added.is_empty() {
        hunks.push(current);
    }
    hunks
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The field before insertion is `before|after`, with the caret at `|`;
    /// returns the anchor and the field's text right after the insertion.
    fn insert(field: &str, inserted: &str) -> (InsertionAnchor, String) {
        let (before, after) = field.split_once('|').unwrap();
        let caret = before.encode_utf16().count();
        let anchor = InsertionAnchor::from_field(&format!("{before}{after}"), caret, 0, inserted);
        (anchor, format!("{before}{inserted}{after}"))
    }

    fn fix(heard: &str, intended: &str) -> Mishearing {
        Mishearing {
            heard: heard.to_string(),
            intended: intended.to_string(),
        }
    }

    #[test]
    fn typo_fix_is_found_and_classified() {
        let (anchor, inserted) = insert(
            "Dear team,\n|\nThanks, Aoife",
            "Their going to ship the release on Friday. ",
        );
        let edited = inserted.replace("Their", "They're");

        let region = anchor.locate(&edited).unwrap();

        assert_eq!(region, "They're going to ship the release on Friday. ");
        assert_eq!(
            classify(&anchor.inserted, &region),
            EditOutcome::Edited {
                fixes: vec![fix("Their", "They're")]
            }
        );
    }

    #[test]
    fn name_fix_is_a_misheard_candidate() {
        let (anchor, inserted) = insert("Notes: |", "Ask cloud code to rebase the branch. ");
        let edited = inserted.replace("cloud code", "Claude Code");

        let region = anchor.locate(&edited).unwrap();

        assert_eq!(
            classify(&anchor.inserted, &region),
            EditOutcome::Edited {
                fixes: vec![fix("cloud code", "Claude Code")]
            }
        );
    }

    #[test]
    fn capitalising_a_name_counts_as_a_fix() {
        assert_eq!(
            classify("send it to niamh today", "send it to Niamh today"),
            EditOutcome::Edited {
                fixes: vec![fix("niamh", "Niamh")]
            }
        );
    }

    #[test]
    fn full_rewrite_yields_no_fixes() {
        let (anchor, _) = insert("Subject line|", " I went to the shop to buy some milk and bread. ");
        let rewritten = "Subject line Picked up groceries this morning, all sorted. ";

        let region = anchor.locate(rewritten).unwrap();

        assert_eq!(classify(&anchor.inserted, &region), EditOutcome::Rewrite);
    }

    #[test]
    fn unrelated_replacement_and_pure_insertions_are_ignored() {
        assert_eq!(
            classify(
                "please send the report to the board",
                "please send the final report to the committee"
            ),
            EditOutcome::Edited { fixes: Vec::new() }
        );
        assert_eq!(
            classify("Meeting at noon.", "Meeting at noon!"),
            EditOutcome::Unchanged
        );
    }

    #[test]
    fn text_typed_before_the_region_shifts_offsets_but_not_the_match() {
        let (anchor, inserted) = insert(
            "Hello Seán,\n\n|\n\nBest wishes",
            "The results from Beaumont came back normal. ",
        );
        // The user adds a paragraph above, then fixes the dictation.
        let edited = inserted
            .replace("Hello Seán,", "Hello Seán,\n\nA quick update before the weekend.")
            .replace("normal", "normal again");

        let region = anchor.locate(&edited).unwrap();

        assert_eq!(region, "The results from Beaumont came back normal again. ");
        assert_eq!(
            classify(&anchor.inserted, &region),
            EditOutcome::Edited { fixes: Vec::new() }
        );
    }

    #[test]
    fn edited_context_falls_back_to_a_shorter_anchor() {
        let (anchor, inserted) = insert(
            "The first paragraph is long enough to need trimming down. |",
            "Book a table at four. ",
        );
        let edited = inserted
            .replace("The first paragraph", "Our opening paragraph")
            .replace("four", "for");

        let region = anchor.locate(&edited).unwrap();

        assert_eq!(region, "Book a table at for. ");
    }

    #[test]
    fn emoji_before_the_caret_converts_utf16_offsets() {
        // Each emoji is two UTF-16 code units but one char.
        let (anchor, inserted) = insert("🎉🎉 Party |🥳 tonight", "with siobhan ");
        assert_eq!(anchor.start, 9);
        assert_eq!(anchor.before, "🎉🎉 Party ");
        assert_eq!(anchor.after, "🥳 tonight");

        let edited = inserted.replace("siobhan", "Siobhán");
        let region = anchor.locate(&edited).unwrap();

        assert_eq!(region, "with Siobhán ");
        assert_eq!(
            classify(&anchor.inserted, &region),
            EditOutcome::Edited {
                fixes: vec![fix("siobhan", "Siobhán")]
            }
        );
    }

    #[test]
    fn selection_replaced_by_the_dictation_is_excluded_from_the_context() {
        let value = "Call 🙂 Bob about it";
        // "Bob" is selected: UTF-16 offset 8 (the emoji counts twice), length 3.
        let anchor = InsertionAnchor::from_field(value, 8, 3, "Rob");
        assert_eq!(anchor.before, "Call 🙂 ");
        assert_eq!(anchor.after, " about it");
    }

    #[test]
    fn deleted_context_or_dictation_is_not_found() {
        let (anchor, _) = insert("Intro text here. |Closing text here.", "Dictated words. ");
        assert_eq!(anchor.locate("Something else entirely"), None);
    }

    #[test]
    fn typing_on_after_a_dictation_at_the_end_is_not_part_of_it() {
        let (anchor, inserted) = insert("Plan:\n|", "Ship the beta on Monday. ");
        let edited = format!("{inserted}Then write the release notes and tell the team.");

        let region = anchor.locate(&edited).unwrap();

        assert_eq!(region, "Ship the beta on Monday. ");
        assert_eq!(classify(&anchor.inserted, &region), EditOutcome::Unchanged);
    }
}
