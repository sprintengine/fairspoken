//! Self-update for the standalone transcription host.
//!
//! The host reads `host-<channel>.json` from the rolling `update-feeds`
//! release (see the update contract, §5), picks the archive for the running
//! OS/arch, and offers it when it is newer — or, on the stable channel while
//! running a nightly, offers the latest stable as "Switch to stable".
//!
//! Installing never leaves a half-replaced binary:
//!
//! 1. the archive is downloaded to a temp file and its sha256 *and* minisign
//!    signature (the desktop updater's key, embedded below) are checked;
//! 2. the host binary is extracted next to the running executable and must
//!    answer `--version` with the expected version before anything changes;
//! 3. the running executable is copied to `<name>.previous`, then the new one
//!    is renamed over it (atomic on Unix; `self-replace` on Windows);
//! 4. the installed binary is smoke-checked again, and `<name>.previous` is
//!    restored if it fails.
//!
//! Restarting is separate: under launchd/systemd the host exits with
//! [`RESTART_EXIT_CODE`] and the service manager starts the new binary;
//! otherwise it re-executes itself with the same arguments.
//!
//! Automatic checks only record what is available. Nothing is installed
//! unattended unless the operator turned on `autoUpdate`.

use super::config::{persist_live_config, HostLiveConfig};
use base64::Engine;
use flate2::read::GzDecoder;
use minisign_verify::{PublicKey, Signature};
use semver::Version;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::env;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

/// Where every update feed lives (contract §2).
pub(super) const DEFAULT_FEED_BASE_URL: &str =
    "https://github.com/sprintengine/fairspoken/releases/download/update-feeds/";
/// The minisign public key the release archives are signed with: the same
/// value as `plugins.updater.pubkey` in `src-tauri/tauri.conf.json` (base64 of
/// a minisign public key file). A unit test keeps the two in step.
pub(super) const UPDATER_PUBLIC_KEY: &str = "dW50cnVzdGVkIGNvbW1lbnQ6IG1pbmlzaWduIHB1YmxpYyBrZXk6IDUxNkI3NkVCNzUwRkEzRUUKUldUdW93OTE2M1pyVVpsUzFNSzFTOUQwZVdsWUZyS0IzTVVSNU12YWFxdWVHM2ZNTTVNYklVdlUK";

/// Overrides the saved/derived channel (operator pinning, e.g. in a unit file).
pub(super) const CHANNEL_ENV: &str = "FAIRSPOKEN_HOST_UPDATE_CHANNEL";
/// Overrides the feed base URL (mirrors, tests).
pub(super) const FEED_URL_ENV: &str = "FAIRSPOKEN_HOST_UPDATE_FEED_URL";
/// `0` turns off the automatic checks (air-gapped hosts). Manual checks and
/// the CLI still work.
pub(super) const CHECKS_ENV: &str = "FAIRSPOKEN_HOST_UPDATE_CHECKS";
/// Set by the packaged launchd plist and systemd unit so the host knows a
/// service manager will start it again after it exits for an update.
pub(super) const SERVICE_ENV: &str = "FAIRSPOKEN_HOST_SERVICE";
/// Set on a re-executed host so it waits for the old listener to go away.
pub(super) const RESTARTED_ENV: &str = "FAIRSPOKEN_HOST_UPDATE_RESTARTED";

pub(super) const FIRST_CHECK_DELAY: Duration = Duration::from_secs(30);
pub(super) const CHECK_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);
/// Exit status after installing an update under a service manager
/// (`EX_TEMPFAIL`). The packaged unit/plist restart on it.
pub(super) const RESTART_EXIT_CODE: i32 = 75;
/// `--check-update` exit status when an update is available.
pub(super) const UPDATE_AVAILABLE_EXIT_CODE: i32 = 10;

/// The host binary's file name inside every release archive.
pub(super) const HOST_BINARY_NAME: &str = if cfg!(windows) {
    "transcription-host.exe"
} else {
    "transcription-host"
};

const MAX_MANIFEST_BYTES: u64 = 256 * 1024;
const MAX_ARCHIVE_BYTES: u64 = 1024 * 1024 * 1024;
const MAX_BINARY_BYTES: u64 = 1024 * 1024 * 1024;
const MANIFEST_TIMEOUT: Duration = Duration::from_secs(30);
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const SMOKE_TIMEOUT: Duration = Duration::from_secs(30);
/// How long a manual restart waits for in-flight transcriptions to finish.
const MANUAL_RESTART_DRAIN: Duration = Duration::from_secs(60);

// ───────────────────────────── Channels ─────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(super) enum UpdateChannel {
    Stable,
    Nightly,
}

impl UpdateChannel {
    pub(super) fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "stable" => Some(Self::Stable),
            "nightly" => Some(Self::Nightly),
            _ => None,
        }
    }

    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Stable => "stable",
            Self::Nightly => "nightly",
        }
    }

    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Stable => "Stable",
            Self::Nightly => "Nightly",
        }
    }

    /// A build's default channel comes from its own version (contract §1).
    pub(super) fn for_version(version: &str) -> Self {
        if is_nightly_version(version) {
            Self::Nightly
        } else {
            Self::Stable
        }
    }
}

pub(super) fn is_nightly_version(version: &str) -> bool {
    version.contains("-nightly.")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(super) enum ChannelSource {
    /// `FAIRSPOKEN_HOST_UPDATE_CHANNEL`.
    Env,
    /// The operator's saved choice (`updateChannel` in the config file).
    Saved,
    /// Neither: derived from the running version.
    Version,
}

/// Env override, then the saved choice, then the running version.
pub(super) fn resolve_channel(
    env_value: Option<&str>,
    saved: Option<UpdateChannel>,
    version: &str,
) -> Result<(UpdateChannel, ChannelSource), String> {
    if let Some(raw) = env_value.map(str::trim).filter(|value| !value.is_empty()) {
        return UpdateChannel::parse(raw)
            .map(|channel| (channel, ChannelSource::Env))
            .ok_or_else(|| format!("{CHANNEL_ENV} must be stable or nightly, not {raw}"));
    }
    if let Some(channel) = saved {
        return Ok((channel, ChannelSource::Saved));
    }
    Ok((UpdateChannel::for_version(version), ChannelSource::Version))
}

/// Update preferences persisted in the host config file next to the
/// dashboard configuration.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct UpdatePrefs {
    /// Absent = follow the running version's channel.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) update_channel: Option<UpdateChannel>,
    /// Install updates found by automatic checks and restart when idle.
    #[serde(default)]
    pub(super) auto_update: bool,
}

// ───────────────────────────── Platforms ─────────────────────────────

/// The manifest's platform key for an OS/arch pair (`std::env::consts`).
pub(super) fn platform_key(os: &str, arch: &str) -> Option<&'static str> {
    match (os, arch) {
        ("linux", "x86_64") => Some("linux-x86_64"),
        ("linux", "aarch64") => Some("linux-aarch64"),
        ("windows", "x86_64") => Some("windows-x86_64"),
        ("macos", "x86_64") => Some("darwin-x86_64"),
        ("macos", "aarch64") => Some("darwin-aarch64"),
        _ => None,
    }
}

pub(super) fn current_platform_key() -> Option<&'static str> {
    platform_key(env::consts::OS, env::consts::ARCH)
}

// ───────────────────────────── Manifest ─────────────────────────────

#[derive(Clone, Debug, Deserialize)]
pub(super) struct HostManifest {
    pub(super) version: String,
    #[serde(default)]
    pub(super) channel: Option<String>,
    #[serde(default)]
    pub(super) pub_date: Option<String>,
    #[serde(default)]
    pub(super) notes: Option<String>,
    pub(super) platforms: BTreeMap<String, PlatformAsset>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub(super) struct PlatformAsset {
    pub(super) url: String,
    pub(super) sha256: String,
    pub(super) signature: String,
    #[serde(default)]
    pub(super) format: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ArchiveFormat {
    TarGz,
    Zip,
}

impl PlatformAsset {
    pub(super) fn archive_format(&self) -> Result<ArchiveFormat, String> {
        match self.format.as_deref().map(str::trim) {
            Some("tar.gz") | Some("tgz") => Ok(ArchiveFormat::TarGz),
            Some("zip") => Ok(ArchiveFormat::Zip),
            Some(other) => Err(format!("Unsupported update archive format: {other}")),
            None => {
                let path = self.url.split(['?', '#']).next().unwrap_or("");
                if path.ends_with(".tar.gz") || path.ends_with(".tgz") {
                    Ok(ArchiveFormat::TarGz)
                } else if path.ends_with(".zip") {
                    Ok(ArchiveFormat::Zip)
                } else {
                    Err(format!("Cannot tell the archive format of {}", self.url))
                }
            }
        }
    }

    fn validate(&self, key: &str) -> Result<(), String> {
        if !(self.url.starts_with("https://") || self.url.starts_with("http://")) {
            return Err(format!("The {key} update URL is not http(s): {}", self.url));
        }
        let sha = self.sha256.trim();
        if sha.len() != 64 || !sha.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(format!("The {key} update has an invalid sha256"));
        }
        if self.signature.trim().is_empty() {
            return Err(format!("The {key} update has no signature"));
        }
        self.archive_format().map(|_| ())
    }
}

impl HostManifest {
    pub(super) fn asset_for(&self, key: &str) -> Result<&PlatformAsset, String> {
        let asset = self.platforms.get(key).ok_or_else(|| {
            format!(
                "Fairspoken host {} has no {key} build in the update feed",
                self.version
            )
        })?;
        asset.validate(key)?;
        Ok(asset)
    }
}

/// Parses a `host-<channel>.json` body and checks it belongs to the channel
/// it was fetched for, so a misplaced feed can never move a host across
/// channels.
pub(super) fn parse_manifest(bytes: &[u8], channel: UpdateChannel) -> Result<HostManifest, String> {
    let mut manifest: HostManifest = serde_json::from_slice(bytes)
        .map_err(|err| format!("Invalid {} host update feed: {err}", channel.as_str()))?;
    manifest.version = manifest.version.trim().trim_start_matches('v').to_string();
    parse_version(&manifest.version)?;
    if let Some(listed) = manifest.channel.as_deref() {
        if UpdateChannel::parse(listed) != Some(channel) {
            return Err(format!(
                "The {} host update feed says it is the {listed} channel",
                channel.as_str()
            ));
        }
    }
    if UpdateChannel::for_version(&manifest.version) != channel {
        return Err(format!(
            "The {} host update feed lists {}, which is not a {} version",
            channel.as_str(),
            manifest.version,
            channel.as_str()
        ));
    }
    Ok(manifest)
}

fn parse_version(version: &str) -> Result<Version, String> {
    Version::parse(version.trim().trim_start_matches('v'))
        .map_err(|err| format!("Invalid version {version}: {err}"))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Offer {
    UpToDate,
    Upgrade,
    /// The channel is stable but this build is a nightly: the latest stable
    /// is offered even though it sorts lower (contract §4).
    SwitchToStable,
}

pub(super) fn evaluate_offer(
    current: &str,
    remote: &str,
    channel: UpdateChannel,
) -> Result<Offer, String> {
    let current_version = parse_version(current)?;
    let remote_version = parse_version(remote)?;
    let order = remote_version.cmp_precedence(&current_version);
    if order == Ordering::Greater {
        return Ok(Offer::Upgrade);
    }
    if channel == UpdateChannel::Stable
        && is_nightly_version(current)
        && !is_nightly_version(remote)
        && order != Ordering::Equal
    {
        return Ok(Offer::SwitchToStable);
    }
    Ok(Offer::UpToDate)
}

// ───────────────────────────── Verification ─────────────────────────────

fn decode_base64_text(raw: &str, what: &str) -> Result<String, String> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(raw.trim())
        .map_err(|err| format!("The {what} is not valid base64: {err}"))?;
    String::from_utf8(bytes).map_err(|_| format!("The {what} is not text"))
}

pub(super) fn decode_public_key(base64_key: &str) -> Result<PublicKey, String> {
    let text = decode_base64_text(base64_key, "update public key")?;
    PublicKey::decode(&text).map_err(|err| format!("Invalid update public key: {err}"))
}

pub(super) fn decode_signature(base64_signature: &str) -> Result<Signature, String> {
    let text = decode_base64_text(base64_signature, "update signature")?;
    Signature::decode(&text).map_err(|err| format!("Invalid update signature: {err}"))
}

pub(super) fn hex_digest(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Checks the archive's sha256 and its minisign signature in one pass.
/// Both must match before anything is extracted.
pub(super) fn verify_archive_file(
    archive: &Path,
    expected_sha256: &str,
    signature_base64: &str,
    public_key_base64: &str,
) -> Result<(), String> {
    let public_key = decode_public_key(public_key_base64)?;
    let signature = decode_signature(signature_base64)?;
    let mut verifier = public_key.verify_stream(&signature).map_err(|err| {
        format!("The update was not signed with the Fairspoken release key: {err}")
    })?;
    let mut file = File::open(archive)
        .map_err(|err| format!("Failed to open the downloaded update: {err}"))?;
    let mut hasher = Sha256::new();
    let mut chunk = vec![0u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut chunk)
            .map_err(|err| format!("Failed to read the downloaded update: {err}"))?;
        if read == 0 {
            break;
        }
        hasher.update(&chunk[..read]);
        verifier.update(&chunk[..read]);
    }
    let actual = hex_digest(hasher.finalize().as_slice());
    if !actual.eq_ignore_ascii_case(expected_sha256.trim()) {
        return Err(format!(
            "The downloaded update's sha256 is {actual}, but the feed lists {}",
            expected_sha256.trim()
        ));
    }
    verifier.finalize().map_err(|_| {
        "The downloaded update's signature does not verify against the Fairspoken release key"
            .to_string()
    })
}

// ───────────────────────────── Extraction ─────────────────────────────

/// The host binary is found by file name anywhere in the archive: release.yml
/// packages `<stem>/<binary>`, but the Windows zip comes from PowerShell's
/// Compress-Archive, so no exact entry path is relied on (either separator).
/// The first match wins; entries climbing out with `..` are ignored.
fn is_binary_entry(entry_path: &str, binary_name: &str) -> bool {
    let parts: Vec<&str> = entry_path
        .split(['/', '\\'])
        .filter(|part| !part.is_empty() && *part != ".")
        .collect();
    !parts.contains(&"..") && parts.last() == Some(&binary_name)
}

fn copy_limited(reader: &mut impl Read, dest: &Path) -> Result<(), String> {
    let mut out =
        File::create(dest).map_err(|err| format!("Failed to write {}: {err}", dest.display()))?;
    let copied = io::copy(&mut reader.take(MAX_BINARY_BYTES + 1), &mut out)
        .map_err(|err| format!("Failed to extract the host binary: {err}"))?;
    if copied > MAX_BINARY_BYTES {
        return Err("The host binary in the update is implausibly large".to_string());
    }
    out.sync_all()
        .map_err(|err| format!("Failed to write {}: {err}", dest.display()))
}

pub(super) fn extract_host_binary(
    archive: &Path,
    format: ArchiveFormat,
    binary_name: &str,
    dest: &Path,
) -> Result<(), String> {
    let file = File::open(archive)
        .map_err(|err| format!("Failed to open the downloaded update: {err}"))?;
    match format {
        ArchiveFormat::TarGz => {
            let mut tar = tar::Archive::new(GzDecoder::new(file));
            let entries = tar
                .entries()
                .map_err(|err| format!("The update archive is not a tar.gz: {err}"))?;
            for entry in entries {
                let mut entry =
                    entry.map_err(|err| format!("The update archive is damaged: {err}"))?;
                if !entry.header().entry_type().is_file() {
                    continue;
                }
                let path = entry
                    .path()
                    .map_err(|err| format!("The update archive is damaged: {err}"))?
                    .to_string_lossy()
                    .into_owned();
                if is_binary_entry(&path, binary_name) {
                    return copy_limited(&mut entry, dest);
                }
            }
        }
        ArchiveFormat::Zip => {
            let mut zip = zip::ZipArchive::new(file)
                .map_err(|err| format!("The update archive is not a zip: {err}"))?;
            for index in 0..zip.len() {
                let mut entry = zip
                    .by_index(index)
                    .map_err(|err| format!("The update archive is damaged: {err}"))?;
                if !entry.is_file() {
                    continue;
                }
                let path = entry.name().to_string();
                if is_binary_entry(&path, binary_name) {
                    return copy_limited(&mut entry, dest);
                }
            }
        }
    }
    Err(format!("The update archive has no {binary_name}"))
}

// ───────────────────────────── Install ─────────────────────────────

fn sibling(target: &Path, name: String) -> PathBuf {
    target.with_file_name(name)
}

fn file_name(target: &Path) -> String {
    target
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| HOST_BINARY_NAME.to_string())
}

/// The new binary is staged in the target's own directory so the final
/// rename never crosses filesystems. It keeps `.exe` so Windows can run it.
pub(super) fn staged_path(target: &Path) -> PathBuf {
    let name = file_name(target);
    match name.strip_suffix(".exe") {
        Some(stem) => sibling(target, format!(".{stem}.update.exe")),
        None => sibling(target, format!(".{name}.update")),
    }
}

/// The copy of the replaced binary kept for rollback.
pub(super) fn previous_path(target: &Path) -> PathBuf {
    sibling(target, format!("{}.previous", file_name(target)))
}

fn make_executable(path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o755))
            .map_err(|err| format!("Failed to mark {} executable: {err}", path.display()))?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

/// Runs `<binary> --version` and requires the expected version in its output.
pub(super) fn smoke_check(binary: &Path, expected_version: &str) -> Result<(), String> {
    let mut child = Command::new(binary)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| format!("could not run {}: {err}", binary.display()))?;
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if started.elapsed() > SMOKE_TIMEOUT => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("`--version` did not finish".to_string());
            }
            Ok(None) => thread::sleep(Duration::from_millis(25)),
            Err(err) => return Err(format!("`--version` failed: {err}")),
        }
    }
    let output = child
        .wait_with_output()
        .map_err(|err| format!("`--version` failed: {err}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    if !output.status.success() {
        return Err(format!("`--version` exited with {}", output.status));
    }
    let expected = expected_version.trim_start_matches('v');
    if stdout
        .split_whitespace()
        .any(|token| token.trim_start_matches('v') == expected)
    {
        Ok(())
    } else {
        Err(format!(
            "`--version` printed {:?}, expected {expected}",
            stdout.trim()
        ))
    }
}

fn is_current_exe(target: &Path) -> bool {
    let canonical = |path: &Path| fs::canonicalize(path).ok();
    match (
        env::current_exe().ok().and_then(|exe| canonical(&exe)),
        canonical(target),
    ) {
        (Some(current), Some(target)) => current == target,
        _ => false,
    }
}

/// Moves `staged` over `target`. On Unix a rename atomically swaps the
/// directory entry (the running process keeps its old inode). Windows can't
/// overwrite a running executable, so the running host goes through
/// `self-replace`, which renames it out of the way first.
fn replace_executable(staged: &Path, target: &Path) -> Result<(), String> {
    #[cfg(windows)]
    {
        if is_current_exe(target) {
            self_replace::self_replace(staged)
                .map_err(|err| format!("Failed to replace the running host: {err}"))?;
            let _ = fs::remove_file(staged);
            return Ok(());
        }
    }
    #[cfg(not(windows))]
    let _ = is_current_exe;
    fs::rename(staged, target)
        .map_err(|err| format!("Failed to replace {}: {err}", target.display()))
}

fn restore_previous(previous: &Path, target: &Path) -> Result<(), String> {
    let staged = staged_path(target);
    fs::copy(previous, &staged)
        .map_err(|err| format!("Failed to copy {}: {err}", previous.display()))?;
    replace_executable(&staged, target)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct InstallReport {
    pub(super) version: String,
    pub(super) target: PathBuf,
    pub(super) previous: PathBuf,
}

/// Verifies `archive`, extracts the host binary, smoke-checks it, swaps it in
/// and smoke-checks the result, rolling back to `<target>.previous` if the
/// installed binary fails. `smoke` is [`smoke_check`] outside tests.
pub(super) fn install_archive(
    archive: &Path,
    asset: &PlatformAsset,
    version: &str,
    public_key_base64: &str,
    target: &Path,
    smoke: &dyn Fn(&Path, &str) -> Result<(), String>,
) -> Result<InstallReport, String> {
    verify_archive_file(archive, &asset.sha256, &asset.signature, public_key_base64)?;
    let format = asset.archive_format()?;
    let staged = staged_path(target);
    let _ = fs::remove_file(&staged);
    let result = (|| {
        extract_host_binary(archive, format, HOST_BINARY_NAME, &staged).map_err(|err| {
            if err.contains("Failed to write") {
                format!(
                    "{err}. The host updates itself in place, so its folder must be writable by the user it runs as."
                )
            } else {
                err
            }
        })?;
        make_executable(&staged)?;
        smoke(&staged, version).map_err(|err| {
            format!(
                "The downloaded host failed its --version check, so nothing was replaced: {err}"
            )
        })?;
        let previous = previous_path(target);
        fs::copy(target, &previous).map_err(|err| {
            format!(
                "Failed to keep a copy of the current host at {}: {err}",
                previous.display()
            )
        })?;
        replace_executable(&staged, target)?;
        if let Err(err) = smoke(target, version) {
            return Err(match restore_previous(&previous, target) {
                Ok(()) => format!(
                    "The installed host failed its --version check, so the previous version was restored: {err}"
                ),
                Err(restore) => format!(
                    "The installed host failed its --version check ({err}) and restoring {} failed: {restore}. Copy it over {} by hand.",
                    previous.display(),
                    target.display()
                ),
            });
        }
        Ok(InstallReport {
            version: version.to_string(),
            target: target.to_path_buf(),
            previous,
        })
    })();
    let _ = fs::remove_file(&staged);
    result
}

// ───────────────────────────── Network ─────────────────────────────

fn http_client(timeout: Duration) -> Result<reqwest::blocking::Client, String> {
    reqwest::blocking::Client::builder()
        .user_agent(format!(
            "fairspoken-transcription-host/{}",
            env!("CARGO_PKG_VERSION")
        ))
        .connect_timeout(Duration::from_secs(15))
        .timeout(timeout)
        .build()
        .map_err(|err| format!("Failed to start the update client: {err}"))
}

pub(super) fn feed_url(base: &str, channel: UpdateChannel) -> String {
    format!(
        "{}/host-{}.json",
        base.trim_end_matches('/'),
        channel.as_str()
    )
}

/// `Ok(None)` when the feed does not exist: `host-stable.json` only appears
/// with the first stable release (and `host-nightly.json` with the first
/// nightly), so a missing feed means "nothing newer", not a failure.
pub(super) fn fetch_manifest(
    base: &str,
    channel: UpdateChannel,
) -> Result<Option<HostManifest>, String> {
    let url = feed_url(base, channel);
    let response = http_client(MANIFEST_TIMEOUT)?
        .get(&url)
        .header("Cache-Control", "no-cache")
        .send()
        .map_err(|err| format!("Could not reach the update feed: {err}"))?;
    let status = response.status();
    if status.as_u16() == 404 {
        return Ok(None);
    }
    if !status.is_success() {
        return Err(format!("The update feed answered {status} ({url})"));
    }
    let mut body = Vec::new();
    response
        .take(MAX_MANIFEST_BYTES + 1)
        .read_to_end(&mut body)
        .map_err(|err| format!("Failed to read the update feed: {err}"))?;
    if body.len() as u64 > MAX_MANIFEST_BYTES {
        return Err("The update feed is implausibly large".to_string());
    }
    parse_manifest(&body, channel).map(Some)
}

pub(super) fn download_to_file(
    url: &str,
    dest: &Path,
    mut progress: impl FnMut(u64, Option<u64>),
) -> Result<(), String> {
    let mut response = http_client(DOWNLOAD_TIMEOUT)?
        .get(url)
        .send()
        .map_err(|err| format!("Could not download the update: {err}"))?;
    if !response.status().is_success() {
        return Err(format!(
            "The update download answered {} ({url})",
            response.status()
        ));
    }
    let total = response.content_length();
    if total.is_some_and(|total| total > MAX_ARCHIVE_BYTES) {
        return Err("The update archive is implausibly large".to_string());
    }
    let mut out =
        File::create(dest).map_err(|err| format!("Failed to create {}: {err}", dest.display()))?;
    let mut chunk = vec![0u8; 64 * 1024];
    let mut downloaded = 0u64;
    progress(0, total);
    loop {
        let read = response
            .read(&mut chunk)
            .map_err(|err| format!("The update download was interrupted: {err}"))?;
        if read == 0 {
            break;
        }
        downloaded += read as u64;
        if downloaded > MAX_ARCHIVE_BYTES {
            return Err("The update archive is implausibly large".to_string());
        }
        out.write_all(&chunk[..read])
            .map_err(|err| format!("Failed to save the update: {err}"))?;
        progress(downloaded, total);
    }
    out.sync_all()
        .map_err(|err| format!("Failed to save the update: {err}"))
}

// ───────────────────────────── Offers ─────────────────────────────

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(super) struct AvailableUpdate {
    pub(super) version: String,
    pub(super) channel: UpdateChannel,
    pub(super) notes: Option<String>,
    pub(super) pub_date: Option<String>,
    /// Nightly → stable: the offer sorts lower than the running build.
    pub(super) switch_to_stable: bool,
    #[serde(skip)]
    pub(super) asset: PlatformAsset,
}

/// Turns a feed into an offer for this platform, or `None` when up to date.
pub(super) fn offer_from_manifest(
    manifest: &HostManifest,
    current: &str,
    channel: UpdateChannel,
    platform: &str,
) -> Result<Option<AvailableUpdate>, String> {
    let offer = evaluate_offer(current, &manifest.version, channel)?;
    if offer == Offer::UpToDate {
        return Ok(None);
    }
    let asset = manifest.asset_for(platform)?.clone();
    Ok(Some(AvailableUpdate {
        version: manifest.version.clone(),
        channel,
        notes: manifest.notes.clone(),
        pub_date: manifest.pub_date.clone(),
        switch_to_stable: offer == Offer::SwitchToStable,
        asset,
    }))
}

pub(super) fn check_feed(
    base: &str,
    current: &str,
    channel: UpdateChannel,
) -> Result<Option<AvailableUpdate>, String> {
    let platform = current_platform_key().ok_or_else(|| {
        format!(
            "No Fairspoken host builds are published for {}-{}",
            env::consts::OS,
            env::consts::ARCH
        )
    })?;
    match fetch_manifest(base, channel)? {
        Some(manifest) => offer_from_manifest(&manifest, current, channel, platform),
        None => Ok(None),
    }
}

/// Downloads, verifies and installs `update` over `target`.
pub(super) fn download_and_install(
    update: &AvailableUpdate,
    public_key_base64: &str,
    target: &Path,
    progress: impl FnMut(u64, Option<u64>),
) -> Result<InstallReport, String> {
    let archive = env::temp_dir().join(format!(
        "fairspoken-host-update-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let result = download_to_file(&update.asset.url, &archive, progress).and_then(|()| {
        install_archive(
            &archive,
            &update.asset,
            &update.version,
            public_key_base64,
            target,
            &smoke_check,
        )
    });
    let _ = fs::remove_file(&archive);
    result
}

/// The update's display name, e.g. "0.3.0 (Nightly)".
pub(super) fn describe_update(update: &AvailableUpdate) -> String {
    if update.switch_to_stable {
        format!("Switch to stable {}", update.version)
    } else {
        format!("{} ({})", update.version, update.channel.label())
    }
}

// ───────────────────────────── Restart ─────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(super) enum RestartMode {
    /// Exit with [`RESTART_EXIT_CODE`]; launchd/systemd start the new binary.
    Service,
    /// Re-execute the (new) binary with the same arguments.
    Reexec,
}

/// `parent` is the parent process: launchd jobs are children of PID 1, and
/// systemd services of a process named `systemd`. Requiring it avoids
/// mistaking a terminal (which may inherit `INVOCATION_ID` or an
/// `XPC_SERVICE_NAME`) for a service manager that will restart the host.
pub(super) fn detect_restart_mode(
    var: impl Fn(&str) -> Option<String>,
    parent: ParentProcess,
) -> RestartMode {
    let set = |name: &str| var(name).filter(|value| !value.trim().is_empty());
    if let Some(value) = set(SERVICE_ENV) {
        return match value.trim().to_ascii_lowercase().as_str() {
            "0" | "false" | "none" | "no" => RestartMode::Reexec,
            _ => RestartMode::Service,
        };
    }
    if set("INVOCATION_ID").is_some() && parent.is_systemd {
        return RestartMode::Service;
    }
    if let Some(label) = set("XPC_SERVICE_NAME") {
        if label != "0" && !label.starts_with("application.") && parent.pid == Some(1) {
            return RestartMode::Service;
        }
    }
    RestartMode::Reexec
}

#[derive(Clone, Copy, Debug, Default)]
pub(super) struct ParentProcess {
    pub(super) pid: Option<u32>,
    pub(super) is_systemd: bool,
}

#[cfg(unix)]
pub(super) fn parent_process() -> ParentProcess {
    let pid = std::os::unix::process::parent_id();
    let is_systemd = fs::read_to_string(format!("/proc/{pid}/comm"))
        .map(|comm| comm.trim() == "systemd")
        .unwrap_or(false);
    ParentProcess {
        pid: Some(pid),
        is_systemd,
    }
}

#[cfg(not(unix))]
pub(super) fn parent_process() -> ParentProcess {
    ParentProcess::default()
}

/// Ends this process so the new binary takes over. Never returns.
pub(super) fn restart_process(mode: RestartMode, exe: &Path) -> ! {
    let _ = io::stdout().flush();
    if mode == RestartMode::Service {
        eprintln!(
            "Fairspoken host update installed; exiting with status {RESTART_EXIT_CODE} so the service manager starts the new version."
        );
        std::process::exit(RESTART_EXIT_CODE);
    }
    let args: Vec<_> = env::args_os().skip(1).collect();
    eprintln!(
        "Fairspoken host update installed; restarting {}",
        exe.display()
    );
    let mut command = Command::new(exe);
    command.args(&args).env(RESTARTED_ENV, "1");
    // Unix: replace this process image (same PID, so a supervisor that did
    // not ask for exit-and-restart sees nothing). Listening sockets are
    // close-on-exec, so the new image can bind the port.
    #[cfg(unix)]
    let err = {
        use std::os::unix::process::CommandExt;
        command.exec()
    };
    // Windows: start the new binary, which retries binding the port until
    // this process has exited.
    #[cfg(not(unix))]
    let err = match command.spawn() {
        Ok(_) => std::process::exit(0),
        Err(err) => err,
    };
    eprintln!("Failed to restart the host: {err}");
    std::process::exit(RESTART_EXIT_CODE);
}

// ───────────────────────────── Live updater ─────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(super) enum UpdatePhase {
    Idle,
    Checking,
    Available,
    Downloading,
    Ready,
    Restarting,
    Error,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct UpdateProgress {
    pub(super) downloaded_bytes: u64,
    pub(super) total_bytes: Option<u64>,
    pub(super) percentage: Option<u8>,
}

/// `GET /v1/update`.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct UpdateStatus {
    pub(super) current_version: String,
    pub(super) channel: UpdateChannel,
    pub(super) channel_source: ChannelSource,
    pub(super) default_channel: UpdateChannel,
    pub(super) auto_update: bool,
    pub(super) checks_enabled: bool,
    pub(super) platform: Option<&'static str>,
    pub(super) state: UpdatePhase,
    pub(super) available: Option<AvailableUpdate>,
    pub(super) progress: Option<UpdateProgress>,
    pub(super) error: Option<String>,
    pub(super) last_checked_ms: Option<u64>,
    pub(super) installed_version: Option<String>,
    pub(super) restart_mode: RestartMode,
}

struct UpdateState {
    phase: UpdatePhase,
    available: Option<AvailableUpdate>,
    error: Option<String>,
    last_checked_ms: Option<u64>,
    progress: Option<UpdateProgress>,
    installed_version: Option<String>,
}

pub(super) struct UpdaterOptions {
    pub(super) current_version: String,
    pub(super) feed_base: String,
    pub(super) public_key: String,
    pub(super) exe_path: PathBuf,
    pub(super) restart_mode: RestartMode,
    pub(super) checks_enabled: bool,
    pub(super) env_channel: Option<String>,
}

impl UpdaterOptions {
    pub(super) fn from_env(current_version: &str) -> Result<Self, String> {
        let env_channel = crate::app_dirs::env_var(CHANNEL_ENV);
        // An invalid pin fails startup, like an invalid FAIRSPOKEN_HOST_MODEL.
        resolve_channel(env_channel.as_deref(), None, current_version)?;
        Ok(Self {
            current_version: current_version.to_string(),
            feed_base: crate::app_dirs::env_var(FEED_URL_ENV)
                .filter(|value| !value.trim().is_empty())
                .unwrap_or_else(|| DEFAULT_FEED_BASE_URL.to_string()),
            public_key: UPDATER_PUBLIC_KEY.to_string(),
            exe_path: env::current_exe()
                .map_err(|err| format!("Cannot locate the host executable: {err}"))?,
            restart_mode: detect_restart_mode(crate::app_dirs::env_var, parent_process()),
            checks_enabled: crate::app_dirs::env_var(CHECKS_ENV)
                .and_then(|value| super::config::parse_bool(&value))
                .unwrap_or(true),
            env_channel,
        })
    }
}

type IdleProbe = Box<dyn Fn() -> bool + Send + Sync>;

pub(super) struct Updater {
    options: UpdaterOptions,
    live: Arc<HostLiveConfig>,
    config_path: PathBuf,
    state: Mutex<UpdateState>,
    /// True when no transcription is queued, running or streaming.
    is_idle: IdleProbe,
}

impl Updater {
    pub(super) fn new(
        options: UpdaterOptions,
        live: Arc<HostLiveConfig>,
        config_path: PathBuf,
        is_idle: IdleProbe,
    ) -> Self {
        Self {
            options,
            live,
            config_path,
            state: Mutex::new(UpdateState {
                phase: UpdatePhase::Idle,
                available: None,
                error: None,
                last_checked_ms: None,
                progress: None,
                installed_version: None,
            }),
            is_idle,
        }
    }

    fn lock(&self) -> MutexGuard<'_, UpdateState> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn channel(&self) -> (UpdateChannel, ChannelSource) {
        let saved = self.live.update_prefs().update_channel;
        resolve_channel(
            self.options.env_channel.as_deref(),
            saved,
            &self.options.current_version,
        )
        // Validated at startup; fall back to the saved/derived channel.
        .unwrap_or_else(|_| {
            resolve_channel(None, saved, &self.options.current_version)
                .unwrap_or((UpdateChannel::Stable, ChannelSource::Version))
        })
    }

    pub(super) fn status(&self) -> UpdateStatus {
        let (channel, channel_source) = self.channel();
        let prefs = self.live.update_prefs();
        let state = self.lock();
        UpdateStatus {
            current_version: self.options.current_version.clone(),
            channel,
            channel_source,
            default_channel: UpdateChannel::for_version(&self.options.current_version),
            auto_update: prefs.auto_update,
            checks_enabled: self.options.checks_enabled,
            platform: current_platform_key(),
            state: state.phase,
            available: state.available.clone(),
            progress: state.progress.clone(),
            error: state.error.clone(),
            last_checked_ms: state.last_checked_ms,
            installed_version: state.installed_version.clone(),
            restart_mode: self.options.restart_mode,
        }
    }

    /// Starts a check on its own thread. `automatic` checks may install when
    /// `autoUpdate` is on; manual ones never do.
    pub(super) fn begin_check(self: &Arc<Self>, automatic: bool) -> Result<(), String> {
        {
            let mut state = self.lock();
            match state.phase {
                UpdatePhase::Checking => return Ok(()),
                UpdatePhase::Downloading | UpdatePhase::Restarting => {
                    return Err("An update is already being installed".to_string())
                }
                UpdatePhase::Ready => {
                    return Err("An update is installed; restart the host to finish".to_string())
                }
                _ => {}
            }
            state.phase = UpdatePhase::Checking;
            state.error = None;
        }
        let updater = Arc::clone(self);
        thread::Builder::new()
            .name("transcription-host-update-check".to_string())
            .spawn(move || updater.run_check(automatic))
            .map(|_| ())
            .map_err(|err| {
                self.fail(format!("Failed to start the update check: {err}"));
                "Failed to start the update check".to_string()
            })
    }

    fn fail(&self, message: String) {
        let mut state = self.lock();
        state.phase = UpdatePhase::Error;
        state.error = Some(message);
        state.progress = None;
    }

    fn run_check(self: &Arc<Self>, automatic: bool) {
        let (channel, _) = self.channel();
        let result = check_feed(
            &self.options.feed_base,
            &self.options.current_version,
            channel,
        );
        let install_now = {
            let mut state = self.lock();
            state.last_checked_ms = Some(super::now_epoch_ms());
            match result {
                Ok(Some(update)) => {
                    let announce = state
                        .available
                        .as_ref()
                        .is_none_or(|known| known.version != update.version);
                    if announce {
                        println!(
                            "Fairspoken host update available: {} — install it from the dashboard or run `transcription-host --update`.",
                            describe_update(&update)
                        );
                    }
                    state.available = Some(update);
                    state.phase = UpdatePhase::Available;
                    state.error = None;
                    automatic && self.live.update_prefs().auto_update
                }
                Ok(None) => {
                    state.available = None;
                    state.phase = UpdatePhase::Idle;
                    state.error = None;
                    false
                }
                Err(err) => {
                    if !automatic {
                        eprintln!("Update check failed: {err}");
                    }
                    state.phase = UpdatePhase::Error;
                    state.error = Some(err);
                    false
                }
            }
        };
        if install_now {
            if let Err(err) = self.begin_install(true) {
                eprintln!("Automatic update failed to start: {err}");
            }
        }
    }

    /// Downloads and installs the available update on its own thread. An
    /// `unattended` install restarts the host as soon as it is idle.
    pub(super) fn begin_install(self: &Arc<Self>, unattended: bool) -> Result<(), String> {
        let update = {
            let mut state = self.lock();
            if matches!(
                state.phase,
                UpdatePhase::Checking | UpdatePhase::Downloading | UpdatePhase::Restarting
            ) {
                return Err("An update check or install is already running".to_string());
            }
            if state.phase == UpdatePhase::Ready {
                return Err("The update is already installed; restart the host".to_string());
            }
            let Some(update) = state.available.clone() else {
                return Err("No update is available; check for updates first".to_string());
            };
            state.phase = UpdatePhase::Downloading;
            state.error = None;
            state.progress = Some(UpdateProgress {
                downloaded_bytes: 0,
                total_bytes: None,
                percentage: Some(0),
            });
            update
        };
        let updater = Arc::clone(self);
        thread::Builder::new()
            .name("transcription-host-update-install".to_string())
            .spawn(move || updater.run_install(update, unattended))
            .map(|_| ())
            .map_err(|err| {
                self.fail(format!("Failed to start the update: {err}"));
                "Failed to start the update".to_string()
            })
    }

    fn run_install(self: &Arc<Self>, update: AvailableUpdate, unattended: bool) {
        println!("Installing Fairspoken host {}…", describe_update(&update));
        let progress_updater = Arc::clone(self);
        let result = download_and_install(
            &update,
            &self.options.public_key,
            &self.options.exe_path,
            move |downloaded, total| {
                let mut state = progress_updater.lock();
                state.progress = Some(UpdateProgress {
                    downloaded_bytes: downloaded,
                    total_bytes: total,
                    percentage: total
                        .filter(|total| *total > 0)
                        .map(|total| ((downloaded.min(total) * 100) / total) as u8),
                });
            },
        );
        match result {
            Ok(report) => {
                println!(
                    "Installed Fairspoken host {} at {} (previous version kept at {}).",
                    report.version,
                    report.target.display(),
                    report.previous.display()
                );
                {
                    let mut state = self.lock();
                    state.phase = UpdatePhase::Ready;
                    state.installed_version = Some(report.version);
                    state.progress = None;
                }
                if unattended {
                    self.restart_when_idle(None);
                }
            }
            Err(err) => {
                eprintln!("Update failed: {err}");
                self.fail(err);
            }
        }
    }

    /// Restarts into the installed update. Waits for in-flight work first.
    pub(super) fn begin_restart(self: &Arc<Self>) -> Result<(), String> {
        {
            let mut state = self.lock();
            if state.phase != UpdatePhase::Ready {
                return Err("No installed update is waiting for a restart".to_string());
            }
            state.phase = UpdatePhase::Restarting;
        }
        let updater = Arc::clone(self);
        thread::Builder::new()
            .name("transcription-host-update-restart".to_string())
            .spawn(move || {
                // Let the HTTP response reach the dashboard first.
                thread::sleep(Duration::from_millis(300));
                updater.restart_when_idle(Some(MANUAL_RESTART_DRAIN));
            })
            .map(|_| ())
            .map_err(|err| format!("Failed to restart: {err}"))
    }

    /// `limit` None waits as long as it takes (unattended updates never cut
    /// off a transcription); Some waits at most that long.
    fn restart_when_idle(&self, limit: Option<Duration>) -> ! {
        let started = Instant::now();
        while !(self.is_idle)() {
            if limit.is_some_and(|limit| started.elapsed() >= limit) {
                eprintln!("Restarting with transcriptions still in flight");
                break;
            }
            thread::sleep(Duration::from_millis(250));
        }
        restart_process(self.options.restart_mode, &self.options.exe_path)
    }

    /// `POST /v1/update/settings`. A channel change triggers a check.
    pub(super) fn apply_settings(
        self: &Arc<Self>,
        settings: &UpdateSettingsRequest,
    ) -> Result<(), String> {
        let channel =
            match settings.channel.as_ref() {
                None => None,
                Some(None) => Some(None),
                Some(Some(raw)) => Some(Some(UpdateChannel::parse(raw).ok_or_else(|| {
                    format!("channel must be stable, nightly or null, not {raw}")
                })?)),
            };
        let before = self.channel().0;
        let previous = self.live.update_prefs();
        let mut next = previous.clone();
        if let Some(channel) = channel {
            next.update_channel = channel;
        }
        if let Some(auto) = settings.auto_update {
            next.auto_update = auto;
        }
        self.live.set_update_prefs(next);
        if let Err(err) = persist_live_config(&self.config_path, &self.live) {
            self.live.set_update_prefs(previous);
            return Err(format!("Saving the update settings failed: {err}"));
        }
        if self.channel().0 != before {
            {
                let mut state = self.lock();
                if matches!(
                    state.phase,
                    UpdatePhase::Available | UpdatePhase::Error | UpdatePhase::Idle
                ) {
                    state.available = None;
                }
            }
            // A busy updater keeps going; the next check uses the new channel.
            let _ = self.begin_check(false);
        }
        Ok(())
    }

    /// First check ~30 s after start, then every 6 h. Only records what is
    /// available unless `autoUpdate` is on.
    pub(super) fn spawn_periodic_checks(self: &Arc<Self>) {
        if !self.options.checks_enabled {
            return;
        }
        let updater = Arc::clone(self);
        let spawned = thread::Builder::new()
            .name("transcription-host-update-timer".to_string())
            .spawn(move || {
                thread::sleep(FIRST_CHECK_DELAY);
                loop {
                    let _ = updater.begin_check(true);
                    thread::sleep(CHECK_INTERVAL);
                }
            });
        if let Err(err) = spawned {
            eprintln!("Automatic update checks are off: {err}");
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct UpdateSettingsRequest {
    /// `"stable"`, `"nightly"`, or `null` to follow the running version.
    #[serde(default, deserialize_with = "double_option")]
    pub(super) channel: Option<Option<String>>,
    #[serde(default)]
    pub(super) auto_update: Option<bool>,
}

fn double_option<'de, D>(deserializer: D) -> Result<Option<Option<String>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Option::<String>::deserialize(deserializer).map(Some)
}

/// Throwaway signing keys and release-shaped archives for tests. Never the
/// real release key.
#[cfg(test)]
pub(super) mod test_support {
    use super::*;
    use std::io::Cursor;

    // ── Signing fixtures: a throwaway keypair, never the release key ──

    pub(in crate::host) struct TestKey {
        pub(in crate::host) keypair: minisign::KeyPair,
        pub(in crate::host) public_base64: String,
    }

    pub(in crate::host) fn test_key() -> TestKey {
        let keypair = minisign::KeyPair::generate_unencrypted_keypair().expect("keypair");
        let public_text = keypair.pk.to_box().expect("public key box").to_string();
        TestKey {
            public_base64: base64::engine::general_purpose::STANDARD.encode(public_text),
            keypair,
        }
    }

    /// The `.sig` contents `tauri signer sign` writes: base64 of the minisign
    /// signature file.
    pub(in crate::host) fn sign(key: &TestKey, data: &[u8]) -> String {
        let signature = minisign::sign(
            Some(&key.keypair.pk),
            &key.keypair.sk,
            Cursor::new(data),
            Some("test"),
            None,
        )
        .expect("sign");
        base64::engine::general_purpose::STANDARD.encode(signature.to_string())
    }

    pub(in crate::host) fn sha256_hex(data: &[u8]) -> String {
        hex_digest(Sha256::digest(data).as_slice())
    }

    pub(in crate::host) fn write(dir: &Path, name: &str, data: &[u8]) -> PathBuf {
        let path = dir.join(name);
        fs::write(&path, data).expect("write fixture");
        path
    }

    pub(in crate::host) fn tar_gz(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(
            Vec::new(),
            flate2::Compression::fast(),
        ));
        for (path, data) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(0o755);
            header.set_cksum();
            builder.append_data(&mut header, path, *data).unwrap();
        }
        builder.into_inner().unwrap().finish().unwrap()
    }

    #[cfg(unix)]
    pub(in crate::host) fn script(version: &str, ok: bool) -> Vec<u8> {
        if ok {
            format!("#!/bin/sh\necho \"transcription-host {version}\"\n").into_bytes()
        } else {
            b"#!/bin/sh\necho broken >&2\nexit 3\n".to_vec()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::*;
    use super::*;
    use std::io::Cursor;

    const NIGHTLY: &str = "0.3.0-nightly.20261005.4";

    #[test]
    fn embedded_public_key_matches_tauri_config() {
        let config: serde_json::Value =
            serde_json::from_str(include_str!("../../tauri.conf.json")).expect("tauri.conf.json");
        assert_eq!(
            config["plugins"]["updater"]["pubkey"].as_str(),
            Some(UPDATER_PUBLIC_KEY)
        );
        decode_public_key(UPDATER_PUBLIC_KEY).expect("embedded key decodes");
    }

    #[test]
    fn channel_resolution_prefers_env_then_saved_then_version() {
        assert_eq!(
            resolve_channel(None, None, "0.3.0").unwrap(),
            (UpdateChannel::Stable, ChannelSource::Version)
        );
        assert_eq!(
            resolve_channel(None, None, NIGHTLY).unwrap(),
            (UpdateChannel::Nightly, ChannelSource::Version)
        );
        assert_eq!(
            resolve_channel(None, Some(UpdateChannel::Stable), NIGHTLY).unwrap(),
            (UpdateChannel::Stable, ChannelSource::Saved)
        );
        assert_eq!(
            resolve_channel(Some(" Nightly "), Some(UpdateChannel::Stable), "0.3.0").unwrap(),
            (UpdateChannel::Nightly, ChannelSource::Env)
        );
        assert_eq!(
            resolve_channel(Some(""), Some(UpdateChannel::Nightly), "0.3.0").unwrap(),
            (UpdateChannel::Nightly, ChannelSource::Saved)
        );
        assert!(resolve_channel(Some("beta"), None, "0.3.0").is_err());
    }

    #[test]
    fn update_prefs_default_when_absent_and_skip_unset_channel() {
        let prefs: UpdatePrefs = serde_json::from_str("{}").unwrap();
        assert_eq!(prefs, UpdatePrefs::default());
        assert_eq!(
            serde_json::to_string(&prefs).unwrap(),
            r#"{"autoUpdate":false}"#
        );
        let prefs: UpdatePrefs =
            serde_json::from_str(r#"{"updateChannel":"nightly","autoUpdate":true}"#).unwrap();
        assert_eq!(prefs.update_channel, Some(UpdateChannel::Nightly));
        assert!(prefs.auto_update);
    }

    #[test]
    fn version_offers_follow_the_contract() {
        use UpdateChannel::*;
        assert_eq!(
            evaluate_offer("0.2.0", "0.3.0", Stable).unwrap(),
            Offer::Upgrade
        );
        assert_eq!(
            evaluate_offer("0.3.0", "0.3.0", Stable).unwrap(),
            Offer::UpToDate
        );
        assert_eq!(
            evaluate_offer("0.3.1", "0.3.0", Stable).unwrap(),
            Offer::UpToDate
        );
        // Numeric prerelease identifiers compare numerically.
        assert_eq!(
            evaluate_offer(
                "0.3.0-nightly.20261005.4",
                "0.3.0-nightly.20261005.10",
                Nightly
            )
            .unwrap(),
            Offer::Upgrade
        );
        assert_eq!(
            evaluate_offer(
                "0.3.0-nightly.20261006.1",
                "0.3.0-nightly.20261005.9",
                Nightly
            )
            .unwrap(),
            Offer::UpToDate
        );
        // Stable → nightly: the nightly is the next base version.
        assert_eq!(
            evaluate_offer("0.2.0", NIGHTLY, Nightly).unwrap(),
            Offer::Upgrade
        );
        // Nightly → stable: offered although it sorts lower.
        assert_eq!(
            evaluate_offer(NIGHTLY, "0.2.0", Stable).unwrap(),
            Offer::SwitchToStable
        );
        // …and a stable cut from that nightly is a plain upgrade.
        assert_eq!(
            evaluate_offer(NIGHTLY, "0.3.0", Stable).unwrap(),
            Offer::Upgrade
        );
        // A nightly on the nightly channel never downgrades to stable.
        assert_eq!(
            evaluate_offer(NIGHTLY, "0.2.0", Nightly).unwrap(),
            Offer::UpToDate
        );
        // Build metadata does not count.
        assert_eq!(
            evaluate_offer("0.3.0+a", "0.3.0+b", Stable).unwrap(),
            Offer::UpToDate
        );
        assert!(evaluate_offer("garbage", "0.3.0", Stable).is_err());
    }

    #[test]
    fn platform_keys_cover_every_published_host() {
        assert_eq!(platform_key("linux", "x86_64"), Some("linux-x86_64"));
        assert_eq!(platform_key("linux", "aarch64"), Some("linux-aarch64"));
        assert_eq!(platform_key("windows", "x86_64"), Some("windows-x86_64"));
        assert_eq!(platform_key("macos", "x86_64"), Some("darwin-x86_64"));
        assert_eq!(platform_key("macos", "aarch64"), Some("darwin-aarch64"));
        assert_eq!(platform_key("windows", "aarch64"), None);
        assert_eq!(platform_key("freebsd", "x86_64"), None);
    }

    fn manifest_json(version: &str, channel: &str) -> String {
        let sha = "a".repeat(64);
        format!(
            r#"{{
              "version": "{version}",
              "channel": "{channel}",
              "pub_date": "2026-10-05T12:00:00Z",
              "notes": "https://github.com/sprintengine/fairspoken/releases/tag/v{version}",
              "platforms": {{
                "linux-x86_64": {{ "url": "https://example.test/h-linux-x64.tar.gz", "sha256": "{sha}", "signature": "c2ln", "format": "tar.gz" }},
                "windows-x86_64": {{ "url": "https://example.test/h-windows-x64.zip", "sha256": "{sha}", "signature": "c2ln", "format": "zip" }},
                "darwin-aarch64": {{ "url": "https://example.test/h-macos-arm64.tar.gz", "sha256": "{sha}", "signature": "c2ln" }}
              }}
            }}"#
        )
    }

    #[test]
    fn manifest_parses_and_checks_its_channel() {
        let manifest = parse_manifest(
            manifest_json(NIGHTLY, "nightly").as_bytes(),
            UpdateChannel::Nightly,
        )
        .expect("nightly feed parses");
        assert_eq!(manifest.version, NIGHTLY);
        assert_eq!(manifest.pub_date.as_deref(), Some("2026-10-05T12:00:00Z"));
        assert_eq!(
            manifest
                .asset_for("windows-x86_64")
                .unwrap()
                .archive_format(),
            Ok(ArchiveFormat::Zip)
        );
        // No explicit format: inferred from the URL.
        assert_eq!(
            manifest
                .asset_for("darwin-aarch64")
                .unwrap()
                .archive_format(),
            Ok(ArchiveFormat::TarGz)
        );
        assert!(manifest
            .asset_for("linux-aarch64")
            .unwrap_err()
            .contains("no linux-aarch64 build"));

        // A nightly version in the stable feed, or a mislabelled feed, is refused.
        assert!(parse_manifest(
            manifest_json(NIGHTLY, "nightly").as_bytes(),
            UpdateChannel::Stable
        )
        .is_err());
        assert!(parse_manifest(
            manifest_json("0.3.0", "nightly").as_bytes(),
            UpdateChannel::Stable
        )
        .is_err());
        assert!(parse_manifest(b"{\"version\":\"x\"}", UpdateChannel::Stable).is_err());

        let offer = offer_from_manifest(&manifest, "0.2.0", UpdateChannel::Nightly, "linux-x86_64")
            .unwrap()
            .expect("newer nightly offered");
        assert!(!offer.switch_to_stable);
        assert_eq!(describe_update(&offer), format!("{NIGHTLY} (Nightly)"));
        assert!(
            offer_from_manifest(&manifest, NIGHTLY, UpdateChannel::Nightly, "linux-x86_64")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn invalid_platform_entries_are_rejected() {
        let mut asset = PlatformAsset {
            url: "https://example.test/a.tar.gz".to_string(),
            sha256: "a".repeat(64),
            signature: "c2ln".to_string(),
            format: None,
        };
        assert!(asset.validate("x").is_ok());
        asset.sha256 = "abc".to_string();
        assert!(asset.validate("x").is_err());
        asset.sha256 = "a".repeat(64);
        asset.url = "file:///etc/passwd".to_string();
        assert!(asset.validate("x").is_err());
        asset.url = "https://example.test/a.rar".to_string();
        assert!(asset.validate("x").is_err());
    }

    #[test]
    fn restart_mode_needs_a_real_service_manager() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |name: &str| {
                pairs
                    .iter()
                    .find(|(key, _)| *key == name)
                    .map(|(_, value)| value.to_string())
            }
        };
        let none = ParentProcess::default();
        let launchd = ParentProcess {
            pid: Some(1),
            is_systemd: false,
        };
        let systemd = ParentProcess {
            pid: Some(900),
            is_systemd: true,
        };
        assert_eq!(detect_restart_mode(env(&[]), none), RestartMode::Reexec);
        assert_eq!(
            detect_restart_mode(env(&[(SERVICE_ENV, "systemd")]), none),
            RestartMode::Service
        );
        assert_eq!(
            detect_restart_mode(env(&[(SERVICE_ENV, "0"), ("INVOCATION_ID", "x")]), systemd),
            RestartMode::Reexec
        );
        assert_eq!(
            detect_restart_mode(env(&[("INVOCATION_ID", "abc")]), systemd),
            RestartMode::Service
        );
        // A terminal inside a systemd-managed session is not a service.
        assert_eq!(
            detect_restart_mode(env(&[("INVOCATION_ID", "abc")]), none),
            RestartMode::Reexec
        );
        assert_eq!(
            detect_restart_mode(
                env(&[("XPC_SERVICE_NAME", "ie.fairspoken.transcription-host")]),
                launchd
            ),
            RestartMode::Service
        );
        assert_eq!(
            detect_restart_mode(
                env(&[("XPC_SERVICE_NAME", "application.com.apple.Terminal.1")]),
                launchd
            ),
            RestartMode::Reexec
        );
        assert_eq!(
            detect_restart_mode(env(&[("XPC_SERVICE_NAME", "0")]), launchd),
            RestartMode::Reexec
        );
    }

    #[test]
    fn settings_request_distinguishes_absent_and_null_channel() {
        let absent: UpdateSettingsRequest = serde_json::from_str(r#"{"autoUpdate":true}"#).unwrap();
        assert_eq!(absent.channel, None);
        let null: UpdateSettingsRequest = serde_json::from_str(r#"{"channel":null}"#).unwrap();
        assert_eq!(null.channel, Some(None));
        let set: UpdateSettingsRequest = serde_json::from_str(r#"{"channel":"stable"}"#).unwrap();
        assert_eq!(set.channel, Some(Some("stable".to_string())));
        assert!(serde_json::from_str::<UpdateSettingsRequest>(r#"{"chanel":"stable"}"#).is_err());
    }

    #[test]
    fn binary_entries_are_found_at_root_or_one_folder_deep() {
        assert!(is_binary_entry("transcription-host", "transcription-host"));
        assert!(is_binary_entry(
            "fairspoken-0.3.0-transcription-host-linux-x64/transcription-host",
            "transcription-host"
        ));
        assert!(is_binary_entry(
            "stem\\transcription-host.exe",
            "transcription-host.exe"
        ));
        assert!(is_binary_entry(
            "./stem/transcription-host",
            "transcription-host"
        ));
        assert!(is_binary_entry(
            "a/b/c/transcription-host",
            "transcription-host"
        ));
        assert!(is_binary_entry(
            "a\\b\\transcription-host.exe",
            "transcription-host.exe"
        ));
        assert!(!is_binary_entry(
            "a/transcription-host.previous",
            "transcription-host"
        ));
        assert!(!is_binary_entry(
            "../transcription-host",
            "transcription-host"
        ));
        assert!(!is_binary_entry("stem/PROTOCOL.md", "transcription-host"));
    }

    #[test]
    fn verification_requires_matching_sha256_and_signature() {
        let dir = tempfile::tempdir().unwrap();
        let key = test_key();
        let data = b"fairspoken host archive bytes";
        let archive = write(dir.path(), "a.tar.gz", data);
        let sha = sha256_hex(data);
        let signature = sign(&key, data);

        verify_archive_file(&archive, &sha, &signature, &key.public_base64)
            .expect("good archive verifies");
        verify_archive_file(
            &archive,
            &sha.to_uppercase(),
            &signature,
            &key.public_base64,
        )
        .expect("sha256 is case-insensitive");

        let wrong_sha = sha256_hex(b"other");
        let err =
            verify_archive_file(&archive, &wrong_sha, &signature, &key.public_base64).unwrap_err();
        assert!(err.contains("sha256"), "{err}");

        // Right checksum, but the signature is for different bytes.
        let other_signature = sign(&key, b"different bytes");
        let err =
            verify_archive_file(&archive, &sha, &other_signature, &key.public_base64).unwrap_err();
        assert!(err.contains("signature"), "{err}");

        // Signed by someone else (also the real release key ≠ test key).
        let stranger = test_key();
        let err = verify_archive_file(&archive, &sha, &sign(&stranger, data), &key.public_base64)
            .unwrap_err();
        assert!(err.contains("release key"), "{err}");
        assert!(verify_archive_file(&archive, &sha, &signature, UPDATER_PUBLIC_KEY).is_err());

        assert!(verify_archive_file(&archive, &sha, "not base64!", &key.public_base64).is_err());
    }

    fn zip_archive(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (path, data) in entries {
            writer
                .start_file(*path, zip::write::SimpleFileOptions::default())
                .unwrap();
            writer.write_all(data).unwrap();
        }
        writer.finish().unwrap().into_inner()
    }

    #[test]
    fn extracts_the_binary_from_release_shaped_archives() {
        let dir = tempfile::tempdir().unwrap();
        let stem = "fairspoken-0.3.0-transcription-host-linux-x64";
        let tgz = write(
            dir.path(),
            "h.tar.gz",
            &tar_gz(&[
                (&format!("{stem}/PROTOCOL.md"), b"docs"),
                (&format!("{stem}/transcription-host"), b"BINARY"),
                (&format!("{stem}/LICENSE"), b"mit"),
            ]),
        );
        let out = dir.path().join("out");
        extract_host_binary(&tgz, ArchiveFormat::TarGz, "transcription-host", &out).unwrap();
        assert_eq!(fs::read(&out).unwrap(), b"BINARY");

        let zip = write(
            dir.path(),
            "h.zip",
            &zip_archive(&[
                (
                    "fairspoken-0.3.0-transcription-host-windows-x64/LICENSE",
                    b"mit",
                ),
                (
                    "fairspoken-0.3.0-transcription-host-windows-x64/transcription-host.exe",
                    b"EXE",
                ),
            ]),
        );
        extract_host_binary(&zip, ArchiveFormat::Zip, "transcription-host.exe", &out).unwrap();
        assert_eq!(fs::read(&out).unwrap(), b"EXE");

        let empty = write(dir.path(), "e.tar.gz", &tar_gz(&[("stem/LICENSE", b"mit")]));
        let err = extract_host_binary(&empty, ArchiveFormat::TarGz, "transcription-host", &out)
            .unwrap_err();
        assert!(err.contains("has no transcription-host"), "{err}");
    }

    // ── End-to-end install against a temp dir ──

    #[cfg(unix)]
    struct Fixture {
        dir: tempfile::TempDir,
        key: TestKey,
        target: PathBuf,
    }

    #[cfg(unix)]
    impl Fixture {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let target = dir.path().join("transcription-host");
            fs::write(&target, script("0.2.0", true)).unwrap();
            make_executable(&target).unwrap();
            Self {
                dir,
                key: test_key(),
                target,
            }
        }

        fn archive(&self, binary: &[u8]) -> (PathBuf, PlatformAsset) {
            let bytes = tar_gz(&[
                (
                    "fairspoken-0.3.0-transcription-host-linux-x64/LICENSE",
                    b"mit",
                ),
                (
                    "fairspoken-0.3.0-transcription-host-linux-x64/transcription-host",
                    binary,
                ),
            ]);
            let path = write(self.dir.path(), "update.tar.gz", &bytes);
            let asset = PlatformAsset {
                url: "https://example.test/update.tar.gz".to_string(),
                sha256: sha256_hex(&bytes),
                signature: sign(&self.key, &bytes),
                format: Some("tar.gz".to_string()),
            };
            (path, asset)
        }

        fn install(
            &self,
            archive: &Path,
            asset: &PlatformAsset,
            smoke: &dyn Fn(&Path, &str) -> Result<(), String>,
        ) -> Result<InstallReport, String> {
            install_archive(
                archive,
                asset,
                "0.3.0",
                &self.key.public_base64,
                &self.target,
                smoke,
            )
        }

        fn leftovers(&self) -> Vec<String> {
            let mut names: Vec<String> = fs::read_dir(self.dir.path())
                .unwrap()
                .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
                .collect();
            names.sort();
            names
        }
    }

    #[cfg(unix)]
    #[test]
    fn install_swaps_the_binary_and_keeps_the_previous_one() {
        let fx = Fixture::new();
        let (archive, asset) = fx.archive(&script("0.3.0", true));
        let report = fx.install(&archive, &asset, &smoke_check).expect("install");
        assert_eq!(report.version, "0.3.0");
        assert_eq!(
            report.previous,
            fx.dir.path().join("transcription-host.previous")
        );
        smoke_check(&fx.target, "0.3.0").expect("new binary in place");
        smoke_check(&report.previous, "0.2.0").expect("old binary kept");
        assert_eq!(
            fx.leftovers(),
            vec![
                "transcription-host",
                "transcription-host.previous",
                "update.tar.gz"
            ]
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_tampered_archive_changes_nothing() {
        let fx = Fixture::new();
        let (archive, mut asset) = fx.archive(&script("0.3.0", true));
        asset.signature = sign(&fx.key, b"something else");
        assert!(fx.install(&archive, &asset, &smoke_check).is_err());
        smoke_check(&fx.target, "0.2.0").expect("untouched");
        assert_eq!(fx.leftovers(), vec!["transcription-host", "update.tar.gz"]);
    }

    #[cfg(unix)]
    #[test]
    fn a_binary_failing_its_version_check_is_never_swapped_in() {
        let fx = Fixture::new();
        // Signed and well formed, but it does not run.
        let (archive, asset) = fx.archive(&script("0.3.0", false));
        let err = fx.install(&archive, &asset, &smoke_check).unwrap_err();
        assert!(err.contains("nothing was replaced"), "{err}");
        smoke_check(&fx.target, "0.2.0").expect("untouched");
        assert_eq!(fx.leftovers(), vec!["transcription-host", "update.tar.gz"]);

        // A binary reporting the wrong version is refused too.
        let (archive, asset) = fx.archive(&script("0.2.9", true));
        assert!(fx.install(&archive, &asset, &smoke_check).is_err());
        smoke_check(&fx.target, "0.2.0").expect("untouched");
    }

    #[cfg(unix)]
    #[test]
    fn a_failing_installed_binary_rolls_back_to_previous() {
        let fx = Fixture::new();
        let (archive, asset) = fx.archive(&script("0.3.0", true));
        let target = fx.target.clone();
        // Passes the staged check, fails once it sits at the target path.
        let smoke = move |binary: &Path, version: &str| {
            if binary == target {
                Err("simulated post-swap failure".to_string())
            } else {
                smoke_check(binary, version)
            }
        };
        let err = fx.install(&archive, &asset, &smoke).unwrap_err();
        assert!(err.contains("previous version was restored"), "{err}");
        smoke_check(&fx.target, "0.2.0").expect("rolled back");
        assert!(fx.dir.path().join("transcription-host.previous").exists());
        assert!(!staged_path(&fx.target).exists());
    }

    #[test]
    fn staged_and_previous_paths_sit_next_to_the_target() {
        let target = Path::new("/opt/fs/transcription-host");
        assert_eq!(
            staged_path(target),
            Path::new("/opt/fs/.transcription-host.update")
        );
        assert_eq!(
            previous_path(target),
            Path::new("/opt/fs/transcription-host.previous")
        );
        let exe = Path::new("C:/fs/transcription-host.exe");
        assert_eq!(
            staged_path(exe),
            Path::new("C:/fs/.transcription-host.update.exe")
        );
    }
}
