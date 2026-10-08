//! The transcription runtime: the job queue, the worker threads that
//! serve it, and how stream and batch audio is transcribed.

use super::*;

// How long an idle worker blocks on the job queue before re-checking its
// assigned model, so runtime model changes and freshly downloaded model files
// are picked up without a job arriving.
pub(super) const WORKER_IDLE_POLL: Duration = Duration::from_millis(250);

pub(super) struct HostRuntime {
    pub(super) job_tx: SyncSender<TranscriptionJob>,
    pub(super) metrics: Arc<Mutex<HostMetrics>>,
    pub(super) live: Arc<HostLiveConfig>,
    pub(super) models: ModelService,
    pub(super) config_path: PathBuf,
    pub(super) next_job_id: AtomicU64,
    pub(super) pairing: Mutex<PairingLimiter>,
    /// Batch uploads between admission and their answer: at most every
    /// worker busy plus a full queue, since more could only be turned away
    /// after buffering their bodies.
    pub(super) batch_admissions: Slots,
    pub(super) public_handlers: Slots,
    pub(super) authenticated_handlers: Slots,
}

pub(super) struct TranscriptionJob {
    pub(super) id: u64,
    pub(super) settings: Settings,
    pub(super) audio: JobAudio,
    pub(super) source: &'static str,
    pub(super) client: Option<String>,
    pub(super) result_tx: mpsc::Sender<Result<TranscriptionOutcome, String>>,
    pub(super) accepted_at: Instant,
}

/// Audio for a job: a complete recording (batch uploads), or frames still
/// arriving from a client that is speaking, which the worker transcribes in
/// chunks as they land so only the tail is left to decode at release.
pub(super) enum JobAudio {
    Recording(Recording),
    Stream(Receiver<StreamInput>),
}

pub(super) enum StreamInput {
    Frame(AudioFrame),
    /// The upload failed part-way; the request handler already answered the
    /// client, so the worker drops the session without recording a failure.
    Abort,
}

/// What a worker hands back for a completed job. The model is the worker's
/// assigned model — the host's choice, not the client's — and is reported in
/// the response so clients always learn what actually ran.
#[derive(Debug)]
pub(super) struct TranscriptionOutcome {
    pub(super) text: String,
    pub(super) backend: String,
    pub(super) model: String,
    /// Set for a job granted super mode.
    pub(super) super_mode: Option<crate::super_mode::SuperModeOutcome>,
}

#[derive(Debug)]
pub(super) enum HostRuntimeError {
    QueueFull(String),
    WorkerFailed(String),
}

pub(super) struct ActiveStreamGuard {
    pub(super) metrics: Arc<Mutex<HostMetrics>>,
    pub(super) stream_id: u64,
    /// Audio received, reported when the stream finishes; stays 0 for an
    /// upload that failed part-way.
    pub(super) audio_seconds: f32,
}

impl Drop for ActiveStreamGuard {
    fn drop(&mut self) {
        if let Ok(mut metrics) = self.metrics.lock() {
            metrics.finish_stream(self.stream_id, self.audio_seconds);
        }
    }
}

impl HostRuntime {
    pub(super) fn start(
        models: ModelService,
        config: HostRuntimeConfig,
        metrics: Arc<Mutex<HostMetrics>>,
        config_path: PathBuf,
    ) -> Result<Self, String> {
        let (job_tx, job_rx) = mpsc::sync_channel::<TranscriptionJob>(config.queue_capacity);
        let job_rx = Arc::new(Mutex::new(job_rx));
        let live = Arc::new(HostLiveConfig::new(&config));

        for worker_index in 0..config.worker_count {
            let worker_rx = Arc::clone(&job_rx);
            let worker_models = models.clone();
            let worker_metrics = Arc::clone(&metrics);
            let worker_live = Arc::clone(&live);
            thread::Builder::new()
                .name(format!("transcription-host-worker-{worker_index}"))
                .spawn(move || {
                    run_host_worker(
                        worker_index,
                        worker_rx,
                        worker_models,
                        worker_metrics,
                        worker_live,
                    );
                })
                .map_err(|err| format!("Failed to start transcription host worker: {err}"))?;
        }

        Ok(Self {
            job_tx,
            metrics,
            live,
            models,
            config_path,
            next_job_id: AtomicU64::new(1),
            pairing: Mutex::new(PairingLimiter::default()),
            batch_admissions: Slots::new(config.worker_count + config.queue_capacity),
            public_handlers: Slots::new(MAX_PUBLIC_HANDLERS),
            authenticated_handlers: Slots::new(MAX_AUTHENTICATED_HANDLERS),
        })
    }

    pub(super) fn try_admit_batch(&self) -> Option<Slot> {
        self.batch_admissions.try_take()
    }

    pub(super) fn max_recording_seconds(&self) -> u16 {
        self.live.max_recording_seconds()
    }

    /// Reads the model files a stats snapshot reports on; call it before
    /// taking the metrics lock.
    pub(super) fn model_inventory(&self) -> ModelInventory {
        ModelInventory::read(&self.models, self.live.worker_models())
    }

    pub(super) fn try_begin_stream(
        &self,
        client: Option<String>,
    ) -> Result<ActiveStreamGuard, String> {
        let mut metrics = self
            .metrics
            .lock()
            .map_err(|_| "Host metrics lock failed".to_string())?;
        if metrics.active_stream_count() >= self.live.max_active_streams() {
            metrics.reject_job(client.as_deref());
            return Err("Server is at active stream capacity".to_string());
        }
        let stream_id = metrics.begin_stream(client);
        Ok(ActiveStreamGuard {
            metrics: Arc::clone(&self.metrics),
            stream_id,
            audio_seconds: 0.0,
        })
    }

    pub(super) fn validate_recording_duration(&self, duration_seconds: f32) -> Result<(), String> {
        let max_recording_seconds = self.live.max_recording_seconds();
        if duration_seconds > f32::from(max_recording_seconds) {
            return Err(format!(
                "Recording exceeds host maximum of {max_recording_seconds} seconds"
            ));
        }
        Ok(())
    }

    pub(super) fn record_rejection(&self, client: Option<&str>) {
        if let Ok(mut metrics) = self.metrics.lock() {
            metrics.reject_job(client);
        }
    }

    pub(super) fn record_client_request(&self, client: Option<&str>) {
        if let Ok(mut metrics) = self.metrics.lock() {
            metrics.client_request(client);
        }
    }

    pub(super) fn transcribe(
        &self,
        recording: Recording,
        settings: Settings,
        source: &'static str,
        client: Option<String>,
    ) -> Result<TranscriptionOutcome, HostRuntimeError> {
        // Callers check the duration first: over the limit is a 413, not a
        // capacity answer.
        let duration_seconds = recording.stats().duration_seconds;
        let result_rx = self.enqueue(
            JobAudio::Recording(recording),
            settings,
            source,
            client,
            duration_seconds,
        )?;
        self.await_result(result_rx)
    }

    pub(super) fn enqueue(
        &self,
        audio: JobAudio,
        mut settings: Settings,
        source: &'static str,
        client: Option<String>,
        duration_seconds: f32,
    ) -> Result<Receiver<Result<TranscriptionOutcome, String>>, HostRuntimeError> {
        // GPU use is an operator decision for the whole host, not a
        // per-request client choice.
        settings.use_gpu = self.live.use_gpu();
        let job_id = self.next_job_id.fetch_add(1, Ordering::Relaxed);
        let (result_tx, result_rx) = mpsc::channel::<Result<TranscriptionOutcome, String>>();
        let job = TranscriptionJob {
            id: job_id,
            settings,
            audio,
            source,
            client: client.clone(),
            result_tx,
            accepted_at: Instant::now(),
        };

        if let Ok(mut metrics) = self.metrics.lock() {
            metrics.enqueue_job(QueuedJobInfo {
                id: job_id,
                // The serving worker isn't known yet; report the host's
                // configured model (or "mixed" when workers differ).
                model: self.live.model_summary(),
                source,
                client: client.clone(),
                audio_seconds: duration_seconds,
                enqueued_at: Instant::now(),
            });
        }

        match self.job_tx.try_send(job) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {
                let message = "Transcription queue is full".to_string();
                if let Ok(mut metrics) = self.metrics.lock() {
                    metrics.drop_queued_job(job_id, &message);
                    metrics.reject_job(client.as_deref());
                }
                return Err(HostRuntimeError::QueueFull(message));
            }
            Err(TrySendError::Disconnected(_)) => {
                let message = "Transcription worker queue is unavailable".to_string();
                if let Ok(mut metrics) = self.metrics.lock() {
                    metrics.drop_queued_job(job_id, &message);
                }
                return Err(HostRuntimeError::WorkerFailed(message));
            }
        }
        Ok(result_rx)
    }

    pub(super) fn await_result(
        &self,
        result_rx: Receiver<Result<TranscriptionOutcome, String>>,
    ) -> Result<TranscriptionOutcome, HostRuntimeError> {
        result_rx
            .recv()
            .map_err(|_| {
                HostRuntimeError::WorkerFailed(
                    "Transcription worker stopped without a result".to_string(),
                )
            })?
            .map_err(HostRuntimeError::WorkerFailed)
    }
}

pub(super) fn run_host_worker(
    worker_index: usize,
    job_rx: Arc<Mutex<Receiver<TranscriptionJob>>>,
    models: ModelService,
    metrics: Arc<Mutex<HostMetrics>>,
    live: Arc<HostLiveConfig>,
) {
    let mut transcription = TranscriptionService::default();
    // Mirrors the model/GPU pair held by this worker's loaded engine, so the
    // dashboard can show a "loading model" phase when an engine (re)load is in
    // flight. Each worker owns its own engine instance, so workers transcribe
    // in parallel off the shared job queue without sharing a model.
    let mut loaded: Option<(SttModel, bool)> = None;
    // Last (model, GPU, file mtime) whose load failed; retried only when the
    // target or the file on disk changes, so a corrupt model file is not
    // re-read on every idle poll.
    let mut last_failed: Option<(SttModel, bool, Option<SystemTime>)> = None;
    loop {
        // The served model is host configuration: load the assigned model
        // while idle so the first dictation never pays the load wait, and so
        // runtime model changes apply without a job arriving.
        let assigned_model = live.worker_model(worker_index);
        let target = (assigned_model, live.use_gpu());
        if loaded != Some(target) {
            if !models.files_present(assigned_model) {
                if loaded.is_some() {
                    // Free the previous context: serving the old model would
                    // be a silent fallback, so the worker holds nothing.
                    transcription.unload();
                    loaded = None;
                }
                last_failed = None;
                if let Ok(mut metrics) = metrics.lock() {
                    metrics.worker_model_unavailable(
                        worker_index,
                        assigned_model.model_id().to_string(),
                        format!(
                            "Model {} is not installed on this host",
                            assigned_model.model_id()
                        ),
                    );
                }
            } else {
                let mtime = models.installed_mtime(assigned_model);
                if last_failed != Some((target.0, target.1, mtime)) {
                    if let Ok(mut metrics) = metrics.lock() {
                        metrics
                            .worker_preloading(worker_index, assigned_model.model_id().to_string());
                    }
                    let settings = Settings {
                        model: assigned_model,
                        use_gpu: target.1,
                        ..Settings::default()
                    };
                    match transcription.preload(&settings, &models) {
                        Ok(()) => {
                            loaded = Some(target);
                            last_failed = None;
                            if let Ok(mut metrics) = metrics.lock() {
                                metrics.worker_preload_ready(
                                    worker_index,
                                    assigned_model.model_id().to_string(),
                                );
                            }
                        }
                        Err(err) => {
                            last_failed = Some((target.0, target.1, mtime));
                            if let Ok(mut metrics) = metrics.lock() {
                                metrics.worker_model_unavailable(
                                    worker_index,
                                    assigned_model.model_id().to_string(),
                                    err,
                                );
                            }
                        }
                    }
                }
            }
        }

        // Super mode: publish the warm engine so another worker can borrow
        // it, and take no jobs while it is lent out.
        let own_engine = (loaded == Some(target))
            .then(|| transcription.loaded_engine())
            .flatten()
            .map(|engine| (assigned_model, engine));
        if let Ok(mut metrics) = metrics.lock() {
            metrics.register_engine(worker_index, own_engine.clone());
            if metrics.worker_lent(worker_index) {
                drop(metrics);
                thread::sleep(super_mode::LENT_POLL);
                continue;
            }
        }

        let job = {
            let receiver = match job_rx.lock() {
                Ok(receiver) => receiver,
                Err(_) => {
                    eprintln!("Transcription host worker {worker_index} receiver lock failed");
                    break;
                }
            };
            match receiver.recv_timeout(WORKER_IDLE_POLL) {
                Ok(job) => job,
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => break,
            }
        };

        // Lent out while it waited on the queue (a borrower found it free):
        // the borrower is using this worker's engine, so the job waits for
        // the lease to end before it starts.
        super_mode::wait_until_returned(&metrics, worker_index);

        let queue_wait = job.accepted_at.elapsed();
        // A live stream's length is only known once the client stops sending.
        let mut duration_seconds = match &job.audio {
            JobAudio::Recording(recording) => recording.stats().duration_seconds,
            JobAudio::Stream(_) => 0.0,
        };
        let backend = job_backend(assigned_model).to_string();
        // The worker's assigned model serves every job it takes; the client's
        // requested model (if any header survived) is deliberately ignored.
        let model = assigned_model.model_id().to_string();
        let mut settings = Settings {
            model: assigned_model,
            use_gpu: target.1,
            ..job.settings
        };
        // A super mode grant rides in the job's settings (see
        // `request_super_mode`); the worker arms it with a borrowed partner.
        let super_job = std::mem::replace(
            &mut settings.super_mode,
            crate::settings::SuperModeSetting::Off,
        ) != crate::settings::SuperModeSetting::Off;
        let source = job.source;
        let client = job.client.clone();
        let needs_load = loaded != Some(target);
        if let Ok(mut metrics) = metrics.lock() {
            metrics.start_job(
                worker_index,
                queue_wait,
                needs_load,
                RunningJobInfo {
                    id: job.id,
                    model: model.clone(),
                    source,
                    client: client.clone(),
                    audio_seconds: duration_seconds,
                    started_at: Instant::now(),
                },
            );
        }

        let super_lease = if super_job {
            super_mode::lend_partner(&metrics, worker_index, own_engine)
        } else {
            None
        };
        let (super_lease, super_engines) = super_lease.unzip();
        if let Some(engines) = super_engines {
            transcription.arm_super_mode(engines);
        }

        let mut started = Instant::now();
        let model_ready = std::cell::Cell::new(false);
        let on_model_ready = || {
            model_ready.set(true);
            if let Ok(mut metrics) = metrics.lock() {
                metrics.worker_model_ready(worker_index, model.clone());
            }
        };
        let result = match job.audio {
            JobAudio::Recording(recording) => transcribe_recording(
                &mut transcription,
                &models,
                &settings,
                recording,
                on_model_ready,
            ),
            JobAudio::Stream(frames) => transcribe_stream(
                &mut transcription,
                &models,
                &settings,
                frames,
                on_model_ready,
            )
            .map(|streamed| {
                duration_seconds = streamed.duration_seconds;
                // Report the wait after the client stopped sending, not the
                // time spent listening to it speak.
                started = streamed.upload_finished_at;
                streamed.text
            }),
        };
        let processing_time = started.elapsed();
        if model_ready.get() {
            loaded = Some(target);
        }

        match &result {
            Err(err) if err == STREAM_ABORTED => {
                if let Ok(mut metrics) = metrics.lock() {
                    metrics.abandon_job(worker_index);
                }
            }
            Ok(_) => {
                if let Ok(mut metrics) = metrics.lock() {
                    metrics.complete_job(
                        worker_index,
                        duration_seconds,
                        backend.clone(),
                        model.clone(),
                        source,
                        client.as_deref(),
                        queue_wait,
                        processing_time,
                    );
                }
            }
            Err(err) => {
                if let Ok(mut metrics) = metrics.lock() {
                    metrics.fail_job(worker_index, client.as_deref(), err);
                }
            }
        }

        let super_end = super_job.then(|| {
            super_mode::finish_super_job(
                &metrics,
                super_lease,
                transcription.take_super_mode_outcome(),
            )
        });
        // With super mode used the response names the Parakeet side, even
        // when the Whisper worker served the job.
        let (backend, model) = match super_end.as_ref().and_then(|end| end.primary_model) {
            Some(primary) => (
                job_backend(primary).to_string(),
                primary.model_id().to_string(),
            ),
            None => (backend.clone(), model.clone()),
        };
        let outcome = result.map(|text| TranscriptionOutcome {
            text,
            backend,
            model,
            super_mode: super_end.map(|end| end.outcome),
        });
        if job.result_tx.send(outcome).is_err() {
            eprintln!(
                "Transcription host worker {worker_index} completed job {} after requester disconnected",
                job.id
            );
        }
    }
}

/// The engine that ran a job. Jobs used to all report the protocol's default
/// `parakeet` id, which mislabelled Whisper work in the dashboard and in
/// clients' transcript history.
pub(super) fn job_backend(model: SttModel) -> &'static str {
    if model.is_whisper() {
        "whisper"
    } else {
        BACKEND_ID
    }
}

pub(super) const STREAM_ABORTED: &str = "Stream upload aborted";

/// How long a stream job waits for the client's next frame before giving
/// up on it: a phone that drops off the network mid-dictation would
/// otherwise hold its worker forever. Clients send frames continuously
/// while recording, so a live stream is never quiet this long.
pub(super) const STREAM_IDLE: Duration = Duration::from_secs(30);

/// The next input of a live stream: `None` once the upload has ended, and
/// `Abort` when nothing arrived for `idle`.
pub(super) fn recv_stream_input(
    frames: &Receiver<StreamInput>,
    idle: Duration,
) -> Option<StreamInput> {
    match frames.recv_timeout(idle) {
        Ok(input) => Some(input),
        Err(RecvTimeoutError::Timeout) => {
            eprintln!(
                "Stream upload sent nothing for {} s; dropping its job",
                idle.as_secs()
            );
            Some(StreamInput::Abort)
        }
        Err(RecvTimeoutError::Disconnected) => None,
    }
}

pub(super) struct StreamedTranscript {
    pub(super) text: String,
    pub(super) duration_seconds: f32,
    pub(super) upload_finished_at: Instant,
}

/// Feeds frames into a chunked session while the client is still speaking,
/// keeping a copy of the full recording so a session that loses frames can
/// fall back to transcribing it whole.
pub(super) fn transcribe_stream(
    transcription: &mut TranscriptionService,
    models: &ModelService,
    settings: &Settings,
    frames: Receiver<StreamInput>,
    on_model_ready: impl FnOnce(),
) -> Result<StreamedTranscript, String> {
    let mut sink = transcription
        .start_session_traced(settings, models, None, None)?
        .audio_tx;
    on_model_ready();
    let mut recording = Recording {
        pcm_i16: Vec::new(),
        sample_rate: 16_000,
        dropped_stream_frames: 0,
    };
    while let Some(input) = recv_stream_input(&frames, STREAM_IDLE) {
        let frame = match input {
            StreamInput::Frame(frame) => frame,
            StreamInput::Abort => {
                transcription.cancel_session();
                return Err(STREAM_ABORTED.to_string());
            }
        };
        recording.sample_rate = frame.sample_rate;
        recording.pcm_i16.extend_from_slice(&frame.pcm_i16);
        if sink.as_ref().is_some_and(|tx| tx.send(frame).is_err()) {
            // The session stopped taking audio; finish_session transcribes
            // the full recording instead.
            sink = None;
            recording.dropped_stream_frames += 1;
        }
    }
    let upload_finished_at = Instant::now();
    drop(sink);
    let duration_seconds = recording.stats().duration_seconds;
    let text = transcription.finish_session(&recording, settings, models)?;
    Ok(StreamedTranscript {
        text,
        duration_seconds,
        upload_finished_at,
    })
}

pub(super) fn transcribe_recording(
    transcription: &mut TranscriptionService,
    models: &ModelService,
    settings: &Settings,
    recording: crate::audio::Recording,
    on_model_ready: impl FnOnce(),
) -> Result<String, String> {
    // start_session loads (or reuses) the Whisper context, so the model is
    // resident once it returns.
    let stream_sink = transcription.start_session(settings, models)?;
    on_model_ready();
    if let Some(sink) = stream_sink {
        for chunk in recording.pcm_i16.chunks(3200) {
            if sink
                .send(AudioFrame {
                    pcm_i16: chunk.to_vec(),
                    sample_rate: recording.sample_rate,
                })
                .is_err()
            {
                transcription.cancel_session();
                return Err("Transcription worker stopped while receiving audio".to_string());
            }
        }
    }

    transcription.finish_session(&recording, settings, models)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::test_support::*;
    #[cfg(feature = "whisper")]
    use crate::models::WhisperModel;

    fn test_outcome(text: &str) -> TranscriptionOutcome {
        TranscriptionOutcome {
            text: text.to_string(),
            backend: "whisper".to_string(),
            model: "base".to_string(),
            super_mode: None,
        }
    }

    fn test_recording(seconds: usize) -> Recording {
        Recording {
            pcm_i16: vec![1; 16_000 * seconds],
            sample_rate: 16_000,
            dropped_stream_frames: 0,
        }
    }

    #[test]
    fn stream_guard_releases_active_stream_capacity_on_drop() {
        let config = test_config();
        let metrics = Arc::new(Mutex::new(HostMetrics::new(&config)));
        let (job_tx, _job_rx) = mpsc::sync_channel(1);
        let runtime = test_runtime(config, job_tx, Arc::clone(&metrics));

        let guard = runtime
            .try_begin_stream(None)
            .expect("first stream admitted");
        let err = match runtime.try_begin_stream(None) {
            Ok(_) => panic!("second stream should exceed capacity"),
            Err(err) => err,
        };

        assert_eq!(err, "Server is at active stream capacity");
        assert_eq!(metrics.lock().expect("metrics").active_stream_count(), 1);

        drop(guard);

        assert_eq!(metrics.lock().expect("metrics").active_stream_count(), 0);
        let _next_guard = runtime
            .try_begin_stream(None)
            .expect("capacity should be released");
    }

    #[test]
    fn full_queue_rejects_without_leaving_queued_metrics() {
        let config = HostRuntimeConfig {
            queue_capacity: 0,
            ..test_config()
        };
        let metrics = Arc::new(Mutex::new(HostMetrics::new(&config)));
        let (job_tx, _job_rx) = mpsc::sync_channel(0);
        let runtime = test_runtime(config, job_tx, Arc::clone(&metrics));

        let err = runtime
            .transcribe(test_recording(1), Settings::default(), "stream", None)
            .expect_err("zero-capacity queue should reject");

        match err {
            HostRuntimeError::QueueFull(message) => {
                assert_eq!(message, "Transcription queue is full");
            }
            HostRuntimeError::WorkerFailed(message) => {
                panic!("unexpected worker failure: {message}");
            }
        }

        let metrics = metrics.lock().expect("metrics");
        assert_eq!(metrics.queued.len(), 0);
        assert_eq!(metrics.rejected_jobs, 1);
        assert_eq!(metrics.running_job_count(), 0);
    }

    #[test]
    fn queued_runtime_returns_results_to_multiple_waiting_callers() {
        let config = HostRuntimeConfig {
            queue_capacity: 2,
            ..test_config()
        };
        let metrics = Arc::new(Mutex::new(HostMetrics::new(&config)));
        let (job_tx, job_rx) = mpsc::sync_channel(2);
        let runtime = Arc::new(test_runtime(config, job_tx, Arc::clone(&metrics)));

        let first_runtime = Arc::clone(&runtime);
        let first = thread::spawn(move || {
            first_runtime.transcribe(test_recording(1), Settings::default(), "stream", None)
        });
        let second_runtime = Arc::clone(&runtime);
        let second = thread::spawn(move || {
            second_runtime.transcribe(test_recording(1), Settings::default(), "stream", None)
        });

        let first_job = job_rx.recv().expect("first queued job");
        let second_job = job_rx.recv().expect("second queued job");
        assert_eq!(metrics.lock().expect("metrics").queued.len(), 2);

        first_job
            .result_tx
            .send(Ok(test_outcome("first transcript")))
            .expect("send first result");
        second_job
            .result_tx
            .send(Ok(test_outcome("second transcript")))
            .expect("send second result");

        let mut results = vec![
            first
                .join()
                .expect("first caller joined")
                .expect("first ok")
                .text,
            second
                .join()
                .expect("second caller joined")
                .expect("second ok")
                .text,
        ];
        results.sort();

        assert_eq!(results, vec!["first transcript", "second transcript"]);
    }

    #[test]
    fn transcribe_reports_the_model_the_worker_actually_ran() {
        let config = test_config();
        let metrics = Arc::new(Mutex::new(HostMetrics::new(&config)));
        let (job_tx, job_rx) = mpsc::sync_channel(1);
        let runtime = Arc::new(test_runtime(config, job_tx, Arc::clone(&metrics)));

        let caller_runtime = Arc::clone(&runtime);
        let caller = thread::spawn(move || {
            caller_runtime.transcribe(test_recording(1), Settings::default(), "stream", None)
        });

        let job = job_rx.recv().expect("queued job");
        // The worker answers with its own assigned model, regardless of what
        // the request carried.
        job.result_tx
            .send(Ok(TranscriptionOutcome {
                text: "transcript".to_string(),
                backend: "whisper".to_string(),
                model: "large-v3-turbo".to_string(),
                super_mode: None,
            }))
            .expect("send result");

        let outcome = caller
            .join()
            .expect("caller joined")
            .expect("transcribe ok");
        assert_eq!(outcome.text, "transcript");
        assert_eq!(outcome.model, "large-v3-turbo");
    }

    #[test]
    fn host_gpu_config_overrides_client_settings() {
        let config = HostRuntimeConfig {
            use_gpu: false,
            ..test_config()
        };
        let metrics = Arc::new(Mutex::new(HostMetrics::new(&config)));
        let (job_tx, job_rx) = mpsc::sync_channel(1);
        let runtime = Arc::new(test_runtime(config, job_tx, Arc::clone(&metrics)));

        let client_settings = Settings::default();
        assert!(client_settings.use_gpu, "client default should request GPU");

        let worker_runtime = Arc::clone(&runtime);
        let caller = thread::spawn(move || {
            worker_runtime.transcribe(test_recording(1), client_settings, "batch", None)
        });

        let job = job_rx.recv().expect("queued job");
        assert!(
            !job.settings.use_gpu,
            "host config must force CPU inference"
        );

        job.result_tx
            .send(Ok(test_outcome("transcript")))
            .expect("send result");
        caller
            .join()
            .expect("caller joined")
            .expect("transcribe ok");
    }

    #[test]
    fn recording_duration_check_rejects_only_over_the_limit() {
        let config = test_config();
        let metrics = Arc::new(Mutex::new(HostMetrics::new(&config)));
        let (job_tx, _job_rx) = mpsc::sync_channel(1);
        let runtime = test_runtime(config, job_tx, Arc::clone(&metrics));

        runtime
            .validate_recording_duration(10.0)
            .expect("at the limit is fine");
        let err = runtime
            .validate_recording_duration(test_recording(11).stats().duration_seconds)
            .expect_err("oversized recording should reject");
        assert!(err.contains("Recording exceeds host maximum of 10 seconds"));
        // The batch handler answers that 413 itself; nothing was queued.
        assert_eq!(metrics.lock().expect("metrics").queued.len(), 0);
    }

    #[cfg(feature = "whisper")]
    #[test]
    fn jobs_report_the_engine_that_ran_them() {
        assert_eq!(super::job_backend(SttModel::Parakeet), "parakeet");
        assert_eq!(super::job_backend(SttModel::ParakeetUltra), "parakeet");
        assert_eq!(
            super::job_backend(SttModel::Whisper(WhisperModel::LargeV3Turbo)),
            "whisper"
        );
    }

    #[test]
    fn a_quiet_stream_is_aborted_after_the_idle_timeout() {
        use super::{recv_stream_input, StreamInput};
        let idle = Duration::from_millis(20);
        let (frames_tx, frames) = mpsc::channel();

        assert!(matches!(
            recv_stream_input(&frames, idle),
            Some(StreamInput::Abort)
        ));
        frames_tx
            .send(StreamInput::Frame(crate::audio::AudioFrame {
                pcm_i16: vec![1, 2],
                sample_rate: 16_000,
            }))
            .unwrap();
        assert!(matches!(
            recv_stream_input(&frames, idle),
            Some(StreamInput::Frame(_))
        ));
        drop(frames_tx);
        assert!(recv_stream_input(&frames, idle).is_none());
    }

    #[test]
    fn batch_uploads_are_admitted_only_up_to_workers_plus_queue() {
        let config = test_config();
        let metrics = Arc::new(Mutex::new(HostMetrics::new(&config)));
        let (job_tx, _job_rx) = mpsc::sync_channel(1);
        // One worker and a one-job queue: two uploads in flight at most.
        let runtime = test_runtime(config, job_tx, metrics);

        let first = runtime.try_admit_batch().expect("first upload");
        let second = runtime.try_admit_batch().expect("second upload");
        assert!(runtime.try_admit_batch().is_none());
        drop(first);
        let _third = runtime.try_admit_batch().expect("a freed place is reused");
        assert!(runtime.try_admit_batch().is_none());
        drop(second);
        assert!(runtime.try_admit_batch().is_some());
    }
}
