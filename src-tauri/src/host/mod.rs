mod catalog;
mod cli;
mod config;
mod events;
mod http;
mod metrics;
mod pairing;
mod routes;
mod super_mode;
#[cfg(test)]
mod test_support;
mod update;
mod worker;

use crate::audio::{AudioFrame, Recording};
use crate::models::{ModelService, SttModel};
use crate::remote_transcription::{
    decode_wav, read_stream_frame, RemoteHealth, RemoteTranscriptionResponse, BACKEND_ID,
};
use crate::settings::Settings;
use crate::transcription::TranscriptionService;
use catalog::{ModelInventory, ModelSnapshot};
use config::{
    apply_config_update, default_host_config_path, load_persisted_config, overlay_persisted_config,
    persist_live_config, HostConfigUpdate, HostLiveConfig, HostRuntimeConfig, DEFAULT_HOST_MODEL,
};
#[cfg(test)]
use config::{parse_bool, parse_worker_models, PersistedHostConfig};
use events::{
    sse_frame, EventHub, FrameWriter, HostEvent, HostEventKind, SseWriter, HEARTBEAT_INTERVAL,
};
use pairing::{
    attempt_pairing, resolve_host_name, Hello, HostAuth, PairOutcome, PairRequest, PairingLimiter,
    HOST_NAME_ENV, PAIRING_PASSWORD_ENV,
};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::cmp::Reverse;
use std::collections::{HashMap, VecDeque};
use std::env;
use socket2::{Domain, Protocol, Socket, TcpKeepalive, Type};
use std::io::Read;
use std::net::{SocketAddr, TcpListener, ToSocketAddrs};
use std::panic::{self, AssertUnwindSafe};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tiny_http::{HTTPVersion, Header, Method, Request, Response, Server, StatusCode};
use update::{UpdateSettingsRequest, Updater, UpdaterOptions, RESTARTED_ENV};
// http, routes, worker and metrics were split out of this module: each
// starts with `use super::*` (these imports included), and the globs below
// put their items back in one namespace, so they call each other unqualified.
use http::*;
use metrics::*;
use routes::*;
use worker::*;

const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");

const MAX_STREAM_SAMPLE_RATE: u32 = 192_000;

pub fn run_transcription_host() -> Result<(), String> {
    let args: Vec<String> = env::args_os()
        .skip(1)
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    let command = cli::parse_args(&args);
    // `--version` and `--help` touch no data, and the updater runs a staged
    // binary's `--version` as a smoke check: neither may move directories.
    if command.as_ref().is_ok_and(cli::HostCommand::uses_data_dirs) {
        crate::app_dirs::migrate_legacy_dirs();
    }
    match command {
        Ok(cli::HostCommand::Serve) => {}
        Ok(command) => {
            let code = cli::run(command)?;
            if code != 0 {
                std::process::exit(code);
            }
            return Ok(());
        }
        Err(message) => {
            eprintln!("{message}\n\n{}", cli::USAGE);
            std::process::exit(2);
        }
    }

    let addr = crate::app_dirs::env_var("FAIRSPOKEN_HOST_ADDR")
        .unwrap_or_else(|| "127.0.0.1:48173".to_string());
    let mut config = HostRuntimeConfig::from_env()?;
    // Dashboard edits are the durable configuration; the environment only
    // seeds the first boot (or a deleted config file).
    let config_path = default_host_config_path();
    let persisted = load_persisted_config(&config_path)?;
    let update_prefs = persisted
        .as_ref()
        .map(|persisted| persisted.update.clone())
        .unwrap_or_default();
    // Unlike the live settings, the token, pairing password and name from
    // the environment win over the file on every start.
    let (auth, generated_token) = HostAuth::resolve(
        crate::app_dirs::env_var("FAIRSPOKEN_HOST_TOKEN"),
        persisted
            .as_ref()
            .and_then(|persisted| persisted.token.clone()),
        crate::app_dirs::env_var(PAIRING_PASSWORD_ENV),
        persisted
            .as_ref()
            .and_then(|persisted| persisted.pairing_password.clone()),
    )?;
    let saved_name = persisted
        .as_ref()
        .and_then(|persisted| persisted.name.clone());
    let host_name = resolve_host_name(
        crate::app_dirs::env_var(HOST_NAME_ENV),
        saved_name.as_deref(),
    );
    if let Some(persisted) = persisted {
        overlay_persisted_config(&mut config, persisted);
    }
    let update_options = UpdaterOptions::from_env(SERVER_VERSION)?;
    let server = bind_server(&addr)?;
    println!("Fairspoken transcription host {SERVER_VERSION} listening on http://{addr}");

    let models = ModelService::default();
    let metrics = Arc::new(Mutex::new(HostMetrics::new(&config)));
    let runtime = Arc::new(HostRuntime::start(
        models.clone(),
        config,
        Arc::clone(&metrics),
        config_path,
    )?);
    runtime.live.set_update_prefs(update_prefs);
    runtime.live.set_auth(auth);
    runtime.live.set_name(host_name, saved_name);
    if generated_token {
        // Pairing hands out the token, so it must survive a restart.
        persist_live_config(&runtime.config_path, &runtime.live)
            .map_err(|err| format!("Failed to save the generated pairing token: {err}"))?;
        println!(
            "Generated a host token for pairing; saved in {}",
            runtime.config_path.display()
        );
    }
    println!(
        "Host name \"{}\"; pairing {}",
        runtime.live.name(),
        if runtime.live.pairing_enabled() {
            "on (clients pair with the password)"
        } else {
            "off"
        }
    );

    let idle_metrics = Arc::clone(&metrics);
    let updater = Arc::new(Updater::new(
        update_options,
        Arc::clone(&runtime.live),
        runtime.config_path.clone(),
        Box::new(move || {
            idle_metrics
                .lock()
                .map(|metrics| metrics.is_idle())
                .unwrap_or(true)
        }),
    ));
    let update_status = updater.status();
    println!(
        "Updates: {} channel ({:?}), automatic checks {}, auto-install {}",
        update_status.channel.as_str(),
        update_status.channel_source,
        if update_status.checks_enabled {
            "on"
        } else {
            "off"
        },
        if update_status.auto_update {
            "on"
        } else {
            "off"
        },
    );
    updater.spawn_periodic_checks();

    // Bound to one non-loopback address (a tailnet IP), the host also answers
    // on 127.0.0.1 so apps on this computer reach it whichever address they
    // saved. Best effort: a taken loopback port leaves the main address up.
    if let Some(loopback_addr) = loopback_companion_addr(&addr) {
        match listen(&loopback_addr) {
            Ok(loopback) => {
                println!("Also listening on http://{loopback_addr} for apps on this computer");
                let runtime = Arc::clone(&runtime);
                let metrics = Arc::clone(&metrics);
                let updater = Arc::clone(&updater);
                let bind_addr = addr.clone();
                thread::spawn(move || serve(&loopback, &runtime, &metrics, &updater, &bind_addr));
            }
            Err(err) => eprintln!("Not also listening on {loopback_addr}: {err}"),
        }
    }

    serve(&server, &runtime, &metrics, &updater, &addr);

    Ok(())
}

/// A fixed number of places, taken without blocking.
struct Slots {
    taken: Arc<AtomicUsize>,
    max: usize,
}

/// A place in `Slots`, given back when dropped.
struct Slot(Arc<AtomicUsize>);

impl Slots {
    fn new(max: usize) -> Self {
        Self {
            taken: Arc::new(AtomicUsize::new(0)),
            max,
        }
    }

    fn try_take(&self) -> Option<Slot> {
        self.taken
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |taken| {
                (taken < self.max).then_some(taken + 1)
            })
            .ok()
            .map(|_| Slot(Arc::clone(&self.taken)))
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Locks a mutex whose critical sections only read or replace the value, so
/// a lock poisoned by a panicking holder still guards a usable one.
pub(super) fn lock_unpoisoned<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    use super::{parse_worker_models, HostLiveConfig, HostMetrics, HostRuntimeConfig};
    use crate::host::test_support::*;
    #[cfg(feature = "whisper")]
    use crate::models::WhisperModel;
    use crate::models::{ModelService, SttModel};
    use std::sync::Arc;
    use std::thread;
    use std::time::{Duration, Instant};

    #[test]
    fn parse_bool_accepts_numeric_and_word_flags() {
        assert_eq!(super::parse_bool("1"), Some(true));
        assert_eq!(super::parse_bool(" TRUE "), Some(true));
        assert_eq!(super::parse_bool("0"), Some(false));
        assert_eq!(super::parse_bool("false"), Some(false));
        assert_eq!(super::parse_bool("yes"), None);
    }

    #[test]
    fn config_update_applies_valid_values_and_rejects_out_of_range_atomically() {
        let config = test_config();
        let live = HostLiveConfig::new(&config);
        assert_eq!(live.max_active_streams(), 1);

        super::apply_config_update(
            &super::HostConfigUpdate {
                max_active_streams: Some(8),
                max_recording_seconds: Some(300),
                use_gpu: Some(false),
                model: None,
                worker_models: None,
                pairing_password: None,
            },
            &live,
        )
        .expect("valid update applies");
        assert_eq!(live.max_active_streams(), 8);
        assert_eq!(live.max_recording_seconds(), 300);
        assert!(!live.use_gpu());

        // One invalid field rejects the whole update without partial writes.
        let err = super::apply_config_update(
            &super::HostConfigUpdate {
                max_active_streams: Some(2),
                max_recording_seconds: Some(5_000),
                use_gpu: Some(true),
                model: None,
                worker_models: None,
                pairing_password: None,
            },
            &live,
        )
        .expect_err("out-of-range update rejects");
        assert!(err.contains("maxRecordingSeconds"));
        assert_eq!(live.max_active_streams(), 8);
        assert_eq!(live.max_recording_seconds(), 300);
        assert!(!live.use_gpu());

        let err = super::apply_config_update(
            &super::HostConfigUpdate {
                max_active_streams: Some(0),
                max_recording_seconds: None,
                use_gpu: None,
                model: None,
                worker_models: None,
                pairing_password: None,
            },
            &live,
        )
        .expect_err("zero streams rejects");
        assert!(err.contains("maxActiveStreams"));
    }

    fn model_update(
        model: Option<&str>,
        worker_models: Option<Vec<&str>>,
    ) -> super::HostConfigUpdate {
        super::HostConfigUpdate {
            max_active_streams: None,
            max_recording_seconds: None,
            use_gpu: None,
            model: model.map(str::to_string),
            worker_models: worker_models
                .map(|models| models.into_iter().map(str::to_string).collect()),
            pairing_password: None,
        }
    }

    #[test]
    fn config_update_sets_served_model_for_all_workers() {
        let config = HostRuntimeConfig {
            worker_count: 2,
            worker_models: vec![SttModel::Parakeet, SttModel::Parakeet],
            ..test_config()
        };
        let live = HostLiveConfig::new(&config);

        super::apply_config_update(&model_update(Some("parakeet"), None), &live)
            .expect("model update applies");
        assert_eq!(
            live.worker_models(),
            vec![SttModel::Parakeet, SttModel::Parakeet]
        );
        assert_eq!(live.model_summary(), "parakeet-tdt-0.6b-v3");

        let err = super::apply_config_update(&model_update(Some("gpt-4"), None), &live)
            .expect_err("unknown model rejects");
        assert!(err.contains("Unsupported model"));
        assert_eq!(live.model_summary(), "parakeet-tdt-0.6b-v3");
    }

    #[test]
    fn config_update_rejects_ambiguous_and_wrong_length_model_lists() {
        let config = HostRuntimeConfig {
            worker_count: 2,
            worker_models: vec![SttModel::Parakeet, SttModel::Parakeet],
            ..test_config()
        };
        let live = HostLiveConfig::new(&config);

        let err = super::apply_config_update(&model_update(None, Some(vec!["parakeet"])), &live)
            .expect_err("wrong list length rejects");
        assert!(err.contains("exactly 2"));

        let err = super::apply_config_update(
            &model_update(Some("parakeet"), Some(vec!["parakeet", "parakeet"])),
            &live,
        )
        .expect_err("ambiguous update rejects");
        assert!(err.contains("not both"));
    }

    #[cfg(feature = "whisper")]
    #[test]
    fn config_update_assigns_whisper_models_per_worker() {
        let config = HostRuntimeConfig {
            worker_count: 2,
            worker_models: vec![SttModel::Parakeet, SttModel::Parakeet],
            ..test_config()
        };
        let live = HostLiveConfig::new(&config);

        super::apply_config_update(
            &model_update(None, Some(vec!["large-v3-turbo", "small"])),
            &live,
        )
        .expect("per-worker update applies");
        assert_eq!(
            live.worker_models(),
            vec![
                SttModel::Whisper(WhisperModel::LargeV3Turbo),
                SttModel::Whisper(WhisperModel::Small)
            ]
        );
        assert_eq!(live.model_summary(), "mixed");
    }

    #[test]
    fn parse_worker_models_expands_defaults_and_rejects_bad_values() {
        assert_eq!(
            parse_worker_models(None, 2).expect("default models"),
            vec![SttModel::Parakeet, SttModel::Parakeet]
        );
        assert_eq!(
            parse_worker_models(Some("parakeet"), 2).expect("single model expands"),
            vec![SttModel::Parakeet, SttModel::Parakeet]
        );

        let err = parse_worker_models(Some("turbo-9000"), 1).expect_err("unknown model fails");
        assert!(err.contains("unsupported model"));
        let err = parse_worker_models(Some("parakeet,parakeet,parakeet"), 2)
            .expect_err("wrong count fails");
        assert!(err.contains("3 models for 2 workers"));
    }

    #[cfg(feature = "whisper")]
    #[test]
    fn parse_worker_models_assigns_whisper_models_per_worker() {
        assert_eq!(
            parse_worker_models(Some(" large-v3-turbo , small "), 2).expect("per-worker list"),
            vec![
                SttModel::Whisper(WhisperModel::LargeV3Turbo),
                SttModel::Whisper(WhisperModel::Small)
            ]
        );
    }

    #[test]
    fn persisted_config_round_trips_dashboard_edits() {
        let path = std::env::temp_dir().join(format!(
            "fairspoken-host-config-roundtrip-{}.json",
            std::process::id()
        ));
        let config = HostRuntimeConfig {
            worker_count: 2,
            worker_models: vec![SttModel::Parakeet, SttModel::Parakeet],
            ..test_config()
        };
        let live = HostLiveConfig::new(&config);
        super::apply_config_update(
            &super::HostConfigUpdate {
                max_active_streams: Some(8),
                max_recording_seconds: Some(120),
                use_gpu: Some(false),
                model: None,
                worker_models: Some(vec!["parakeet".to_string(), "parakeet".to_string()]),
                pairing_password: None,
            },
            &live,
        )
        .expect("dashboard edit applies");

        super::persist_live_config(&path, &live).expect("persist");
        let persisted = super::load_persisted_config(&path)
            .expect("load")
            .expect("config present after an edit");

        let mut restarted = HostRuntimeConfig {
            worker_count: 2,
            worker_models: vec![SttModel::Parakeet, SttModel::Parakeet],
            ..test_config()
        };
        super::overlay_persisted_config(&mut restarted, persisted);
        assert_eq!(restarted.max_active_streams, 8);
        assert_eq!(restarted.max_recording_seconds, 120);
        assert!(!restarted.use_gpu);
        assert_eq!(
            restarted.worker_models,
            vec![SttModel::Parakeet, SttModel::Parakeet]
        );

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn config_update_sets_and_clears_the_pairing_password() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("host-config.json");
        let config = test_config();
        let live = HostLiveConfig::new(&config);
        let update = |body: &str| -> Result<(), String> {
            let update: super::HostConfigUpdate =
                serde_json::from_str(body).map_err(|err| err.to_string())?;
            super::apply_config_update(&update, &live)
        };

        // Too short: rejected with nothing applied, and the error doesn't echo it.
        let err = update(r#"{"maxActiveStreams":3,"pairingPassword":"abc"}"#).unwrap_err();
        assert!(!err.contains("abc"));
        assert_eq!(live.max_active_streams(), 1);
        assert!(!live.pairing_enabled());

        // A host with no token generates one so pairing has something to hand out.
        update(r#"{"pairingPassword":"123456"}"#).unwrap();
        assert!(live.pairing_enabled());
        let token = live.token().expect("generated token");
        super::persist_live_config(&path, &live).unwrap();
        let saved = super::load_persisted_config(&path).unwrap().unwrap();
        assert_eq!(saved.token.as_deref(), Some(token.as_str()));
        assert_eq!(saved.pairing_password.as_deref(), Some("123456"));

        let metrics = HostMetrics::new(&config);
        let stats =
            serde_json::to_value(metrics.snapshot(
                "127.0.0.1:0",
                &live,
                inventory(&ModelService::default(), &live),
            ))
            .unwrap();
        assert_eq!(stats["pairingEnabled"], true);
        assert!(!stats.to_string().contains("123456"));

        // Absent leaves it alone; null and "" turn it off and keep the token.
        update(r#"{"useGpu":false}"#).unwrap();
        assert!(live.pairing_enabled());
        update(r#"{"pairingPassword":null}"#).unwrap();
        assert!(!live.pairing_enabled());
        assert_eq!(live.token(), Some(token));
        update(r#"{"pairingPassword":"654321"}"#).unwrap();
        update(r#"{"pairingPassword":""}"#).unwrap();
        assert!(!live.pairing_enabled());
        assert!(update(r#"{"pairingPassword":123456}"#).is_err());
    }

    #[test]
    fn overlay_clamps_ranges_and_adapts_stale_worker_model_lists() {
        let mut config = HostRuntimeConfig {
            worker_count: 2,
            worker_models: vec![SttModel::Parakeet, SttModel::Parakeet],
            ..test_config()
        };
        super::overlay_persisted_config(
            &mut config,
            super::PersistedHostConfig {
                max_active_streams: 999,
                max_recording_seconds: 1,
                use_gpu: false,
                // Written when the host ran three workers; now it runs two.
                worker_models: vec![SttModel::Parakeet, SttModel::Parakeet, SttModel::Parakeet],
                update: Default::default(),
                token: None,
                pairing_password: None,
                name: None,
                super_mode: None,
            },
        );
        assert_eq!(config.max_active_streams, 32);
        assert_eq!(config.max_recording_seconds, 10);
        assert!(!config.use_gpu);
        assert_eq!(
            config.worker_models,
            vec![SttModel::Parakeet, SttModel::Parakeet]
        );
    }

    #[test]
    fn missing_config_file_is_first_boot_and_corrupt_file_fails() {
        let missing = std::env::temp_dir().join(format!(
            "fairspoken-host-config-missing-{}.json",
            std::process::id()
        ));
        assert!(super::load_persisted_config(&missing)
            .expect("missing file is fine")
            .is_none());

        let corrupt = std::env::temp_dir().join(format!(
            "fairspoken-host-config-corrupt-{}.json",
            std::process::id()
        ));
        std::fs::write(&corrupt, b"{not json").expect("write corrupt fixture");
        let err = super::load_persisted_config(&corrupt).expect_err("corrupt file fails");
        assert!(err.contains("Invalid host config"));
        let _ = std::fs::remove_file(corrupt);
    }

    #[test]
    fn concurrent_config_writes_leave_a_whole_private_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("host-config.json");
        let live = Arc::new(HostLiveConfig::new(&test_config()));
        super::persist_live_config(&path, &live).expect("first write");

        let writers: Vec<_> = (0..4)
            .map(|writer| {
                let live = Arc::clone(&live);
                let path = path.clone();
                thread::spawn(move || {
                    for round in 0..25 {
                        let streams = 1 + (writer * 25 + round) % 32;
                        live.max_active_streams
                            .store(streams, std::sync::atomic::Ordering::Relaxed);
                        super::persist_live_config(&path, &live).expect("write");
                    }
                })
            })
            .collect();
        let reader = {
            let path = path.clone();
            thread::spawn(move || {
                for _ in 0..200 {
                    super::load_persisted_config(&path)
                        .expect("never a torn file")
                        .expect("never missing");
                }
            })
        };
        for writer in writers {
            writer.join().unwrap();
        }
        reader.join().unwrap();

        let saved = super::load_persisted_config(&path).unwrap().unwrap();
        assert_eq!(saved.max_active_streams, live.max_active_streams());
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(leftovers, vec![std::ffi::OsString::from("host-config.json")]);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    /// Serves files from `routes` (path → body) until the test ends.
    fn serve_files(routes: Vec<(String, Vec<u8>)>) -> String {
        let server = tiny_http::Server::http("127.0.0.1:0").expect("bind feed");
        let addr = server.server_addr().to_ip().expect("ip address");
        thread::spawn(move || {
            for request in server.incoming_requests() {
                let found = routes
                    .iter()
                    .find(|(path, _)| path == request.url())
                    .map(|(_, body)| body.clone());
                let _ = match found {
                    Some(body) => request.respond(tiny_http::Response::from_data(body)),
                    None => request
                        .respond(tiny_http::Response::from_string("missing").with_status_code(404)),
                };
            }
        });
        format!("http://{addr}/")
    }

    fn wait_for_state(
        updater: &super::Updater,
        busy: &[super::update::UpdatePhase],
    ) -> super::update::UpdateStatus {
        let started = Instant::now();
        loop {
            let status = updater.status();
            if !busy.contains(&status.state) {
                return status;
            }
            assert!(
                started.elapsed() < Duration::from_secs(20),
                "updater stuck in {:?}",
                status.state
            );
            thread::sleep(Duration::from_millis(20));
        }
    }

    fn host_manifest(version: &str, channel: &str, asset: serde_json::Value) -> Vec<u8> {
        let platform = super::update::current_platform_key().unwrap_or("linux-x86_64");
        serde_json::json!({
            "version": version,
            "channel": channel,
            "pub_date": "2026-10-05T12:00:00Z",
            "notes": format!("https://github.com/sprintengine/fairspoken/releases/tag/v{version}"),
            "platforms": { platform: asset },
        })
        .to_string()
        .into_bytes()
    }

    #[test]
    fn update_settings_persist_and_resolve_the_channel() {
        use super::update::{ChannelSource, UpdateChannel, UpdateSettingsRequest};
        let dir = tempfile::tempdir().unwrap();
        let runtime = updater_runtime(dir.path().join("host-config.json"));
        let updater = test_updater(&runtime, "0.3.0-nightly.20261005.4", "http://127.0.0.1:9/");

        let status = updater.status();
        assert_eq!(status.channel, UpdateChannel::Nightly);
        assert_eq!(status.channel_source, ChannelSource::Version);
        assert!(!status.auto_update);

        let settings: UpdateSettingsRequest =
            serde_json::from_str(r#"{"channel":"stable","autoUpdate":true}"#).unwrap();
        updater.apply_settings(&settings).expect("settings apply");
        let status = updater.status();
        assert_eq!(status.channel, UpdateChannel::Stable);
        assert_eq!(status.channel_source, ChannelSource::Saved);
        assert!(status.auto_update);

        let persisted = super::load_persisted_config(&runtime.config_path)
            .unwrap()
            .expect("saved");
        assert_eq!(persisted.update.update_channel, Some(UpdateChannel::Stable));
        assert!(persisted.update.auto_update);
        // The dashboard configuration is saved alongside, unchanged.
        assert_eq!(persisted.max_active_streams, 1);

        // A config edit keeps the update settings.
        super::persist_live_config(&runtime.config_path, &runtime.live).unwrap();
        let persisted = super::load_persisted_config(&runtime.config_path)
            .unwrap()
            .unwrap();
        assert!(persisted.update.auto_update);

        let bad: UpdateSettingsRequest = serde_json::from_str(r#"{"channel":"beta"}"#).unwrap();
        assert!(updater.apply_settings(&bad).is_err());
        assert_eq!(updater.status().channel, UpdateChannel::Stable);

        // null follows the running version again.
        let follow: UpdateSettingsRequest = serde_json::from_str(r#"{"channel":null}"#).unwrap();
        updater.apply_settings(&follow).unwrap();
        assert_eq!(updater.status().channel_source, ChannelSource::Version);
        let _ = wait_for_state(&updater, &[super::update::UpdatePhase::Checking]);
    }

    #[test]
    fn update_check_reads_the_channel_feed_and_offers_switch_to_stable() {
        use super::update::UpdatePhase;
        let asset = serde_json::json!({
            "url": "https://example.test/h.tar.gz",
            "sha256": "a".repeat(64),
            "signature": "c2ln",
            "format": "tar.gz",
        });
        let feed = serve_files(vec![
            (
                "/host-stable.json".to_string(),
                host_manifest("0.2.0", "stable", asset.clone()),
            ),
            (
                "/host-nightly.json".to_string(),
                host_manifest("0.3.0-nightly.20261005.4", "nightly", asset),
            ),
        ]);
        let dir = tempfile::tempdir().unwrap();

        // A stable build on the stable feed: up to date.
        let runtime = updater_runtime(dir.path().join("a.json"));
        let updater = test_updater(&runtime, "0.2.0", &feed);
        updater.begin_check(false).unwrap();
        let status = wait_for_state(&updater, &[UpdatePhase::Checking]);
        assert_eq!(status.state, UpdatePhase::Idle, "{:?}", status.error);
        assert!(status.last_checked_ms.is_some());

        // A nightly build switched to stable: the lower stable is offered.
        let runtime = updater_runtime(dir.path().join("b.json"));
        runtime.live.set_update_prefs(super::update::UpdatePrefs {
            update_channel: Some(super::update::UpdateChannel::Stable),
            auto_update: false,
        });
        let updater = test_updater(&runtime, "0.2.1-nightly.20261001.1", &feed);
        updater.begin_check(false).unwrap();
        let status = wait_for_state(&updater, &[UpdatePhase::Checking]);
        assert_eq!(status.state, UpdatePhase::Available, "{:?}", status.error);
        let available = status.available.expect("offer");
        assert_eq!(available.version, "0.2.0");
        assert!(available.switch_to_stable);

        // A stable build on the nightly channel gets the nightly.
        let runtime = updater_runtime(dir.path().join("c.json"));
        runtime.live.set_update_prefs(super::update::UpdatePrefs {
            update_channel: Some(super::update::UpdateChannel::Nightly),
            auto_update: false,
        });
        let updater = test_updater(&runtime, "0.2.0", &feed);
        updater.begin_check(false).unwrap();
        let status = wait_for_state(&updater, &[UpdatePhase::Checking]);
        assert_eq!(
            status.available.map(|update| update.version).as_deref(),
            Some("0.3.0-nightly.20261005.4")
        );

        // No feed published yet (404 before the first release): up to date.
        let runtime = updater_runtime(dir.path().join("e.json"));
        let updater = test_updater(&runtime, "0.2.0", &serve_files(Vec::new()));
        updater.begin_check(false).unwrap();
        let status = wait_for_state(&updater, &[UpdatePhase::Checking]);
        assert_eq!(status.state, UpdatePhase::Idle, "{:?}", status.error);
        assert!(status.available.is_none() && status.error.is_none());

        // Nothing to install yet → install refuses; a dead feed is an error.
        let runtime = updater_runtime(dir.path().join("d.json"));
        let updater = test_updater(&runtime, "0.2.0", "http://127.0.0.1:9/");
        assert!(updater.begin_install(false).is_err());
        updater.begin_check(false).unwrap();
        let status = wait_for_state(&updater, &[UpdatePhase::Checking]);
        assert_eq!(status.state, UpdatePhase::Error);
        assert!(status.error.is_some());
        assert!(updater.begin_restart().is_err());
    }

    #[cfg(unix)]
    #[test]
    fn update_installs_end_to_end_from_a_local_feed() {
        use super::update::test_support::{script, sha256_hex, sign, tar_gz, test_key};
        use super::update::UpdatePhase;
        let key = test_key();
        let archive = tar_gz(&[
            (
                "fairspoken-0.3.0-transcription-host-linux-x64/LICENSE",
                b"mit",
            ),
            (
                "fairspoken-0.3.0-transcription-host-linux-x64/transcription-host",
                &script("0.3.0", true),
            ),
        ]);
        let base = serve_files(vec![("/h.tar.gz".to_string(), archive.clone())]);
        let asset = serde_json::json!({
            "url": format!("{base}h.tar.gz"),
            "sha256": sha256_hex(&archive),
            "signature": sign(&key, &archive),
            "format": "tar.gz",
        });
        let feed = serve_files(vec![(
            "/host-stable.json".to_string(),
            host_manifest("0.3.0", "stable", asset),
        )]);

        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("transcription-host");
        std::fs::write(&exe, script("0.2.0", true)).unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let runtime = updater_runtime(dir.path().join("host-config.json"));
        let updater = test_updater_with(&runtime, "0.2.0", &feed, &key.public_base64, exe.clone());

        updater.begin_check(false).unwrap();
        let status = wait_for_state(&updater, &[UpdatePhase::Checking]);
        assert_eq!(status.state, UpdatePhase::Available, "{:?}", status.error);
        updater.begin_install(false).unwrap();
        let status = wait_for_state(&updater, &[UpdatePhase::Downloading]);
        assert_eq!(status.state, UpdatePhase::Ready, "{:?}", status.error);
        assert_eq!(status.installed_version.as_deref(), Some("0.3.0"));
        super::update::smoke_check(&exe, "0.3.0").expect("new binary installed");
        super::update::smoke_check(&dir.path().join("transcription-host.previous"), "0.2.0")
            .expect("previous kept");
        // Ready: a further check or install is refused until the restart.
        assert!(updater.begin_check(false).is_err());
        assert!(updater.begin_install(false).is_err());
    }
}
