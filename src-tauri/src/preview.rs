//! A bounded latest-revision slot. Inference never holds this lock.
use crate::transcription::TranscriptPreview;
use std::sync::Mutex;
#[derive(Default)]
pub struct LatestPreview {
    state: Mutex<(u64, Option<(u64, TranscriptPreview)>)>,
}
impl LatestPreview {
    pub fn submit(&self, preview: TranscriptPreview) -> u64 {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.0 += 1;
        let revision = state.0;
        state.1 = Some((revision, preview));
        revision
    }
    pub fn take(&self) -> Option<(u64, TranscriptPreview)> {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .1
            .take()
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
}
