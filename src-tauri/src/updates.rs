//! Self-update from the project's GitHub releases.
//!
//! A build follows the train its own version names: `X.Y.Z-nightly.DATE.RUN`
//! follows nightlies, anything else follows stable. Stable asks the endpoint in
//! tauri.conf.json, which GitHub redirects to the newest release not marked
//! prerelease. Nightlies are prereleases, so no fixed URL reaches the newest
//! one: a nightly build reads the releases feed, takes the first nightly tag in
//! it, and asks that release for its `nightly.json`. scripts/release verifies
//! every published release against exactly these reads.
//!
//! An update downloads in the background and waits. Restarting mid-dictation
//! would lose the recording, so it is installed only when the user asks.

use serde::Serialize;
use std::sync::Mutex;
use std::thread;
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager, Url};
use tauri_plugin_updater::{Update, UpdaterExt};

const FIRST_CHECK_DELAY: Duration = Duration::from_secs(30);
const CHECK_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);
const FEED_TIMEOUT: Duration = Duration::from_secs(20);
const DOWNLOAD_REPORT_STEP: u64 = 1024 * 1024;
const STABLE_ENDPOINT_SUFFIX: &str = "/latest/download/latest.json";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Channel {
    Stable,
    Nightly,
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

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "state", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum UpdateState {
    /// Development builds never update themselves.
    Disabled,
    Idle,
    Checking,
    UpToDate,
    Downloading { version: String, downloaded: u64, total: Option<u64> },
    Ready { version: String },
    Failed { message: String },
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateStatus {
    current_version: String,
    channel: Channel,
    #[serde(flatten)]
    state: UpdateState,
}

struct Pending {
    update: Update,
    bytes: Vec<u8>,
}

pub struct Updates {
    state: Mutex<UpdateState>,
    pending: Mutex<Option<Pending>>,
}

impl Default for Updates {
    fn default() -> Self {
        let state = if cfg!(debug_assertions) { UpdateState::Disabled } else { UpdateState::Idle };
        Self { state: Mutex::new(state), pending: Mutex::new(None) }
    }
}

fn channel(app: &AppHandle) -> Channel {
    channel_for_prerelease(app.package_info().version.pre.as_str())
}

pub fn status(app: &AppHandle) -> UpdateStatus {
    let state = app.state::<Updates>().state.lock().map(|state| state.clone()).unwrap_or(UpdateState::Idle);
    UpdateStatus {
        current_version: app.package_info().version.to_string(),
        channel: channel(app),
        state,
    }
}

fn set_state(app: &AppHandle, state: UpdateState) {
    if let Ok(mut current) = app.state::<Updates>().state.lock() {
        *current = state;
    }
    let _ = app.emit("update-status", status(app));
}

/// Checks once now and then every few hours, for the life of the app.
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

/// Looks for an update on this build's train and downloads it. Does nothing
/// while a check is running or a downloaded update is waiting for a restart.
pub async fn check(app: &AppHandle) -> Result<UpdateStatus, String> {
    let busy = {
        let updates = app.state::<Updates>();
        let Ok(mut state) = updates.state.lock() else {
            return Err("Update state is unavailable.".into());
        };
        let busy = matches!(
            *state,
            UpdateState::Disabled | UpdateState::Checking | UpdateState::Downloading { .. } | UpdateState::Ready { .. }
        );
        if !busy {
            *state = UpdateState::Checking;
        }
        busy
    };
    if busy {
        return Ok(status(app));
    }
    let _ = app.emit("update-status", status(app));
    match find_and_download(app).await {
        Ok(state) => set_state(app, state),
        Err(message) => set_state(app, UpdateState::Failed { message }),
    }
    Ok(status(app))
}

async fn find_and_download(app: &AppHandle) -> Result<UpdateState, String> {
    let Some(endpoint) = endpoint(app).await? else {
        return Ok(UpdateState::UpToDate);
    };
    let updater = app
        .updater_builder()
        .endpoints(vec![endpoint])
        .and_then(|builder| builder.build())
        .map_err(|err| format!("Could not set up the updater: {err}"))?;
    let Some(update) = updater.check().await.map_err(|err| format!("Could not check for updates: {err}"))? else {
        return Ok(UpdateState::UpToDate);
    };

    let version = update.version.clone();
    let mut downloaded = 0u64;
    let mut reported = 0u64;
    set_state(app, UpdateState::Downloading { version: version.clone(), downloaded, total: None });
    let bytes = update
        .download(
            |chunk, total| {
                downloaded += chunk as u64;
                // Chunks are a few KB; the Settings screen only needs a step
                // every megabyte to show movement.
                if downloaded - reported >= DOWNLOAD_REPORT_STEP {
                    reported = downloaded;
                    set_state(app, UpdateState::Downloading { version: version.clone(), downloaded, total });
                }
            },
            || {},
        )
        .await
        .map_err(|err| format!("Could not download {version}: {err}"))?;
    if let Ok(mut pending) = app.state::<Updates>().pending.lock() {
        *pending = Some(Pending { update, bytes });
    }
    Ok(UpdateState::Ready { version })
}

/// Installs the downloaded update and relaunches into it.
pub fn install_and_restart(app: &AppHandle) -> Result<(), String> {
    let pending = app
        .state::<Updates>()
        .pending
        .lock()
        .map_err(|_| "Update state is unavailable.".to_string())?
        .take()
        .ok_or("No update has been downloaded.")?;
    if let Err(err) = pending.update.install(&pending.bytes) {
        let message = format!("Could not install {}: {err}", pending.update.version);
        set_state(app, UpdateState::Failed { message: message.clone() });
        return Err(message);
    }
    app.restart();
}

/// The manifest this build's train reads, or None when the train has nothing
/// published yet.
async fn endpoint(app: &AppHandle) -> Result<Option<Url>, String> {
    let stable = stable_endpoint(app)?;
    let url = match channel(app) {
        Channel::Stable => stable,
        Channel::Nightly => {
            let releases = releases_base(&stable).ok_or_else(|| format!("Unexpected updater endpoint {stable}"))?;
            let feed_url = format!("{releases}.atom");
            let feed = tauri::async_runtime::spawn_blocking(move || fetch_text(&feed_url))
                .await
                .map_err(|err| err.to_string())??;
            match newest_nightly_tag(&feed) {
                Some(tag) => format!("{releases}/download/{tag}/nightly.json"),
                None => return Ok(None),
            }
        }
    };
    Url::parse(&url).map(Some).map_err(|err| format!("Bad updater URL {url}: {err}"))
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

fn fetch_text(url: &str) -> Result<String, String> {
    reqwest::blocking::Client::builder()
        .timeout(FEED_TIMEOUT)
        .build()
        .and_then(|client| client.get(url).header("accept", "application/atom+xml").send())
        .and_then(|response| response.error_for_status())
        .and_then(|response| response.text())
        .map_err(|err| format!("Could not read {url}: {err}"))
}

/// `https://github.com/o/r/releases/latest/download/latest.json` ->
/// `https://github.com/o/r/releases`.
fn releases_base(stable_endpoint: &str) -> Option<&str> {
    stable_endpoint.strip_suffix(STABLE_ENDPOINT_SUFFIX).filter(|base| base.ends_with("/releases"))
}

/// The first nightly tag in a GitHub releases feed, which lists newest first.
fn newest_nightly_tag(feed: &str) -> Option<String> {
    feed.split("/releases/tag/").skip(1).find_map(|rest| {
        let tag = &rest[..rest.find(['"', '/', '<']).unwrap_or(rest.len())];
        let pre = tag.strip_prefix('v')?.split_once('-')?.1;
        (channel_for_prerelease(pre) == Channel::Nightly).then(|| tag.to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_version_follows_the_train_its_first_prerelease_identifier_names() {
        assert_eq!(channel_for_prerelease(""), Channel::Stable);
        assert_eq!(channel_for_prerelease("nightly.20260929.5"), Channel::Nightly);
        assert_eq!(channel_for_prerelease("nightly"), Channel::Nightly);
        assert_eq!(channel_for_prerelease("nightlyish.1"), Channel::Stable);
        assert_eq!(channel_for_prerelease("beta.nightly"), Channel::Stable);
    }

    #[test]
    fn the_nightly_feed_lives_beside_the_stable_endpoint() {
        assert_eq!(
            releases_base("https://github.com/acme/multivoice/releases/latest/download/latest.json"),
            Some("https://github.com/acme/multivoice/releases")
        );
        assert_eq!(releases_base("https://example.com/latest.json"), None);
        assert_eq!(releases_base("https://example.com/latest/download/latest.json"), None);
    }

    #[test]
    fn the_newest_nightly_is_the_first_nightly_entry_in_the_feed() {
        let entry = |tag: &str| {
            format!(r#"<entry><link rel="alternate" href="https://github.com/acme/multivoice/releases/tag/{tag}"/></entry>"#)
        };
        let feed = [
            entry("v0.5.0"),
            entry("v0.5.1-beta.1"),
            entry("v0.5.1-nightly.20260929.12"),
            entry("v0.5.1-nightly.20260928.9"),
        ]
        .concat();
        assert_eq!(newest_nightly_tag(&feed).as_deref(), Some("v0.5.1-nightly.20260929.12"));
        assert_eq!(newest_nightly_tag(&entry("v0.5.0")), None);
        assert_eq!(newest_nightly_tag("<feed></feed>"), None);
    }
}
