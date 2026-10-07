//! Finds Fairspoken transcription hosts on the user's tailnet and pairs with
//! one (PROTOCOL.md "Discovery and pairing"). The Tailscale CLI lists the
//! tailnet's machines; each online computer is probed for `GET /v1/hello`
//! on its MagicDNS name (HTTPS, then HTTP, for a host published with
//! `tailscale serve`) and on its IPv4 address and the default port (for one
//! bound to its tailnet address).

use reqwest::blocking::Client;
use reqwest::{StatusCode, Url};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

pub const DEFAULT_HOST_PORT: u16 = 48173;
const MAX_PARALLEL_PROBES: usize = 32;
const SCAN_PROBE_TIMEOUT: Duration = Duration::from_millis(1500);
/// A typed name may need a cold DNS lookup, so it gets longer than a scan.
const TYPED_PROBE_TIMEOUT: Duration = Duration::from_secs(4);
const STATUS_TIMEOUT: Duration = Duration::from_secs(5);
const PAIR_TIMEOUT: Duration = Duration::from_secs(10);
const HOST_SERVICE: &str = "fairspoken-host";

/// A host that answered `/v1/hello`, as the settings list shows it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscoveredHost {
    /// The display name the host reports.
    pub name: String,
    /// The tailnet machine it runs on (or the address the user typed).
    pub machine: String,
    /// Base URL to save as the remote host URL.
    pub url: String,
    /// `none`, `password` (pair with a password) or `token` (paste the token).
    pub auth: String,
    pub server_version: Option<String>,
    /// This computer's own host.
    pub is_self: bool,
}

/// Lists every Fairspoken host on the tailnet, this computer's included.
pub fn discover_hosts() -> Result<Vec<DiscoveredHost>, String> {
    let status = load_tailscale_status()?;
    let client = probe_client(SCAN_PROBE_TIMEOUT)?;
    Ok(probe_candidates(&client, &scan_candidates(&status)))
}

/// Probes a typed machine name, `name:port` or URL the same way a scan
/// probes a peer. A bare machine name is expanded to its MagicDNS name when
/// the Tailscale CLI knows it, so the HTTPS certificate matches.
pub fn probe_address(input: &str) -> Result<DiscoveredHost, String> {
    let address = parse_typed_address(input)?;
    let full_name = match &address {
        TypedAddress::Name { host, .. } if !host.contains('.') => load_tailscale_status()
            .ok()
            .and_then(|status| magic_dns_name(&status, host)),
        _ => None,
    };
    let candidate = typed_candidate(&address, full_name.as_deref());
    let client = probe_client(TYPED_PROBE_TIMEOUT)?;
    probe_candidates(&client, std::slice::from_ref(&candidate))
        .into_iter()
        .next()
        .ok_or_else(|| {
            format!(
                "No Fairspoken host answered at {}. Check the name, that the host is running, and that it accepts connections from your tailnet.",
                input.trim()
            )
        })
}

/// Trades the host's pairing password for its token. `Ok(None)` is a host
/// without a token, where there is nothing to pair.
pub fn pair_with_host(
    url: &str,
    password: &str,
    client_name: &str,
) -> Result<Option<String>, String> {
    let endpoint = endpoint(url, "v1/pair")?;
    let client = Client::builder()
        .timeout(PAIR_TIMEOUT)
        .build()
        .map_err(|err| format!("Failed to create the pairing client: {err}"))?;
    let response = client
        .post(endpoint)
        .json(&serde_json::json!({ "password": password, "clientName": client_name }))
        .send()
        .map_err(|err| format!("Couldn't reach the host: {err}"))?;
    let status = response.status();
    let body = response
        .json::<serde_json::Value>()
        .unwrap_or(serde_json::Value::Null);
    pair_result(status, &body)
}

fn pair_result(status: StatusCode, body: &serde_json::Value) -> Result<Option<String>, String> {
    let message = body.get("error").and_then(|value| value.as_str());
    match status.as_u16() {
        200 => match body.get("token") {
            Some(serde_json::Value::String(token)) if !token.is_empty() => Ok(Some(token.clone())),
            Some(serde_json::Value::Null) => Ok(None),
            _ => Err("The host's pairing answer had no token.".to_string()),
        },
        401 => Err("That password is wrong.".to_string()),
        404 => Err(
            "This host has no pairing password. Paste its token in the Token field instead."
                .to_string(),
        ),
        429 => {
            let seconds = body
                .get("retryAfterSeconds")
                .and_then(|value| value.as_u64())
                .unwrap_or(600);
            let minutes = seconds.div_ceil(60).max(1);
            Err(format!(
                "Too many wrong passwords. Try again in {minutes} minute{}.",
                if minutes == 1 { "" } else { "s" }
            ))
        }
        _ => Err(match message {
            Some(message) => format!("The host refused to pair ({status}): {message}"),
            None => format!("The host refused to pair: {status}"),
        }),
    }
}

// ───────────────────────────── Tailscale CLI ─────────────────────────────

#[derive(Debug, Default, Deserialize)]
struct TailscaleStatus {
    #[serde(rename = "BackendState", default)]
    backend_state: String,
    #[serde(rename = "Self")]
    self_node: Option<TailscaleNode>,
    /// `null` on a tailnet with no other machines.
    #[serde(rename = "Peer", default, deserialize_with = "null_as_empty")]
    peers: HashMap<String, TailscaleNode>,
}

fn null_as_empty<'de, D>(deserializer: D) -> Result<HashMap<String, TailscaleNode>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Option::deserialize(deserializer)?.unwrap_or_default())
}

#[derive(Clone, Debug, Default, Deserialize)]
struct TailscaleNode {
    #[serde(rename = "HostName", default)]
    host_name: String,
    #[serde(rename = "DNSName", default)]
    dns_name: String,
    #[serde(rename = "TailscaleIPs", default)]
    tailscale_ips: Vec<String>,
    #[serde(rename = "Online", default)]
    online: bool,
    #[serde(rename = "OS", default)]
    os: String,
}

impl TailscaleNode {
    fn dns_name(&self) -> Option<&str> {
        Some(self.dns_name.trim_end_matches('.')).filter(|name| !name.is_empty())
    }

    fn ipv4(&self) -> Option<std::net::Ipv4Addr> {
        self.tailscale_ips
            .iter()
            .find_map(|ip| ip.parse::<std::net::Ipv4Addr>().ok())
    }

    /// Phones and TVs can't run a host, so a scan doesn't probe them.
    fn is_phone_or_tv(&self) -> bool {
        ["ios", "android", "tvos"]
            .iter()
            .any(|os| self.os.eq_ignore_ascii_case(os))
    }

    /// The human name ("Conal's Mac mini"), else the first MagicDNS label.
    fn machine(&self) -> String {
        let name = self.host_name.trim();
        if !name.is_empty() {
            return name.to_string();
        }
        self.dns_name()
            .and_then(|dns| dns.split('.').next())
            .unwrap_or_default()
            .to_string()
    }
}

fn load_tailscale_status() -> Result<TailscaleStatus, String> {
    let cli = find_tailscale_cli().ok_or_else(|| NOT_INSTALLED.to_string())?;
    let output = run_with_timeout(&cli, &["status", "--json"], STATUS_TIMEOUT)?;
    interpret_status(&output.stdout, &output.stderr)
}

const NOT_INSTALLED: &str =
    "Tailscale isn't installed on this computer. Install it, or type the host's address instead.";
const NOT_RUNNING: &str = "Tailscale isn't running. Open Tailscale, connect, and try again.";

/// Reads `tailscale status --json`. The CLI prints JSON (with a
/// `BackendState`) even when it isn't connected, and only stderr when it
/// can't reach the Tailscale service at all.
fn interpret_status(stdout: &[u8], stderr: &[u8]) -> Result<TailscaleStatus, String> {
    let Ok(status) = serde_json::from_slice::<TailscaleStatus>(stdout) else {
        let stderr = String::from_utf8_lossy(stderr).trim().to_string();
        return Err(if stderr.is_empty() {
            NOT_RUNNING.to_string()
        } else {
            format!("{NOT_RUNNING} ({stderr})")
        });
    };
    match status.backend_state.as_str() {
        "Running" => Ok(status),
        "NeedsLogin" => {
            Err("Tailscale is logged out. Sign in to Tailscale and try again.".to_string())
        }
        "NeedsMachineAuth" => Err(
            "This computer is waiting for approval in your tailnet's admin console.".to_string(),
        ),
        "Stopped" => Err("Tailscale is turned off. Connect it and try again.".to_string()),
        "Starting" | "NoState" => {
            Err("Tailscale is still starting. Try again in a moment.".to_string())
        }
        "" => Err(NOT_RUNNING.to_string()),
        other => Err(format!("Tailscale isn't connected (state: {other}).")),
    }
}

/// `tailscale` on `PATH`, then where the desktop apps install it. GUI apps
/// often get a minimal `PATH`, so the fixed locations matter.
fn find_tailscale_cli() -> Option<PathBuf> {
    let file_name = if cfg!(windows) {
        "tailscale.exe"
    } else {
        "tailscale"
    };
    let on_path = std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|dir| dir.join(file_name))
            .find(|candidate| candidate.is_file())
    });
    on_path.or_else(|| {
        fixed_cli_locations()
            .into_iter()
            .find(|path| path.is_file())
    })
}

fn fixed_cli_locations() -> Vec<PathBuf> {
    let mut locations = Vec::new();
    if cfg!(target_os = "macos") {
        locations.push(PathBuf::from(
            "/Applications/Tailscale.app/Contents/MacOS/Tailscale",
        ));
        locations.push(PathBuf::from("/usr/local/bin/tailscale"));
        locations.push(PathBuf::from("/opt/homebrew/bin/tailscale"));
    } else if cfg!(windows) {
        let program_files =
            std::env::var_os("ProgramFiles").unwrap_or_else(|| "C:\\Program Files".into());
        locations.push(
            PathBuf::from(program_files)
                .join("Tailscale")
                .join("tailscale.exe"),
        );
    } else {
        locations.push(PathBuf::from("/usr/bin/tailscale"));
        locations.push(PathBuf::from("/usr/local/bin/tailscale"));
    }
    locations
}

struct CommandOutput {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

/// Runs the CLI without letting a wedged Tailscale service hang the scan.
fn run_with_timeout(
    program: &Path,
    args: &[&str],
    timeout: Duration,
) -> Result<CommandOutput, String> {
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        // No console window flashing up from a GUI app.
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = command.spawn().map_err(|err| {
        if err.kind() == std::io::ErrorKind::NotFound {
            NOT_INSTALLED.to_string()
        } else {
            format!("Couldn't run Tailscale ({}): {err}", program.display())
        }
    })?;
    let read_all = |pipe: Option<Box<dyn Read + Send>>| {
        thread::spawn(move || {
            let mut bytes = Vec::new();
            if let Some(mut pipe) = pipe {
                let _ = pipe.read_to_end(&mut bytes);
            }
            bytes
        })
    };
    let stdout = read_all(
        child
            .stdout
            .take()
            .map(|pipe| Box::new(pipe) as Box<dyn Read + Send>),
    );
    let stderr = read_all(
        child
            .stderr
            .take()
            .map(|pipe| Box::new(pipe) as Box<dyn Read + Send>),
    );
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(25)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(
                    "Tailscale didn't answer in time. Check that it's running and try again."
                        .to_string(),
                );
            }
            Err(err) => return Err(format!("Couldn't run Tailscale: {err}")),
        }
    }
    Ok(CommandOutput {
        stdout: stdout.join().unwrap_or_default(),
        stderr: stderr.join().unwrap_or_default(),
    })
}

/// The MagicDNS name of the machine whose host name or first DNS label is
/// `name` (case-insensitive).
fn magic_dns_name(status: &TailscaleStatus, name: &str) -> Option<String> {
    status
        .self_node
        .iter()
        .chain(status.peers.values())
        .find(|node| {
            node.host_name.eq_ignore_ascii_case(name)
                || node
                    .dns_name()
                    .and_then(|dns| dns.split('.').next())
                    .is_some_and(|label| label.eq_ignore_ascii_case(name))
        })
        .and_then(|node| node.dns_name().map(str::to_string))
}

// ───────────────────────────── Candidates ─────────────────────────────

/// One machine to probe, with its base URLs in order of preference.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Candidate {
    machine: String,
    urls: Vec<String>,
    is_self: bool,
}

/// `Self` and every online peer that isn't a phone or TV: its MagicDNS name
/// over HTTPS, then plain HTTP (both `tailscale serve`), then plain HTTP on
/// its IPv4 address and the default port.
fn scan_candidates(status: &TailscaleStatus) -> Vec<Candidate> {
    let mut peers: Vec<&TailscaleNode> = status
        .peers
        .values()
        .filter(|node| node.online && !node.is_phone_or_tv())
        .collect();
    peers.sort_by_key(|node| node.machine().to_lowercase());
    status
        .self_node
        .iter()
        .map(|node| (node, true))
        .chain(peers.into_iter().map(|node| (node, false)))
        .filter_map(|(node, is_self)| {
            let mut urls = Vec::new();
            if let Some(dns) = node.dns_name() {
                urls.push(format!("https://{dns}"));
                urls.push(format!("http://{dns}"));
            }
            if let Some(ip) = node.ipv4() {
                urls.push(format!("http://{ip}:{DEFAULT_HOST_PORT}"));
            }
            (!urls.is_empty()).then(|| Candidate {
                machine: node.machine(),
                urls,
                is_self,
            })
        })
        .collect()
}

#[derive(Debug, PartialEq, Eq)]
enum TypedAddress {
    /// A full URL: probed exactly as typed.
    Url(String),
    Name {
        host: String,
        port: Option<u16>,
    },
}

fn parse_typed_address(input: &str) -> Result<TypedAddress, String> {
    let input = input.trim().trim_end_matches('/');
    if input.is_empty() {
        return Err("Type the host's machine name, or name:port.".to_string());
    }
    if input.starts_with("http://") || input.starts_with("https://") {
        let url = Url::parse(input).map_err(|err| format!("That address isn't valid: {err}"))?;
        if url.host_str().is_none() {
            return Err("That address has no host name.".to_string());
        }
        return Ok(TypedAddress::Url(input.to_string()));
    }
    let (host, port) = match input.rsplit_once(':') {
        Some((host, port)) if !host.contains(':') => {
            let port = port
                .parse::<u16>()
                .ok()
                .filter(|port| *port > 0)
                .ok_or_else(|| format!("{port} isn't a port number."))?;
            (host, Some(port))
        }
        _ => (input, None),
    };
    let host = host.trim_end_matches('.');
    if host.is_empty()
        || !host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_'))
    {
        return Err(format!(
            "{input} isn't a machine name. Type a name like studio-mac or studio-mac:8080."
        ));
    }
    Ok(TypedAddress::Name {
        host: host.to_string(),
        port,
    })
}

fn typed_candidate(address: &TypedAddress, full_name: Option<&str>) -> Candidate {
    match address {
        TypedAddress::Url(url) => Candidate {
            machine: Url::parse(url)
                .ok()
                .and_then(|url| url.host_str().map(str::to_string))
                .unwrap_or_else(|| url.clone()),
            urls: vec![url.clone()],
            is_self: false,
        },
        TypedAddress::Name { host, port } => {
            // `tailscale serve` certificates name the full `*.ts.net` host, so
            // a bare name the CLI couldn't expand is only tried over HTTP.
            let https_host = full_name.or_else(|| host.contains('.').then_some(host.as_str()));
            let mut urls = Vec::new();
            match port {
                Some(port) => {
                    urls.extend(https_host.map(|https| format!("https://{https}:{port}")));
                    urls.push(format!("http://{host}:{port}"));
                }
                None => {
                    urls.extend(https_host.map(|https| format!("https://{https}")));
                    urls.push(format!("http://{host}"));
                    urls.push(format!("http://{host}:{DEFAULT_HOST_PORT}"));
                }
            }
            Candidate {
                machine: host.clone(),
                urls,
                is_self: false,
            }
        }
    }
}

// ───────────────────────────── Probing ─────────────────────────────

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Hello {
    service: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    server_version: Option<String>,
    #[serde(default)]
    auth: String,
}

fn probe_client(timeout: Duration) -> Result<Client, String> {
    Client::builder()
        .timeout(timeout)
        .connect_timeout(timeout)
        .build()
        .map_err(|err| format!("Failed to create the discovery client: {err}"))
}

fn endpoint(base_url: &str, path: &str) -> Result<Url, String> {
    let base = Url::parse(&format!("{}/", base_url.trim().trim_end_matches('/')))
        .map_err(|err| format!("Invalid host URL: {err}"))?;
    base.join(path)
        .map_err(|err| format!("Invalid host URL: {err}"))
}

/// `GET <url>/v1/hello`; only a Fairspoken host's answer counts.
fn probe_hello(client: &Client, base_url: &str) -> Option<Hello> {
    let response = client
        .get(endpoint(base_url, "v1/hello").ok()?)
        .send()
        .ok()?;
    if !response.status().is_success() {
        return None;
    }
    response
        .json::<Hello>()
        .ok()
        .filter(|hello| hello.service == HOST_SERVICE)
}

/// Probes every candidate URL with at most 32 requests in flight, then keeps
/// each machine once, at its most preferred URL that answered (HTTPS over
/// HTTP), and each URL once.
fn probe_candidates(client: &Client, candidates: &[Candidate]) -> Vec<DiscoveredHost> {
    let work: VecDeque<(usize, usize)> = candidates
        .iter()
        .enumerate()
        .flat_map(|(machine, candidate)| (0..candidate.urls.len()).map(move |url| (machine, url)))
        .collect();
    let total = work.len();
    let work = Arc::new(Mutex::new(work));
    let answers = Arc::new(Mutex::new(HashMap::<(usize, usize), Hello>::new()));
    let workers: Vec<_> = (0..total.min(MAX_PARALLEL_PROBES))
        .map(|_| {
            let work = Arc::clone(&work);
            let answers = Arc::clone(&answers);
            let client = client.clone();
            let urls: Vec<Vec<String>> = candidates.iter().map(|c| c.urls.clone()).collect();
            thread::spawn(move || loop {
                let Some((machine, url)) = work.lock().ok().and_then(|mut work| work.pop_front())
                else {
                    return;
                };
                if let Some(hello) = probe_hello(&client, &urls[machine][url]) {
                    if let Ok(mut answers) = answers.lock() {
                        answers.insert((machine, url), hello);
                    }
                }
            })
        })
        .collect();
    for worker in workers {
        let _ = worker.join();
    }
    let mut answers = std::mem::take(&mut *answers.lock().unwrap_or_else(|p| p.into_inner()));
    pick_answers(candidates, &mut answers)
}

fn pick_answers(
    candidates: &[Candidate],
    answers: &mut HashMap<(usize, usize), Hello>,
) -> Vec<DiscoveredHost> {
    let mut seen = HashSet::new();
    let mut hosts = Vec::new();
    for (machine, candidate) in candidates.iter().enumerate() {
        let Some((url, hello)) = (0..candidate.urls.len())
            .find_map(|url| answers.remove(&(machine, url)).map(|hello| (url, hello)))
        else {
            continue;
        };
        let url = candidate.urls[url].clone();
        if !seen.insert(url.clone()) {
            continue;
        }
        let name = Some(hello.name.trim().to_string())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| candidate.machine.clone());
        hosts.push(DiscoveredHost {
            name,
            machine: candidate.machine.clone(),
            url,
            // An auth mode this build doesn't know falls back to pasting the token.
            auth: match hello.auth.as_str() {
                "none" | "password" => hello.auth,
                _ => "token".to_string(),
            },
            server_version: hello.server_version,
            is_self: candidate.is_self,
        });
    }
    hosts
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Trimmed from real `tailscale status --json` output.
    const STATUS: &str = r#"{
      "Version": "1.90.0",
      "BackendState": "Running",
      "TailscaleIPs": ["100.91.70.66", "fd7a:115c:a1e0::f03b:4643"],
      "Self": {
        "HostName": "Conal’s Mac mini",
        "DNSName": "conals-mac-mini.tail1b4c5c.ts.net.",
        "TailscaleIPs": ["100.91.70.66", "fd7a:115c:a1e0::f03b:4643"],
        "Online": true, "OS": "macOS"
      },
      "MagicDNSSuffix": "tail1b4c5c.ts.net",
      "Peer": {
        "nodekey:aa": {
          "HostName": "Studio", "DNSName": "studio.tail1b4c5c.ts.net.",
          "TailscaleIPs": ["fd7a:115c:a1e0::1", "100.106.119.1"], "Online": true, "OS": "linux"
        },
        "nodekey:bb": {
          "HostName": "DESKTOP-L03FA3T", "DNSName": "desktop-l03fa3t.tail1b4c5c.ts.net.",
          "TailscaleIPs": ["100.107.254.41"], "Online": false, "OS": "windows"
        },
        "nodekey:cc": {
          "HostName": "", "DNSName": "android-box.tail1b4c5c.ts.net.",
          "TailscaleIPs": ["100.66.34.126"], "Online": true, "OS": "android"
        }
      }
    }"#;

    fn status() -> TailscaleStatus {
        interpret_status(STATUS.as_bytes(), b"").expect("running status parses")
    }

    #[test]
    fn scan_probes_self_and_online_peers_https_first() {
        let candidates = scan_candidates(&status());
        assert_eq!(
            candidates,
            vec![
                Candidate {
                    machine: "Conal’s Mac mini".to_string(),
                    urls: vec![
                        "https://conals-mac-mini.tail1b4c5c.ts.net".to_string(),
                        "http://conals-mac-mini.tail1b4c5c.ts.net".to_string(),
                        "http://100.91.70.66:48173".to_string(),
                    ],
                    is_self: true,
                },
                // The offline Windows box and the Android phone aren't probed.
                Candidate {
                    machine: "Studio".to_string(),
                    // The first IPv4, not the IPv6 listed before it.
                    urls: vec![
                        "https://studio.tail1b4c5c.ts.net".to_string(),
                        "http://studio.tail1b4c5c.ts.net".to_string(),
                        "http://100.106.119.1:48173".to_string(),
                    ],
                    is_self: false,
                },
            ]
        );
    }

    #[test]
    fn status_states_become_clear_errors() {
        let with_state = |state: &str| {
            interpret_status(
                format!(r#"{{"BackendState":"{state}","Peer":null}}"#).as_bytes(),
                b"",
            )
            .unwrap_err()
        };
        assert!(with_state("NeedsLogin").contains("logged out"));
        assert!(with_state("Stopped").contains("turned off"));
        assert!(with_state("Starting").contains("starting"));
        let not_running = interpret_status(
            b"",
            b"failed to connect to local tailscaled; it doesn't appear to be running",
        )
        .unwrap_err();
        assert!(not_running.starts_with(NOT_RUNNING));
        assert_eq!(interpret_status(b"", b"").unwrap_err(), NOT_RUNNING);
        // A null peer map (a tailnet of one) still parses.
        let alone = interpret_status(br#"{"BackendState":"Running","Peer":null}"#, b"").unwrap();
        assert!(alone.peers.is_empty());
    }

    #[test]
    fn typed_addresses_probe_https_then_http() {
        assert_eq!(
            parse_typed_address(" studio "),
            Ok(TypedAddress::Name {
                host: "studio".to_string(),
                port: None
            })
        );
        assert_eq!(
            parse_typed_address("studio.tail1b4c5c.ts.net:8080"),
            Ok(TypedAddress::Name {
                host: "studio.tail1b4c5c.ts.net".to_string(),
                port: Some(8080)
            })
        );
        assert_eq!(
            parse_typed_address("https://studio.tail1b4c5c.ts.net/"),
            Ok(TypedAddress::Url(
                "https://studio.tail1b4c5c.ts.net".to_string()
            ))
        );
        assert!(parse_typed_address("").is_err());
        assert!(parse_typed_address("studio:http").is_err());
        assert!(parse_typed_address("studio mac").is_err());

        let bare = parse_typed_address("studio").unwrap();
        let full = magic_dns_name(&status(), "studio");
        assert_eq!(full.as_deref(), Some("studio.tail1b4c5c.ts.net"));
        assert_eq!(
            typed_candidate(&bare, full.as_deref()).urls,
            vec![
                "https://studio.tail1b4c5c.ts.net",
                "http://studio",
                "http://studio:48173"
            ]
        );
        // Without the CLI a bare name can't match a certificate: HTTP only.
        assert_eq!(
            typed_candidate(&bare, None).urls,
            vec!["http://studio", "http://studio:48173"]
        );
        let with_port = parse_typed_address("studio:9000").unwrap();
        assert_eq!(
            typed_candidate(&with_port, None).urls,
            vec!["http://studio:9000"]
        );
        let full_with_port = parse_typed_address("studio.tail1b4c5c.ts.net:9000").unwrap();
        assert_eq!(
            typed_candidate(&full_with_port, None).urls,
            vec![
                "https://studio.tail1b4c5c.ts.net:9000",
                "http://studio.tail1b4c5c.ts.net:9000"
            ]
        );
        assert_eq!(
            magic_dns_name(&status(), "DESKTOP-l03fa3t").as_deref(),
            Some("desktop-l03fa3t.tail1b4c5c.ts.net")
        );
    }

    fn hello(auth: &str) -> Hello {
        Hello {
            service: HOST_SERVICE.to_string(),
            name: "Studio Mac".to_string(),
            server_version: Some("0.4.0".to_string()),
            auth: auth.to_string(),
        }
    }

    #[test]
    fn each_machine_is_listed_once_at_its_preferred_url() {
        let candidates = scan_candidates(&status());
        let mut answers = HashMap::from([
            ((0, 0), hello("password")),
            ((0, 2), hello("password")),
            ((1, 1), hello("future-mode")),
            ((1, 2), hello("future-mode")),
        ]);
        let hosts = pick_answers(&candidates, &mut answers);
        assert_eq!(hosts.len(), 2);
        assert_eq!(hosts[0].url, "https://conals-mac-mini.tail1b4c5c.ts.net");
        assert!(hosts[0].is_self);
        assert_eq!(hosts[0].auth, "password");
        // HTTP on the MagicDNS name beats the IPv4 fallback.
        assert_eq!(hosts[1].url, "http://studio.tail1b4c5c.ts.net");
        assert_eq!(hosts[1].machine, "Studio");
        assert_eq!(hosts[1].auth, "token");
    }

    #[test]
    fn pairing_answers_become_tokens_or_messages() {
        let ok = pair_result(
            StatusCode::OK,
            &serde_json::json!({"token": "t", "name": "Studio"}),
        );
        assert_eq!(ok, Ok(Some("t".to_string())));
        let open = pair_result(
            StatusCode::OK,
            &serde_json::json!({"token": null, "name": "Studio"}),
        );
        assert_eq!(open, Ok(None));
        assert!(pair_result(
            StatusCode::UNAUTHORIZED,
            &serde_json::json!({"error": "wrong password"})
        )
        .unwrap_err()
        .contains("wrong"));
        assert!(pair_result(StatusCode::NOT_FOUND, &serde_json::json!({}))
            .unwrap_err()
            .contains("token"));
        assert_eq!(
            pair_result(
                StatusCode::TOO_MANY_REQUESTS,
                &serde_json::json!({"error": "too many attempts", "retryAfterSeconds": 61})
            )
            .unwrap_err(),
            "Too many wrong passwords. Try again in 2 minutes."
        );
    }

    /// Scans the tailnet this machine is on. Run by hand with a host
    /// listening: `cargo test --lib tailnet_discovery -- --ignored --nocapture`.
    #[test]
    #[ignore = "needs Tailscale and a running host"]
    fn discovers_hosts_on_the_real_tailnet() {
        let hosts = discover_hosts().expect("scan");
        println!("{hosts:#?}");
        assert!(!hosts.is_empty());
    }

    /// A stand-in host answering `/v1/hello` and `/v1/pair`.
    fn fake_host(hello_body: &'static str) -> String {
        let server = tiny_http::Server::http("127.0.0.1:0").expect("bind");
        let addr = server.server_addr().to_ip().expect("ip address");
        thread::spawn(move || {
            for mut request in server.incoming_requests() {
                let (status, body) = match request.url() {
                    "/v1/hello" => (200, hello_body.to_string()),
                    "/v1/pair" => {
                        let mut body = String::new();
                        let _ = request.as_reader().read_to_string(&mut body);
                        let pair: serde_json::Value = serde_json::from_str(&body).unwrap();
                        assert!(pair["clientName"].is_string());
                        if pair["password"] == "123456" {
                            (200, r#"{"token":"tok","name":"Studio"}"#.to_string())
                        } else {
                            (401, r#"{"error":"wrong password"}"#.to_string())
                        }
                    }
                    _ => (404, r#"{"error":"not_found"}"#.to_string()),
                };
                let _ = request
                    .respond(tiny_http::Response::from_string(body).with_status_code(status));
            }
        });
        format!("http://{addr}")
    }

    #[test]
    fn probes_and_pairs_with_a_real_http_host() {
        let url = fake_host(
            r#"{"service":"fairspoken-host","protocol":1,"name":"Studio","serverVersion":"0.4.0","auth":"password"}"#,
        );
        let other = fake_host(r#"{"service":"something-else"}"#);
        let client = probe_client(SCAN_PROBE_TIMEOUT).unwrap();
        let candidates = vec![
            Candidate {
                machine: "studio".to_string(),
                // Nothing listens on port 9; the HTTP fallback answers.
                urls: vec!["http://127.0.0.1:9".to_string(), url.clone()],
                is_self: false,
            },
            Candidate {
                machine: "other".to_string(),
                urls: vec![other],
                is_self: false,
            },
        ];
        let hosts = probe_candidates(&client, &candidates);
        assert_eq!(
            hosts,
            vec![DiscoveredHost {
                name: "Studio".to_string(),
                machine: "studio".to_string(),
                url: url.clone(),
                auth: "password".to_string(),
                server_version: Some("0.4.0".to_string()),
                is_self: false,
            }]
        );
        assert_eq!(
            pair_with_host(&url, "123456", "Test laptop"),
            Ok(Some("tok".to_string()))
        );
        assert_eq!(
            pair_with_host(&format!("{url}/"), "nope!!", "Test laptop"),
            Err("That password is wrong.".to_string())
        );
        assert!(probe_address(&url).is_ok());
    }
}
