//! Incremental polish state for one dictation.
//!
//! Polish used to be a single pass over the whole transcript at release, which
//! costs O(entire dictation) at exactly the moment the user is waiting. Instead
//! the stream keeps a *sealed* polished prefix and re-polishes only the
//! volatile tail behind it, so every pass — including the one at release —
//! costs O(last utterance).
//!
//! Two properties from the ASR layer make this safe:
//! * chunk merging only ever appends, so a raw prefix never changes meaning;
//! * a chunk that did not overlap its predecessor began after a silence gap,
//!   so the merge before it is final (`TranscriptPreview::sealed_len`).
//!
//! Sealing therefore happens at utterance boundaries only. Text inside the
//! sentence the user is still speaking stays volatile and keeps getting
//! re-polished until they pause.

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

    /// Describes the pass needed to bring `raw` up to date, or `None` when the
    /// tail is already polished. `sealed_len` is the ASR's seal point; the tail
    /// is sealed when the boundary has moved past everything we are polishing.
    ///
    /// Returns `None` for a stale session so a late worker cannot resurrect it.
    pub fn next_job(&self, session: u64, raw: &str, sealed_len: usize) -> Option<TailJob> {
        if session != self.session {
            return None;
        }
        // A raw stream that no longer extends what we sealed means the
        // transcript was rebuilt rather than appended to; the caller falls
        // back to a whole-text pass.
        if self.sealed_raw_len > raw.len() || !raw.is_char_boundary(self.sealed_raw_len) {
            return None;
        }
        let tail_raw = raw[self.sealed_raw_len..].trim();
        if tail_raw.is_empty() || tail_raw == self.tail_raw {
            return None;
        }
        Some(TailJob {
            raw: tail_raw.to_string(),
            context: context_tail(&self.sealed),
            raw_len: raw.len(),
            seal: sealed_len >= raw.len(),
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
    /// Returns the tail still needing a pass, or `None` when the stream cannot
    /// describe `raw` — the caller then polishes the whole transcript, which
    /// is the pre-streaming behavior and always correct.
    pub fn finish_job(&self, session: u64, raw: &str) -> Option<TailJob> {
        if session != self.session
            || self.sealed_raw_len > raw.len()
            || !raw.is_char_boundary(self.sealed_raw_len)
        {
            return None;
        }
        Some(TailJob {
            raw: raw[self.sealed_raw_len..].trim().to_string(),
            context: context_tail(&self.sealed),
            raw_len: raw.len(),
            seal: true,
        })
    }
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
        assert!(stream.has_output(), "the stream still covers the transcript");
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
        let job = stream.next_job(4, "the long original text", 22).expect("job");
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
}
