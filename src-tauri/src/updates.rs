//! Self-update from the project's rolling `update-feeds` release.
//!
//! Every stable or nightly publish re-uploads `desktop-stable.json` and
//! `desktop-nightly.json` (Tauri static updater manifests) to one prerelease
//! tagged `update-feeds`. The stable feed's URL is the updater endpoint in
//! tauri.conf.json, the one place the repository is named; the nightly feed is
//! its sibling.
//!
//! The channel is the user's saved choice (`updateChannel` in
//! `update-preferences.json`) or, without one, the train this build's own
//! version names. A nightly build that is switched to stable is offered the
//! latest stable even though it sorts lower ("Switch to stable X.Y.Z").
//!
//! Checks run 30 s after launch, every six hours and on demand; a check only
//! finds an update. Nothing downloads until the user asks, and restarting is a
//! separate click because a restart mid-dictation would lose the recording.

use semver::Version;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Emitter, Manager, Url};
use tauri_plugin_updater::{Update, UpdaterExt};

const FIRST_CHECK_DELAY: Duration = Duration::from_secs(30);
const CHECK_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);
const FEED_TIMEOUT: Duration = Duration::from_secs(20);
/// Without a Content-Length, report progress every this many bytes.
const UNSIZED_REPORT_STEP: u64 = 512 * 1024;
const STABLE_FEED: &str = "desktop-stable.json";
const NIGHTLY_FEED: &str = "desktop-nightly.json";
const PREFERENCES_FILE: &str = "update-preferences.json";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Channel {
    Stable,
    Nightly,
}

impl Channel {
    fn feed(self) -> &'static str {
        match self {
            Channel::Stable => STABLE_FEED,
            Channel::Nightly => NIGHTLY_FEED,
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "stable" => Some(Channel::Stable),
            "nightly" => Some(Channel::Nightly),
            _ => None,
        }
    }
}

/// The train a version follows, read from its first prerelease identifier the
/// same way `channelForVersion` in scripts/release/release-lib.mjs does.
pub fn channel_for_prerelease(pre: &str) -> Channel {
    if pre.split('.').next() == Some("nightly") {
        Channel::Nightly
    } else {
        Channel::Stable
    }
}

pub fn channel_for_version(version: &Version) -> Channel {
    channel_for_prerelease(version.pre.as_str())
}

/// The saved choice wins; without one, the build's own version decides.
pub fn resolve_channel(saved: Option<Channel>, version: &Version) -> Channel {
    saved.unwrap_or_else(|| channel_for_version(version))
}

/// Whether a stable-channel offer would move a nightly build back to stable.
pub fn switches_to_stable(channel: Channel, current: &Version) -> bool {
    channel == Channel::Stable && channel_for_version(current) == Channel::Nightly
}

/// The updater's version comparator. Normally only a newer version is offered;
/// a nightly build on the stable channel is offered any stable that differs
/// from it, because every nightly sorts above the stable it follows.
pub fn should_offer(channel: Channel, current: &Version, remote: &Version) -> bool {
    if switches_to_stable(channel, current) {
        remote != current
    } else {
        remote > current
    }
}

/// `…/releases/download/update-feeds/desktop-stable.json` -> the same folder's
/// feed for `channel`.
pub fn feed_url(stable_endpoint: &str, channel: Channel) -> Option<String> {
    let base = stable_endpoint.strip_suffix(STABLE_FEED)?;
    base.ends_with('/').then(|| format!("{base}{}", channel.feed()))
}

/// The manifest platform key this install reads, when it differs from the
/// plugin's own `{os}-{arch}` guess. A Linux .deb install reads
/// `linux-<arch>-deb`; an AppImage (which sets `APPIMAGE` for the process it
/// launches) reads `linux-<arch>`.
pub fn update_target(os: &str, arch: &str, appimage: bool) -> Option<String> {
    (os == "linux").then(|| if appimage { format!("linux-{arch}") } else { format!("linux-{arch}-deb") })
}

fn runtime_target() -> Option<String> {
    update_target(std::env::consts::OS, std::env::consts::ARCH, std::env::var_os("APPIMAGE").is_some())
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Preferences {
    pub update_channel: Option<Channel>,
    /// Unix milliseconds of the last check that reached the feed.
    pub last_checked_at: Option<u64>,
}

impl Preferences {
    /// Unreadable or malformed values read as absent, so a store we cannot
    /// trust never moves anybody onto a channel they did not choose.
    pub fn parse(raw: &str) -> Self {
        let Ok(Value::Object(map)) = serde_json::from_str::<Value>(raw) else {
            return Self::default();
        };
        Self {
            update_channel: map.get("updateChannel").and_then(Value::as_str).and_then(Channel::parse),
            last_checked_at: map.get("lastCheckedAt").and_then(Value::as_u64),
        }
    }

    pub fn to_json(&self) -> String {
        let mut map = serde_json::Map::new();
        if let Some(channel) = self.update_channel {
            map.insert("updateChannel".into(), serde_json::to_value(channel).unwrap_or(Value::Null));
        }
        if let Some(at) = self.last_checked_at {
            map.insert("lastCheckedAt".into(), at.into());
        }
        serde_json::to_string_pretty(&Value::Object(map)).unwrap_or_else(|_| "{}".into())
    }

    fn load(path: &Path) -> Self {
        fs::read_to_string(path).map(|raw| Self::parse(&raw)).unwrap_or_default()
    }

    fn save(&self, path: &Path) -> Result<(), String> {
        crate::app_dirs::write_atomic(path, self.to_json().as_bytes())
            .map_err(|err| format!("Could not save the update channel: {err}"))
    }
}

fn preferences_path() -> PathBuf {
    crate::app_dirs::config_dir()
        .map(|dir| dir.join(PREFERENCES_FILE))
        .unwrap_or_else(|| PathBuf::from(PREFERENCES_FILE))
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "state", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum UpdateState {
    /// Development builds never update themselves.
    Disabled,
    Idle,
    Checking,
    UpToDate,
    /// A check found `version`; nothing is downloaded until the user asks.
    Available { version: String, switch_to_stable: bool },
    Downloading { version: String, switch_to_stable: bool, downloaded: u64, total: Option<u64> },
    /// Downloaded and installed (on Windows: downloaded, installed by the
    /// restart); the new version runs after a restart.
    Ready { version: String, switch_to_stable: bool },
    Failed { message: String },
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateStatus {
    current_version: String,
    /// The channel checks follow.
    channel: Channel,
    /// The channel this build's version names (the default without a choice).
    build_channel: Channel,
    last_checked_at: Option<u64>,
    #[serde(flatten)]
    state: UpdateState,
}

enum Pending {
    None,
    Found(Update),
    Downloaded(Update, Vec<u8>),
}

pub struct Updates {
    state: Mutex<UpdateState>,
    pending: Mutex<Pending>,
    preferences: Mutex<Preferences>,
    path: PathBuf,
    /// Bumped by a channel change so a check already in flight for the old
    /// channel is discarded rather than reported.
    generation: AtomicU64,
}

impl Default for Updates {
    fn default() -> Self {
        let state = if cfg!(debug_assertions) { UpdateState::Disabled } else { UpdateState::Idle };
        let path = preferences_path();
        Self {
            state: Mutex::new(state),
            pending: Mutex::new(Pending::None),
            preferences: Mutex::new(Preferences::load(&path)),
            path,
            generation: AtomicU64::new(0),
        }
    }
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

fn preferences(app: &AppHandle) -> Preferences {
    app.state::<Updates>().preferences.lock().map(|p| p.clone()).unwrap_or_default()
}

pub fn channel(app: &AppHandle) -> Channel {
    resolve_channel(preferences(app).update_channel, &app.package_info().version)
}

pub fn status(app: &AppHandle) -> UpdateStatus {
    let state = app.state::<Updates>().state.lock().map(|state| state.clone()).unwrap_or(UpdateState::Idle);
    let version = &app.package_info().version;
    let prefs = preferences(app);
    UpdateStatus {
        current_version: version.to_string(),
        channel: resolve_channel(prefs.update_channel, version),
        build_channel: channel_for_version(version),
        last_checked_at: prefs.last_checked_at,
        state,
    }
}

fn set_state(app: &AppHandle, state: UpdateState) {
    if let Ok(mut current) = app.state::<Updates>().state.lock() {
        *current = state;
    }
    emit(app);
}

fn emit(app: &AppHandle) {
    let _ = app.emit("update-status", status(app));
}

fn current_state(app: &AppHandle) -> UpdateState {
    app.state::<Updates>().state.lock().map(|s| s.clone()).unwrap_or(UpdateState::Idle)
}

/// Checks once shortly after launch and then every few hours, for the life of
/// the app.
pub fn start(app: AppHandle) {
    if cfg!(debug_assertions) {
        return;
    }
    thread::spawn(move || {
        thread::sleep(FIRST_CHECK_DELAY);
        loop {
            let _ = tauri::async_runtime::block_on(check(&app));
            thread::sleep(CHECK_INTERVAL);
        }
    });
}

/// Looks for an update on the current channel. Does nothing while a check or
/// download is running or an installed update is waiting for a restart.
pub async fn check(app: &AppHandle) -> Result<UpdateStatus, String> {
    {
        let updates = app.state::<Updates>();
        let Ok(mut state) = updates.state.lock() else {
            return Err("Update state is unavailable.".into());
        };
        if matches!(
            *state,
            UpdateState::Disabled | UpdateState::Checking | UpdateState::Downloading { .. } | UpdateState::Ready { .. }
        ) {
            drop(state);
            return Ok(status(app));
        }
        *state = UpdateState::Checking;
    }
    emit(app);
    loop {
        let generation = app.state::<Updates>().generation.load(Ordering::SeqCst);
        let channel = channel(app);
        let result = find(app, channel).await;
        let reached_feed = result.is_ok();
        {
            // Compared and published under the state lock, which set_channel
            // holds while it bumps the generation, so the old channel's result
            // can never land after a switch.
            let updates = app.state::<Updates>();
            let Ok(mut state) = updates.state.lock() else {
                return Err("Update state is unavailable.".into());
            };
            if generation != updates.generation.load(Ordering::SeqCst) {
                // The channel changed while this check ran: ask the new feed.
                continue;
            }
            *state = match result {
                Ok(found) => {
                    let next = match &found {
                        Some(update) => UpdateState::Available {
                            version: update.version.clone(),
                            switch_to_stable: switches_to_stable(channel, &app.package_info().version),
                        },
                        None => UpdateState::UpToDate,
                    };
                    if let Ok(mut pending) = updates.pending.lock() {
                        *pending = found.map_or(Pending::None, Pending::Found);
                    }
                    next
                }
                Err(message) => UpdateState::Failed { message },
            };
        }
        if reached_feed {
            record_check(app);
        }
        emit(app);
        return Ok(status(app));
    }
}

fn record_check(app: &AppHandle) {
    let updates = app.state::<Updates>();
    if let Ok(mut prefs) = updates.preferences.lock() {
        prefs.last_checked_at = Some(now_ms());
        // Best effort: a lost timestamp only shows an older "last checked".
        let _ = prefs.save(&updates.path);
    };
}

async fn find(app: &AppHandle, channel: Channel) -> Result<Option<Update>, String> {
    let stable = stable_endpoint(app)?;
    let url = feed_url(&stable, channel).ok_or_else(|| format!("Unexpected updater endpoint {stable}"))?;
    let url = Url::parse(&url).map_err(|err| format!("Bad updater URL {url}: {err}"))?;
    let mut builder = app
        .updater_builder()
        .endpoints(vec![url])
        .map_err(|err| format!("Could not set up the updater: {err}"))?
        .timeout(FEED_TIMEOUT)
        .version_comparator(move |current, remote| should_offer(channel, &current, &remote.version));
    if let Some(target) = runtime_target() {
        builder = builder.target(target);
    }
    let updater = builder.build().map_err(|err| format!("Could not set up the updater: {err}"))?;
    match updater.check().await {
        Ok(update) => Ok(update),
        // The feed is not published yet (or lists nothing for this channel).
        Err(tauri_plugin_updater::Error::ReleaseNotFound) => Ok(None),
        Err(err) => Err(format!("Could not check for updates: {err}")),
    }
}

fn stable_endpoint(app: &AppHandle) -> Result<String, String> {
    app.config()
        .plugins
        .0
        .get("updater")
        .and_then(|updater| updater.get("endpoints"))
        .and_then(|endpoints| endpoints.get(0))
        .and_then(|endpoint| endpoint.as_str())
        .map(str::to_string)
        .ok_or_else(|| "No updater endpoint is configured.".to_string())
}

/// Saves the channel choice and checks the new channel right away.
pub fn set_channel(app: &AppHandle, next: Channel) -> Result<UpdateStatus, String> {
    let updates = app.state::<Updates>();
    {
        let mut prefs = updates.preferences.lock().map_err(|_| "Update state is unavailable.".to_string())?;
        let mut saved = prefs.clone();
        saved.update_channel = Some(next);
        saved.save(&updates.path)?;
        *prefs = saved;
    }
    // An offer from the other channel no longer applies. The generation moves
    // under the state lock: a check finishing now either sees the switch or
    // has already published and is reset here.
    let state = updates.state.lock();
    updates.generation.fetch_add(1, Ordering::SeqCst);
    if let Ok(mut state) = state {
        if matches!(*state, UpdateState::Available { .. } | UpdateState::UpToDate | UpdateState::Failed { .. }) {
            *state = UpdateState::Idle;
            if let Ok(mut pending) = updates.pending.lock() {
                if matches!(*pending, Pending::Found(_)) {
                    *pending = Pending::None;
                }
            }
        }
    }
    emit(app);
    let handle = app.clone();
    tauri::async_runtime::spawn(async move {
        let _ = check(&handle).await;
    });
    Ok(status(app))
}

/// Downloads the offered update and installs it, reporting progress through
/// `update-status`. Ends in `ready`: the new version runs after a restart.
pub async fn download_and_install(app: &AppHandle) -> Result<UpdateStatus, String> {
    let (update, switch_to_stable) = {
        let updates = app.state::<Updates>();
        let mut state = updates.state.lock().map_err(|_| "Update state is unavailable.".to_string())?;
        let UpdateState::Available { switch_to_stable, .. } = *state else {
            drop(state);
            return Ok(status(app));
        };
        let mut pending = updates.pending.lock().map_err(|_| "Update state is unavailable.".to_string())?;
        let Pending::Found(update) = std::mem::replace(&mut *pending, Pending::None) else {
            return Err("No update has been found.".into());
        };
        *state = UpdateState::Downloading {
            version: update.version.clone(),
            switch_to_stable,
            downloaded: 0,
            total: None,
        };
        (update, switch_to_stable)
    };
    emit(app);

    let version = update.version.clone();
    let mut downloaded = 0u64;
    let mut reported = 0u64;
    let bytes = update
        .download(
            |chunk, total| {
                downloaded += chunk as u64;
                if progress_step_due(reported, downloaded, total) {
                    reported = downloaded;
                    set_state(
                        app,
                        UpdateState::Downloading { version: version.clone(), switch_to_stable, downloaded, total },
                    );
                }
            },
            || {},
        )
        .await;
    let bytes = match bytes {
        Ok(bytes) => bytes,
        Err(err) => {
            set_state(app, UpdateState::Failed { message: format!("Could not download {version}: {err}") });
            return Ok(status(app));
        }
    };

    if let Err(message) = install(app, update, bytes).await {
        set_state(app, UpdateState::Failed { message });
        return Ok(status(app));
    }
    set_state(app, UpdateState::Ready { version, switch_to_stable });
    Ok(status(app))
}

/// Progress events move the ring in whole percents; without a size, every
/// half megabyte.
fn progress_step_due(reported: u64, downloaded: u64, total: Option<u64>) -> bool {
    match total {
        Some(total) if total > 0 => downloaded * 100 / total > reported * 100 / total,
        _ => downloaded - reported >= UNSIZED_REPORT_STEP,
    }
}

/// macOS and Linux install now (replacing the bundle on disk; a .deb asks for
/// an administrator through pkexec) and the restart only relaunches. The
/// Windows installer exits the app as it runs, so there the bytes wait and the
/// restart installs them.
async fn install(app: &AppHandle, update: Update, bytes: Vec<u8>) -> Result<(), String> {
    if cfg!(windows) {
        if let Ok(mut pending) = app.state::<Updates>().pending.lock() {
            *pending = Pending::Downloaded(update, bytes);
        }
        return Ok(());
    }
    let version = update.version.clone();
    tauri::async_runtime::spawn_blocking(move || update.install(&bytes))
        .await
        .map_err(|err| err.to_string())?
        .map_err(|err| format!("Could not install {version}: {err}"))
}

/// Restarts into the installed update.
pub fn restart(app: &AppHandle) -> Result<(), String> {
    if !matches!(current_state(app), UpdateState::Ready { .. }) {
        return Err("No update is ready to install.".into());
    }
    let pending = app
        .state::<Updates>()
        .pending
        .lock()
        .map(|mut pending| std::mem::replace(&mut *pending, Pending::None))
        .map_err(|_| "Update state is unavailable.".to_string())?;
    if let Pending::Downloaded(update, bytes) = pending {
        if let Err(err) = update.install(&bytes) {
            let message = format!("Could not install {}: {err}", update.version);
            set_state(app, UpdateState::Failed { message: message.clone() });
            return Err(message);
        }
    }
    app.restart();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(version: &str) -> Version {
        Version::parse(version).unwrap()
    }

    #[test]
    fn a_version_follows_the_train_its_first_prerelease_identifier_names() {
        assert_eq!(channel_for_prerelease(""), Channel::Stable);
        assert_eq!(channel_for_prerelease("nightly.20260929.5"), Channel::Nightly);
        assert_eq!(channel_for_prerelease("nightly"), Channel::Nightly);
        assert_eq!(channel_for_prerelease("nightlyish.1"), Channel::Stable);
        assert_eq!(channel_for_prerelease("beta.nightly"), Channel::Stable);
    }

    #[test]
    fn a_saved_channel_wins_and_without_one_the_version_decides() {
        let stable = v("0.3.0");
        let nightly = v("0.4.0-nightly.20261005.4");
        assert_eq!(resolve_channel(None, &stable), Channel::Stable);
        assert_eq!(resolve_channel(None, &nightly), Channel::Nightly);
        assert_eq!(resolve_channel(Some(Channel::Nightly), &stable), Channel::Nightly);
        assert_eq!(resolve_channel(Some(Channel::Stable), &nightly), Channel::Stable);
    }

    #[test]
    fn only_newer_versions_are_offered_on_the_builds_own_channel() {
        assert!(should_offer(Channel::Stable, &v("0.3.0"), &v("0.3.1")));
        assert!(!should_offer(Channel::Stable, &v("0.3.0"), &v("0.3.0")));
        assert!(!should_offer(Channel::Stable, &v("0.3.1"), &v("0.3.0")));
        assert!(should_offer(Channel::Nightly, &v("0.4.0-nightly.20261005.4"), &v("0.4.0-nightly.20261005.10")));
        assert!(should_offer(Channel::Nightly, &v("0.4.0-nightly.20261005.4"), &v("0.4.0-nightly.20261006.1")));
        assert!(!should_offer(Channel::Nightly, &v("0.4.0-nightly.20261005.4"), &v("0.4.0-nightly.20261005.4")));
        assert!(!should_offer(Channel::Nightly, &v("0.4.0-nightly.20261005.4"), &v("0.4.0-nightly.20261004.9")));
    }

    #[test]
    fn stable_to_nightly_is_an_upgrade_because_nightlies_use_the_next_base_version() {
        assert!(!switches_to_stable(Channel::Nightly, &v("0.3.0")));
        assert!(should_offer(Channel::Nightly, &v("0.3.0"), &v("0.3.1-nightly.20261005.1")));
    }

    #[test]
    fn a_nightly_on_the_stable_channel_is_offered_any_other_stable_even_a_lower_one() {
        let nightly = v("0.4.0-nightly.20261005.4");
        assert!(switches_to_stable(Channel::Stable, &nightly));
        assert!(!switches_to_stable(Channel::Nightly, &nightly));
        assert!(should_offer(Channel::Stable, &nightly, &v("0.3.0")));
        assert!(should_offer(Channel::Stable, &nightly, &v("0.4.0")));
        assert!(!should_offer(Channel::Stable, &nightly, &nightly));
    }

    #[test]
    fn the_nightly_feed_sits_beside_the_stable_feed() {
        let stable = "https://github.com/sprintengine/fairspoken/releases/download/update-feeds/desktop-stable.json";
        assert_eq!(feed_url(stable, Channel::Stable).as_deref(), Some(stable));
        assert_eq!(
            feed_url(stable, Channel::Nightly).as_deref(),
            Some("https://github.com/sprintengine/fairspoken/releases/download/update-feeds/desktop-nightly.json")
        );
        assert_eq!(feed_url("https://example.com/latest.json", Channel::Nightly), None);
        assert_eq!(feed_url("desktop-stable.json", Channel::Nightly), None);
    }

    #[test]
    fn the_endpoint_in_tauri_conf_is_the_stable_feed_on_the_update_feeds_release() {
        let config: Value = serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
        let endpoint = config["plugins"]["updater"]["endpoints"][0].as_str().unwrap();
        assert_eq!(
            endpoint,
            "https://github.com/sprintengine/fairspoken/releases/download/update-feeds/desktop-stable.json"
        );
    }

    #[test]
    fn linux_deb_installs_read_the_deb_platform_key() {
        assert_eq!(update_target("linux", "x86_64", false).as_deref(), Some("linux-x86_64-deb"));
        assert_eq!(update_target("linux", "x86_64", true).as_deref(), Some("linux-x86_64"));
        assert_eq!(update_target("windows", "x86_64", false), None);
        assert_eq!(update_target("macos", "aarch64", false), None);
    }

    #[test]
    fn preferences_round_trip_and_bad_values_read_as_absent() {
        let prefs = Preferences { update_channel: Some(Channel::Nightly), last_checked_at: Some(1_790_000_000_000) };
        assert_eq!(Preferences::parse(&prefs.to_json()), prefs);
        assert_eq!(Preferences::parse("{}"), Preferences::default());
        assert_eq!(Preferences::parse("not json"), Preferences::default());
        assert_eq!(Preferences::parse("[]"), Preferences::default());
        let bad = Preferences::parse(r#"{"updateChannel":"beta","lastCheckedAt":12}"#);
        assert_eq!(bad, Preferences { update_channel: None, last_checked_at: Some(12) });
        assert!(!Preferences::default().to_json().contains("updateChannel"));
    }

    #[test]
    fn preferences_save_to_disk() {
        let dir = std::env::temp_dir().join(format!("fairspoken-updates-{}", uuid::Uuid::new_v4()));
        let path = dir.join(PREFERENCES_FILE);
        assert_eq!(Preferences::load(&path), Preferences::default());
        let prefs = Preferences { update_channel: Some(Channel::Stable), last_checked_at: None };
        prefs.save(&path).unwrap();
        assert_eq!(Preferences::load(&path), prefs);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn progress_is_reported_per_percent_or_per_half_megabyte() {
        assert!(!progress_step_due(0, 5, Some(1000)));
        assert!(progress_step_due(0, 10, Some(1000)));
        assert!(!progress_step_due(10, 19, Some(1000)));
        assert!(progress_step_due(10, 20, Some(1000)));
        assert!(!progress_step_due(0, UNSIZED_REPORT_STEP - 1, None));
        assert!(progress_step_due(0, UNSIZED_REPORT_STEP, None));
        assert!(progress_step_due(0, UNSIZED_REPORT_STEP, Some(0)));
    }
}
