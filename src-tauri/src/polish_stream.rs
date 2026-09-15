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
//! A job never slices a model result to match a chunk boundary — polish output
//! does not line up with raw offsets. Newly frozen text is either promoted from
//! an exact tail match, polished as its own seal pass, or adopted as raw when
//! the worker skipped far ahead.

/// How much polished context to hand the model as the lead-in for a tail pass,
/// so it can continue a sentence rather than re-opening one. Bounded because
/// it is prompt weight on every pass.
const CONTEXT_CHARS: usize = 320;

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
                let volatile_len = raw.len() - frozen;
                // The worker skipped several chunks: adopting the gap as raw
                // keeps this pass bounded. A keep-up freeze is about one chunk
                // and is smaller than the remaining window, so it is polished.
                if slice.len() > volatile_len && volatile_len > 0 {
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
            if !slice.is_empty() {
                if slice == self.tail_raw {
                    self.sealed = join(&self.sealed, &self.tail);
                } else {
                    self.sealed = join(&self.sealed, slice);
                }
            }
            self.sealed_raw_len = frozen;
            self.tail.clear();
            self.tail_raw.clear();
        }
        Some(TailJob {
            raw: raw[self.sealed_raw_len..].trim().to_string(),
            context: context_tail(&self.sealed),
            raw_len: raw.len(),
            seal: true,
        })
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

fn join(previous: &str, next: &str) -> String {
    let previous = previous.trim_end();
    let next = next.trim();
    if previous.is_empty() {
        return next.to_string();
    }
    if next.is_empty() {
        return previous.to_string();
    }
    format!("{previous} {next}")
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
        let raw = "one two three four five six";
        // A large freeze with a short remaining tail means the worker skipped.
        let frozen = raw.len() - "six".len();
        let job = stream.next_job(5, raw, frozen).expect("volatile only");
        assert!(!job.seal);
        assert_eq!(job.raw, "six");
        assert_eq!(stream.composed(), "one two three four five");
        assert!(stream.apply(5, &job, "Six."));
        assert_eq!(stream.composed(), "one two three four five Six.");
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
