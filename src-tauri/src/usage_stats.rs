use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::PathBuf;

const SECONDS_PER_DAY: u64 = 86_400;
// Common reference typing speed. Used only to express "time saved" against
// typing; the speaking pace shown to the user is measured, not assumed.
const TYPING_WPM: f64 = 40.0;
const WEEK_DAYS: u64 = 7;
// What the recorded audio would have cost on a metered cloud transcription
// API. $0.006/min is OpenAI's rate for both whisper-1 and gpt-4o-transcribe
// (verified June 2026). a commercial dictation app is deliberately not the anchor here: it is
// a flat $15/mo subscription with no per-minute price, so a per-minute
// comparison against it would be invented math.
const CLOUD_TRANSCRIPTION_USD_PER_MINUTE: f64 = 0.006;

// ── Zero-edit metric (backlog/zero-edit-metric.md) ──────────────────────
// A dictation counts as "edited" when a correction signal lands within
// EDIT_WINDOW of its completion: pill Undo / history Copy-original, history
// delete, a new correction/dictionary entry, or a quick re-dictation. The
// definition must stay exact so numbers remain comparable over time.
const EDIT_WINDOW_SECS: u64 = 5 * 60;
/// A new recording starting this soon after a short dictation is treated as
/// the user scrapping it and trying again.
const REDICTATION_WINDOW_SECS: u64 = 20;
const REDICTATION_MAX_CHARS: u32 = 100;
/// Below this many completed dictations in the window the rate is noise —
/// report `None` and the UI hides the line.
const MIN_DICTATIONS_FOR_RATE: u64 = 20;
/// Ring cap for per-dictation records (day buckets carry the aggregate, the
/// ring only needs to cover the edit window generously).
const RECENT_DICTATIONS_CAP: usize = 500;
const RECENT_DICTATIONS_MAX_AGE_SECS: u64 = 30 * SECONDS_PER_DAY;

/// One UTC day's dictation totals. Buckets are keyed by day index
/// (days since the Unix epoch) so streaks and the weekly chart need no
/// calendar library.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct DayBucket {
    words: u64,
    recording_seconds: f64,
    dictations: u64,
    /// Zero-edit metric counters. `completed` can differ from `dictations`
    /// (which predates the metric and skips zero-word dictations recorded
    /// through a different call).
    #[serde(default)]
    completed: u64,
    #[serde(default)]
    edited: u64,
    #[serde(default)]
    polished_completed: u64,
    #[serde(default)]
    polished_edited: u64,
}

/// One completed dictation in the recent ring — the state needed to attribute
/// a later edit signal to the right dictation and day. No transcript content,
/// only its length.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct DictationRecord {
    history_id: String,
    completed_at: u64,
    polished: bool,
    edited: bool,
    text_chars: u32,
}

/// The persisted aggregate. This is deliberately separate from the 50-item
/// transcript history, which is capped and de-duplicated and so cannot back
/// lifetime stats.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct UsageStatsData {
    total_words: u64,
    total_recording_seconds: f64,
    total_dictations: u64,
    #[serde(default)]
    days: BTreeMap<u64, DayBucket>,
    /// The user's measured typing speed, set from the speed test. When present
    /// it replaces the assumed `TYPING_WPM` in the time-saved calculation, so
    /// the headline stat reflects this user rather than a category average.
    #[serde(default)]
    measured_typing_wpm: Option<f64>,
    #[serde(default)]
    recent_dictations: Vec<DictationRecord>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WeeklyBucket {
    pub day_label: String,
    pub words: u64,
    pub is_today: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageStatsSummary {
    pub has_data: bool,
    /// Fraction of completed dictations with no correction signal within the
    /// edit window, trailing 7/30 days. `None` under 20 completed dictations.
    pub zero_edit_rate_7d: Option<f64>,
    pub zero_edit_rate_30d: Option<f64>,
    /// The polished/raw split of the 30-day rate; each `None` until its arm
    /// has 20 completed dictations.
    pub polished_zero_edit_rate_30d: Option<f64>,
    pub raw_zero_edit_rate_30d: Option<f64>,
    pub total_words: u64,
    pub total_dictations: u64,
    pub total_recording_seconds: f64,
    pub time_saved_seconds: f64,
    /// The typing speed the time-saved figure is computed against — the
    /// measured value when set, otherwise the `TYPING_WPM` reference.
    pub typing_wpm: f64,
    pub speaking_wpm: u64,
    /// What the lifetime recorded audio would have cost on a metered cloud
    /// transcription API, in US dollars.
    pub money_saved_usd: f64,
    /// The per-minute rate behind `money_saved_usd`, so the UI label always
    /// matches the math.
    pub cloud_rate_usd_per_minute: f64,
    pub current_streak: u64,
    pub best_streak: u64,
    pub this_week_words: u64,
    pub last_week_words: u64,
    pub week: Vec<WeeklyBucket>,
}

pub struct UsageStatsService {
    data: UsageStatsData,
    path: PathBuf,
}

impl Default for UsageStatsService {
    fn default() -> Self {
        let path = default_usage_stats_path();
        let data = fs::read_to_string(&path)
            .ok()
            .and_then(|raw| serde_json::from_str::<UsageStatsData>(&raw).ok())
            .unwrap_or_default();
        Self { data, path }
    }
}

impl UsageStatsService {
    /// Append one completed dictation. `at_epoch_secs` is the wall-clock time
    /// the dictation finished; passing it in keeps the method testable.
    pub fn record(
        &mut self,
        words: u64,
        recording_seconds: f32,
        at_epoch_secs: u64,
    ) -> Result<(), String> {
        if words == 0 {
            return Ok(());
        }

        let recording_seconds = f64::from(recording_seconds.max(0.0));
        self.data.total_words += words;
        self.data.total_recording_seconds += recording_seconds;
        self.data.total_dictations += 1;

        let bucket = self
            .data
            .days
            .entry(at_epoch_secs / SECONDS_PER_DAY)
            .or_default();
        bucket.words += words;
        bucket.recording_seconds += recording_seconds;
        bucket.dictations += 1;

        self.save()
    }

    pub fn summary(&self, now_epoch_secs: u64) -> UsageStatsSummary {
        summarize(&self.data, now_epoch_secs)
    }

    /// Zero-edit metric: register a completed dictation. Best-effort like the
    /// rest of the stats — callers ignore the Result.
    pub fn record_dictation_completed(
        &mut self,
        history_id: &str,
        polished: bool,
        text_chars: u32,
        at_epoch_secs: u64,
    ) -> Result<(), String> {
        self.data.recent_dictations.push(DictationRecord {
            history_id: history_id.to_string(),
            completed_at: at_epoch_secs,
            polished,
            edited: false,
            text_chars,
        });
        prune_recent(&mut self.data.recent_dictations, at_epoch_secs);

        let bucket = self
            .data
            .days
            .entry(at_epoch_secs / SECONDS_PER_DAY)
            .or_default();
        bucket.completed += 1;
        if polished {
            bucket.polished_completed += 1;
        }
        self.save()
    }

    /// Zero-edit metric: an explicit edit signal (pill Undo / Copy original /
    /// history delete) for a specific dictation. Counts only within the edit
    /// window; later signals are ignored by definition.
    pub fn mark_edited(&mut self, history_id: &str, at_epoch_secs: u64) -> Result<(), String> {
        let Some(record) = self
            .data
            .recent_dictations
            .iter_mut()
            .find(|record| record.history_id == history_id)
        else {
            return Ok(());
        };
        if record.edited || at_epoch_secs.saturating_sub(record.completed_at) > EDIT_WINDOW_SECS {
            return Ok(());
        }
        record.edited = true;
        let (completed_at, polished) = (record.completed_at, record.polished);
        self.bump_edited(completed_at, polished);
        self.save()
    }

    /// Zero-edit metric: the user just taught the app a fix (new correction
    /// or dictionary entry) — attribute it to the most recent dictation when
    /// it falls inside the edit window.
    pub fn mark_recent_edited(&mut self, at_epoch_secs: u64) -> Result<(), String> {
        let Some(record) = self
            .data
            .recent_dictations
            .iter_mut()
            .max_by_key(|record| record.completed_at)
        else {
            return Ok(());
        };
        if record.edited || at_epoch_secs.saturating_sub(record.completed_at) > EDIT_WINDOW_SECS {
            return Ok(());
        }
        record.edited = true;
        let (completed_at, polished) = (record.completed_at, record.polished);
        self.bump_edited(completed_at, polished);
        self.save()
    }

    /// Zero-edit metric: re-dictation proxy. A recording starting right after
    /// a short dictation reads as the user scrapping it and trying again.
    pub fn note_recording_started(&mut self, at_epoch_secs: u64) -> Result<(), String> {
        let Some(record) = self
            .data
            .recent_dictations
            .iter_mut()
            .max_by_key(|record| record.completed_at)
        else {
            return Ok(());
        };
        if record.edited
            || record.text_chars >= REDICTATION_MAX_CHARS
            || at_epoch_secs.saturating_sub(record.completed_at) > REDICTATION_WINDOW_SECS
        {
            return Ok(());
        }
        record.edited = true;
        let (completed_at, polished) = (record.completed_at, record.polished);
        self.bump_edited(completed_at, polished);
        self.save()
    }

    /// Increment the edited counters on the day the dictation completed (not
    /// the day of the signal), so window sums stay consistent.
    fn bump_edited(&mut self, completed_at: u64, polished: bool) {
        let bucket = self
            .data
            .days
            .entry(completed_at / SECONDS_PER_DAY)
            .or_default();
        bucket.edited += 1;
        if polished {
            bucket.polished_edited += 1;
        }
    }

    /// Persist the user's measured typing speed (from the speed test). Clamped
    /// to a sane range so a bad measurement cannot distort the time-saved stat.
    pub fn set_measured_typing_wpm(&mut self, wpm: f64) -> Result<(), String> {
        if !wpm.is_finite() || wpm <= 0.0 {
            return Err("Typing speed must be a positive number".to_string());
        }
        self.data.measured_typing_wpm = Some(wpm.clamp(5.0, 400.0));
        self.save()
    }

    fn save(&self) -> Result<(), String> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)
                .map_err(|err| format!("Failed to create usage stats directory: {err}"))?;
        }
        let payload = serde_json::to_string_pretty(&self.data)
            .map_err(|err| format!("Failed to serialize usage stats: {err}"))?;
        fs::write(&self.path, payload).map_err(|err| format!("Failed to write usage stats: {err}"))
    }
}

fn prune_recent(records: &mut Vec<DictationRecord>, now_epoch_secs: u64) {
    records.retain(|record| {
        now_epoch_secs.saturating_sub(record.completed_at) <= RECENT_DICTATIONS_MAX_AGE_SECS
    });
    if records.len() > RECENT_DICTATIONS_CAP {
        let excess = records.len() - RECENT_DICTATIONS_CAP;
        records.drain(0..excess);
    }
}

/// `1 - edited/completed` over the trailing `window_days`, or `None` below
/// the minimum sample size.
fn zero_edit_rate(
    data: &UsageStatsData,
    today: u64,
    window_days: u64,
    counts: impl Fn(&DayBucket) -> (u64, u64),
) -> Option<f64> {
    let (completed, edited) = (0..window_days)
        .map(|offset| today.saturating_sub(offset))
        .filter_map(|day| data.days.get(&day))
        .fold((0_u64, 0_u64), |(completed, edited), bucket| {
            let (c, e) = counts(bucket);
            (completed + c, edited + e)
        });
    if completed < MIN_DICTATIONS_FOR_RATE {
        return None;
    }
    Some(1.0 - edited.min(completed) as f64 / completed as f64)
}

fn summarize(data: &UsageStatsData, now_epoch_secs: u64) -> UsageStatsSummary {
    let today = now_epoch_secs / SECONDS_PER_DAY;

    let typing_wpm = data.measured_typing_wpm.unwrap_or(TYPING_WPM);
    let typing_seconds = data.total_words as f64 / typing_wpm * 60.0;
    let time_saved_seconds = (typing_seconds - data.total_recording_seconds).max(0.0);

    let speaking_wpm = if data.total_recording_seconds > 0.0 {
        (data.total_words as f64 / (data.total_recording_seconds / 60.0)).round() as u64
    } else {
        0
    };

    let day_words = |day: u64| data.days.get(&day).map(|bucket| bucket.words).unwrap_or(0);
    let day_active = |day: u64| {
        data.days
            .get(&day)
            .map(|bucket| bucket.dictations > 0)
            .unwrap_or(false)
    };

    let week: Vec<WeeklyBucket> = (0..WEEK_DAYS)
        .map(|offset| {
            let day = today.saturating_sub(WEEK_DAYS - 1 - offset);
            WeeklyBucket {
                day_label: weekday_label(day).to_string(),
                words: day_words(day),
                is_today: day == today,
            }
        })
        .collect();

    let this_week_words: u64 = (0..WEEK_DAYS)
        .map(|offset| day_words(today.saturating_sub(offset)))
        .sum();
    let last_week_words: u64 = (WEEK_DAYS..WEEK_DAYS * 2)
        .map(|offset| day_words(today.saturating_sub(offset)))
        .sum();

    UsageStatsSummary {
        has_data: data.total_dictations > 0,
        zero_edit_rate_7d: zero_edit_rate(data, today, 7, |b| (b.completed, b.edited)),
        zero_edit_rate_30d: zero_edit_rate(data, today, 30, |b| (b.completed, b.edited)),
        polished_zero_edit_rate_30d: zero_edit_rate(data, today, 30, |b| {
            (b.polished_completed, b.polished_edited)
        }),
        raw_zero_edit_rate_30d: zero_edit_rate(data, today, 30, |b| {
            (
                b.completed - b.polished_completed.min(b.completed),
                b.edited - b.polished_edited.min(b.edited),
            )
        }),
        total_words: data.total_words,
        total_dictations: data.total_dictations,
        total_recording_seconds: data.total_recording_seconds,
        time_saved_seconds,
        typing_wpm,
        speaking_wpm,
        money_saved_usd: data.total_recording_seconds / 60.0 * CLOUD_TRANSCRIPTION_USD_PER_MINUTE,
        cloud_rate_usd_per_minute: CLOUD_TRANSCRIPTION_USD_PER_MINUTE,
        current_streak: current_streak(today, day_active),
        best_streak: best_streak(&data.days),
        this_week_words,
        last_week_words,
        week,
    }
}

/// Consecutive active days ending today — or yesterday, so a streak does not
/// read as broken until a full day is genuinely missed.
fn current_streak(today: u64, day_active: impl Fn(u64) -> bool) -> u64 {
    let mut day = if day_active(today) {
        today
    } else if today > 0 && day_active(today - 1) {
        today - 1
    } else {
        return 0;
    };

    let mut streak = 0;
    while day_active(day) {
        streak += 1;
        if day == 0 {
            break;
        }
        day -= 1;
    }
    streak
}

fn best_streak(days: &BTreeMap<u64, DayBucket>) -> u64 {
    let mut best = 0;
    let mut run = 0;
    let mut previous: Option<u64> = None;
    for (&day, bucket) in days {
        if bucket.dictations == 0 {
            continue;
        }
        run = match previous {
            Some(prev) if day == prev + 1 => run + 1,
            _ => 1,
        };
        best = best.max(run);
        previous = Some(day);
    }
    best
}

fn weekday_label(day_index: u64) -> &'static str {
    // Unix epoch day 0 (1970-01-01) was a Thursday.
    const LABELS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    LABELS[((day_index + 4) % 7) as usize]
}

fn default_usage_stats_path() -> PathBuf {
    if let Some(path) = env::var_os("MULTIVOICE_TAURI_USAGE_STATS_PATH") {
        return PathBuf::from(path);
    }

    if let Some(local_app_data) = env::var_os("LOCALAPPDATA") {
        return PathBuf::from(local_app_data)
            .join("Multivoice Tauri")
            .join("usage-stats.json");
    }

    if let Some(home) = env::var_os("HOME") {
        return PathBuf::from(home)
            .join(".config")
            .join("multivoice-tauri")
            .join("usage-stats.json");
    }

    PathBuf::from("usage-stats.json")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn data_with_days(days: &[(u64, u64)]) -> UsageStatsData {
        let mut data = UsageStatsData::default();
        for &(day, words) in days {
            data.total_words += words;
            data.total_dictations += 1;
            data.days.insert(
                day,
                DayBucket {
                    words,
                    recording_seconds: 0.0,
                    dictations: 1,
                    ..Default::default()
                },
            );
        }
        data
    }

    #[test]
    fn record_accumulates_lifetime_and_daily_totals() {
        let mut service = UsageStatsService {
            data: UsageStatsData::default(),
            path: std::env::temp_dir().join("multivoice-usage-stats-test-unused.json"),
        };
        let day_secs = 100 * SECONDS_PER_DAY + 500;
        // Two dictations on the same day, one the next day.
        let _ = service.record(10, 30.0, day_secs);
        let _ = service.record(5, 15.0, day_secs + 10);
        let _ = service.record(20, 60.0, day_secs + SECONDS_PER_DAY);

        assert_eq!(service.data.total_words, 35);
        assert_eq!(service.data.total_dictations, 3);
        assert_eq!(service.data.total_recording_seconds, 105.0);
        assert_eq!(service.data.days.get(&100).unwrap().words, 15);
        assert_eq!(service.data.days.get(&100).unwrap().recording_seconds, 45.0);
        assert_eq!(service.data.days.get(&101).unwrap().words, 20);
        assert_eq!(service.data.days.get(&101).unwrap().recording_seconds, 60.0);
    }

    #[test]
    fn record_ignores_empty_dictations() {
        let mut service = UsageStatsService {
            data: UsageStatsData::default(),
            path: std::env::temp_dir().join("multivoice-usage-stats-test-unused.json"),
        };
        let _ = service.record(0, 5.0, 12_345);
        assert_eq!(service.data.total_dictations, 0);
        assert!(service.data.days.is_empty());
    }

    #[test]
    fn time_saved_compares_typing_against_actual_recording() {
        // 40 words typed at 40 wpm = 60s; dictated in 20s → 40s saved.
        let data = UsageStatsData {
            total_words: 40,
            total_dictations: 1,
            total_recording_seconds: 20.0,
            ..Default::default()
        };

        let summary = summarize(&data, SECONDS_PER_DAY * 100);
        assert_eq!(summary.time_saved_seconds.round() as i64, 40);
        assert_eq!(summary.speaking_wpm, 120);
    }

    #[test]
    fn measured_typing_wpm_replaces_the_assumed_constant_in_time_saved() {
        // 40 words dictated in 20s. At the assumed 40 wpm, typing would take 60s
        // → 40s saved. A faster measured typist (80 wpm) types them in 30s, so
        // the personalized saving is only 10s.
        let mut data = UsageStatsData {
            total_words: 40,
            total_dictations: 1,
            total_recording_seconds: 20.0,
            ..Default::default()
        };

        let default_summary = summarize(&data, SECONDS_PER_DAY * 100);
        assert_eq!(default_summary.typing_wpm, TYPING_WPM);
        assert_eq!(default_summary.time_saved_seconds.round() as i64, 40);

        data.measured_typing_wpm = Some(80.0);
        let personalized = summarize(&data, SECONDS_PER_DAY * 100);
        assert_eq!(personalized.typing_wpm, 80.0);
        assert_eq!(personalized.time_saved_seconds.round() as i64, 10);
    }

    #[test]
    fn set_measured_typing_wpm_clamps_and_rejects_bad_values() {
        let mut service = UsageStatsService {
            data: UsageStatsData::default(),
            path: std::env::temp_dir().join("multivoice-usage-stats-wpm-test.json"),
        };
        assert!(service.set_measured_typing_wpm(0.0).is_err());
        assert!(service.set_measured_typing_wpm(f64::NAN).is_err());
        service
            .set_measured_typing_wpm(1000.0)
            .expect("clamps high");
        assert_eq!(service.data.measured_typing_wpm, Some(400.0));
        let _ = std::fs::remove_file(&service.path);
    }

    #[test]
    fn money_saved_prices_recorded_audio_at_the_cloud_rate() {
        // One hour of recorded audio at $0.006/min = $0.36.
        let data = UsageStatsData {
            total_words: 9000,
            total_dictations: 1,
            total_recording_seconds: 3600.0,
            ..Default::default()
        };

        let summary = summarize(&data, SECONDS_PER_DAY * 100);
        assert!((summary.money_saved_usd - 0.36).abs() < 1e-9);
        assert_eq!(summary.cloud_rate_usd_per_minute, 0.006);
    }

    #[test]
    fn current_streak_counts_back_from_today() {
        let today = 200;
        let data = data_with_days(&[(today, 5), (today - 1, 5), (today - 2, 5), (today - 4, 5)]);
        let summary = summarize(&data, today * SECONDS_PER_DAY);
        assert_eq!(summary.current_streak, 3);
        assert_eq!(summary.best_streak, 3);
    }

    #[test]
    fn current_streak_survives_a_day_with_no_dictation_yet_today() {
        let today = 200;
        // Nothing today, but yesterday and the day before are active.
        let data = data_with_days(&[(today - 1, 5), (today - 2, 5)]);
        let summary = summarize(&data, today * SECONDS_PER_DAY);
        assert_eq!(summary.current_streak, 2);
    }

    #[test]
    fn current_streak_is_zero_when_today_and_yesterday_missed() {
        let today = 200;
        let data = data_with_days(&[(today - 3, 5), (today - 4, 5)]);
        let summary = summarize(&data, today * SECONDS_PER_DAY);
        assert_eq!(summary.current_streak, 0);
        assert_eq!(summary.best_streak, 2);
    }

    #[test]
    fn week_has_seven_days_and_marks_today() {
        let today = 200;
        let data = data_with_days(&[(today, 12), (today - 6, 3), (today - 7, 99)]);
        let summary = summarize(&data, today * SECONDS_PER_DAY);
        assert_eq!(summary.week.len(), 7);
        assert!(summary.week.last().unwrap().is_today);
        assert_eq!(summary.week.last().unwrap().words, 12);
        assert_eq!(summary.week.first().unwrap().words, 3); // today - 6
                                                            // today-7 is outside this week but inside last week.
        assert_eq!(summary.this_week_words, 15);
        assert_eq!(summary.last_week_words, 99);
    }

    #[test]
    fn data_round_trips_through_json_with_integer_day_keys() {
        // The Default impl silently falls back to zeros if this fails, so a
        // broken round-trip would silently reset a user's lifetime stats on
        // the next launch. Guard against that.
        let mut data = UsageStatsData {
            total_words: 35,
            total_recording_seconds: 45.0,
            total_dictations: 3,
            ..Default::default()
        };
        data.days.insert(
            100,
            DayBucket {
                words: 15,
                recording_seconds: 30.0,
                dictations: 2,
                ..Default::default()
            },
        );
        data.days.insert(
            101,
            DayBucket {
                words: 20,
                recording_seconds: 15.0,
                dictations: 1,
                ..Default::default()
            },
        );

        let json = serde_json::to_string(&data).expect("serialize");
        let parsed: UsageStatsData = serde_json::from_str(&json).expect("deserialize");

        assert_eq!(parsed.total_words, 35);
        assert_eq!(parsed.total_dictations, 3);
        assert_eq!(parsed.days.len(), 2);
        assert_eq!(parsed.days.get(&100).unwrap().words, 15);
        assert_eq!(parsed.days.get(&101).unwrap().dictations, 1);
    }

    fn test_service(name: &str) -> UsageStatsService {
        UsageStatsService {
            data: UsageStatsData::default(),
            path: std::env::temp_dir().join(format!("multivoice-usage-stats-{name}.json")),
        }
    }

    #[test]
    fn zero_edit_rate_needs_twenty_completed_dictations() {
        let mut service = test_service("zero-edit-min");
        let now = 200 * SECONDS_PER_DAY;
        for i in 0..19 {
            let _ = service.record_dictation_completed(&format!("t-{i}"), false, 50, now);
        }
        assert_eq!(service.summary(now).zero_edit_rate_30d, None);

        let _ = service.record_dictation_completed("t-19", false, 50, now);
        assert_eq!(service.summary(now).zero_edit_rate_30d, Some(1.0));
        let _ = std::fs::remove_file(&service.path);
    }

    #[test]
    fn edit_window_boundary_is_five_minutes() {
        let mut service = test_service("zero-edit-window");
        let now = 200 * SECONDS_PER_DAY;
        for i in 0..20 {
            let _ = service.record_dictation_completed(&format!("t-{i}"), false, 50, now);
        }

        // 4:59 after completion → counts as edited.
        let _ = service.mark_edited("t-0", now + EDIT_WINDOW_SECS - 1);
        // 5:01 after completion → outside the window, ignored.
        let _ = service.mark_edited("t-1", now + EDIT_WINDOW_SECS + 1);
        // Double-marking the same dictation counts once.
        let _ = service.mark_edited("t-0", now + 10);

        let summary = service.summary(now + EDIT_WINDOW_SECS + 2);
        assert_eq!(summary.zero_edit_rate_30d, Some(1.0 - 1.0 / 20.0));
        let _ = std::fs::remove_file(&service.path);
    }

    #[test]
    fn redictation_proxy_marks_only_quick_short_retries() {
        let mut service = test_service("zero-edit-redictate");
        let now = 200 * SECONDS_PER_DAY;
        for i in 0..19 {
            let _ = service.record_dictation_completed(&format!("t-{i}"), false, 500, now - 3600);
        }

        // Short dictation, new recording 10s later → edited.
        let _ = service.record_dictation_completed("t-short", false, 50, now);
        let _ = service.note_recording_started(now + 10);
        let summary = service.summary(now + 20);
        assert_eq!(summary.zero_edit_rate_30d, Some(1.0 - 1.0 / 20.0));

        // Long dictation followed by a quick restart → NOT edited.
        let _ = service.record_dictation_completed("t-long", false, 500, now + 100);
        let _ = service.note_recording_started(now + 110);
        // Short dictation but restart after 30s → NOT edited.
        let _ = service.record_dictation_completed("t-slow", false, 50, now + 200);
        let _ = service.note_recording_started(now + 200 + REDICTATION_WINDOW_SECS + 1);

        let summary = service.summary(now + 300);
        assert_eq!(summary.zero_edit_rate_30d, Some(1.0 - 1.0 / 22.0));
        let _ = std::fs::remove_file(&service.path);
    }

    #[test]
    fn polished_and_raw_splits_are_computed_separately() {
        let mut service = test_service("zero-edit-split");
        let now = 200 * SECONDS_PER_DAY;
        for i in 0..20 {
            let _ = service.record_dictation_completed(&format!("p-{i}"), true, 50, now);
        }
        for i in 0..20 {
            let _ = service.record_dictation_completed(&format!("r-{i}"), false, 50, now);
        }
        let _ = service.mark_edited("p-0", now + 10);
        let _ = service.mark_edited("p-1", now + 10);
        let _ = service.mark_edited("r-0", now + 10);

        let summary = service.summary(now + 20);
        assert_eq!(summary.zero_edit_rate_30d, Some(1.0 - 3.0 / 40.0));
        assert_eq!(summary.polished_zero_edit_rate_30d, Some(1.0 - 2.0 / 20.0));
        assert_eq!(summary.raw_zero_edit_rate_30d, Some(1.0 - 1.0 / 20.0));
        let _ = std::fs::remove_file(&service.path);
    }

    #[test]
    fn seven_day_window_excludes_older_dictations() {
        let mut service = test_service("zero-edit-7d");
        let now = 200 * SECONDS_PER_DAY;
        // 20 dictations 10 days ago (inside 30d, outside 7d).
        for i in 0..20 {
            let _ = service.record_dictation_completed(
                &format!("old-{i}"),
                false,
                50,
                now - 10 * SECONDS_PER_DAY,
            );
        }
        let summary = service.summary(now);
        assert_eq!(summary.zero_edit_rate_7d, None);
        assert_eq!(summary.zero_edit_rate_30d, Some(1.0));
        let _ = std::fs::remove_file(&service.path);
    }

    #[test]
    fn legacy_stats_json_without_metric_fields_loads() {
        let legacy = r#"{
            "totalWords": 100,
            "totalRecordingSeconds": 60.0,
            "totalDictations": 5,
            "days": { "100": { "words": 100, "recordingSeconds": 60.0, "dictations": 5 } }
        }"#;

        let data: UsageStatsData = serde_json::from_str(legacy).expect("legacy stats load");
        assert_eq!(data.total_words, 100);
        assert!(data.recent_dictations.is_empty());
        assert_eq!(data.days.get(&100).unwrap().completed, 0);

        let summary = summarize(&data, 100 * SECONDS_PER_DAY);
        assert_eq!(summary.zero_edit_rate_30d, None);
    }

    #[test]
    fn recent_ring_prunes_by_age_and_cap() {
        let now = 200 * SECONDS_PER_DAY;
        let mut records: Vec<DictationRecord> = (0..(RECENT_DICTATIONS_CAP + 40))
            .map(|i| DictationRecord {
                history_id: format!("t-{i}"),
                completed_at: now,
                polished: false,
                edited: false,
                text_chars: 10,
            })
            .collect();
        records.push(DictationRecord {
            history_id: "ancient".to_string(),
            completed_at: now - RECENT_DICTATIONS_MAX_AGE_SECS - 1,
            polished: false,
            edited: false,
            text_chars: 10,
        });

        prune_recent(&mut records, now);

        assert_eq!(records.len(), RECENT_DICTATIONS_CAP);
        assert!(records.iter().all(|record| record.history_id != "ancient"));
    }

    #[test]
    fn weekday_labels_align_with_known_epoch_day() {
        // 2021-01-01 was a Friday; that is day 18628 since the epoch.
        assert_eq!(weekday_label(18_628), "Fri");
        assert_eq!(weekday_label(0), "Thu");
    }
}
