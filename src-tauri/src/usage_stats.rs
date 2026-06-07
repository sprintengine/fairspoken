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

/// One UTC day's dictation totals. Buckets are keyed by day index
/// (days since the Unix epoch) so streaks and the weekly chart need no
/// calendar library.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct DayBucket {
    words: u64,
    recording_seconds: f64,
    dictations: u64,
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
    pub total_words: u64,
    pub total_dictations: u64,
    pub total_recording_seconds: f64,
    pub time_saved_seconds: f64,
    pub speaking_wpm: u64,
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

fn summarize(data: &UsageStatsData, now_epoch_secs: u64) -> UsageStatsSummary {
    let today = now_epoch_secs / SECONDS_PER_DAY;

    let typing_seconds = data.total_words as f64 / TYPING_WPM * 60.0;
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
        total_words: data.total_words,
        total_dictations: data.total_dictations,
        total_recording_seconds: data.total_recording_seconds,
        time_saved_seconds,
        speaking_wpm,
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
        assert_eq!(service.data.days.get(&100).unwrap().words, 15);
        assert_eq!(service.data.days.get(&101).unwrap().words, 20);
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
        let mut data = UsageStatsData::default();
        data.total_words = 40;
        data.total_dictations = 1;
        data.total_recording_seconds = 20.0;

        let summary = summarize(&data, SECONDS_PER_DAY * 100);
        assert_eq!(summary.time_saved_seconds.round() as i64, 40);
        assert_eq!(summary.speaking_wpm, 120);
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
        let mut data = UsageStatsData::default();
        data.total_words = 35;
        data.total_recording_seconds = 45.0;
        data.total_dictations = 3;
        data.days.insert(
            100,
            DayBucket {
                words: 15,
                recording_seconds: 30.0,
                dictations: 2,
            },
        );
        data.days.insert(
            101,
            DayBucket {
                words: 20,
                recording_seconds: 15.0,
                dictations: 1,
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

    #[test]
    fn weekday_labels_align_with_known_epoch_day() {
        // 2021-01-01 was a Friday; that is day 18628 since the epoch.
        assert_eq!(weekday_label(18_628), "Fri");
        assert_eq!(weekday_label(0), "Thu");
    }
}
