//! Opt-in development recording evidence. No audio, credentials or headers.
use crate::polish::{PolishDecision, PolishTargetApp};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

pub const AVAILABLE: bool = cfg!(debug_assertions);
const MAX_TRACE_BYTES: usize = 8 * 1024 * 1024;
const MAX_EVENTS: usize = 2500;
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TraceEvent {
    pub sequence: usize,
    pub elapsed_ms: u64,
    pub kind: String,
    pub data: Value,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NoteMetadata {
    pub version: u8,
    pub session_id: String,
    pub started_at: u64,
    pub original_transcript: Option<String>,
    pub polished_transcript: Option<String>,
    pub saved_text: Option<String>,
    pub clipboard_text: Option<String>,
    pub events: Vec<TraceEvent>,
    pub omitted_events: usize,
}
struct State {
    metadata: NoteMetadata,
    bytes: usize,
    sealed: bool,
}
#[derive(Clone)]
pub struct Trace {
    state: Arc<Mutex<State>>,
    started: Instant,
}
impl Trace {
    pub fn new(enabled: bool) -> Option<Self> {
        if !AVAILABLE || !enabled {
            return None;
        }
        Some(Self {
            started: Instant::now(),
            state: Arc::new(Mutex::new(State {
                metadata: NoteMetadata {
                    version: 1,
                    session_id: uuid::Uuid::new_v4().to_string(),
                    started_at: SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_millis() as u64,
                    original_transcript: None,
                    polished_transcript: None,
                    saved_text: None,
                    clipboard_text: None,
                    events: Vec::new(),
                    omitted_events: 0,
                },
                bytes: 0,
                sealed: false,
            })),
        })
    }
    pub fn event(&self, kind: &str, data: Value) -> Option<usize> {
        let mut state = self.state.lock().ok()?;
        if state.sealed {
            return None;
        }
        let bytes = data.to_string().len();
        if state.bytes + bytes > MAX_TRACE_BYTES || state.metadata.events.len() >= MAX_EVENTS {
            state.metadata.omitted_events += 1;
            return None;
        }
        let sequence = state.metadata.events.len() + 1;
        state.bytes += bytes;
        state.metadata.events.push(TraceEvent {
            sequence,
            elapsed_ms: self.started.elapsed().as_millis() as u64,
            kind: kind.into(),
            data,
        });
        Some(sequence)
    }
    pub fn finish(
        &self,
        original: &str,
        polished: &str,
        saved: &str,
        clipboard: &str,
    ) -> NoteMetadata {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.sealed = true;
        state.metadata.original_transcript = Some(original.into());
        state.metadata.polished_transcript = Some(polished.into());
        state.metadata.saved_text = Some(saved.into());
        state.metadata.clipboard_text = Some(clipboard.into());
        state.metadata.clone()
    }
    pub fn span(&self, phase: &str, raw: &str, target: Option<&PolishTargetApp>) -> Span {
        let id = self.event("polish-start", json!({"phase":phase,"input":raw,"targetApp":target.map(|a|json!({"name":a.name,"bundleId":a.bundle_id,"format":a.format}))}));
        Span {
            trace: self.clone(),
            id,
            started: Instant::now(),
        }
    }
}
pub struct Span {
    trace: Trace,
    id: Option<usize>,
    started: Instant,
}
impl Span {
    pub fn event(&self, kind: &str, data: Value) {
        self.trace
            .event(kind, json!({"attempt":self.id,"value":data}));
    }
    pub fn finish(&self, raw: &str, decision: &PolishDecision) {
        let (status, output, reason) = match decision {
            PolishDecision::Polished(o) => ("polished", o.text.as_str(), None),
            PolishDecision::Unchanged { .. } => ("unchanged", raw, None),
            PolishDecision::Disabled => ("disabled", raw, None),
            PolishDecision::Skipped(reason) => ("skipped", raw, Some(reason.to_string())),
            PolishDecision::Failed(reason) => ("failed", raw, Some(reason.clone())),
        };
        self.event("polish-result", json!({"status":status,"acceptedOutput":output,"reason":reason,"durationMs":self.started.elapsed().as_millis() as u64}));
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn disabled_capture_allocates_nothing() {
        assert!(Trace::new(false).is_none());
        assert_eq!(Trace::new(true).is_some(), cfg!(debug_assertions));
    }
    #[test]
    #[cfg(debug_assertions)]
    fn spans_keep_actual_request_rejected_output_and_accepted_raw_separate() {
        let trace = Trace::new(true).unwrap();
        let span = trace.span("preview-2", "raw words", None);
        span.event(
            "polish-request",
            json!({"messages":[{"role":"user","content":"raw words"}]}),
        );
        span.event("polish-response", json!({"text":"invented answer"}));
        span.finish(
            "raw words",
            &PolishDecision::Failed("guard rejected".into()),
        );
        let metadata = trace.finish("raw words", "raw words", "Raw words.", "Raw words. ");
        assert_eq!(metadata.events.len(), 4);
        assert_eq!(
            metadata.events[3].data["value"]["acceptedOutput"],
            "raw words"
        );
        assert!(trace.event("late", json!({})).is_none());
        assert_eq!(metadata.original_transcript.as_deref(), Some("raw words"));
    }
    #[test]
    #[cfg(debug_assertions)]
    fn trace_cap_is_explicit_and_final_text_survives() {
        let trace = Trace::new(true).unwrap();
        trace.event("too-big", json!("x".repeat(MAX_TRACE_BYTES + 1)));
        let metadata = trace.finish("original", "cleaned", "saved", "clipboard");
        assert_eq!(metadata.omitted_events, 1);
        assert_eq!(metadata.saved_text.as_deref(), Some("saved"));
    }
}
