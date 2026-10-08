//! A bounded latest-revision slot. Inference never holds this lock.
use crate::transcription::TranscriptPreview;
use std::sync::{Condvar, Mutex};
use std::time::Duration;
#[derive(Default)]
pub struct LatestPreview {
    state: Mutex<(u64, Option<(u64, TranscriptPreview)>)>,
    /// Signalled on every submit, so an idle worker wakes at once.
    submitted: Condvar,
}
impl LatestPreview {
    pub fn submit(&self, preview: TranscriptPreview) -> u64 {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.0 += 1;
        let revision = state.0;
        state.1 = Some((revision, preview));
        self.submitted.notify_all();
        revision
    }
    #[cfg(test)]
    pub fn take(&self) -> Option<(u64, TranscriptPreview)> {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .1
            .take()
    }
    /// The pending revision, waiting up to `timeout` for one to be submitted.
    pub fn wait_take(&self, timeout: Duration) -> Option<(u64, TranscriptPreview)> {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let (mut state, _) = self
            .submitted
            .wait_timeout_while(state, timeout, |state| state.1.is_none())
            .unwrap_or_else(|e| e.into_inner());
        state.1.take()
    }
    pub fn is_current(&self, revision: u64) -> bool {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).0 == revision
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn preview(text: &str) -> TranscriptPreview {
        TranscriptPreview {
            index: 0,
            text: text.into(),
            sealed_len: 0,
            final_preview: false,
        }
    }
    #[test]
    fn slow_inference_keeps_only_latest_raw_revision() {
        let queue = LatestPreview::default();
        let first = queue.submit(preview("book it for thursday"));
        let in_flight = queue.take().unwrap();
        queue.submit(preview("book it for thursday no"));
        let latest = queue.submit(preview("book it for thursday no friday"));
        assert!(!queue.is_current(first));
        assert_eq!(in_flight.1.text, "book it for thursday");
        let pending = queue.take().unwrap();
        assert_eq!(pending.0, latest);
        assert_eq!(pending.1.text, "book it for thursday no friday");
        assert!(queue.take().is_none());
        assert!(queue.is_current(latest));
    }
    #[test]
    fn an_idle_worker_wakes_on_submit_not_on_a_poll() {
        let queue = std::sync::Arc::new(LatestPreview::default());
        assert!(queue.wait_take(Duration::from_millis(5)).is_none());
        let submitter = queue.clone();
        let handle = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            submitter.submit(preview("hello"))
        });
        let started = std::time::Instant::now();
        let taken = queue.wait_take(Duration::from_secs(10)).expect("woken by submit");
        assert_eq!(taken.0, handle.join().unwrap());
        assert!(started.elapsed() < Duration::from_secs(5));
    }
}
