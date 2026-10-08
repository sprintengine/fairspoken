//! Incremental polish state for one dictation.
//!
//! Polish used to be a single pass over the whole transcript at release, which
//! costs O(entire dictation) at exactly the moment the user is waiting. Instead
//! the stream keeps a *sealed* polished prefix and re-polishes only the
//! volatile tail behind it, so every pass — including the one at release —
//! costs O(the last few ASR chunks).
//!
//! Two properties from the ASR layer make this safe:
//! * chunk merging only ever appends, so a raw prefix never changes meaning;
//! * `TranscriptPreview::sealed_len` is the frozen prefix: text behind the last
//!   silence gap, or everything except the last three chunks, whichever is
//!   further along.
//!
//! The ASR freeze point is where an audio chunk happened to end, which is
//! often mid-sentence — and a model handed half a sentence capitalizes it,
//! closes it with a period, and cannot resolve a correction or a list that
//! continues in the next chunk. So the stream seals at the last *sentence* end
//! behind the freeze point and carries the unfinished remainder in the tail
//! (`sentence_seal_point`), bounded so a speaker who never finishes a sentence
//! cannot grow the tail without limit.
//!
//! A job never slices a model result to match a chunk boundary — polish output
//! does not line up with raw offsets. Newly frozen text is either promoted from
//! an exact tail match, polished as its own seal pass, or adopted as raw when
//! the worker skipped far ahead.

/// How much polished context to hand the model as the lead-in for a tail pass,
/// so it can continue a sentence rather than re-opening one. Bounded because
/// it is prompt weight on every pass.
const CONTEXT_CHARS: usize = 320;

/// How much frozen text may wait unsealed for its sentence to end. Past this
/// the freeze point is used as is: the tail is re-polished on every pass, so
/// it has to stay short.
const MAX_UNSEALED_BYTES: usize = 400;

/// The most frozen text one seal pass will polish. More than this means the
/// worker skipped several previews, and the backlog is adopted raw instead.
const SEAL_PASS_MAX_BYTES: usize = 600;

/// At release, frozen text the worker never reached is polished with the tail
/// when together they stay under this; a larger backlog is adopted raw so the
/// wait at release stays bounded.
const FINAL_PASS_MAX_BYTES: usize = 600;

#[derive(Debug, Default)]
pub struct PolishStream {
    /// The session this state belongs to; a mismatch means a stale writer.
    session: u64,
    /// Polished text for the raw prefix covered by `sealed_raw_len`.
    sealed: String,
    /// Bytes of the cumulative raw transcript the sealed text covers.
    sealed_raw_len: usize,
    /// Latest polished rendering of the volatile tail.
    tail: String,
    /// Raw text `tail` was produced from, so a repeat pass can be skipped.
    tail_raw: String,
    /// Whether any pass actually rewrote its tail. Passes that fail or trip a
    /// guardrail contribute the raw text, so output alone does not mean the
    /// transcript was polished.
    changed: bool,
    /// Last ASR frozen length observed, so commit can bound a pass even when
    /// live polish never applied.
    frozen_len: usize,
}

/// One unit of work for the polish worker.
#[derive(Clone, Debug, PartialEq)]
pub struct TailJob {
    /// The raw text to polish — only the part after the seal point.
    pub raw: String,
    /// Polished lead-in, supplied to the model as surrounding context.
    pub context: String,
    /// Raw bytes consumed so far, carried back into `apply`.
    pub raw_len: usize,
    /// Seal the tail into the prefix once this job lands.
    pub seal: bool,
}

impl PolishStream {
    /// Drops all state and binds the stream to a new dictation.
    pub fn begin(&mut self, session: u64) {
        *self = Self {
            session,
            ..Self::default()
        };
    }

    /// The text to show or commit: sealed prefix plus the latest tail.
    pub fn composed(&self) -> String {
        join(&self.sealed, &self.tail)
    }

    /// True once any pass has produced output for this session, polished or
    /// not — i.e. the stream can describe the transcript.
    #[cfg(test)]
    pub fn has_output(&self) -> bool {
        !self.sealed.is_empty() || !self.tail.is_empty()
    }

    /// True when at least one pass genuinely rewrote the text.
    pub fn changed(&self) -> bool {
        self.changed
    }

    /// Raw bytes already sealed, so callers can tell how much work is left.
    #[cfg(test)]
    pub fn sealed_raw_len(&self) -> usize {
        self.sealed_raw_len
    }

    /// Records the ASR frozen prefix so commit can bound work even if no pass
    /// has applied yet.
    pub fn observe(&mut self, session: u64, frozen_len: usize) {
        if session == self.session {
            self.frozen_len = self.frozen_len.max(frozen_len);
        }
    }

    /// Describes the pass needed to bring `raw` up to date, or `None` when the
    /// tail is already polished. `frozen_len` is the ASR freeze point: bytes
    /// that must not be sent again. A freeze that has advanced past what we
    /// have sealed is handled first (promote, seal-pass, or adopt as raw);
    /// otherwise only the volatile tail is polished.
    ///
    /// Returns `None` for a stale session so a late worker cannot resurrect it.
    pub fn next_job(&mut self, session: u64, raw: &str, frozen_len: usize) -> Option<TailJob> {
        if session != self.session {
            return None;
        }
        // A raw stream that no longer extends what we sealed means the
        // transcript was rebuilt rather than appended to; the caller falls
        // back to a whole-text pass.
        if self.sealed_raw_len > raw.len() || !raw.is_char_boundary(self.sealed_raw_len) {
            return None;
        }
        self.observe(session, clamp_boundary(raw, frozen_len));
        let frozen = clamp_boundary(raw, self.frozen_len.min(raw.len()));
        let frozen = sentence_seal_point(raw, self.sealed_raw_len, frozen);
        if frozen > self.sealed_raw_len {
            let slice = raw[self.sealed_raw_len..frozen].trim();
            if slice.is_empty() {
                self.sealed_raw_len = frozen;
            } else if slice == self.tail_raw {
                self.sealed = join(&self.sealed, &self.tail);
                self.sealed_raw_len = frozen;
                self.tail.clear();
                self.tail_raw.clear();
            } else {
                // The worker fell far behind: adopting the backlog as raw
                // keeps this pass bounded. A keep-up seal is a sentence or
                // two, well under the limit, so it is polished.
                if slice.len() > SEAL_PASS_MAX_BYTES {
                    self.sealed = join(&self.sealed, slice);
                    self.sealed_raw_len = frozen;
                    self.tail.clear();
                    self.tail_raw.clear();
                } else {
                    return Some(TailJob {
                        raw: slice.to_string(),
                        context: context_tail(&self.sealed),
                        raw_len: frozen,
                        seal: true,
                    });
                }
            }
        }
        let tail_raw = raw[self.sealed_raw_len..].trim();
        if tail_raw.is_empty() || tail_raw == self.tail_raw {
            return None;
        }
        Some(TailJob {
            raw: tail_raw.to_string(),
            context: context_tail(&self.sealed),
            raw_len: raw.len(),
            seal: false,
        })
    }

    /// Records a completed pass. Ignored when the session moved on or the job
    /// covered less raw text than a pass that already landed, so a slow worker
    /// can never walk the transcript backwards.
    pub fn apply(&mut self, session: u64, job: &TailJob, polished: &str) -> bool {
        if session != self.session || job.raw_len < self.sealed_raw_len {
            return false;
        }
        let polished = polished.trim();
        self.changed |= polished != job.raw.trim();
        if job.seal {
            self.sealed = join(&self.sealed, polished);
            self.sealed_raw_len = job.raw_len;
            self.tail.clear();
            self.tail_raw.clear();
        } else {
            self.tail = polished.to_string();
            self.tail_raw = job.raw.clone();
        }
        true
    }

    /// Seals whatever has been polished, for the commit path. `raw` is the
    /// authoritative final transcript.
    ///
    /// Returns the tail still needing a pass (possibly empty when everything
    /// is already sealed), or `None` when the stream cannot describe `raw` —
    /// the caller then polishes the whole transcript, which is the
    /// pre-streaming behavior and always correct.
    pub fn finish_job(&mut self, session: u64, raw: &str) -> Option<TailJob> {
        if session != self.session
            || self.sealed_raw_len > raw.len()
            || !raw.is_char_boundary(self.sealed_raw_len)
        {
            return None;
        }
        let frozen = clamp_boundary(raw, self.frozen_len.min(raw.len()));
        if frozen > self.sealed_raw_len {
            let slice = raw[self.sealed_raw_len..frozen].trim();
            let promoted = !slice.is_empty() && slice == self.tail_raw;
            // Frozen text no pass reached is polished with the tail rather
            // than adopted raw, as long as the release pass stays bounded.
            let backlog = raw.len() - self.sealed_raw_len;
            if promoted || slice.is_empty() || backlog > FINAL_PASS_MAX_BYTES {
                if promoted {
                    self.sealed = join(&self.sealed, &self.tail);
                } else {
                    self.sealed = join(&self.sealed, slice);
                }
                self.sealed_raw_len = frozen;
                self.tail.clear();
                self.tail_raw.clear();
            }
        }
        Some(TailJob {
            raw: raw[self.sealed_raw_len..].trim().to_string(),
            context: context_tail(&self.sealed),
            raw_len: raw.len(),
            seal: true,
        })
    }
}

fn ends_sentence(ch: char) -> bool {
    matches!(ch, '.' | '?' | '!' | '…' | '。' | '？' | '！')
}

/// Where to seal, given the ASR froze `raw[..frozen]`: the last sentence end
/// in the newly frozen text. A sentence ends at closing punctuation that is
/// followed by whitespace (so "2.4" and "settings.json" do not count) and
/// then by something other than a lowercase letter — ASR drops stray periods
/// mid-sentence ("into the system. yet."), and those are not ends.
///
/// With no sentence end yet, sealing waits (returns `sealed`) until
/// `MAX_UNSEALED_BYTES` of frozen text has piled up. Text from an ASR that
/// does not punctuate gives no evidence either way, so its freeze point is
/// used as is.
fn sentence_seal_point(raw: &str, sealed: usize, frozen: usize) -> usize {
    if frozen <= sealed {
        return frozen;
    }
    if !raw.chars().any(ends_sentence) {
        return frozen;
    }
    let mut last_end = None;
    for (offset, ch) in raw[sealed..frozen].char_indices() {
        if !ends_sentence(ch) {
            continue;
        }
        let end = sealed + offset + ch.len_utf8();
        let mut rest = raw[end..].chars();
        let wide = !ch.is_ascii() && ch != '…';
        if !wide && rest.clone().next().is_some_and(|next| !next.is_whitespace()) {
            continue;
        }
        if rest
            .find(|next| !next.is_whitespace())
            .is_none_or(|next| !next.is_lowercase())
        {
            last_end = Some(end);
        }
    }
    match last_end {
        Some(end) => end,
        None if frozen - sealed > MAX_UNSEALED_BYTES => frozen,
        None => sealed,
    }
}

fn clamp_boundary(text: &str, mut index: usize) -> usize {
    if index > text.len() {
        index = text.len();
    }
    while index > 0 && !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}

/// The last whole-ish slice of polished text, cut at a character boundary and
/// preferring to start after a space so the model is not handed a word stump.
fn context_tail(sealed: &str) -> String {
    if sealed.len() <= CONTEXT_CHARS {
        return sealed.to_string();
    }
    let mut start = sealed.len() - CONTEXT_CHARS;
    while start < sealed.len() && !sealed.is_char_boundary(start) {
        start += 1;
    }
    let slice = &sealed[start..];
    match slice.find(' ') {
        Some(space) => slice[space + 1..].to_string(),
        None => slice.to_string(),
    }
}

/// Joins two passes with a space, except where a format layout set a line on
/// its own at the seam: a greeting that ended the earlier pass ("Hi Mary,"),
/// or a sign-off block that opens the later one ("Thanks,\nSam"). Flattening
/// those with a space would undo the email layout the model was asked for.
fn join(previous: &str, next: &str) -> String {
    use crate::transcript_cleanup::is_salutation_line;
    let previous = previous.trim_end();
    let next = next.trim();
    if previous.is_empty() {
        return next.to_string();
    }
    if next.is_empty() {
        return previous.to_string();
    }
    let greeting_ends_previous = previous.lines().last().is_some_and(is_salutation_line);
    let sign_off_opens_next = next.contains('\n') && next.lines().next().is_some_and(is_salutation_line);
    if greeting_ends_previous || sign_off_opens_next {
        format!("{previous}\n\n{next}")
    } else {
        format!("{previous} {next}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_tail_is_repolished_and_a_gap_seals_it() {
        let mut stream = PolishStream::default();
        stream.begin(7);

        // Mid-utterance: nothing is sealed yet, so the whole thing is tail.
        let raw = "book it for thursday";
        let job = stream.next_job(7, raw, 0).expect("first pass");
        assert_eq!(job.raw, "book it for thursday");
        assert!(!job.seal);
        assert!(stream.apply(7, &job, "Book it for Thursday"));
        assert_eq!(stream.composed(), "Book it for Thursday");

        // Same raw text again: no repeat pass.
        assert!(stream.next_job(7, raw, 0).is_none());

        // The user pauses; the ASR seals everything spoken so far.
        let raw = "book it for thursday";
        let job = stream.next_job(7, raw, 0);
        assert!(job.is_none(), "unchanged tail needs no work");

        // New words arrive and the gap boundary now covers the old ones.
        let raw = "book it for thursday no friday";
        let job = stream.next_job(7, raw, raw.len()).expect("sealing pass");
        assert_eq!(job.raw, "book it for thursday no friday");
        assert!(job.seal);
        assert!(stream.apply(7, &job, "Book it for Friday."));
        assert_eq!(stream.composed(), "Book it for Friday.");
        assert_eq!(stream.sealed_raw_len(), raw.len());

        // Past the seal, only the new words are ever sent again.
        let raw = "book it for thursday no friday and tell sam";
        let job = stream.next_job(7, raw, 0).expect("tail after seal");
        assert_eq!(job.raw, "and tell sam");
        assert_eq!(job.context, "Book it for Friday.");
        assert!(stream.apply(7, &job, "And tell Sam."));
        assert_eq!(stream.composed(), "Book it for Friday. And tell Sam.");
        assert!(stream.changed());
    }

    #[test]
    fn passes_that_fell_back_to_raw_text_do_not_count_as_polished() {
        let mut stream = PolishStream::default();
        stream.begin(1);
        let job = stream.next_job(1, "already clean text", 18).expect("job");
        // A failed or guardrail-tripped pass contributes the raw tail.
        assert!(stream.apply(1, &job, "already clean text"));
        assert!(
            stream.has_output(),
            "the stream still covers the transcript"
        );
        assert!(!stream.changed(), "but nothing was actually polished");
        assert_eq!(stream.composed(), "already clean text");
    }

    #[test]
    fn stale_sessions_and_late_workers_cannot_rewrite_the_stream() {
        let mut stream = PolishStream::default();
        stream.begin(2);
        let job = stream.next_job(2, "hello there", 11).expect("job");
        assert!(stream.apply(2, &job, "Hello there."));

        // A worker from an older recording.
        assert!(stream.next_job(1, "anything", 0).is_none());
        assert!(!stream.apply(1, &job, "stale"));

        // A pass that covers less raw text than one already sealed.
        let short = TailJob {
            raw: "hello".into(),
            context: String::new(),
            raw_len: 3,
            seal: true,
        };
        assert!(!stream.apply(2, &short, "Old."));
        assert_eq!(stream.composed(), "Hello there.");
    }

    #[test]
    fn a_rebuilt_transcript_falls_back_to_a_whole_text_pass() {
        let mut stream = PolishStream::default();
        stream.begin(4);
        let job = stream
            .next_job(4, "the long original text", 22)
            .expect("job");
        assert!(stream.apply(4, &job, "The long original text."));

        // The final transcript is shorter than what we sealed: the prefix
        // cannot be trusted, so no incremental job is offered.
        assert!(stream.finish_job(4, "short").is_none());
        assert!(stream.next_job(4, "short", 0).is_none());

        // A transcript that does extend the sealed prefix still works.
        let finish = stream
            .finish_job(4, "the long original text plus more")
            .expect("finish job");
        assert_eq!(finish.raw, "plus more");
        assert!(finish.seal);
    }

    #[test]
    fn context_is_bounded_and_never_splits_a_character() {
        let sealed = format!("{} café au lait", "padding ".repeat(80));
        let context = context_tail(&sealed);
        assert!(context.len() <= CONTEXT_CHARS);
        assert!(context.ends_with("café au lait"));
        assert!(!context.starts_with(' '));
    }

    #[test]
    fn a_realistic_freeze_seals_only_the_prefix_then_polishes_the_tail() {
        let mut stream = PolishStream::default();
        stream.begin(3);

        let first = "book it for thursday";
        let job = stream.next_job(3, first, 0).expect("volatile opening");
        assert!(!job.seal);
        assert!(stream.apply(3, &job, "Book it for Thursday"));

        // ASR freezes the first utterance as the next chunk starts. The last
        // volatile pass covered exactly that slice, so it is promoted without
        // another model call.
        let raw = "book it for thursday no friday";
        let frozen = first.len();
        let job = stream.next_job(3, raw, frozen).expect("volatile tail");
        assert!(!job.seal);
        assert_eq!(job.raw, "no friday");
        assert_eq!(stream.sealed_raw_len(), frozen);
        assert!(stream.apply(3, &job, "No Friday."));
        assert_eq!(stream.composed(), "Book it for Thursday No Friday.");

        let finish = stream.finish_job(3, raw).expect("commit");
        assert_eq!(finish.raw, "no friday");
        assert!(finish.seal);
    }

    #[test]
    fn a_partial_freeze_of_a_longer_tail_is_a_seal_pass() {
        let mut stream = PolishStream::default();
        stream.begin(9);
        let opening = "one two three";
        let job = stream.next_job(9, opening, 0).expect("window");
        assert!(stream.apply(9, &job, "One two three"));

        let raw = "one two three four";
        let frozen = "one".len();
        let job = stream.next_job(9, raw, frozen).expect("falling-out chunk");
        assert!(job.seal);
        assert_eq!(job.raw, "one");
        assert!(stream.apply(9, &job, "One"));
        let job = stream.next_job(9, raw, frozen).expect("remaining window");
        assert!(!job.seal);
        assert_eq!(job.raw, "two three four");
    }

    #[test]
    fn skipped_frozen_text_is_adopted_raw_so_the_pass_stays_bounded() {
        let mut stream = PolishStream::default();
        stream.begin(5);
        stream.observe(5, 0);
        let backlog = "one two three four five ".repeat(30);
        assert!(backlog.len() > SEAL_PASS_MAX_BYTES);
        let raw = format!("{backlog}six");
        // A freeze too large for one pass means the worker skipped.
        let frozen = backlog.len();
        let job = stream.next_job(5, &raw, frozen).expect("volatile only");
        assert!(!job.seal);
        assert_eq!(job.raw, "six");
        assert_eq!(stream.composed(), backlog.trim());
        assert!(stream.apply(5, &job, "Six."));
        assert_eq!(stream.composed(), format!("{} Six.", backlog.trim()));
    }

    #[test]
    fn punctuated_text_seals_at_the_last_sentence_end_behind_the_freeze() {
        // The chunk ended mid-sentence, after a complete one.
        let raw = "Whisperflow handles quotes. It also knows if the user is";
        let frozen = raw.len();
        assert_eq!(sentence_seal_point(raw, 0, frozen), "Whisperflow handles quotes.".len());

        // A stray ASR period before a lowercase word, an ellipsis, a version
        // number and a file name are not sentence ends.
        for raw in [
            "that we'll put into the system. yet I think we can",
            "Let's get rid of... the ability for users to",
            "we need version 2.4 of settings.json before the",
        ] {
            assert_eq!(sentence_seal_point(raw, 0, raw.len()), 0, "{raw}");
        }

        // A finished sentence at the very end of the text seals whole.
        let raw = "Okay, this is great.";
        assert_eq!(sentence_seal_point(raw, 0, raw.len()), raw.len());

        // Only newly frozen text is searched.
        let raw = "One. Two. and then three";
        assert_eq!(sentence_seal_point(raw, "One. Two.".len(), raw.len()), "One. Two.".len());
    }

    #[test]
    fn a_sentence_that_never_ends_is_sealed_once_it_is_long() {
        let run_on = format!("Okay. {}", "and then we went on ".repeat(30));
        let sealed = "Okay.".len();
        assert!(run_on.len() - sealed > MAX_UNSEALED_BYTES);
        assert_eq!(sentence_seal_point(&run_on, sealed, run_on.len()), run_on.len());
        let short = "Okay. and then we went on";
        assert_eq!(sentence_seal_point(short, sealed, short.len()), sealed);
    }

    #[test]
    fn an_unfinished_sentence_stays_in_the_tail_across_a_freeze() {
        let mut stream = PolishStream::default();
        stream.begin(6);
        let raw = "We should ship it. So we don't need to worry about the";
        // The ASR froze everything at a pause mid-sentence.
        let job = stream.next_job(6, raw, raw.len()).expect("seal the finished sentence");
        assert!(job.seal);
        assert_eq!(job.raw, "We should ship it.");
        assert!(stream.apply(6, &job, "We should ship it."));
        let job = stream.next_job(6, raw, raw.len()).expect("unfinished tail");
        assert!(!job.seal);
        assert_eq!(job.raw, "So we don't need to worry about the");
        assert!(stream.apply(6, &job, "So we don't need to worry about the"));

        // The sentence finishes in the next chunk and is polished whole.
        let raw = "We should ship it. So we don't need to worry about the ads yet.";
        let job = stream.next_job(6, raw, raw.len()).expect("seal the finished sentence");
        assert!(job.seal);
        assert_eq!(job.raw, "So we don't need to worry about the ads yet.");
    }

    #[test]
    fn release_polishes_frozen_text_no_pass_reached_unless_the_backlog_is_large() {
        let mut stream = PolishStream::default();
        stream.begin(10);
        let raw = "book it for thursday no friday and tell sam";
        stream.observe(10, "book it for thursday".len());
        let finish = stream.finish_job(10, raw).expect("commit");
        assert_eq!(finish.raw, raw, "the unreached prefix rides along with the tail");
        assert_eq!(stream.composed(), "");

        let mut stream = PolishStream::default();
        stream.begin(11);
        let long = format!("{}and tell sam", "book it for thursday ".repeat(40));
        let frozen = long.len() - "and tell sam".len();
        stream.observe(11, frozen);
        let finish = stream.finish_job(11, &long).expect("commit");
        assert_eq!(finish.raw, "and tell sam");
        assert_eq!(stream.composed(), long[..frozen].trim());
    }

    #[test]
    fn an_email_layout_survives_the_seams_between_passes() {
        // A greeting sealed on its own keeps its line.
        assert_eq!(
            join("Hi Mary,", "Thanks for the slides."),
            "Hi Mary,\n\nThanks for the slides."
        );
        // A sign-off block that opens a later pass keeps its lines.
        assert_eq!(
            join("Can we move the call to Thursday?", "Thanks,\nSam"),
            "Can we move the call to Thursday?\n\nThanks,\nSam"
        );
        // Plain cleanup never produces either, and is joined as before.
        assert_eq!(join("Hi Mary.", "Thanks for the slides."), "Hi Mary. Thanks for the slides.");
        assert_eq!(
            join("We went there, and then,", "we left."),
            "We went there, and then, we left."
        );
        assert_eq!(join("Okay.", "Thanks, Sam."), "Okay. Thanks, Sam.");

        let mut stream = PolishStream::default();
        stream.begin(12);
        let raw = "Hi Mary. Thanks for the slides.";
        let job = stream.next_job(12, raw, "Hi Mary.".len()).expect("greeting seal");
        assert!(job.seal);
        assert!(stream.apply(12, &job, "Hi Mary,"));
        let job = stream.next_job(12, raw, 0).expect("tail");
        assert_eq!(job.context, "Hi Mary,");
        assert!(stream.apply(12, &job, "Thanks for the slides."));
        assert_eq!(stream.composed(), "Hi Mary,\n\nThanks for the slides.");
    }

    #[test]
    fn an_exact_tail_promotes_into_the_seal_without_another_pass() {
        let mut stream = PolishStream::default();
        stream.begin(8);
        let raw = "hello there";
        let job = stream.next_job(8, raw, 0).expect("volatile");
        assert!(stream.apply(8, &job, "Hello there."));
        assert!(stream.next_job(8, raw, raw.len()).is_none());
        assert_eq!(stream.sealed_raw_len(), raw.len());
        assert_eq!(stream.composed(), "Hello there.");
    }
}
