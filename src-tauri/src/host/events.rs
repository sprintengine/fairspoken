//! Live monitoring feed behind `GET /v1/events`: host metrics mutations are
//! published here as Server-Sent Events and fanned out to every connected
//! dashboard. Events describe *activity* only (ids, clients, models, timings)
//! — never transcript text or audio.

use serde::Serialize;
use std::io::{self, Write};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Concurrent `/v1/events` connections the host will hold open. Each one costs
/// a thread, so a runaway tab (or a reconnect storm) can't exhaust the host.
pub(super) const MAX_EVENT_SUBSCRIBERS: usize = 16;
/// Frames buffered per subscriber. A subscriber that falls this far behind is
/// dropped (its browser reconnects and resyncs from a fresh snapshot) so a
/// stalled socket never blocks a transcription worker.
pub(super) const SUBSCRIBER_BUFFER: usize = 256;
/// Comment line sent while idle so proxies keep the connection open and a
/// vanished client is noticed by the next failed write.
pub(super) const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(15);
const HEARTBEAT_FRAME: &str = ": ping\n\n";

/// One `/v1/events` frame. `at` is epoch milliseconds when the host recorded
/// the change; the payload fields are flattened next to it.
#[derive(Clone, Debug, Serialize)]
pub(super) struct HostEvent {
    pub(super) at: u64,
    #[serde(flatten)]
    pub(super) kind: HostEventKind,
}

#[derive(Clone, Debug, Serialize)]
#[serde(untagged, rename_all_fields = "camelCase")]
pub(super) enum HostEventKind {
    StreamStarted {
        stream_id: u64,
        client: Option<String>,
    },
    StreamFinished {
        stream_id: u64,
        client: Option<String>,
        audio_seconds: f32,
    },
    JobQueued {
        job_id: u64,
        client: Option<String>,
        source: &'static str,
        model: String,
        audio_seconds: f32,
    },
    JobStarted {
        job_id: u64,
        worker: usize,
        model: String,
        client: Option<String>,
        queue_wait_ms: u64,
    },
    JobCompleted {
        job_id: u64,
        worker: usize,
        model: String,
        client: Option<String>,
        audio_seconds: f32,
        processing_ms: u64,
    },
    /// `worker` is null when the job never reached one (the queue was full).
    JobFailed {
        job_id: u64,
        worker: Option<usize>,
        client: Option<String>,
        error: String,
    },
    WorkerState {
        worker: usize,
        state: &'static str,
        model: Option<String>,
    },
    ModelDownload {
        model: String,
        stage: String,
        percentage: u8,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
}

impl HostEventKind {
    pub(super) fn name(&self) -> &'static str {
        match self {
            Self::StreamStarted { .. } => "stream_started",
            Self::StreamFinished { .. } => "stream_finished",
            Self::JobQueued { .. } => "job_queued",
            Self::JobStarted { .. } => "job_started",
            Self::JobCompleted { .. } => "job_completed",
            Self::JobFailed { .. } => "job_failed",
            Self::WorkerState { .. } => "worker_state",
            Self::ModelDownload { .. } => "model_download",
        }
    }
}

/// Formats one SSE frame. JSON never contains a raw newline, so the payload
/// is always a single `data:` line.
pub(super) fn sse_frame(event: &str, data: &str) -> String {
    format!("event: {event}\ndata: {data}\n\n")
}

impl HostEvent {
    pub(super) fn frame(&self) -> Result<String, String> {
        let data = serde_json::to_string(self)
            .map_err(|err| format!("Failed to serialize host event: {err}"))?;
        Ok(sse_frame(self.kind.name(), &data))
    }
}

struct Subscriber {
    id: u64,
    tx: SyncSender<Arc<str>>,
}

#[derive(Default)]
struct HubState {
    next_id: u64,
    subscribers: Vec<Subscriber>,
}

/// Broadcast of bounded per-subscriber channels. Publishing never blocks: a
/// full or closed channel removes that subscriber instead.
#[derive(Default)]
pub(super) struct EventHub {
    state: Mutex<HubState>,
}

impl EventHub {
    pub(super) fn subscribe(self: &Arc<Self>) -> Result<EventSubscription, String> {
        let mut state = self.lock();
        if state.subscribers.len() >= MAX_EVENT_SUBSCRIBERS {
            return Err(format!(
                "Too many event subscribers (limit {MAX_EVENT_SUBSCRIBERS})"
            ));
        }
        state.next_id = state.next_id.saturating_add(1);
        let id = state.next_id;
        let (tx, rx) = mpsc::sync_channel(SUBSCRIBER_BUFFER);
        state.subscribers.push(Subscriber { id, tx });
        Ok(EventSubscription {
            id,
            rx,
            hub: Arc::clone(self),
        })
    }

    #[cfg(test)]
    pub(super) fn subscriber_count(&self) -> usize {
        self.lock().subscribers.len()
    }

    pub(super) fn publish(&self, event: HostEvent) {
        let mut state = self.lock();
        if state.subscribers.is_empty() {
            return;
        }
        let frame: Arc<str> = match event.frame() {
            Ok(frame) => frame.into(),
            Err(err) => {
                eprintln!("{err}");
                return;
            }
        };
        state.subscribers.retain(
            |subscriber| match subscriber.tx.try_send(Arc::clone(&frame)) {
                Ok(()) => true,
                Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => false,
            },
        );
    }

    fn unsubscribe(&self, id: u64) {
        self.lock()
            .subscribers
            .retain(|subscriber| subscriber.id != id);
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HubState> {
        // Critical sections only push/retain the subscriber list, so a
        // poisoned lock still holds a usable value.
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// A connected `/v1/events` stream. Dropping it frees the subscriber slot.
pub(super) struct EventSubscription {
    id: u64,
    rx: Receiver<Arc<str>>,
    hub: Arc<EventHub>,
}

impl Drop for EventSubscription {
    fn drop(&mut self) {
        self.hub.unsubscribe(self.id);
    }
}

impl EventSubscription {
    #[cfg(test)]
    pub(super) fn try_next(&self) -> Option<Arc<str>> {
        self.rx.try_recv().ok()
    }

    /// Writes queued frames (and idle heartbeats) until the client goes away
    /// or the hub drops this subscriber for falling behind. Every frame is
    /// flushed immediately so events reach the browser as they happen.
    pub(super) fn pump(
        &self,
        writer: &mut impl FrameWriter,
        heartbeat: Duration,
    ) -> io::Result<()> {
        loop {
            match self.rx.recv_timeout(heartbeat) {
                Ok(frame) => writer.write_frame(frame.as_bytes())?,
                Err(RecvTimeoutError::Timeout) => writer.write_frame(HEARTBEAT_FRAME.as_bytes())?,
                Err(RecvTimeoutError::Disconnected) => return Ok(()),
            }
        }
    }
}

/// The response body framing for an event stream: HTTP/1.1 clients get
/// chunked transfer (one chunk per frame), HTTP/1.0 clients a body delimited
/// by connection close.
pub(super) trait FrameWriter {
    fn write_frame(&mut self, frame: &[u8]) -> io::Result<()>;
}

pub(super) struct SseWriter<W: Write> {
    inner: W,
    chunked: bool,
}

impl<W: Write> SseWriter<W> {
    /// Writes the response head straight onto the connection. tiny_http's own
    /// `respond` pipes a body through an 8 KiB chunk encoder that only flushes
    /// when full, which would hold small events back indefinitely.
    pub(super) fn start(mut inner: W, chunked: bool) -> io::Result<Self> {
        let framing = if chunked {
            "Transfer-Encoding: chunked\r\n"
        } else {
            ""
        };
        write!(
            inner,
            "HTTP/1.1 200 OK\r\n\
             Content-Type: text/event-stream\r\n\
             Cache-Control: no-cache\r\n\
             Connection: close\r\n\
             X-Accel-Buffering: no\r\n\
             {framing}\r\n"
        )?;
        inner.flush()?;
        Ok(Self { inner, chunked })
    }

    #[cfg(test)]
    pub(super) fn into_inner(self) -> W {
        self.inner
    }
}

impl<W: Write> FrameWriter for SseWriter<W> {
    fn write_frame(&mut self, frame: &[u8]) -> io::Result<()> {
        if self.chunked {
            write!(self.inner, "{:x}\r\n", frame.len())?;
            self.inner.write_all(frame)?;
            self.inner.write_all(b"\r\n")?;
        } else {
            self.inner.write_all(frame)?;
        }
        self.inner.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::{
        sse_frame, EventHub, FrameWriter, HostEvent, HostEventKind, SseWriter,
        MAX_EVENT_SUBSCRIBERS, SUBSCRIBER_BUFFER,
    };
    use std::io;
    use std::sync::Arc;
    use std::time::Duration;

    fn queued(job_id: u64) -> HostEvent {
        HostEvent {
            at: 1_700_000_000_000,
            kind: HostEventKind::JobQueued {
                job_id,
                client: Some("192.168.1.31".to_string()),
                source: "stream",
                model: "parakeet-tdt-0.6b-v3".to_string(),
                audio_seconds: 0.0,
            },
        }
    }

    #[test]
    fn events_serialize_as_named_sse_frames_with_camel_case_payloads() {
        assert_eq!(
            queued(7).frame().unwrap(),
            "event: job_queued\ndata: {\"at\":1700000000000,\"jobId\":7,\
             \"client\":\"192.168.1.31\",\"source\":\"stream\",\
             \"model\":\"parakeet-tdt-0.6b-v3\",\"audioSeconds\":0.0}\n\n"
        );

        let completed = HostEvent {
            at: 5,
            kind: HostEventKind::JobCompleted {
                job_id: 7,
                worker: 1,
                model: "base".to_string(),
                client: None,
                audio_seconds: 2.5,
                processing_ms: 120,
            },
        };
        let value: serde_json::Value = serde_json::from_str(
            completed
                .frame()
                .unwrap()
                .split("data: ")
                .nth(1)
                .unwrap()
                .trim(),
        )
        .unwrap();
        assert_eq!(
            value,
            serde_json::json!({"at": 5, "jobId": 7, "worker": 1, "model": "base",
                "client": null, "audioSeconds": 2.5, "processingMs": 120})
        );

        let rejected = HostEvent {
            at: 9,
            kind: HostEventKind::JobFailed {
                job_id: 8,
                worker: None,
                client: None,
                error: "Transcription queue is full".to_string(),
            },
        };
        assert!(rejected
            .frame()
            .unwrap()
            .starts_with("event: job_failed\ndata: {\"at\":9,\"jobId\":8,\"worker\":null,"));

        let names = [
            HostEventKind::StreamStarted {
                stream_id: 1,
                client: None,
            },
            HostEventKind::StreamFinished {
                stream_id: 1,
                client: None,
                audio_seconds: 1.0,
            },
            HostEventKind::JobStarted {
                job_id: 1,
                worker: 0,
                model: "base".into(),
                client: None,
                queue_wait_ms: 3,
            },
            HostEventKind::WorkerState {
                worker: 0,
                state: "idle",
                model: None,
            },
            HostEventKind::ModelDownload {
                model: "base".into(),
                stage: "downloading".into(),
                percentage: 40,
                error: None,
            },
        ]
        .map(|kind| kind.name());
        assert_eq!(
            names,
            [
                "stream_started",
                "stream_finished",
                "job_started",
                "worker_state",
                "model_download"
            ]
        );
        // The download error is only present once a download failed.
        let download = HostEvent {
            at: 1,
            kind: HostEventKind::ModelDownload {
                model: "base".into(),
                stage: "downloading".into(),
                percentage: 40,
                error: None,
            },
        };
        assert!(!download.frame().unwrap().contains("error"));
        assert_eq!(sse_frame("snapshot", "{}"), "event: snapshot\ndata: {}\n\n");
    }

    #[test]
    fn hub_caps_subscribers_and_frees_slots_on_drop() {
        let hub = Arc::new(EventHub::default());
        let subscriptions: Vec<_> = (0..MAX_EVENT_SUBSCRIBERS)
            .map(|_| hub.subscribe().expect("within cap"))
            .collect();
        assert!(hub.subscribe().is_err());
        assert_eq!(hub.subscriber_count(), MAX_EVENT_SUBSCRIBERS);

        drop(subscriptions);
        assert_eq!(hub.subscriber_count(), 0);
        let _again = hub.subscribe().expect("slot freed");
    }

    #[test]
    fn hub_drops_a_slow_subscriber_without_blocking_or_starving_others() {
        let hub = Arc::new(EventHub::default());
        let slow = hub.subscribe().expect("slow");
        let fast = hub.subscribe().expect("fast");

        for id in 0..SUBSCRIBER_BUFFER as u64 {
            hub.publish(queued(id));
            fast.rx.try_recv().expect("fast subscriber keeps up");
        }
        assert_eq!(hub.subscriber_count(), 2);

        // One frame past the slow subscriber's buffer evicts it; publish
        // returns immediately rather than waiting for it to drain.
        hub.publish(queued(9_999));
        assert_eq!(hub.subscriber_count(), 1);
        assert!(fast.rx.try_recv().unwrap().contains("\"jobId\":9999"));

        // The evicted subscriber drains what it had, then sees the end.
        let drained = std::iter::from_fn(|| slow.rx.try_recv().ok()).count();
        assert_eq!(drained, SUBSCRIBER_BUFFER);
        assert!(matches!(
            slow.rx.try_recv(),
            Err(std::sync::mpsc::TryRecvError::Disconnected)
        ));
    }

    #[test]
    fn sse_writer_frames_chunks_and_flushes_every_event() {
        struct Recorder {
            bytes: Vec<u8>,
            flushes: usize,
        }
        impl io::Write for Recorder {
            fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
                self.bytes.extend_from_slice(buf);
                Ok(buf.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                self.flushes += 1;
                Ok(())
            }
        }

        let mut writer = SseWriter::start(
            Recorder {
                bytes: Vec::new(),
                flushes: 0,
            },
            true,
        )
        .unwrap();
        writer.write_frame(b": ping\n\n").unwrap();
        let recorder = writer.into_inner();
        let text = String::from_utf8(recorder.bytes).unwrap();
        assert!(text.starts_with("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n"));
        assert!(text.contains("Cache-Control: no-cache\r\n"));
        assert!(text.ends_with("\r\n\r\n8\r\n: ping\n\n\r\n"));
        assert_eq!(recorder.flushes, 2);
    }

    #[test]
    fn pump_writes_heartbeats_while_idle_and_stops_on_write_error() {
        struct Failing {
            frames: Vec<String>,
        }
        impl FrameWriter for Failing {
            fn write_frame(&mut self, frame: &[u8]) -> io::Result<()> {
                self.frames
                    .push(String::from_utf8_lossy(frame).into_owned());
                if self.frames.len() == 2 {
                    return Err(io::Error::from(io::ErrorKind::BrokenPipe));
                }
                Ok(())
            }
        }

        let hub = Arc::new(EventHub::default());
        let subscription = hub.subscribe().unwrap();
        hub.publish(queued(1));
        let mut writer = Failing { frames: Vec::new() };
        let result = subscription.pump(&mut writer, Duration::from_millis(5));

        assert!(result.is_err());
        assert!(writer.frames[0].starts_with("event: job_queued\n"));
        assert_eq!(writer.frames[1], ": ping\n\n");
        drop(subscription);
        assert_eq!(hub.subscriber_count(), 0);
    }
}
