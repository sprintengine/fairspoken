//! Live host metrics, published through `/v1/stats` and `/v1/events`.

use super::*;

pub(super) const RECENT_CAPACITY: usize = 50;

pub(super) const MAX_TRACKED_CLIENTS: usize = 32;

#[derive(Clone, Copy, PartialEq)]
pub(super) enum WorkerState {
    Idle,
    Loading,
    Transcribing,
    /// The worker's assigned model cannot be served — missing or unloadable
    /// model file. A visible operator state, not a per-job surprise.
    ModelUnavailable,
}

impl WorkerState {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Loading => "loading",
            Self::Transcribing => "transcribing",
            Self::ModelUnavailable => "model-unavailable",
        }
    }
}

pub(super) struct WorkerStatus {
    pub(super) state: WorkerState,
    pub(super) loaded_model: Option<String>,
    pub(super) job: Option<RunningJobInfo>,
    pub(super) completed_jobs: u64,
    pub(super) last_error: Option<String>,
    /// Last state/model pair sent as a `worker_state` event.
    pub(super) published: Option<(WorkerState, Option<String>)>,
}

impl WorkerStatus {
    pub(super) fn new() -> Self {
        Self {
            state: WorkerState::Idle,
            loaded_model: None,
            job: None,
            completed_jobs: 0,
            last_error: None,
            published: None,
        }
    }
}

pub(super) struct RunningJobInfo {
    pub(super) id: u64,
    pub(super) model: String,
    pub(super) source: &'static str,
    pub(super) client: Option<String>,
    pub(super) audio_seconds: f32,
    pub(super) started_at: Instant,
}

pub(super) struct QueuedJobInfo {
    pub(super) id: u64,
    pub(super) model: String,
    pub(super) source: &'static str,
    pub(super) client: Option<String>,
    pub(super) audio_seconds: f32,
    pub(super) enqueued_at: Instant,
}

pub(super) struct ActiveStreamInfo {
    pub(super) id: u64,
    pub(super) client: Option<String>,
    pub(super) started_at: Instant,
}

pub(super) struct ClientStats {
    pub(super) requests: u64,
    pub(super) completed: u64,
    pub(super) rejected: u64,
    pub(super) failed: u64,
    pub(super) total_audio_seconds: f64,
    pub(super) last_seen_ms: u64,
    pub(super) last_model: Option<String>,
}

impl ClientStats {
    pub(super) fn new(now_ms: u64) -> Self {
        Self {
            requests: 0,
            completed: 0,
            rejected: 0,
            failed: 0,
            total_audio_seconds: 0.0,
            last_seen_ms: now_ms,
            last_model: None,
        }
    }
}

pub(super) fn now_epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

pub(super) struct HostMetrics {
    pub(super) started: Instant,
    pub(super) worker_count: usize,
    pub(super) queue_capacity: usize,
    pub(super) next_stream_id: u64,
    pub(super) active_streams: Vec<ActiveStreamInfo>,
    pub(super) queued: VecDeque<QueuedJobInfo>,
    pub(super) workers: Vec<WorkerStatus>,
    pub(super) rejected_jobs: u64,
    pub(super) failed_jobs: u64,
    pub(super) started_jobs: u64,
    pub(super) total_transcriptions: u64,
    pub(super) total_audio_seconds: f64,
    pub(super) total_queue_wait_ms: u128,
    pub(super) total_processing_ms: u128,
    pub(super) next_record_id: u64,
    pub(super) recent: VecDeque<TranscriptionRecord>,
    pub(super) clients: HashMap<String, ClientStats>,
    pub(super) model_download: Option<ModelDownloadState>,
    pub(super) events: Arc<EventHub>,
    pub(super) super_mode: super_mode::SuperModeState,
}

/// Progress of an operator-initiated model download, published through
/// `/v1/stats` so the dashboard can show a missing model being recovered.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ModelDownloadState {
    pub(super) model: String,
    pub(super) stage: String,
    pub(super) percentage: u8,
    pub(super) error: Option<String>,
}

impl ModelDownloadState {
    pub(super) fn in_progress(&self) -> bool {
        self.stage != "ready" && self.stage != "error"
    }
}

impl HostMetrics {
    pub(super) fn new(config: &HostRuntimeConfig) -> Self {
        Self {
            started: Instant::now(),
            worker_count: config.worker_count,
            queue_capacity: config.queue_capacity,
            next_stream_id: 0,
            active_streams: Vec::new(),
            queued: VecDeque::new(),
            workers: (0..config.worker_count)
                .map(|_| WorkerStatus::new())
                .collect(),
            rejected_jobs: 0,
            failed_jobs: 0,
            started_jobs: 0,
            total_transcriptions: 0,
            total_audio_seconds: 0.0,
            total_queue_wait_ms: 0,
            total_processing_ms: 0,
            next_record_id: 0,
            recent: VecDeque::with_capacity(RECENT_CAPACITY),
            clients: HashMap::new(),
            model_download: None,
            events: Arc::new(EventHub::default()),
            super_mode: super_mode::SuperModeState::new(config.worker_count),
        }
    }

    pub(super) fn publish(&self, kind: HostEventKind) {
        self.events.publish(HostEvent {
            at: now_epoch_ms(),
            kind,
        });
    }

    /// Publishes a worker's state when it differs from the last one sent, so
    /// the idle poll re-reporting a missing model doesn't flood subscribers.
    /// `model` is what the worker holds or is working towards.
    pub(super) fn publish_worker_state(&mut self, worker_index: usize, model: Option<String>) {
        let Some(worker) = self.workers.get_mut(worker_index) else {
            return;
        };
        let current = (worker.state, model);
        if worker.published.as_ref() == Some(&current) {
            return;
        }
        worker.published = Some(current.clone());
        self.publish(HostEventKind::WorkerState {
            worker: worker_index,
            state: current.0.as_str(),
            model: current.1,
        });
    }

    /// Frees a worker after its job ended, publishing the job's terminal
    /// event (built from the job it held) before the worker's idle state.
    pub(super) fn release_worker(
        &mut self,
        worker_index: usize,
        terminal_event: impl FnOnce(RunningJobInfo) -> HostEventKind,
    ) {
        let Some(worker) = self.workers.get_mut(worker_index) else {
            return;
        };
        worker.state = WorkerState::Idle;
        let job = worker.job.take();
        let model = worker.loaded_model.clone();
        if let Some(job) = job {
            self.publish(terminal_event(job));
        }
        self.publish_worker_state(worker_index, model);
    }

    pub(super) fn set_model_download(&mut self, state: ModelDownloadState) {
        // Multi-file downloads report the same overall percentage several
        // times; only changes are worth an event.
        let unchanged = self.model_download.as_ref().is_some_and(|current| {
            current.model == state.model
                && current.stage == state.stage
                && current.percentage == state.percentage
                && current.error == state.error
        });
        if unchanged {
            return;
        }
        self.publish(HostEventKind::ModelDownload {
            model: state.model.clone(),
            stage: state.stage.clone(),
            percentage: state.percentage,
            error: state.error.clone(),
        });
        self.model_download = Some(state);
    }

    pub(super) fn active_stream_count(&self) -> u32 {
        self.active_streams.len() as u32
    }

    pub(super) fn running_job_count(&self) -> u32 {
        self.workers.iter().filter(|w| w.job.is_some()).count() as u32
    }

    /// Nothing streaming, queued or running: safe to restart for an update.
    pub(super) fn is_idle(&self) -> bool {
        self.active_streams.is_empty() && self.queued.is_empty() && self.running_job_count() == 0
    }

    pub(super) fn begin_stream(&mut self, client: Option<String>) -> u64 {
        self.next_stream_id = self.next_stream_id.saturating_add(1);
        let id = self.next_stream_id;
        self.publish(HostEventKind::StreamStarted {
            stream_id: id,
            client: client.clone(),
        });
        self.active_streams.push(ActiveStreamInfo {
            id,
            client,
            started_at: Instant::now(),
        });
        id
    }

    pub(super) fn finish_stream(&mut self, stream_id: u64, audio_seconds: f32) {
        let Some(position) = self
            .active_streams
            .iter()
            .position(|stream| stream.id == stream_id)
        else {
            return;
        };
        let stream = self.active_streams.remove(position);
        self.publish(HostEventKind::StreamFinished {
            stream_id,
            client: stream.client,
            audio_seconds,
        });
    }

    pub(super) fn enqueue_job(&mut self, job: QueuedJobInfo) {
        self.publish(HostEventKind::JobQueued {
            job_id: job.id,
            client: job.client.clone(),
            source: job.source,
            model: job.model.clone(),
            audio_seconds: job.audio_seconds,
        });
        self.queued.push_back(job);
    }

    pub(super) fn dequeue_job(&mut self, job_id: u64) {
        self.queued.retain(|job| job.id != job_id);
    }

    /// Removes a job that never reached a worker (the queue was full or gone).
    pub(super) fn drop_queued_job(&mut self, job_id: u64, error: &str) {
        let client = self
            .queued
            .iter()
            .find(|job| job.id == job_id)
            .and_then(|job| job.client.clone());
        self.dequeue_job(job_id);
        self.publish(HostEventKind::JobFailed {
            job_id,
            worker: None,
            client,
            error: error.to_string(),
        });
    }

    pub(super) fn start_job(
        &mut self,
        worker_index: usize,
        queue_wait: Duration,
        needs_load: bool,
        job: RunningJobInfo,
    ) {
        self.dequeue_job(job.id);
        self.started_jobs = self.started_jobs.saturating_add(1);
        self.total_queue_wait_ms = self
            .total_queue_wait_ms
            .saturating_add(queue_wait.as_millis());
        self.publish(HostEventKind::JobStarted {
            job_id: job.id,
            worker: worker_index,
            model: job.model.clone(),
            client: job.client.clone(),
            queue_wait_ms: queue_wait.as_millis() as u64,
        });
        let model = job.model.clone();
        if let Some(worker) = self.workers.get_mut(worker_index) {
            worker.state = if needs_load {
                WorkerState::Loading
            } else {
                WorkerState::Transcribing
            };
            worker.job = Some(job);
        }
        self.publish_worker_state(worker_index, Some(model));
    }

    pub(super) fn worker_model_ready(&mut self, worker_index: usize, model: String) {
        if let Some(worker) = self.workers.get_mut(worker_index) {
            worker.loaded_model = Some(model.clone());
            worker.state = WorkerState::Transcribing;
        }
        self.publish_worker_state(worker_index, Some(model));
    }

    /// Marks a worker as loading its assigned model outside a job (the warm
    /// preload at startup or after a runtime model change).
    pub(super) fn worker_preloading(&mut self, worker_index: usize, model: String) {
        if let Some(worker) = self.workers.get_mut(worker_index) {
            worker.state = WorkerState::Loading;
        }
        self.publish_worker_state(worker_index, Some(model));
    }

    pub(super) fn worker_preload_ready(&mut self, worker_index: usize, model: String) {
        if let Some(worker) = self.workers.get_mut(worker_index) {
            worker.state = WorkerState::Idle;
            worker.loaded_model = Some(model.clone());
            worker.last_error = None;
        }
        self.publish_worker_state(worker_index, Some(model));
    }

    pub(super) fn worker_model_unavailable(
        &mut self,
        worker_index: usize,
        model: String,
        message: String,
    ) {
        if let Some(worker) = self.workers.get_mut(worker_index) {
            worker.state = WorkerState::ModelUnavailable;
            worker.loaded_model = None;
            worker.last_error = Some(message);
        }
        self.publish_worker_state(worker_index, Some(model));
    }

    pub(super) fn reject_job(&mut self, client: Option<&str>) {
        self.rejected_jobs = self.rejected_jobs.saturating_add(1);
        if let Some(stats) = self.touch_client(client) {
            stats.rejected = stats.rejected.saturating_add(1);
        }
    }

    pub(super) fn client_request(&mut self, client: Option<&str>) {
        if let Some(stats) = self.touch_client(client) {
            stats.requests = stats.requests.saturating_add(1);
        }
    }

    /// Returns the (created-if-needed) stats entry for a client address and
    /// refreshes its last-seen time. Tracking is bounded: when a new address
    /// arrives at capacity, the least recently seen entry is evicted.
    pub(super) fn touch_client(&mut self, client: Option<&str>) -> Option<&mut ClientStats> {
        let address = client?;
        let now_ms = now_epoch_ms();
        if !self.clients.contains_key(address) && self.clients.len() >= MAX_TRACKED_CLIENTS {
            if let Some(oldest) = self
                .clients
                .iter()
                .min_by_key(|(_, stats)| stats.last_seen_ms)
                .map(|(address, _)| address.clone())
            {
                self.clients.remove(&oldest);
            }
        }
        let stats = self
            .clients
            .entry(address.to_string())
            .or_insert_with(|| ClientStats::new(now_ms));
        stats.last_seen_ms = now_ms;
        Some(stats)
    }

    /// Frees a worker whose job ended because the client's upload failed. The
    /// request handler already counted that as a rejection; subscribers still
    /// see the job end, so every `job_queued` gets exactly one terminal event.
    pub(super) fn abandon_job(&mut self, worker_index: usize) {
        self.release_worker(worker_index, |job| HostEventKind::JobFailed {
            job_id: job.id,
            worker: Some(worker_index),
            client: job.client,
            error: STREAM_ABORTED.to_string(),
        });
    }

    pub(super) fn fail_job(&mut self, worker_index: usize, client: Option<&str>, error: &str) {
        self.failed_jobs = self.failed_jobs.saturating_add(1);
        if let Some(worker) = self.workers.get_mut(worker_index) {
            worker.last_error = Some(error.to_string());
        }
        self.release_worker(worker_index, |job| HostEventKind::JobFailed {
            job_id: job.id,
            worker: Some(worker_index),
            client: job.client,
            error: error.to_string(),
        });
        if let Some(stats) = self.touch_client(client) {
            stats.failed = stats.failed.saturating_add(1);
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn complete_job(
        &mut self,
        worker_index: usize,
        duration_seconds: f32,
        backend: String,
        model: String,
        source: &'static str,
        client: Option<&str>,
        queue_wait: Duration,
        processing_time: Duration,
    ) {
        self.total_transcriptions = self.total_transcriptions.saturating_add(1);
        self.total_audio_seconds += duration_seconds as f64;
        self.total_processing_ms = self
            .total_processing_ms
            .saturating_add(processing_time.as_millis());
        self.next_record_id = self.next_record_id.saturating_add(1);

        if let Some(worker) = self.workers.get_mut(worker_index) {
            worker.completed_jobs = worker.completed_jobs.saturating_add(1);
            worker.last_error = None;
        }
        self.release_worker(worker_index, |job| HostEventKind::JobCompleted {
            job_id: job.id,
            worker: worker_index,
            model: model.clone(),
            client: job.client,
            audio_seconds: duration_seconds,
            processing_ms: processing_time.as_millis() as u64,
        });
        if let Some(stats) = self.touch_client(client) {
            stats.completed = stats.completed.saturating_add(1);
            stats.total_audio_seconds += duration_seconds as f64;
            stats.last_model = Some(model.clone());
        }

        self.recent.push_front(TranscriptionRecord {
            id: self.next_record_id,
            completed_at_ms: now_epoch_ms(),
            duration_seconds,
            backend,
            model,
            source,
            client: client.map(str::to_string),
            queue_wait_ms: queue_wait.as_millis() as u64,
            processing_ms: processing_time.as_millis() as u64,
        });
        while self.recent.len() > RECENT_CAPACITY {
            self.recent.pop_back();
        }
    }

    /// `inventory` is read from disk before the caller takes the metrics
    /// lock (see `HostRuntime::model_inventory`).
    pub(super) fn snapshot(
        &self,
        bind_addr: &str,
        live: &HostLiveConfig,
        inventory: ModelInventory,
    ) -> StatsSnapshot<'_> {
        let workers = self
            .workers
            .iter()
            .enumerate()
            .map(|(index, worker)| {
                let (assigned, model_available) = inventory
                    .workers
                    .get(index)
                    .copied()
                    .unwrap_or((DEFAULT_HOST_MODEL, false));
                WorkerSnapshot {
                    index,
                    state: worker.state.as_str(),
                    assigned_model: assigned.model_id(),
                    model_available,
                    loaded_model: worker.loaded_model.clone(),
                    completed_jobs: worker.completed_jobs,
                    last_error: worker.last_error.clone(),
                    job: worker.job.as_ref().map(|job| RunningJobSnapshot {
                        id: job.id,
                        model: job.model.clone(),
                        source: job.source,
                        client: job.client.clone(),
                        audio_seconds: job.audio_seconds,
                        elapsed_ms: job.started_at.elapsed().as_millis() as u64,
                    }),
                }
            })
            .collect();

        let queue = self
            .queued
            .iter()
            .map(|job| QueuedJobSnapshot {
                id: job.id,
                model: job.model.clone(),
                source: job.source,
                client: job.client.clone(),
                audio_seconds: job.audio_seconds,
                waiting_ms: job.enqueued_at.elapsed().as_millis() as u64,
            })
            .collect();

        let streams = self
            .active_streams
            .iter()
            .map(|stream| StreamSnapshot {
                id: stream.id,
                client: stream.client.clone(),
                elapsed_ms: stream.started_at.elapsed().as_millis() as u64,
            })
            .collect();

        let mut clients: Vec<ClientSnapshot> = self
            .clients
            .iter()
            .map(|(address, stats)| ClientSnapshot {
                address: address.clone(),
                requests: stats.requests,
                completed: stats.completed,
                rejected: stats.rejected,
                failed: stats.failed,
                total_audio_seconds: stats.total_audio_seconds,
                last_seen_ms: stats.last_seen_ms,
                last_model: stats.last_model.clone(),
            })
            .collect();
        clients.sort_by_key(|client| Reverse(client.last_seen_ms));

        let active_streams = self.active_stream_count();
        let running_jobs = self.running_job_count();
        StatsSnapshot {
            server_version: SERVER_VERSION,
            bind_addr: bind_addr.to_string(),
            uptime_seconds: self.started.elapsed().as_secs(),
            active_sessions: active_streams.saturating_add(running_jobs),
            active_streams,
            queued_jobs: self.queued.len() as u32,
            running_jobs,
            worker_count: self.worker_count,
            queue_capacity: self.queue_capacity,
            max_active_streams: live.max_active_streams(),
            max_recording_seconds: live.max_recording_seconds(),
            use_gpu: live.use_gpu(),
            pairing_enabled: live.pairing_enabled(),
            pairing_password_source: live.auth().pairing_password_source(),
            model: live.model_summary(),
            models: inventory.models,
            model_download: self.model_download.clone(),
            rejected_jobs: self.rejected_jobs,
            failed_jobs: self.failed_jobs,
            total_transcriptions: self.total_transcriptions,
            total_audio_seconds: self.total_audio_seconds,
            average_queue_ms: average_ms(self.total_queue_wait_ms, self.started_jobs),
            average_processing_ms: average_ms(self.total_processing_ms, self.total_transcriptions),
            workers,
            queue,
            streams,
            clients,
            recent: self.recent.iter().collect(),
            super_mode: self.super_mode_stats(live.super_mode),
        }
    }
}

pub(super) fn average_ms(total: u128, count: u64) -> u64 {
    if count == 0 {
        0
    } else {
        (total / u128::from(count)) as u64
    }
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct TranscriptionRecord {
    pub(super) id: u64,
    pub(super) completed_at_ms: u64,
    pub(super) duration_seconds: f32,
    pub(super) backend: String,
    pub(super) model: String,
    pub(super) source: &'static str,
    pub(super) client: Option<String>,
    pub(super) queue_wait_ms: u64,
    pub(super) processing_ms: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct WorkerSnapshot {
    pub(super) index: usize,
    pub(super) state: &'static str,
    pub(super) assigned_model: &'static str,
    pub(super) model_available: bool,
    pub(super) loaded_model: Option<String>,
    pub(super) completed_jobs: u64,
    pub(super) last_error: Option<String>,
    pub(super) job: Option<RunningJobSnapshot>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RunningJobSnapshot {
    pub(super) id: u64,
    pub(super) model: String,
    pub(super) source: &'static str,
    pub(super) client: Option<String>,
    pub(super) audio_seconds: f32,
    pub(super) elapsed_ms: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct QueuedJobSnapshot {
    pub(super) id: u64,
    pub(super) model: String,
    pub(super) source: &'static str,
    pub(super) client: Option<String>,
    pub(super) audio_seconds: f32,
    pub(super) waiting_ms: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct StreamSnapshot {
    pub(super) id: u64,
    pub(super) client: Option<String>,
    pub(super) elapsed_ms: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ClientSnapshot {
    pub(super) address: String,
    pub(super) requests: u64,
    pub(super) completed: u64,
    pub(super) rejected: u64,
    pub(super) failed: u64,
    pub(super) total_audio_seconds: f64,
    pub(super) last_seen_ms: u64,
    pub(super) last_model: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct StatsSnapshot<'a> {
    pub(super) server_version: &'static str,
    pub(super) bind_addr: String,
    pub(super) uptime_seconds: u64,
    pub(super) active_sessions: u32,
    pub(super) active_streams: u32,
    pub(super) queued_jobs: u32,
    pub(super) running_jobs: u32,
    pub(super) worker_count: usize,
    pub(super) queue_capacity: usize,
    pub(super) max_active_streams: u32,
    pub(super) max_recording_seconds: u16,
    pub(super) use_gpu: bool,
    /// Whether clients can pair with a password; the password itself is
    /// never reported.
    pub(super) pairing_enabled: bool,
    /// `env` (`FAIRSPOKEN_HOST_PAIRING_PASSWORD`, read-only here), `saved`
    /// (the config file), or `null` with pairing off.
    pub(super) pairing_password_source: Option<&'static str>,
    /// The served model: one id when uniform across workers, else "mixed".
    pub(super) model: String,
    /// Every model this build can serve, installed or not, with the workers
    /// assigned to each.
    pub(super) models: Vec<ModelSnapshot>,
    pub(super) model_download: Option<ModelDownloadState>,
    pub(super) rejected_jobs: u64,
    pub(super) failed_jobs: u64,
    pub(super) total_transcriptions: u64,
    pub(super) total_audio_seconds: f64,
    pub(super) average_queue_ms: u64,
    pub(super) average_processing_ms: u64,
    pub(super) workers: Vec<WorkerSnapshot>,
    pub(super) queue: Vec<QueuedJobSnapshot>,
    pub(super) streams: Vec<StreamSnapshot>,
    pub(super) clients: Vec<ClientSnapshot>,
    pub(super) recent: Vec<&'a TranscriptionRecord>,
    pub(super) super_mode: super_mode::SuperModeStats,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::test_support::*;

    fn test_queued_job(id: u64, client: Option<&str>) -> QueuedJobInfo {
        QueuedJobInfo {
            id,
            model: "large-v3-turbo".to_string(),
            source: "stream",
            client: client.map(str::to_string),
            audio_seconds: 2.5,
            enqueued_at: Instant::now(),
        }
    }

    fn test_running_job(id: u64, client: Option<&str>) -> RunningJobInfo {
        RunningJobInfo {
            id,
            model: "large-v3-turbo".to_string(),
            source: "stream",
            client: client.map(str::to_string),
            audio_seconds: 2.5,
            started_at: Instant::now(),
        }
    }

    #[test]
    fn metrics_snapshot_separates_stream_queue_and_running_work() {
        let config = HostRuntimeConfig {
            worker_count: 2,
            queue_capacity: 4,
            max_active_streams: 3,
            max_recording_seconds: 120,
            use_gpu: true,
            worker_models: vec![SttModel::Parakeet, SttModel::Parakeet],
            ..test_config()
        };
        let live = HostLiveConfig::new(&config);
        let models = ModelService::default();
        let mut metrics = HostMetrics::new(&config);

        metrics.begin_stream(Some("192.168.1.31".to_string()));
        metrics.client_request(Some("192.168.1.31"));
        metrics.enqueue_job(test_queued_job(1, Some("192.168.1.31")));
        metrics.start_job(
            0,
            Duration::from_millis(30),
            true,
            test_running_job(1, Some("192.168.1.31")),
        );
        metrics.worker_model_ready(0, "parakeet-tdt-0.6b-v3".to_string());
        metrics.complete_job(
            0,
            2.5,
            "parakeet".to_string(),
            "parakeet-tdt-0.6b-v3".to_string(),
            "stream",
            Some("192.168.1.31"),
            Duration::from_millis(30),
            Duration::from_millis(90),
        );

        let snapshot = metrics.snapshot("127.0.0.1:48173", &live, inventory(&models, &live));

        assert_eq!(snapshot.active_streams, 1);
        assert_eq!(snapshot.queued_jobs, 0);
        assert_eq!(snapshot.running_jobs, 0);
        assert_eq!(snapshot.worker_count, 2);
        assert_eq!(snapshot.queue_capacity, 4);
        assert_eq!(snapshot.max_active_streams, 3);
        assert_eq!(snapshot.max_recording_seconds, 120);
        assert!(snapshot.use_gpu);
        assert_eq!(snapshot.total_transcriptions, 1);
        assert_eq!(snapshot.average_queue_ms, 30);
        assert_eq!(snapshot.average_processing_ms, 90);
        assert_eq!(snapshot.recent.len(), 1);
        assert_eq!(snapshot.recent[0].queue_wait_ms, 30);
        assert_eq!(snapshot.recent[0].processing_ms, 90);
        assert_eq!(snapshot.recent[0].client.as_deref(), Some("192.168.1.31"));

        assert_eq!(snapshot.model, "parakeet-tdt-0.6b-v3");
        assert_eq!(snapshot.workers.len(), 2);
        assert_eq!(snapshot.workers[0].state, "idle");
        assert_eq!(snapshot.workers[0].assigned_model, "parakeet-tdt-0.6b-v3");
        assert_eq!(
            snapshot.workers[0].loaded_model.as_deref(),
            Some("parakeet-tdt-0.6b-v3")
        );
        assert_eq!(snapshot.workers[0].completed_jobs, 1);
        assert_eq!(snapshot.workers[1].state, "idle");
        assert_eq!(snapshot.workers[1].assigned_model, "parakeet-tdt-0.6b-v3");
        assert_eq!(snapshot.workers[1].loaded_model, None);

        assert_eq!(snapshot.streams.len(), 1);
        assert_eq!(snapshot.streams[0].client.as_deref(), Some("192.168.1.31"));

        assert_eq!(snapshot.clients.len(), 1);
        assert_eq!(snapshot.clients[0].address, "192.168.1.31");
        assert_eq!(snapshot.clients[0].requests, 1);
        assert_eq!(snapshot.clients[0].completed, 1);
        assert_eq!(
            snapshot.clients[0].last_model.as_deref(),
            Some("parakeet-tdt-0.6b-v3")
        );
    }

    #[test]
    fn failed_job_clears_running_state_and_counts_failure() {
        let config = test_config();
        let mut metrics = HostMetrics::new(&config);

        metrics.enqueue_job(test_queued_job(1, None));
        metrics.start_job(
            0,
            Duration::from_millis(5),
            false,
            test_running_job(1, None),
        );
        metrics.fail_job(0, None, "Whisper context failed");

        assert_eq!(metrics.queued.len(), 0);
        assert_eq!(metrics.running_job_count(), 0);
        assert_eq!(metrics.failed_jobs, 1);
        assert_eq!(metrics.total_transcriptions, 0);
        assert_eq!(
            metrics.workers[0].last_error.as_deref(),
            Some("Whisper context failed")
        );
    }

    #[test]
    fn worker_loading_state_transitions_to_transcribing_when_model_ready() {
        let config = test_config();
        let mut metrics = HostMetrics::new(&config);

        metrics.enqueue_job(test_queued_job(1, None));
        metrics.start_job(0, Duration::from_millis(5), true, test_running_job(1, None));
        assert!(matches!(
            metrics.workers[0].state,
            super::WorkerState::Loading
        ));
        assert_eq!(metrics.workers[0].loaded_model, None);

        metrics.worker_model_ready(0, "large-v3-turbo".to_string());
        assert!(matches!(
            metrics.workers[0].state,
            super::WorkerState::Transcribing
        ));
        assert_eq!(
            metrics.workers[0].loaded_model.as_deref(),
            Some("large-v3-turbo")
        );
    }

    #[test]
    fn queue_snapshot_lists_waiting_jobs_in_arrival_order() {
        let config = test_config();
        let live = HostLiveConfig::new(&config);
        let models = ModelService::default();
        let mut metrics = HostMetrics::new(&config);

        metrics.enqueue_job(test_queued_job(1, Some("192.168.1.10")));
        metrics.enqueue_job(test_queued_job(2, Some("192.168.1.11")));

        let snapshot = metrics.snapshot("127.0.0.1:48173", &live, inventory(&models, &live));
        assert_eq!(snapshot.queued_jobs, 2);
        assert_eq!(snapshot.queue.len(), 2);
        assert_eq!(snapshot.queue[0].id, 1);
        assert_eq!(snapshot.queue[1].id, 2);
        assert_eq!(snapshot.queue[0].client.as_deref(), Some("192.168.1.10"));

        metrics.dequeue_job(1);
        let snapshot = metrics.snapshot("127.0.0.1:48173", &live, inventory(&models, &live));
        assert_eq!(snapshot.queue.len(), 1);
        assert_eq!(snapshot.queue[0].id, 2);
    }

    #[test]
    fn client_tracking_is_bounded_and_evicts_least_recently_seen() {
        let config = test_config();
        let mut metrics = HostMetrics::new(&config);

        for index in 0..MAX_TRACKED_CLIENTS {
            metrics.client_request(Some(&format!("10.0.0.{index}")));
        }
        assert_eq!(metrics.clients.len(), MAX_TRACKED_CLIENTS);

        // Refresh one entry, then force an eviction with a brand-new address.
        let oldest = metrics
            .clients
            .iter()
            .min_by_key(|(_, stats)| stats.last_seen_ms)
            .map(|(address, _)| address.clone())
            .expect("oldest client");
        metrics.client_request(Some("10.0.1.1"));

        assert_eq!(metrics.clients.len(), MAX_TRACKED_CLIENTS);
        assert!(!metrics.clients.contains_key(&oldest));
        assert!(metrics.clients.contains_key("10.0.1.1"));
    }

    #[test]
    fn worker_model_lifecycle_reports_unavailable_and_warm_states() {
        let config = test_config();
        let mut metrics = HostMetrics::new(&config);

        metrics.worker_model_unavailable(
            0,
            "base".to_string(),
            "Model base is not installed on this host".into(),
        );
        assert!(matches!(
            metrics.workers[0].state,
            super::WorkerState::ModelUnavailable
        ));
        assert_eq!(metrics.workers[0].loaded_model, None);
        assert!(metrics.workers[0]
            .last_error
            .as_deref()
            .unwrap()
            .contains("not installed"));

        metrics.worker_preloading(0, "base".to_string());
        assert!(matches!(
            metrics.workers[0].state,
            super::WorkerState::Loading
        ));

        metrics.worker_preload_ready(0, "base".to_string());
        assert!(matches!(metrics.workers[0].state, super::WorkerState::Idle));
        assert_eq!(metrics.workers[0].loaded_model.as_deref(), Some("base"));
        assert_eq!(metrics.workers[0].last_error, None);
    }

    #[test]
    fn model_download_state_reports_progress_in_snapshot() {
        let config = test_config();
        let live = HostLiveConfig::new(&config);
        let models = ModelService::default();
        let mut metrics = HostMetrics::new(&config);

        assert!(metrics
            .snapshot("127.0.0.1:48173", &live, inventory(&models, &live))
            .model_download
            .is_none());

        metrics.model_download = Some(ModelDownloadState {
            model: "base".to_string(),
            stage: "downloading".to_string(),
            percentage: 42,
            error: None,
        });
        let snapshot = metrics.snapshot("127.0.0.1:48173", &live, inventory(&models, &live));
        let download = snapshot.model_download.expect("download state");
        assert_eq!(download.model, "base");
        assert_eq!(download.percentage, 42);
        assert!(download.in_progress());

        let finished = ModelDownloadState {
            model: "base".to_string(),
            stage: "ready".to_string(),
            percentage: 100,
            error: None,
        };
        assert!(!finished.in_progress());
    }

    #[test]
    fn rejection_with_client_attributes_to_client_stats() {
        let config = test_config();
        let mut metrics = HostMetrics::new(&config);

        metrics.client_request(Some("192.168.1.20"));
        metrics.reject_job(Some("192.168.1.20"));
        metrics.reject_job(None);

        assert_eq!(metrics.rejected_jobs, 2);
        let stats = metrics.clients.get("192.168.1.20").expect("client stats");
        assert_eq!(stats.requests, 1);
        assert_eq!(stats.rejected, 1);
    }

    fn drain_event_names(subscription: &super::events::EventSubscription) -> Vec<String> {
        std::iter::from_fn(|| subscription.try_next())
            .map(|frame| {
                assert!(
                    !frame.contains("\"text\""),
                    "events never carry transcripts"
                );
                frame
                    .lines()
                    .next()
                    .and_then(|line| line.strip_prefix("event: "))
                    .unwrap_or_default()
                    .to_string()
            })
            .collect()
    }

    #[test]
    fn metrics_mutations_publish_one_lifecycle_per_job() {
        let config = test_config();
        let mut metrics = HostMetrics::new(&config);
        let subscription = metrics.events.subscribe().expect("subscribe");

        let stream_id = metrics.begin_stream(Some("192.168.1.31".to_string()));
        metrics.enqueue_job(test_queued_job(1, Some("192.168.1.31")));
        metrics.start_job(
            0,
            Duration::from_millis(4),
            true,
            test_running_job(1, Some("192.168.1.31")),
        );
        metrics.worker_model_ready(0, "large-v3-turbo".to_string());
        metrics.finish_stream(stream_id, 2.5);
        metrics.complete_job(
            0,
            2.5,
            "whisper".to_string(),
            "large-v3-turbo".to_string(),
            "stream",
            Some("192.168.1.31"),
            Duration::from_millis(4),
            Duration::from_millis(80),
        );

        assert_eq!(
            drain_event_names(&subscription),
            [
                "stream_started",
                "job_queued",
                "job_started",
                "worker_state",
                "worker_state",
                "stream_finished",
                "job_completed",
                "worker_state",
            ]
        );
        assert_eq!(metrics.recent[0].backend, "whisper");

        // A job the queue turned away still ends, with no worker.
        metrics.enqueue_job(test_queued_job(2, None));
        metrics.drop_queued_job(2, "Transcription queue is full");
        let frames: Vec<_> = std::iter::from_fn(|| subscription.try_next()).collect();
        assert_eq!(frames.len(), 2);
        assert!(frames[1].starts_with("event: job_failed\n"));
        assert!(frames[1].contains("\"jobId\":2,\"worker\":null,"));
        assert!(metrics.queued.is_empty());
    }

    #[test]
    fn worker_state_events_are_deduplicated_across_idle_polls() {
        let config = test_config();
        let mut metrics = HostMetrics::new(&config);
        let subscription = metrics.events.subscribe().expect("subscribe");

        for _ in 0..5 {
            metrics.worker_model_unavailable(0, "base".to_string(), "missing".to_string());
        }
        metrics.worker_preloading(0, "base".to_string());
        metrics.worker_preload_ready(0, "base".to_string());

        let frames: Vec<_> = std::iter::from_fn(|| subscription.try_next()).collect();
        assert_eq!(frames.len(), 3);
        assert!(frames[0].contains("\"state\":\"model-unavailable\",\"model\":\"base\""));
        assert!(frames[1].contains("\"state\":\"loading\""));
        assert!(frames[2].contains("\"state\":\"idle\""));
    }

    #[test]
    fn abandoned_and_failed_jobs_publish_job_failed() {
        let config = test_config();
        let mut metrics = HostMetrics::new(&config);
        metrics.start_job(0, Duration::ZERO, false, test_running_job(3, None));
        let subscription = metrics.events.subscribe().expect("subscribe");

        metrics.abandon_job(0);
        metrics.start_job(0, Duration::ZERO, false, test_running_job(4, None));
        metrics.fail_job(0, None, "Whisper context failed");

        let frames: Vec<_> = std::iter::from_fn(|| subscription.try_next()).collect();
        let failures: Vec<_> = frames
            .iter()
            .filter(|frame| frame.starts_with("event: job_failed\n"))
            .collect();
        assert_eq!(failures.len(), 2);
        assert!(failures[0].contains("\"jobId\":3,\"worker\":0,"));
        assert!(failures[0].contains(super::STREAM_ABORTED));
        assert!(failures[1].contains("\"error\":\"Whisper context failed\""));
        assert_eq!(metrics.failed_jobs, 1);
    }

    #[test]
    fn model_download_events_skip_repeated_progress() {
        let config = test_config();
        let mut metrics = HostMetrics::new(&config);
        let subscription = metrics.events.subscribe().expect("subscribe");
        let progress = |percentage| ModelDownloadState {
            model: "base".to_string(),
            stage: "downloading".to_string(),
            percentage,
            error: None,
        };

        metrics.set_model_download(progress(10));
        metrics.set_model_download(progress(10));
        metrics.set_model_download(progress(11));

        let frames: Vec<_> = std::iter::from_fn(|| subscription.try_next()).collect();
        assert_eq!(frames.len(), 2);
        assert!(frames[1].contains("\"stage\":\"downloading\",\"percentage\":11"));
        assert_eq!(metrics.model_download.as_ref().unwrap().percentage, 11);
    }

    #[test]
    fn stats_snapshot_lists_servable_models_with_worker_assignments() {
        let config = HostRuntimeConfig {
            worker_count: 2,
            worker_models: vec![SttModel::Parakeet, SttModel::ParakeetUltra],
            ..test_config()
        };
        let live = HostLiveConfig::new(&config);
        let models = ModelService::default();
        let metrics = HostMetrics::new(&config);

        let json = serde_json::to_value(metrics.snapshot(
            "127.0.0.1:48173",
            &live,
            inventory(&models, &live),
        ))
        .expect("serialize snapshot");
        let listed = json["models"].as_array().expect("models array");
        assert!(listed.len() >= 3);
        let by_id = |id: &str| {
            listed
                .iter()
                .find(|model| model["id"] == id)
                .unwrap_or_else(|| panic!("{id} listed"))
        };
        let ultra = by_id("parakeet-ultra");
        assert_eq!(ultra["name"], "Parakeet Ultra 0.6B");
        assert_eq!(ultra["publisher"], "Moondream");
        assert_eq!(ultra["assignedWorkers"], serde_json::json!([1]));
        assert!(ultra["sizeBytes"].as_u64().unwrap() > 0);
        assert!(ultra["installed"].is_boolean());
        assert_eq!(
            by_id("parakeet-tdt-0.6b-v3")["assignedWorkers"],
            serde_json::json!([0])
        );
        assert_eq!(
            by_id("parakeet-tdt-0.6b-v2")["assignedWorkers"],
            serde_json::json!([])
        );
    }
}
