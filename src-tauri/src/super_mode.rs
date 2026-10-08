//! Super mode: Parakeet and Whisper transcribe the same chunks and
//! `ensemble::merge` reconciles them.
//!
//! The composite engine here plugs into the ordinary chunked pipeline, so
//! both models work while the user is still speaking. Per chunk, Parakeet
//! runs first; its text picks the dictionary terms for a targeted Whisper
//! prompt; Whisper then decodes the same audio on its own thread. While the
//! user speaks a slow Whisper result is waited for generously. Once the key
//! is released, the remaining chunks share `RELEASE_BUDGET` of waiting for
//! Whisper past Parakeet's results, and a chunk it misses keeps Parakeet's
//! text, so super mode never adds more than that to the wait for text. A
//! chunk the merge could not change (no dictionary terms, no confidences)
//! skips Whisper altogether.

use crate::ensemble::{
    self, Disagreement, Hypothesis, MergeConfig, MergeInput, PROMPT_TOKEN_BUDGET,
};
use crate::models::{ModelService, SttModel};
use crate::settings::{Settings, SuperModeSetting, TranscriptionLocation};
use crate::transcription::{speech_activity_profile, ChunkTranscriber, CANCELLED, NO_SPEECH};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

/// After release, how long all remaining chunks together wait for Whisper
/// beyond Parakeet's results.
pub const RELEASE_BUDGET: Duration = Duration::from_millis(300);
/// While speaking, a chunk waits this long plus half its audio length.
const SPEAKING_BUDGET_FLOOR: Duration = Duration::from_millis(2500);
/// Disagreements kept per dictation for history and debugging.
const MAX_REPORTED_DISAGREEMENTS: usize = 40;

/// The two engines of one super mode session.
#[derive(Clone)]
pub struct SuperModeEngines {
    pub primary: Arc<dyn ChunkTranscriber>,
    pub secondary: Arc<dyn ChunkTranscriber>,
    pub secondary_model: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SuperModeStatus {
    /// Both engines ran and their results were merged.
    Used,
    /// Asked for, but skipped to save power or capacity.
    Shed,
    /// Asked for, but the second engine is not available.
    Unavailable,
    /// Not asked for, or turned off by the host operator.
    Off,
}

impl SuperModeStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Used => "used",
            Self::Shed => "shed",
            Self::Unavailable => "unavailable",
            Self::Off => "off",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        [Self::Used, Self::Shed, Self::Unavailable, Self::Off]
            .into_iter()
            .find(|status| status.as_str() == value)
    }
}

/// What a remote host reported about super mode, for history. A host only
/// reports it when asked, and only reports the status.
pub fn outcome_from_response(
    response: &crate::remote_transcription::RemoteTranscriptionResponse,
) -> Option<SuperModeOutcome> {
    let status = SuperModeStatus::parse(response.super_mode.as_deref()?)?;
    Some(SuperModeOutcome::new(
        status,
        response.secondary_model.clone(),
        None,
    ))
}

/// What super mode did for one dictation.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SuperModeOutcome {
    pub status: SuperModeStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secondary_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Chunks both engines were asked for.
    #[serde(default)]
    pub chunks: usize,
    /// Chunks whose Whisper result arrived in time and was merged.
    #[serde(default)]
    pub merged_chunks: usize,
    /// Chunks that kept Parakeet's text because Whisper ran out of time.
    #[serde(default)]
    pub secondary_timeouts: usize,
    /// Chunks Whisper was not asked for because the merge could not have
    /// changed them (no dictionary terms and no confidences to weigh).
    #[serde(default)]
    pub skipped_chunks: usize,
    #[serde(default)]
    pub secondary_failures: usize,
    #[serde(default)]
    pub secondary_wins: usize,
    #[serde(default)]
    pub disagreements: Vec<Disagreement>,
}

impl SuperModeOutcome {
    pub fn new(
        status: SuperModeStatus,
        secondary_model: Option<String>,
        reason: Option<&str>,
    ) -> Self {
        Self {
            status,
            secondary_model,
            reason: reason.map(str::to_string),
            chunks: 0,
            merged_chunks: 0,
            secondary_timeouts: 0,
            skipped_chunks: 0,
            secondary_failures: 0,
            secondary_wins: 0,
            disagreements: Vec::new(),
        }
    }

    /// Folds a finished session's per-chunk tally in.
    pub fn absorb(&mut self, tally: &SessionTally) {
        let counts = tally.snapshot();
        self.chunks = counts.chunks;
        self.merged_chunks = counts.merged_chunks;
        self.secondary_timeouts = counts.secondary_timeouts;
        self.skipped_chunks = counts.skipped_chunks;
        self.secondary_failures = counts.secondary_failures;
        self.secondary_wins = counts.secondary_wins;
        self.disagreements = counts.disagreements;
        if self.reason.is_none() {
            self.reason = counts.last_failure;
        }
        self.settle();
    }

    /// `Used` is claimed before a session runs; once its chunks are in, a
    /// session in which Whisper never contributed says why instead.
    fn settle(&mut self) {
        if self.status != SuperModeStatus::Used || self.merged_chunks > 0 {
            return;
        }
        if self.secondary_failures > 0 {
            // `absorb` already took the last failure as the reason.
            self.status = SuperModeStatus::Unavailable;
        } else if self.secondary_timeouts > 0 {
            self.status = SuperModeStatus::Unavailable;
            self.reason
                .get_or_insert_with(|| "Whisper did not finish in time for any chunk".to_string());
        } else if self.skipped_chunks > 0 {
            self.status = SuperModeStatus::Shed;
            self.reason.get_or_insert_with(|| {
                "nothing for Whisper to check: no dictionary terms were in play".to_string()
            });
        }
    }

    /// The session's audio was transcribed in one pass by the primary alone
    /// (a fallback), so super mode did not run whatever was planned.
    pub fn skipped_by_fallback(&mut self) {
        if self.status == SuperModeStatus::Used {
            self.status = SuperModeStatus::Unavailable;
            self.reason = Some(
                "the recording was transcribed in one pass without super mode".to_string(),
            );
        }
    }
}

/// Per-chunk counters a session's composite engine fills in.
#[derive(Default)]
pub struct SessionTally {
    inner: Mutex<TallyCounts>,
}

#[derive(Clone, Default)]
struct TallyCounts {
    chunks: usize,
    merged_chunks: usize,
    secondary_timeouts: usize,
    skipped_chunks: usize,
    secondary_failures: usize,
    secondary_wins: usize,
    disagreements: Vec<Disagreement>,
    last_failure: Option<String>,
}

impl SessionTally {
    fn update(&self, change: impl FnOnce(&mut TallyCounts)) {
        let mut counts = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        change(&mut counts);
    }

    fn snapshot(&self) -> TallyCounts {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
}

/// Whether this app should run super mode locally for the next dictation.
pub enum LocalDecision {
    Off,
    #[cfg_attr(not(feature = "whisper"), allow(dead_code))]
    Run(SttModel),
    Skip(SuperModeOutcome),
}

pub fn local_decision(settings: &Settings, models: &ModelService) -> LocalDecision {
    local_decision_with(
        settings,
        |model| models.files_present(model),
        power::on_ac_power,
    )
}

fn local_decision_with(
    settings: &Settings,
    installed: impl Fn(SttModel) -> bool,
    on_ac_power: impl FnOnce() -> bool,
) -> LocalDecision {
    if settings.super_mode == SuperModeSetting::Off
        || settings.transcription_location != TranscriptionLocation::Local
    {
        return LocalDecision::Off;
    }
    let model_id = settings.super_mode_model.model_id().to_string();
    let skip = |status, reason: &str| {
        LocalDecision::Skip(SuperModeOutcome::new(
            status,
            Some(model_id.clone()),
            Some(reason),
        ))
    };
    if settings.model.is_whisper() {
        return skip(
            SuperModeStatus::Unavailable,
            "the speech model is already Whisper; super mode pairs Parakeet with Whisper",
        );
    }
    #[cfg(not(feature = "whisper"))]
    {
        let _ = (installed, on_ac_power);
        skip(
            SuperModeStatus::Unavailable,
            "this build has no Whisper engine",
        )
    }
    #[cfg(feature = "whisper")]
    {
        let secondary = SttModel::Whisper(settings.super_mode_model);
        if !installed(settings.model) || !installed(secondary) {
            return skip(
                SuperModeStatus::Unavailable,
                "both the Parakeet model and the super mode Whisper model must be installed",
            );
        }
        if settings.super_mode == SuperModeSetting::Auto && !on_ac_power() {
            return skip(SuperModeStatus::Shed, "on battery power");
        }
        LocalDecision::Run(secondary)
    }
}

/// How many pack terms a chunk may add to the dictionary it merges with.
const PACK_TERMS_PER_CHUNK: usize = 15;

/// The composite engine of one super mode session.
pub struct EnsembleTranscriber {
    primary: Arc<dyn ChunkTranscriber>,
    jobs: Mutex<mpsc::Sender<SecondaryJob>>,
    dictionary: Vec<String>,
    /// Packs whose terms each chunk retrieves from the primary's text.
    enabled_packs: Vec<String>,
    config: MergeConfig,
    released_at: Mutex<Option<Instant>>,
    /// What is left of `RELEASE_BUDGET` for the chunks after release.
    release_budget_left: Mutex<Duration>,
    /// The session was cancelled: no chunk waits for or starts Whisper.
    cancelled: AtomicBool,
    /// The abort flag of the Whisper decode in flight, for `on_cancel`.
    current_abort: Mutex<Option<Arc<AtomicBool>>>,
    tally: Arc<SessionTally>,
    trace: Option<crate::note_debug::Trace>,
}

struct SecondaryJob {
    samples: Vec<i16>,
    sample_rate: u32,
    language: String,
    prompt: Option<String>,
    abort: Arc<AtomicBool>,
    reply: mpsc::Sender<Result<Hypothesis, String>>,
}

impl EnsembleTranscriber {
    /// The user's dictionary, then the enabled packs' terms that this chunk's
    /// primary transcript sounds like (`vocabulary_packs::retrieve_terms`):
    /// the whole pack is far too big for a Whisper prompt or a merge.
    fn chunk_dictionary(&self, primary_text: &str) -> Vec<String> {
        let mut dictionary = self.dictionary.clone();
        let retrieved = crate::vocabulary_packs::retrieve_terms(
            primary_text,
            &[],
            &self.enabled_packs,
            PACK_TERMS_PER_CHUNK,
        );
        for term in retrieved {
            if !dictionary.iter().any(|known| known.eq_ignore_ascii_case(&term)) {
                dictionary.push(term);
            }
        }
        dictionary
    }

    pub fn start(
        engines: SuperModeEngines,
        settings: &Settings,
        tally: Arc<SessionTally>,
        trace: Option<crate::note_debug::Trace>,
    ) -> Result<Self, String> {
        let (jobs, job_rx) = mpsc::channel::<SecondaryJob>();
        let secondary = engines.secondary;
        // Whisper gets its own thread so a chunk can stop waiting for it.
        // The thread ends when the session drops this engine.
        thread::Builder::new()
            .name("super-mode-secondary".into())
            .spawn(move || {
                for job in job_rx {
                    if job.abort.load(Ordering::Relaxed) {
                        continue;
                    }
                    let result = secondary.transcribe_detailed(
                        &job.samples,
                        job.sample_rate,
                        &job.language,
                        job.prompt.as_deref(),
                        Some(Arc::clone(&job.abort)),
                    );
                    let _ = job.reply.send(result);
                }
            })
            .map_err(|err| format!("Could not start the super mode engine: {err}"))?;
        Ok(Self {
            primary: engines.primary,
            jobs: Mutex::new(jobs),
            dictionary: settings.dictionary_terms(),
            enabled_packs: settings.enabled_packs.clone(),
            config: MergeConfig::default(),
            released_at: Mutex::new(None),
            release_budget_left: Mutex::new(RELEASE_BUDGET),
            cancelled: AtomicBool::new(false),
            current_abort: Mutex::new(None),
            tally,
            trace,
        })
    }

    /// Waits for Whisper's result within the budget that applies now.
    fn wait_for_secondary(
        &self,
        reply: &mpsc::Receiver<Result<Hypothesis, String>>,
        primary_done: Instant,
        audio_seconds: f32,
    ) -> Option<Result<Hypothesis, String>> {
        let speaking_budget = SPEAKING_BUDGET_FLOOR + Duration::from_secs_f32(audio_seconds * 0.5);
        // Chunks run one at a time, so the shared budget cannot change
        // under this wait.
        let budget_left = *self
            .release_budget_left
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        // When this chunk began waiting on the release budget.
        let mut waiting_since: Option<Instant> = None;
        let result = loop {
            if self.cancelled.load(Ordering::SeqCst) {
                break None;
            }
            let released = *self.released_at.lock().unwrap_or_else(|p| p.into_inner());
            let deadline = match released {
                Some(released) => {
                    *waiting_since.get_or_insert(released.max(primary_done)) + budget_left
                }
                None => primary_done + speaking_budget,
            };
            let now = Instant::now();
            if now >= deadline {
                break None;
            }
            // Wake up regularly: a release shortens the deadline.
            match reply.recv_timeout((deadline - now).min(Duration::from_millis(20))) {
                Ok(result) => break Some(result),
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => {
                    break Some(Err("The super mode engine stopped".to_string()))
                }
            }
        };
        if let Some(since) = waiting_since {
            let mut left = self
                .release_budget_left
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            *left = left.saturating_sub(since.elapsed());
        }
        result
    }
}

impl ChunkTranscriber for EnsembleTranscriber {
    fn ready(&self) -> Result<(), String> {
        self.primary.ready()
    }

    fn load_failed(&self) -> bool {
        self.primary.load_failed()
    }

    fn on_release(&self) {
        let mut released = self.released_at.lock().unwrap_or_else(|p| p.into_inner());
        released.get_or_insert_with(Instant::now);
    }

    fn on_cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
        if let Some(abort) = &*self.current_abort.lock().unwrap_or_else(|p| p.into_inner()) {
            abort.store(true, Ordering::Relaxed);
        }
    }

    fn transcribe_pcm(
        &self,
        samples: &[i16],
        sample_rate: u32,
        language: &str,
        _initial_prompt: Option<&str>,
    ) -> Result<String, String> {
        if self.cancelled.load(Ordering::SeqCst) {
            return Err(CANCELLED.to_string());
        }
        let started = Instant::now();
        // Parakeet takes no prompt; a chunk it hears no speech in is empty
        // no matter what Whisper makes of it.
        let primary =
            self.primary
                .transcribe_detailed(samples, sample_rate, language, None, None)?;
        if primary.is_empty() {
            return Err(NO_SPEECH.to_string());
        }
        let primary_done = Instant::now();
        let primary_text = primary.text();
        let dictionary = self.chunk_dictionary(&primary_text);
        // With no dictionary term to steer towards and no primary confidence
        // to weigh, every disagreement goes to the primary: Whisper's decode
        // could not change a word, so it is not run.
        if dictionary.is_empty() && primary.words.iter().all(|word| word.confidence.is_none()) {
            self.tally.update(|t| t.skipped_chunks += 1);
            self.trace_chunk(
                &primary_text,
                None,
                &primary_text,
                None,
                &None,
                started,
                primary_done,
                "skipped",
            );
            return Ok(primary_text);
        }
        let prompt = ensemble::targeted_prompt(&primary_text, &dictionary, PROMPT_TOKEN_BUDGET);

        let abort = Arc::new(AtomicBool::new(false));
        *self.current_abort.lock().unwrap_or_else(|p| p.into_inner()) = Some(Arc::clone(&abort));
        // Checked after publishing the flag, so a cancel that raced past
        // `on_cancel`'s look at it is still seen here.
        if self.cancelled.load(Ordering::SeqCst) {
            return Err(CANCELLED.to_string());
        }
        let (reply_tx, reply_rx) = mpsc::channel();
        let sent = self
            .jobs
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .send(SecondaryJob {
                samples: samples.to_vec(),
                sample_rate,
                language: language.to_string(),
                prompt: prompt.clone(),
                abort: Arc::clone(&abort),
                reply: reply_tx,
            })
            .is_ok();
        self.tally.update(|t| t.chunks += 1);
        let audio_seconds = samples.len() as f32 / sample_rate.max(1) as f32;
        let secondary = if sent {
            self.wait_for_secondary(&reply_rx, primary_done, audio_seconds)
        } else {
            Some(Err("The super mode engine stopped".to_string()))
        };

        if self.cancelled.load(Ordering::SeqCst) {
            abort.store(true, Ordering::Relaxed);
            return Err(CANCELLED.to_string());
        }
        let secondary = match secondary {
            None => {
                abort.store(true, Ordering::Relaxed);
                self.tally.update(|t| t.secondary_timeouts += 1);
                self.trace_chunk(
                    &primary_text,
                    None,
                    &primary_text,
                    None,
                    &prompt,
                    started,
                    primary_done,
                    "timeout",
                );
                return Ok(primary_text);
            }
            Some(Err(err)) if err == NO_SPEECH => Hypothesis::default(),
            Some(Err(err)) => {
                self.tally.update(|t| {
                    t.secondary_failures += 1;
                    t.last_failure = Some(err.clone());
                });
                self.trace_chunk(
                    &primary_text,
                    None,
                    &primary_text,
                    None,
                    &prompt,
                    started,
                    primary_done,
                    &err,
                );
                return Ok(primary_text);
            }
            Some(Ok(hypothesis)) => hypothesis,
        };

        let energy = speech_activity_profile(samples, sample_rate);
        let merged = ensemble::merge(
            &MergeInput {
                primary: &primary,
                secondary: &secondary,
                dictionary: &dictionary,
                energy: Some(&energy),
            },
            &self.config,
        );
        let secondary_text = secondary.text();
        self.trace_chunk(
            &primary_text,
            Some(&secondary_text),
            &merged.text,
            Some(&merged.report),
            &prompt,
            started,
            primary_done,
            "merged",
        );
        let wins = merged.report.secondary_wins();
        self.tally.update(|t| {
            t.merged_chunks += 1;
            t.secondary_wins += wins;
            let room = MAX_REPORTED_DISAGREEMENTS.saturating_sub(t.disagreements.len());
            t.disagreements
                .extend(merged.report.disagreements.into_iter().take(room));
        });
        Ok(merged.text)
    }
}

impl EnsembleTranscriber {
    #[allow(clippy::too_many_arguments)]
    fn trace_chunk(
        &self,
        primary: &str,
        secondary: Option<&str>,
        merged: &str,
        report: Option<&ensemble::MergeReport>,
        prompt: &Option<String>,
        started: Instant,
        primary_done: Instant,
        result: &str,
    ) {
        if let Some(trace) = &self.trace {
            trace.event(
                "super-mode-chunk",
                serde_json::json!({
                    "primary": primary,
                    "secondary": secondary,
                    "merged": merged,
                    "report": report,
                    "prompt": prompt,
                    "primaryMs": (primary_done - started).as_millis() as u64,
                    "totalMs": started.elapsed().as_millis() as u64,
                    "result": result,
                }),
            );
        }
    }
}

/// Whether the machine runs on mains power, checked cheaply and cached.
pub mod power {
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

    const CACHE_FOR: Duration = Duration::from_secs(30);
    static CACHE: Mutex<Option<(Instant, bool)>> = Mutex::new(None);

    /// True on AC power. A source that cannot be read counts as battery, so
    /// `auto` stays off rather than guess.
    pub fn on_ac_power() -> bool {
        let mut cache = CACHE.lock().unwrap_or_else(|p| p.into_inner());
        if let Some((at, value)) = *cache {
            if at.elapsed() < CACHE_FOR {
                return value;
            }
        }
        let value = read_power_source().unwrap_or(false);
        *cache = Some((Instant::now(), value));
        value
    }

    #[cfg(target_os = "macos")]
    fn read_power_source() -> Option<bool> {
        use core_foundation::base::{CFRelease, CFTypeRef, TCFType};
        use core_foundation::string::{CFString, CFStringRef};

        // IOKit/ps/IOPowerSources.h: what `pmset -g batt` reads.
        #[link(name = "IOKit", kind = "framework")]
        extern "C" {
            fn IOPSCopyPowerSourcesInfo() -> CFTypeRef;
            fn IOPSGetProvidingPowerSourceType(snapshot: CFTypeRef) -> CFStringRef;
        }
        // SAFETY: the snapshot is a +1 reference released below; the source
        // type string follows the get rule and is retained by the wrapper
        // before the snapshot goes.
        unsafe {
            let snapshot = IOPSCopyPowerSourcesInfo();
            if snapshot.is_null() {
                return None;
            }
            let source = IOPSGetProvidingPowerSourceType(snapshot);
            let kind =
                (!source.is_null()).then(|| CFString::wrap_under_get_rule(source).to_string());
            CFRelease(snapshot);
            kind.map(|kind| kind == "AC Power")
        }
    }

    #[cfg(target_os = "linux")]
    fn read_power_source() -> Option<bool> {
        let read = |path: std::path::PathBuf| {
            std::fs::read_to_string(path)
                .map(|value| value.trim().to_string())
                .unwrap_or_default()
        };
        let mut has_battery = false;
        let mut mains_online = false;
        for entry in std::fs::read_dir("/sys/class/power_supply").ok()?.flatten() {
            let path = entry.path();
            // A wireless mouse or headset battery (scope `Device`) does not
            // power the machine.
            if read(path.join("scope")) == "Device" {
                continue;
            }
            match read(path.join("type")).as_str() {
                "Mains" | "USB" => mains_online |= read(path.join("online")) == "1",
                "Battery" => has_battery = true,
                _ => {}
            }
        }
        // A machine without a battery is on mains by definition.
        Some(mains_online || !has_battery)
    }

    #[cfg(target_os = "windows")]
    fn read_power_source() -> Option<bool> {
        #[repr(C)]
        struct SystemPowerStatus {
            ac_line_status: u8,
            battery_flag: u8,
            battery_life_percent: u8,
            system_status_flag: u8,
            battery_life_time: u32,
            battery_full_life_time: u32,
        }
        #[link(name = "kernel32")]
        extern "system" {
            fn GetSystemPowerStatus(status: *mut SystemPowerStatus) -> i32;
        }
        let mut status = SystemPowerStatus {
            ac_line_status: 255,
            battery_flag: 255,
            battery_life_percent: 255,
            system_status_flag: 0,
            battery_life_time: 0,
            battery_full_life_time: 0,
        };
        // SAFETY: the struct matches SYSTEM_POWER_STATUS and outlives the call.
        if unsafe { GetSystemPowerStatus(&mut status) } == 0 {
            return None;
        }
        match status.ac_line_status {
            1 => Some(true),
            0 => Some(false),
            // 128: no system battery.
            _ => (status.battery_flag == 128).then_some(true),
        }
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    fn read_power_source() -> Option<bool> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    fn settings(mode: SuperModeSetting) -> Settings {
        Settings {
            super_mode: mode,
            ..Settings::default()
        }
    }

    #[test]
    fn off_and_remote_locations_never_run_locally() {
        assert!(matches!(
            local_decision_with(&settings(SuperModeSetting::Off), |_| true, || true),
            LocalDecision::Off
        ));
        let remote = Settings {
            transcription_location: TranscriptionLocation::RemoteHost,
            ..settings(SuperModeSetting::On)
        };
        assert!(matches!(
            local_decision_with(&remote, |_| true, || true),
            LocalDecision::Off
        ));
    }

    #[cfg(feature = "whisper")]
    #[test]
    fn auto_runs_only_on_ac_power_with_both_models_installed() {
        let auto = settings(SuperModeSetting::Auto);
        assert!(matches!(
            local_decision_with(&auto, |_| true, || true),
            LocalDecision::Run(_)
        ));
        match local_decision_with(&auto, |_| true, || false) {
            LocalDecision::Skip(outcome) => assert_eq!(outcome.status, SuperModeStatus::Shed),
            _ => panic!("auto on battery must shed"),
        }
        match local_decision_with(&auto, |model| !model.is_whisper(), || true) {
            LocalDecision::Skip(outcome) => {
                assert_eq!(outcome.status, SuperModeStatus::Unavailable)
            }
            _ => panic!("a missing Whisper model is unavailable"),
        }
        // On ignores the power source.
        let on = settings(SuperModeSetting::On);
        assert!(matches!(
            local_decision_with(&on, |_| true, || false),
            LocalDecision::Run(_)
        ));
        let whisper_primary = Settings {
            model: SttModel::Whisper(crate::models::WhisperModel::Base),
            ..on
        };
        assert!(matches!(
            local_decision_with(&whisper_primary, |_| true, || true),
            LocalDecision::Skip(_)
        ));
    }

    /// An engine that answers after a delay and remembers the prompts it got.
    struct FakeEngine {
        text: &'static str,
        delay: Duration,
        prompts: Arc<Mutex<Vec<Option<String>>>>,
        calls: Arc<AtomicUsize>,
    }

    impl ChunkTranscriber for FakeEngine {
        fn transcribe_pcm(
            &self,
            _: &[i16],
            _: u32,
            _: &str,
            prompt: Option<&str>,
        ) -> Result<String, String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.prompts
                .lock()
                .unwrap()
                .push(prompt.map(str::to_string));
            thread::sleep(self.delay);
            Ok(self.text.to_string())
        }
    }

    type Prompts = Arc<Mutex<Vec<Option<String>>>>;

    fn engine(text: &'static str, delay_ms: u64) -> (Arc<FakeEngine>, Prompts) {
        let prompts = Arc::new(Mutex::new(Vec::new()));
        (
            Arc::new(FakeEngine {
                text,
                delay: Duration::from_millis(delay_ms),
                prompts: Arc::clone(&prompts),
                calls: Arc::new(AtomicUsize::new(0)),
            }),
            prompts,
        )
    }

    fn ensemble_with(
        secondary_delay_ms: u64,
    ) -> (EnsembleTranscriber, Arc<SessionTally>, Prompts) {
        let (transcriber, tally, prompts, _) =
            ensemble_for(secondary_delay_ms, &["Grafana", "Kubernetes"]);
        (transcriber, tally, prompts)
    }

    fn ensemble_for(
        secondary_delay_ms: u64,
        dictionary: &[&str],
    ) -> (EnsembleTranscriber, Arc<SessionTally>, Prompts, Arc<AtomicUsize>) {
        let (primary, _) = engine("Deploy it on cooper netties, then restart.", 0);
        let (secondary, prompts) =
            engine("Deploy it on Kubernetes then restart.", secondary_delay_ms);
        let secondary_calls = Arc::clone(&secondary.calls);
        let tally = Arc::new(SessionTally::default());
        let settings = Settings {
            vocabulary_hints: dictionary.iter().map(|term| term.to_string()).collect(),
            ..Settings::default()
        };
        let transcriber = EnsembleTranscriber::start(
            SuperModeEngines {
                primary,
                secondary,
                secondary_model: "small".into(),
            },
            &settings,
            Arc::clone(&tally),
            None,
        )
        .unwrap();
        (transcriber, tally, prompts, secondary_calls)
    }

    fn speech() -> Vec<i16> {
        vec![4_000; 16_000]
    }

    #[test]
    fn a_chunk_merges_both_engines_with_a_targeted_prompt() {
        let (transcriber, tally, prompts) = ensemble_with(0);
        let text = transcriber
            .transcribe_pcm(&speech(), 16_000, "en", None)
            .unwrap();
        assert_eq!(text, "Deploy it on Kubernetes, then restart.");
        assert_eq!(
            prompts.lock().unwrap()[0].as_deref(),
            Some("Relevant names and terms: Grafana, Kubernetes.")
        );
        let mut outcome = SuperModeOutcome::new(SuperModeStatus::Used, Some("small".into()), None);
        outcome.absorb(&tally);
        assert_eq!(
            (
                outcome.chunks,
                outcome.merged_chunks,
                outcome.secondary_wins
            ),
            (1, 1, 1)
        );
        assert_eq!(outcome.disagreements[0].term.as_deref(), Some("Kubernetes"));
    }

    #[test]
    fn after_release_a_late_secondary_falls_back_to_the_primary_within_budget() {
        let (transcriber, tally, _) = ensemble_with(2_000);
        transcriber.on_release();
        let started = Instant::now();
        let text = transcriber
            .transcribe_pcm(&speech(), 16_000, "en", None)
            .unwrap();
        let waited = started.elapsed();
        assert_eq!(text, "Deploy it on cooper netties, then restart.");
        assert!(waited >= RELEASE_BUDGET, "{waited:?}");
        assert!(
            waited < RELEASE_BUDGET + Duration::from_millis(250),
            "{waited:?}"
        );
        let mut outcome = SuperModeOutcome::new(SuperModeStatus::Used, None, None);
        outcome.absorb(&tally);
        assert_eq!(outcome.secondary_timeouts, 1);
    }

    #[test]
    fn chunks_after_release_share_one_release_budget() {
        let (transcriber, tally, _) = ensemble_with(2_000);
        transcriber.on_release();
        let started = Instant::now();
        for _ in 0..3 {
            let text = transcriber
                .transcribe_pcm(&speech(), 16_000, "en", None)
                .unwrap();
            assert_eq!(text, "Deploy it on cooper netties, then restart.");
        }
        // Three late chunks wait one budget in total, not one each.
        let waited = started.elapsed();
        assert!(
            waited < RELEASE_BUDGET + Duration::from_millis(250),
            "{waited:?}"
        );
        let mut outcome = SuperModeOutcome::new(SuperModeStatus::Used, None, None);
        outcome.absorb(&tally);
        assert_eq!(outcome.secondary_timeouts, 3);
    }

    #[test]
    fn a_chunk_the_merge_cannot_change_skips_whisper() {
        let (transcriber, tally, _, secondary_calls) = ensemble_for(0, &[]);
        let text = transcriber
            .transcribe_pcm(&speech(), 16_000, "en", None)
            .unwrap();
        assert_eq!(text, "Deploy it on cooper netties, then restart.");
        // Give a wrongly dispatched job time to reach the engine.
        thread::sleep(Duration::from_millis(50));
        assert_eq!(secondary_calls.load(Ordering::SeqCst), 0);
        let mut outcome = SuperModeOutcome::new(SuperModeStatus::Used, None, None);
        outcome.absorb(&tally);
        assert_eq!((outcome.chunks, outcome.skipped_chunks), (0, 1));
    }

    #[test]
    fn a_release_while_waiting_shortens_the_wait() {
        let (transcriber, _, _) = ensemble_with(5_000);
        let transcriber = Arc::new(transcriber);
        let waiting = Arc::clone(&transcriber);
        let started = Instant::now();
        let chunk = thread::spawn(move || waiting.transcribe_pcm(&speech(), 16_000, "en", None));
        thread::sleep(Duration::from_millis(100));
        transcriber.on_release();
        let text = chunk.join().unwrap().unwrap();
        assert_eq!(text, "Deploy it on cooper netties, then restart.");
        assert!(
            started.elapsed() < Duration::from_millis(900),
            "{:?}",
            started.elapsed()
        );
    }

    #[test]
    fn a_session_whisper_never_contributed_to_is_not_reported_as_used() {
        let settled = |change: fn(&mut TallyCounts)| {
            let tally = SessionTally::default();
            tally.update(change);
            let mut outcome =
                SuperModeOutcome::new(SuperModeStatus::Used, Some("small".into()), None);
            outcome.absorb(&tally);
            outcome
        };
        let failed = settled(|t| {
            t.chunks = 2;
            t.secondary_failures = 2;
            t.last_failure = Some("Failed to load Whisper model: bad file".into());
        });
        assert_eq!(failed.status, SuperModeStatus::Unavailable);
        assert_eq!(
            failed.reason.as_deref(),
            Some("Failed to load Whisper model: bad file")
        );
        let late = settled(|t| {
            t.chunks = 1;
            t.secondary_timeouts = 1;
        });
        assert_eq!(late.status, SuperModeStatus::Unavailable);
        assert!(late.reason.is_some());
        let skipped = settled(|t| t.skipped_chunks = 3);
        assert_eq!(skipped.status, SuperModeStatus::Shed);
        // One merged chunk is enough to have used super mode.
        let used = settled(|t| {
            t.chunks = 2;
            t.merged_chunks = 1;
            t.secondary_failures = 1;
            t.last_failure = Some("decode failed".into());
        });
        assert_eq!(used.status, SuperModeStatus::Used);
        // A session with no speech at all keeps what was planned.
        assert_eq!(settled(|_| {}).status, SuperModeStatus::Used);

        let mut fallback = SuperModeOutcome::new(SuperModeStatus::Used, None, None);
        fallback.skipped_by_fallback();
        assert_eq!(fallback.status, SuperModeStatus::Unavailable);
        let mut shed = SuperModeOutcome::new(SuperModeStatus::Shed, None, Some("on battery power"));
        shed.skipped_by_fallback();
        assert_eq!(
            (shed.status, shed.reason.as_deref()),
            (SuperModeStatus::Shed, Some("on battery power"))
        );
    }

    /// A Whisper stand-in that decodes until its abort flag is raised.
    struct AbortableEngine {
        aborted: Arc<AtomicBool>,
    }

    impl ChunkTranscriber for AbortableEngine {
        fn transcribe_pcm(&self, _: &[i16], _: u32, _: &str, _: Option<&str>) -> Result<String, String> {
            unreachable!("super mode asks for the detailed transcript")
        }

        fn transcribe_detailed(
            &self,
            _: &[i16],
            _: u32,
            _: &str,
            _: Option<&str>,
            abort: Option<Arc<AtomicBool>>,
        ) -> Result<Hypothesis, String> {
            let abort = abort.expect("super mode passes an abort flag");
            let started = Instant::now();
            while started.elapsed() < Duration::from_secs(5) {
                if abort.load(Ordering::Relaxed) {
                    self.aborted.store(true, Ordering::SeqCst);
                    return Err("aborted".into());
                }
                thread::sleep(Duration::from_millis(5));
            }
            Ok(Hypothesis::from_text("too late"))
        }
    }

    #[test]
    fn cancel_stops_the_wait_and_aborts_the_whisper_decode() {
        let (primary, _) = engine("Deploy it on cooper netties, then restart.", 0);
        let aborted = Arc::new(AtomicBool::new(false));
        let tally = Arc::new(SessionTally::default());
        let settings = Settings {
            vocabulary_hints: vec!["Kubernetes".into()],
            ..Settings::default()
        };
        let transcriber = Arc::new(
            EnsembleTranscriber::start(
                SuperModeEngines {
                    primary,
                    secondary: Arc::new(AbortableEngine {
                        aborted: Arc::clone(&aborted),
                    }),
                    secondary_model: "small".into(),
                },
                &settings,
                tally,
                None,
            )
            .unwrap(),
        );
        let waiting = Arc::clone(&transcriber);
        let started = Instant::now();
        // Still speaking, so the chunk would wait seconds for Whisper.
        let chunk = thread::spawn(move || waiting.transcribe_pcm(&speech(), 16_000, "en", None));
        thread::sleep(Duration::from_millis(100));
        transcriber.on_cancel();
        assert_eq!(chunk.join().unwrap().unwrap_err(), CANCELLED);
        assert!(started.elapsed() < Duration::from_millis(600), "{:?}", started.elapsed());
        let deadline = Instant::now() + Duration::from_secs(2);
        while !aborted.load(Ordering::SeqCst) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
        }
        assert!(aborted.load(Ordering::SeqCst), "the Whisper decode was not aborted");
        // A cancelled session starts no more chunks.
        assert_eq!(
            transcriber
                .transcribe_pcm(&speech(), 16_000, "en", None)
                .unwrap_err(),
            CANCELLED
        );
    }

    #[test]
    fn outcome_serializes_for_history_and_responses() {
        let outcome = SuperModeOutcome::new(
            SuperModeStatus::Shed,
            Some("small".into()),
            Some("on battery power"),
        );
        let json = serde_json::to_value(&outcome).unwrap();
        assert_eq!(json["status"], "shed");
        assert_eq!(json["secondaryModel"], "small");
        let back: SuperModeOutcome = serde_json::from_value(json).unwrap();
        assert_eq!(back, outcome);
    }
}
