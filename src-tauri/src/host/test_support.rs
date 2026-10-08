//! Helpers shared by the host's unit tests.

use super::*;

pub(super) fn test_config() -> HostRuntimeConfig {
    HostRuntimeConfig {
        worker_count: 1,
        queue_capacity: 1,
        max_active_streams: 1,
        max_recording_seconds: 10,
        use_gpu: true,
        worker_models: vec![SttModel::Parakeet],
        super_mode: super::super_mode::SuperModePolicy::Allow,
    }
}

pub(super) fn test_runtime(
    config: HostRuntimeConfig,
    job_tx: mpsc::SyncSender<super::TranscriptionJob>,
    metrics: Arc<Mutex<HostMetrics>>,
) -> HostRuntime {
    HostRuntime {
        job_tx,
        metrics,
        live: Arc::new(HostLiveConfig::new(&config)),
        models: ModelService::default(),
        config_path: std::env::temp_dir().join(format!(
            "fairspoken-host-config-test-{}.json",
            std::process::id()
        )),
        next_job_id: AtomicU64::new(1),
        pairing: Mutex::new(super::PairingLimiter::default()),
        batch_admissions: super::Slots::new(config.worker_count + config.queue_capacity),
        public_handlers: super::Slots::new(super::MAX_PUBLIC_HANDLERS),
        authenticated_handlers: super::Slots::new(super::MAX_AUTHENTICATED_HANDLERS),
    }
}

pub(super) fn inventory(models: &ModelService, live: &HostLiveConfig) -> super::ModelInventory {
    super::ModelInventory::read(models, live.worker_models())
}

/// Serves `runtime` on a loopback port through the host's own accept
/// loop until the test process ends.
pub(super) fn serve_test_host(runtime: Arc<HostRuntime>) -> std::net::SocketAddr {
    let updater = test_updater(&runtime, "0.2.0", "http://127.0.0.1:9/");
    let metrics = Arc::clone(&runtime.metrics);
    let server = tiny_http::Server::http("127.0.0.1:0").expect("bind");
    let addr = server.server_addr().to_ip().expect("ip address");
    thread::spawn(move || super::serve(&server, &runtime, &metrics, &updater, "127.0.0.1:0"));
    addr
}

/// Sends one raw request (asking for `Connection: close`) and returns the
/// answer's status code and body.
pub(super) fn exchange(addr: std::net::SocketAddr, raw: &str) -> (u16, String) {
    use std::io::{Read, Write};
    let mut socket = std::net::TcpStream::connect(addr).expect("connect");
    socket
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("timeout");
    socket.write_all(raw.as_bytes()).expect("send request");
    let mut response = String::new();
    socket
        .read_to_string(&mut response)
        .unwrap_or_else(|err| panic!("no answer to {:?}: {err}", raw.lines().next()));
    let status = response
        .split(' ')
        .nth(1)
        .and_then(|code| code.parse().ok())
        .unwrap_or_else(|| panic!("no status line in {response:?}"));
    let body = response
        .split_once("\r\n\r\n")
        .map(|(_, body)| body.to_string())
        .unwrap_or_default();
    (status, body)
}

pub(super) fn token_runtime(
    config_path: std::path::PathBuf,
    auth: super::HostAuth,
) -> Arc<HostRuntime> {
    let runtime = updater_runtime(config_path);
    runtime.live.set_auth(auth);
    Arc::new(runtime)
}

pub(super) fn test_updater(
    runtime: &HostRuntime,
    current_version: &str,
    feed_base: &str,
) -> Arc<super::Updater> {
    test_updater_with(
        runtime,
        current_version,
        feed_base,
        super::update::UPDATER_PUBLIC_KEY,
        std::env::temp_dir().join("transcription-host-never-replaced"),
    )
}

pub(super) fn test_updater_with(
    runtime: &HostRuntime,
    current_version: &str,
    feed_base: &str,
    public_key: &str,
    exe_path: std::path::PathBuf,
) -> Arc<super::Updater> {
    Arc::new(super::Updater::new(
        super::UpdaterOptions {
            current_version: current_version.to_string(),
            feed_base: feed_base.to_string(),
            public_key: public_key.to_string(),
            exe_path,
            restart_mode: super::update::RestartMode::Reexec,
            checks_enabled: false,
            env_channel: None,
        },
        Arc::clone(&runtime.live),
        runtime.config_path.clone(),
        Box::new(|| true),
    ))
}

pub(super) fn updater_runtime(config_path: std::path::PathBuf) -> HostRuntime {
    let config = test_config();
    let metrics = Arc::new(Mutex::new(HostMetrics::new(&config)));
    let (job_tx, _job_rx) = mpsc::sync_channel(1);
    let mut runtime = test_runtime(config, job_tx, metrics);
    runtime.config_path = config_path;
    runtime
}
