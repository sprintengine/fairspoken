//! Super mode on the host, with load shedding.
//!
//! A dictation that asks for it (`x-fairspoken-super-mode: 1`) runs on two
//! workers at once: the worker that takes the job borrows an idle worker
//! serving the other engine, and the pair runs the same ensemble as the
//! desktop app (`crate::super_mode`). The host decides once, when the
//! dictation starts, and only grants super mode with room to spare: the
//! operator allows it, a Parakeet and a Whisper engine are loaded, the queue
//! is empty and free workers number at least twice the dictations that want
//! super mode right now. Everything else is shed, never queued.

use super::{protocol_header, HostMetrics, HostRuntime};
use crate::models::SttModel;
use crate::settings::{Settings, SuperModeSetting};
use crate::super_mode::{SuperModeEngines, SuperModeOutcome, SuperModeStatus};
use crate::transcription::ChunkTranscriber;
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};
use tiny_http::Request;

/// How often a lent worker checks whether it is back.
pub(super) const LENT_POLL: std::time::Duration = std::time::Duration::from_millis(50);

/// Host configuration `superMode` / `FAIRSPOKEN_HOST_SUPER_MODE`.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(super) enum SuperModePolicy {
    #[default]
    Allow,
    Off,
}

impl SuperModePolicy {
    pub(super) fn from_env() -> Result<Self, String> {
        match crate::app_dirs::env_var("FAIRSPOKEN_HOST_SUPER_MODE") {
            None => Ok(Self::Allow),
            Some(value) if value.trim().is_empty() => Ok(Self::Allow),
            Some(value) => Self::parse(&value).ok_or_else(|| {
                format!("FAIRSPOKEN_HOST_SUPER_MODE must be allow or off, not {value}")
            }),
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "allow" | "on" | "1" | "true" => Some(Self::Allow),
            "off" | "0" | "false" => Some(Self::Off),
            _ => None,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Allow => "allow",
            Self::Off => "off",
        }
    }
}

/// The workers' loaded engines and super mode's counters, kept inside
/// `HostMetrics` so a decision sees worker and queue state atomically.
#[derive(Default)]
pub(super) struct SuperModeState {
    slots: Vec<EngineSlot>,
    /// Dictations granted super mode whose job has not reached a worker.
    pending_grants: usize,
    used: u64,
    shed: u64,
    unavailable: u64,
}

#[derive(Clone, Default)]
struct EngineSlot {
    model: Option<SttModel>,
    engine: Option<Arc<dyn ChunkTranscriber>>,
    lent: bool,
}

impl SuperModeState {
    pub(super) fn new(worker_count: usize) -> Self {
        Self {
            slots: vec![EngineSlot::default(); worker_count],
            ..Self::default()
        }
    }

    /// Some worker has a Whisper (`true`) or Parakeet (`false`) engine loaded.
    fn engine_loaded(&self, whisper: bool) -> bool {
        self.slots.iter().any(|slot| {
            slot.engine.is_some() && slot.model.is_some_and(|m| m.is_whisper() == whisper)
        })
    }

    /// Both engines are loaded somewhere, so super mode can run at all.
    fn both_engines_loaded(&self) -> bool {
        self.engine_loaded(true) && self.engine_loaded(false)
    }
}

/// What the host decided for one dictation when it started.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SuperModeRequest {
    NotRequested,
    Granted,
    Refused(SuperModeStatus),
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct SuperModeStats {
    policy: &'static str,
    /// A Parakeet and a Whisper engine are loaded on some workers.
    available: bool,
    used: u64,
    shed: u64,
    unavailable: u64,
    /// Workers currently lent to another worker's super mode job.
    lent_workers: Vec<usize>,
}

impl HostMetrics {
    /// A worker's loaded engine (or none), refreshed on every idle poll.
    pub(super) fn register_engine(
        &mut self,
        worker: usize,
        loaded: Option<(SttModel, Arc<dyn ChunkTranscriber>)>,
    ) {
        if let Some(slot) = self.super_mode.slots.get_mut(worker) {
            let (model, engine) = loaded.map_or((None, None), |(m, e)| (Some(m), Some(e)));
            slot.model = model;
            slot.engine = engine;
        }
    }

    pub(super) fn worker_lent(&self, worker: usize) -> bool {
        self.super_mode
            .slots
            .get(worker)
            .is_some_and(|slot| slot.lent)
    }

    fn free_worker(&self, index: usize) -> bool {
        let slot = &self.super_mode.slots[index];
        slot.engine.is_some()
            && !slot.lent
            && self
                .workers
                .get(index)
                .is_some_and(|worker| worker.job.is_none())
    }

    fn decide_super_mode(&mut self, policy: SuperModePolicy) -> SuperModeRequest {
        if policy == SuperModePolicy::Off {
            return SuperModeRequest::Refused(SuperModeStatus::Off);
        }
        if !self.super_mode.both_engines_loaded() {
            self.super_mode.unavailable += 1;
            return SuperModeRequest::Refused(SuperModeStatus::Unavailable);
        }
        let free: Vec<usize> = (0..self.super_mode.slots.len())
            .filter(|&index| self.free_worker(index))
            .collect();
        let free_of = |whisper: bool| {
            free.iter()
                .filter(|&&index| {
                    self.super_mode.slots[index]
                        .model
                        .is_some_and(|m| m.is_whisper() == whisper)
                })
                .count()
        };
        let wanting = self.super_mode.pending_grants + 1;
        let room = free.len() >= 2 * wanting
            && free_of(true) >= 1
            && free_of(false) >= 1
            && self.queued.is_empty();
        if room {
            self.super_mode.pending_grants += 1;
            SuperModeRequest::Granted
        } else {
            self.super_mode.shed += 1;
            SuperModeRequest::Refused(SuperModeStatus::Shed)
        }
    }

    pub(super) fn super_mode_stats(&self, policy: SuperModePolicy) -> SuperModeStats {
        SuperModeStats {
            policy: policy.as_str(),
            available: self.super_mode.both_engines_loaded(),
            used: self.super_mode.used,
            shed: self.super_mode.shed,
            unavailable: self.super_mode.unavailable,
            lent_workers: (0..self.super_mode.slots.len())
                .filter(|&index| self.super_mode.slots[index].lent)
                .collect(),
        }
    }
}

impl HostRuntime {
    /// Reads `x-fairspoken-super-mode` and decides for the whole dictation.
    /// A granted dictation carries the grant to its worker in
    /// `settings.super_mode`.
    pub(super) fn request_super_mode(
        &self,
        request: &Request,
        settings: &mut Settings,
    ) -> SuperModeRequest {
        settings.super_mode = SuperModeSetting::Off;
        let requested = protocol_header(request, "super-mode")
            .and_then(super::config::parse_bool)
            .unwrap_or(false);
        if !requested {
            return SuperModeRequest::NotRequested;
        }
        let decision = self
            .metrics
            .lock()
            .map(|mut metrics| metrics.decide_super_mode(self.live.super_mode))
            .unwrap_or(SuperModeRequest::Refused(SuperModeStatus::Shed));
        if decision == SuperModeRequest::Granted {
            settings.super_mode = SuperModeSetting::On;
        }
        decision
    }

    /// A granted dictation whose job never reached a worker.
    pub(super) fn cancel_super_grant(&self, decision: SuperModeRequest) {
        if decision == SuperModeRequest::Granted {
            if let Ok(mut metrics) = self.metrics.lock() {
                metrics.super_mode.pending_grants =
                    metrics.super_mode.pending_grants.saturating_sub(1);
            }
        }
    }
}

/// A worker lent to a super mode job; returned when dropped.
pub(super) struct PartnerLease {
    metrics: Arc<Mutex<HostMetrics>>,
    worker: usize,
    /// The Parakeet side of the pair, which the response names.
    primary_model: SttModel,
}

impl Drop for PartnerLease {
    fn drop(&mut self) {
        if let Ok(mut metrics) = self.metrics.lock() {
            if let Some(slot) = metrics.super_mode.slots.get_mut(self.worker) {
                slot.lent = false;
            }
        }
    }
}

/// Called by the worker that took a granted job: borrows a free worker
/// serving the other engine and pairs the two engines, Parakeet first.
/// `None` when this worker's engine is not loaded or no partner is free.
pub(super) fn lend_partner(
    metrics: &Arc<Mutex<HostMetrics>>,
    borrower: usize,
    own: Option<(SttModel, Arc<dyn ChunkTranscriber>)>,
) -> Option<(PartnerLease, SuperModeEngines)> {
    let mut guard = metrics.lock().ok()?;
    guard.super_mode.pending_grants = guard.super_mode.pending_grants.saturating_sub(1);
    let (own_model, own_engine) = own?;
    let partner = (0..guard.super_mode.slots.len()).find(|&index| {
        index != borrower
            && guard.free_worker(index)
            && guard.super_mode.slots[index]
                .model
                .is_some_and(|model| model.is_whisper() != own_model.is_whisper())
    })?;
    let slot = &mut guard.super_mode.slots[partner];
    // Mark it lent only once nothing below can bail out, or a slot missing
    // its model would stay lent (taking no jobs) with no lease to return it.
    let partner_model = slot.model?;
    let partner_engine = slot.engine.clone()?;
    slot.lent = true;
    let engines = if own_model.is_whisper() {
        SuperModeEngines {
            primary: partner_engine,
            secondary: own_engine,
            secondary_model: own_model.model_id().to_string(),
        }
    } else {
        SuperModeEngines {
            primary: own_engine,
            secondary: partner_engine,
            secondary_model: partner_model.model_id().to_string(),
        }
    };
    drop(guard);
    let primary_model = if own_model.is_whisper() {
        partner_model
    } else {
        own_model
    };
    Some((
        PartnerLease {
            metrics: Arc::clone(metrics),
            worker: partner,
            primary_model,
        },
        engines,
    ))
}

/// Blocks a worker while it is lent to another worker's job, in
/// `LENT_POLL` steps, so it never starts a job of its own on an engine the
/// borrower is using.
pub(super) fn wait_until_returned(metrics: &Arc<Mutex<HostMetrics>>, worker: usize) {
    while metrics
        .lock()
        .is_ok_and(|metrics| metrics.worker_lent(worker))
    {
        std::thread::sleep(LENT_POLL);
    }
}

/// How a granted job ended, and the primary (Parakeet) model the response
/// names when both engines ran.
pub(super) struct SuperJobEnd {
    pub(super) outcome: SuperModeOutcome,
    pub(super) primary_model: Option<SttModel>,
}

/// Returns the partner and settles how a granted job ended: `used` when both
/// engines ran, otherwise `shed` (no partner was free once the job started).
pub(super) fn finish_super_job(
    metrics: &Arc<Mutex<HostMetrics>>,
    lease: Option<PartnerLease>,
    outcome: Option<SuperModeOutcome>,
) -> SuperJobEnd {
    let primary_model = lease.as_ref().map(|lease| lease.primary_model);
    let leased = lease.is_some();
    drop(lease);
    let outcome = match outcome {
        Some(outcome) if leased && outcome.status == SuperModeStatus::Used => outcome,
        _ => SuperModeOutcome::new(
            SuperModeStatus::Shed,
            None,
            Some("no free worker for the second engine when the job started"),
        ),
    };
    let used = outcome.status == SuperModeStatus::Used;
    if let Ok(mut metrics) = metrics.lock() {
        if used {
            metrics.super_mode.used += 1;
        } else {
            metrics.super_mode.shed += 1;
        }
    }
    SuperJobEnd {
        outcome,
        primary_model: primary_model.filter(|_| used),
    }
}

/// The response's `superMode` and `secondaryModel`.
pub(super) fn response_fields(
    decision: SuperModeRequest,
    outcome: Option<&SuperModeOutcome>,
) -> (Option<String>, Option<String>) {
    match decision {
        SuperModeRequest::NotRequested => (None, None),
        SuperModeRequest::Refused(status) => (Some(status.as_str().to_string()), None),
        SuperModeRequest::Granted => match outcome {
            Some(outcome) if outcome.status == SuperModeStatus::Used => (
                Some(outcome.status.as_str().to_string()),
                outcome.secondary_model.clone(),
            ),
            _ => (Some(SuperModeStatus::Shed.as_str().to_string()), None),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::config::HostRuntimeConfig;
    #[cfg(feature = "whisper")]
    use crate::host::RunningJobInfo;
    #[cfg(feature = "whisper")]
    use std::time::Instant;

    struct Stub;
    impl ChunkTranscriber for Stub {
        fn transcribe_pcm(
            &self,
            _: &[i16],
            _: u32,
            _: &str,
            _: Option<&str>,
        ) -> Result<String, String> {
            Ok("stub".into())
        }
    }

    #[cfg(feature = "whisper")]
    fn whisper() -> SttModel {
        SttModel::Whisper(crate::models::WhisperModel::Base)
    }

    fn metrics(workers: usize) -> HostMetrics {
        let config = HostRuntimeConfig {
            worker_count: workers,
            queue_capacity: 4,
            max_active_streams: 4,
            max_recording_seconds: 600,
            use_gpu: false,
            worker_models: vec![SttModel::Parakeet; workers],
            super_mode: SuperModePolicy::Allow,
        };
        HostMetrics::new(&config)
    }

    fn engine() -> Arc<dyn ChunkTranscriber> {
        Arc::new(Stub)
    }

    #[cfg(feature = "whisper")]
    fn busy(metrics: &mut HostMetrics, worker: usize) {
        metrics.start_job(
            worker,
            std::time::Duration::ZERO,
            false,
            RunningJobInfo {
                id: 99,
                model: "m".into(),
                source: "stream",
                client: None,
                audio_seconds: 0.0,
                started_at: Instant::now(),
            },
        );
    }

    #[test]
    fn policy_parses_the_documented_values() {
        assert_eq!(
            SuperModePolicy::parse("allow"),
            Some(SuperModePolicy::Allow)
        );
        assert_eq!(SuperModePolicy::parse(" OFF "), Some(SuperModePolicy::Off));
        assert_eq!(SuperModePolicy::parse("maybe"), None);
        assert_eq!(
            serde_json::to_string(&SuperModePolicy::Off).unwrap(),
            "\"off\""
        );
    }

    #[test]
    fn without_both_engines_super_mode_is_unavailable() {
        let mut m = metrics(2);
        m.register_engine(0, Some((SttModel::Parakeet, engine())));
        m.register_engine(1, Some((SttModel::Parakeet, engine())));
        assert_eq!(
            m.decide_super_mode(SuperModePolicy::Allow),
            SuperModeRequest::Refused(SuperModeStatus::Unavailable)
        );
        assert_eq!(
            m.decide_super_mode(SuperModePolicy::Off),
            SuperModeRequest::Refused(SuperModeStatus::Off)
        );
        let stats = m.super_mode_stats(SuperModePolicy::Allow);
        assert!(!stats.available);
        assert_eq!(stats.unavailable, 1);
    }

    #[cfg(feature = "whisper")]
    #[test]
    fn grants_need_twice_the_wanting_dictations_in_free_workers_and_an_empty_queue() {
        let mut m = metrics(4);
        m.register_engine(0, Some((SttModel::Parakeet, engine())));
        m.register_engine(1, Some((whisper(), engine())));
        m.register_engine(2, Some((SttModel::Parakeet, engine())));
        m.register_engine(3, Some((whisper(), engine())));
        assert_eq!(
            m.decide_super_mode(SuperModePolicy::Allow),
            SuperModeRequest::Granted
        );
        // One grant pending: a second needs four free workers, and has them.
        assert_eq!(
            m.decide_super_mode(SuperModePolicy::Allow),
            SuperModeRequest::Granted
        );
        // A third would need six.
        assert_eq!(
            m.decide_super_mode(SuperModePolicy::Allow),
            SuperModeRequest::Refused(SuperModeStatus::Shed)
        );
        m.super_mode.pending_grants = 0;
        busy(&mut m, 1);
        busy(&mut m, 3);
        // Two free workers, both Parakeet: no Whisper partner.
        assert_eq!(
            m.decide_super_mode(SuperModePolicy::Allow),
            SuperModeRequest::Refused(SuperModeStatus::Shed)
        );
        let mut m = metrics(2);
        m.register_engine(0, Some((SttModel::Parakeet, engine())));
        m.register_engine(1, Some((whisper(), engine())));
        m.queued.push_back(super::super::QueuedJobInfo {
            id: 1,
            model: "m".into(),
            source: "batch",
            client: None,
            audio_seconds: 1.0,
            enqueued_at: Instant::now(),
        });
        assert_eq!(
            m.decide_super_mode(SuperModePolicy::Allow),
            SuperModeRequest::Refused(SuperModeStatus::Shed)
        );
        assert_eq!(m.super_mode_stats(SuperModePolicy::Allow).shed, 1);
    }

    #[cfg(feature = "whisper")]
    #[test]
    fn a_lent_worker_pairs_parakeet_first_and_comes_back() {
        let metrics = Arc::new(Mutex::new(metrics(2)));
        {
            let mut m = metrics.lock().unwrap();
            m.register_engine(0, Some((SttModel::Parakeet, engine())));
            m.register_engine(1, Some((whisper(), engine())));
            assert_eq!(
                m.decide_super_mode(SuperModePolicy::Allow),
                SuperModeRequest::Granted
            );
            busy(&mut m, 1);
        }
        // The Whisper worker took the job; Parakeet is lent to it.
        let (lease, engines) = lend_partner(&metrics, 1, Some((whisper(), engine()))).unwrap();
        assert_eq!(engines.secondary_model, "base");
        {
            let m = metrics.lock().unwrap();
            assert!(m.worker_lent(0));
            assert_eq!(m.super_mode.pending_grants, 0);
            assert_eq!(
                m.super_mode_stats(SuperModePolicy::Allow).lent_workers,
                vec![0]
            );
        }
        let used = SuperModeOutcome::new(SuperModeStatus::Used, Some("base".into()), None);
        let end = finish_super_job(&metrics, Some(lease), Some(used));
        assert_eq!(end.outcome.status, SuperModeStatus::Used);
        // The Whisper worker served the job; the response names Parakeet.
        assert_eq!(end.primary_model, Some(SttModel::Parakeet));
        let m = metrics.lock().unwrap();
        assert!(!m.worker_lent(0));
        assert_eq!(m.super_mode_stats(SuperModePolicy::Allow).used, 1);
    }

    #[test]
    fn a_lent_worker_waits_for_its_lease_to_end() {
        let metrics = Arc::new(Mutex::new(metrics(2)));
        wait_until_returned(&metrics, 0);
        metrics.lock().unwrap().super_mode.slots[0].lent = true;
        let lease = PartnerLease {
            metrics: Arc::clone(&metrics),
            worker: 0,
            primary_model: SttModel::Parakeet,
        };
        let returned = std::thread::spawn(move || {
            std::thread::sleep(LENT_POLL * 3);
            drop(lease);
            Instant::now()
        });
        wait_until_returned(&metrics, 0);
        let done = Instant::now();
        assert!(done >= returned.join().unwrap());
        assert!(!metrics.lock().unwrap().worker_lent(0));
    }

    #[test]
    fn a_job_without_a_partner_is_shed_and_reported() {
        let metrics = Arc::new(Mutex::new(metrics(1)));
        metrics.lock().unwrap().super_mode.pending_grants = 1;
        assert!(lend_partner(&metrics, 0, Some((SttModel::Parakeet, engine()))).is_none());
        let end = finish_super_job(&metrics, None, None);
        assert_eq!(end.primary_model, None);
        let outcome = end.outcome;
        assert_eq!(outcome.status, SuperModeStatus::Shed);
        let m = metrics.lock().unwrap();
        assert_eq!(m.super_mode.pending_grants, 0);
        assert_eq!(m.super_mode_stats(SuperModePolicy::Allow).shed, 1);
        assert_eq!(
            response_fields(SuperModeRequest::Granted, Some(&outcome)),
            (Some("shed".to_string()), None)
        );
        assert_eq!(
            response_fields(SuperModeRequest::NotRequested, None),
            (None, None)
        );
        assert_eq!(
            response_fields(
                SuperModeRequest::Refused(SuperModeStatus::Unavailable),
                None
            ),
            (Some("unavailable".to_string()), None)
        );
    }
}
