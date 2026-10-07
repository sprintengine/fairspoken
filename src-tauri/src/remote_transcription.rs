use crate::audio::{AudioFrame, Recording};
use crate::settings::{cloud_url, Settings, TranscriptionLocation, CLOUD_UNAVAILABLE};
use reqwest::blocking::Client;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use reqwest::Url;
use serde::{Deserialize, Serialize};
use std::io::{Cursor, Read};
use std::net::IpAddr;
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::thread::{self, JoinHandle};
use std::time::Duration;

const STREAM_CONTENT_TYPE: &str = "application/vnd.fairspoken.pcm-stream";
const STREAM_CHANNEL_DEPTH: usize = 12;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteHealth {
    pub ok: bool,
    pub mode: String,
    pub backend: String,
    pub server_version: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteTranscriptionResponse {
    pub text: String,
    pub duration_seconds: f32,
    pub backend: String,
    pub model: String,
    pub server_version: Option<String>,
    /// `used | shed | unavailable | off`, present when the request asked
    /// for super mode (`x-fairspoken-super-mode`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub super_mode: Option<String>,
    /// The Whisper model a host paired with the primary when super mode ran.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secondary_model: Option<String>,
}

/// The remote endpoint a session talks to, resolved from the transcription
/// location: `RemoteHost` uses the user's own URL and token; `Cloud` uses the
/// Fairspoken Cloud URL (compile-time `FAIRSPOKEN_CLOUD_URL`, runtime override
/// for dev builds; absent in builds without a cloud) and the multiauth token. The wire protocol is
/// identical, so everything downstream of resolution is shared.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteTarget {
    base_url: Url,
    auth_token: String,
    is_cloud: bool,
}

impl RemoteTarget {
    fn display_name(&self) -> &'static str {
        if self.is_cloud {
            "Fairspoken Cloud"
        } else {
            "Remote transcription host"
        }
    }
}

pub fn resolve_remote_target(settings: &Settings) -> Result<RemoteTarget, String> {
    resolve_remote_target_with(settings, cloud_url())
}

fn resolve_remote_target_with(
    settings: &Settings,
    cloud_url: Option<String>,
) -> Result<RemoteTarget, String> {
    match settings.transcription_location {
        TranscriptionLocation::RemoteHost => Ok(RemoteTarget {
            base_url: validate_remote_base_url(&settings.remote_url)?,
            auth_token: settings.remote_auth_token.trim().to_string(),
            is_cloud: false,
        }),
        TranscriptionLocation::Cloud => Ok(RemoteTarget {
            // Still validated so a bad FAIRSPOKEN_CLOUD_URL override fails
            // loudly instead of dictating into the void.
            base_url: validate_remote_base_url(
                &cloud_url.ok_or_else(|| CLOUD_UNAVAILABLE.to_string())?,
            )?,
            auth_token: settings.cloud_auth_token.trim().to_string(),
            is_cloud: true,
        }),
        TranscriptionLocation::Local => {
            Err("Local transcription does not use a remote host".to_string())
        }
    }
}

/// Build the error message for a non-2xx response, surfacing the server's
/// JSON `{"error": …}` body (both the standalone host and the cloud Worker
/// return it) so the user sees real reasons — the cloud 402 carries the
/// used/allowance minutes, for example — instead of a bare status code.
fn remote_response_error(target: &RemoteTarget, response: reqwest::blocking::Response) -> String {
    let status = response.status();
    let server_message = response
        .json::<serde_json::Value>()
        .ok()
        .and_then(|value| {
            value
                .get("error")
                .and_then(|message| message.as_str())
                .map(str::to_string)
        })
        .filter(|message| !message.trim().is_empty());
    format_remote_error(target, status.as_u16(), &status.to_string(), server_message)
}

fn format_remote_error(
    target: &RemoteTarget,
    status_code: u16,
    status_display: &str,
    server_message: Option<String>,
) -> String {
    if target.is_cloud {
        match (status_code, &server_message) {
            (401, _) => {
                return "Cloud sign-in expired or invalid — update your token in Settings"
                    .to_string()
            }
            (402, Some(message)) => {
                return format!("{message} — switch to Local in Settings to keep dictating")
            }
            (402, None) => {
                return "Cloud transcription allowance used up this month — switch to Local in Settings to keep dictating".to_string()
            }
            _ => {}
        }
    }

    match server_message {
        Some(message) => format!(
            "{} returned an error ({status_display}): {message}",
            target.display_name()
        ),
        None => format!(
            "{} returned an error: {status_display}",
            target.display_name()
        ),
    }
}

pub struct RemoteStreamingSession {
    audio_tx: SyncSender<AudioFrame>,
    result_rx: Receiver<Result<RemoteTranscriptionResponse, String>>,
    worker: Option<JoinHandle<()>>,
    /// How long `finish()` waits for the host's transcript once the audio
    /// stream has ended, so a host that accepts the stream and then hangs
    /// cannot block the dictation forever.
    finish_timeout: Duration,
}

pub fn test_remote_transcription_host(settings: &Settings) -> Result<RemoteHealth, String> {
    let target = resolve_remote_target(settings)?;
    let timeout_seconds = u64::from(
        settings
            .remote_timeout_seconds
            .saturating_add(settings.max_recording_seconds),
    );
    let client = Client::builder()
        .timeout(Duration::from_secs(timeout_seconds))
        .build()
        .map_err(|err| format!("Failed to create remote transcription client: {err}"))?;
    let url = target
        .base_url
        .join("v1/health")
        .map_err(|err| format!("Invalid remote health URL: {err}"))?;
    let response = client
        .get(url)
        .headers(auth_headers(&target)?)
        .send()
        .map_err(|err| format!("{} is unreachable: {err}", target.display_name()))?;
    if !response.status().is_success() {
        return Err(remote_response_error(&target, response));
    }

    response
        .json::<RemoteHealth>()
        .map_err(|err| format!("Remote transcription health response was invalid: {err}"))
}

pub fn start_remote_streaming_session(
    settings: &Settings,
) -> Result<(RemoteStreamingSession, SyncSender<AudioFrame>), String> {
    let finish_timeout = remote_connect_timeout(settings);
    let settings = settings.clone();
    let (audio_tx, audio_rx) = mpsc::sync_channel::<AudioFrame>(STREAM_CHANNEL_DEPTH);
    // The capture callback drops frames when its bounded channel is full, and
    // a slow network write would fill it in ~130 ms. The relay moves frames
    // into an unbounded queue straight away, so a stalled link delays audio
    // instead of losing it.
    let (upload_tx, upload_rx) = mpsc::channel::<AudioFrame>();
    thread::Builder::new()
        .name("remote-streaming-relay".to_string())
        .spawn(move || {
            for frame in audio_rx {
                if upload_tx.send(frame).is_err() {
                    break;
                }
            }
        })
        .map_err(|err| format!("Failed to start remote streaming relay: {err}"))?;
    let (result_tx, result_rx) = mpsc::channel::<Result<RemoteTranscriptionResponse, String>>();
    let worker = thread::Builder::new()
        .name("remote-streaming-transcription".to_string())
        .spawn(move || {
            let result = transcribe_remote_stream(upload_rx, &settings);
            let _ = result_tx.send(result);
        })
        .map_err(|err| format!("Failed to start remote streaming worker: {err}"))?;

    let session = RemoteStreamingSession {
        audio_tx: audio_tx.clone(),
        result_rx,
        worker: Some(worker),
        finish_timeout,
    };
    Ok((session, audio_tx))
}

impl RemoteStreamingSession {
    pub fn finish(mut self) -> Result<RemoteTranscriptionResponse, String> {
        drop(self.audio_tx);
        let result = match self.result_rx.recv_timeout(self.finish_timeout) {
            Ok(result) => result,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                // The worker is blocked on a host that stopped responding.
                // Abandon it instead of joining — joining would hang the app
                // exactly the way this timeout exists to prevent.
                self.worker.take();
                return Err(format!(
                    "Remote transcription host did not respond within {}s of the recording ending",
                    self.finish_timeout.as_secs()
                ));
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err("Remote streaming worker stopped without a transcript".to_string())
            }
        };
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        result
    }

    pub fn cancel(mut self) {
        drop(self.audio_tx);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn transcribe_remote_stream(
    audio_rx: Receiver<AudioFrame>,
    settings: &Settings,
) -> Result<RemoteTranscriptionResponse, String> {
    let target = resolve_remote_target(settings)?;
    let client = Client::builder()
        // No total timeout: the streaming body lasts as long as the recording.
        // The hung-host protection lives in RemoteStreamingSession::finish(),
        // which stops waiting `finish_timeout` after the audio stream ends.
        .timeout(None)
        .connect_timeout(remote_connect_timeout(settings))
        .build()
        .map_err(|err| format!("Failed to create remote transcription client: {err}"))?;
    let url = target
        .base_url
        .join("v1/transcriptions/stream")
        .map_err(|err| format!("Invalid remote streaming transcription URL: {err}"))?;
    let mut headers = transcription_headers(settings, &target)?;
    headers.insert(CONTENT_TYPE, HeaderValue::from_static(STREAM_CONTENT_TYPE));

    let response = client
        .post(url)
        .headers(headers)
        .body(reqwest::blocking::Body::new(AudioFrameReader::new(
            audio_rx,
        )))
        .send()
        .map_err(|err| format!("Remote streaming transcription request failed: {err}"))?;
    if !response.status().is_success() {
        return Err(remote_response_error(&target, response));
    }
    let result = response
        .json::<RemoteTranscriptionResponse>()
        .map_err(|err| format!("Remote streaming transcription response was invalid: {err}"))?;

    if result.text.trim().is_empty() {
        return Err("Remote streaming transcription returned no speech".to_string());
    }

    Ok(result)
}

fn remote_connect_timeout(settings: &Settings) -> Duration {
    Duration::from_secs(u64::from(settings.remote_timeout_seconds))
}

#[allow(dead_code)]
pub fn transcribe_remote(
    recording: &Recording,
    settings: &Settings,
) -> Result<RemoteTranscriptionResponse, String> {
    let target = resolve_remote_target(settings)?;
    let wav = encode_wav(recording)?;
    let client = Client::builder()
        .timeout(Duration::from_secs(u64::from(
            settings.remote_timeout_seconds,
        )))
        .build()
        .map_err(|err| format!("Failed to create remote transcription client: {err}"))?;
    let url = target
        .base_url
        .join("v1/transcriptions")
        .map_err(|err| format!("Invalid remote transcription URL: {err}"))?;
    let mut headers = transcription_headers(settings, &target)?;
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("audio/wav"));

    let response = client
        .post(url)
        .headers(headers)
        .body(wav)
        .send()
        .map_err(|err| format!("Remote transcription request failed: {err}"))?;
    if !response.status().is_success() {
        return Err(remote_response_error(&target, response));
    }
    let result = response
        .json::<RemoteTranscriptionResponse>()
        .map_err(|err| format!("Remote transcription response was invalid: {err}"))?;

    if result.text.trim().is_empty() {
        return Err("Remote transcription returned no speech".to_string());
    }

    Ok(result)
}

pub fn read_stream_frame(reader: &mut impl Read) -> Result<Option<AudioFrame>, String> {
    let mut header = [0_u8; 8];
    if !read_exact_or_eof(reader, &mut header)? {
        return Ok(None);
    }

    let sample_rate = u32::from_le_bytes([header[0], header[1], header[2], header[3]]);
    let sample_count = u32::from_le_bytes([header[4], header[5], header[6], header[7]]) as usize;
    if sample_rate == 0 || sample_rate > 192_000 {
        return Err("Remote stream frame has an unsupported sample rate".to_string());
    }
    if sample_count == 0 {
        return Ok(Some(AudioFrame {
            pcm_i16: Vec::new(),
            sample_rate,
        }));
    }
    if sample_count > 192_000 {
        return Err("Remote stream frame is too large".to_string());
    }

    let mut payload = vec![0_u8; sample_count * 2];
    reader
        .read_exact(&mut payload)
        .map_err(|err| format!("Failed to read remote stream frame: {err}"))?;
    let pcm_i16 = payload
        .chunks_exact(2)
        .map(|chunk| i16::from_le_bytes([chunk[0], chunk[1]]))
        .collect();
    Ok(Some(AudioFrame {
        pcm_i16,
        sample_rate,
    }))
}

fn write_stream_frame(frame: &AudioFrame, target: &mut Vec<u8>) {
    target.extend_from_slice(&frame.sample_rate.to_le_bytes());
    target.extend_from_slice(&(frame.pcm_i16.len() as u32).to_le_bytes());
    for sample in &frame.pcm_i16 {
        target.extend_from_slice(&sample.to_le_bytes());
    }
}

struct AudioFrameReader {
    rx: Receiver<AudioFrame>,
    pending: Cursor<Vec<u8>>,
}

impl AudioFrameReader {
    fn new(rx: Receiver<AudioFrame>) -> Self {
        Self {
            rx,
            pending: Cursor::new(Vec::new()),
        }
    }
}

impl Read for AudioFrameReader {
    fn read(&mut self, target: &mut [u8]) -> std::io::Result<usize> {
        loop {
            let read = self.pending.read(target)?;
            if read > 0 {
                return Ok(read);
            }

            match self.rx.recv() {
                Ok(frame) => {
                    let mut encoded = Vec::with_capacity(8 + frame.pcm_i16.len() * 2);
                    write_stream_frame(&frame, &mut encoded);
                    self.pending = Cursor::new(encoded);
                }
                Err(_) => return Ok(0),
            }
        }
    }
}

fn read_exact_or_eof(reader: &mut impl Read, target: &mut [u8]) -> Result<bool, String> {
    let mut read = 0;
    while read < target.len() {
        match reader.read(&mut target[read..]) {
            Ok(0) if read == 0 => return Ok(false),
            Ok(0) => return Err("Remote stream ended mid-frame".to_string()),
            Ok(n) => read += n,
            Err(err) => return Err(format!("Failed to read remote stream: {err}")),
        }
    }
    Ok(true)
}

#[allow(dead_code)]
pub fn encode_wav(recording: &Recording) -> Result<Vec<u8>, String> {
    let mut cursor = Cursor::new(Vec::new());
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: recording.sample_rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    {
        let mut writer = hound::WavWriter::new(&mut cursor, spec)
            .map_err(|err| format!("Failed to create WAV encoder: {err}"))?;
        for sample in &recording.pcm_i16 {
            writer
                .write_sample(*sample)
                .map_err(|err| format!("Failed to encode WAV sample: {err}"))?;
        }
        writer
            .finalize()
            .map_err(|err| format!("Failed to finalize WAV: {err}"))?;
    }
    Ok(cursor.into_inner())
}

pub fn decode_wav(bytes: &[u8]) -> Result<Recording, String> {
    let cursor = Cursor::new(bytes);
    let mut reader =
        hound::WavReader::new(cursor).map_err(|err| format!("Failed to read WAV: {err}"))?;
    let spec = reader.spec();
    if spec.channels == 0 {
        return Err("WAV file has no audio channels".to_string());
    }
    if spec.bits_per_sample != 16 || spec.sample_format != hound::SampleFormat::Int {
        return Err("Only 16-bit PCM WAV audio is supported".to_string());
    }

    let channels = usize::from(spec.channels);
    let mut pcm_i16 = Vec::new();
    let mut frame = Vec::with_capacity(channels);
    for sample in reader.samples::<i16>() {
        frame.push(sample.map_err(|err| format!("Failed to decode WAV sample: {err}"))?);
        if frame.len() == channels {
            let sum: i32 = frame.iter().map(|sample| i32::from(*sample)).sum();
            pcm_i16.push((sum / channels as i32) as i16);
            frame.clear();
        }
    }

    Ok(Recording {
        pcm_i16,
        sample_rate: spec.sample_rate,
        dropped_stream_frames: 0,
    })
}

/// Engine identifier sent over the remote-host protocol and stored in
/// transcript history. Parakeet is the default engine; a host built with the
/// `whisper` feature also accepts the legacy "whisper" identifier.
pub const BACKEND_ID: &str = "parakeet";

pub fn selected_model_id(settings: &Settings) -> &'static str {
    settings.model.model_id()
}

fn auth_headers(target: &RemoteTarget) -> Result<HeaderMap, String> {
    let mut headers = HeaderMap::new();
    let token = target.auth_token.trim();
    if !token.is_empty() {
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {token}"))
                .map_err(|err| format!("Invalid remote auth token: {err}"))?,
        );
    }
    Ok(headers)
}

// Every x-fairspoken-* header is sent to both targets: the cloud Worker
// ignores the self-host-only ones (backend/model) by contract.
fn transcription_headers(settings: &Settings, target: &RemoteTarget) -> Result<HeaderMap, String> {
    let mut headers = auth_headers(target)?;
    headers.insert(
        "x-fairspoken-client",
        HeaderValue::from_static("fairspoken-desktop"),
    );
    headers.insert("x-fairspoken-backend", HeaderValue::from_static(BACKEND_ID));
    headers.insert(
        "x-fairspoken-model",
        HeaderValue::from_str(selected_model_id(settings))
            .map_err(|err| format!("Invalid selected model header: {err}"))?,
    );
    headers.insert(
        "x-fairspoken-language",
        HeaderValue::from_str(&settings.language)
            .map_err(|err| format!("Invalid language header: {err}"))?,
    );
    // The user's terms, then the enabled packs' always-on terms, capped.
    let hints = crate::vocabulary_packs::remote_hints(
        &settings.vocabulary_hints,
        &settings.enabled_packs,
    );
    if !hints.is_empty() {
        let hints = serde_json::to_string(&hints)
            .map_err(|err| format!("Failed to serialize vocabulary hints: {err}"))?;
        headers.insert(
            "x-fairspoken-vocabulary-hints",
            HeaderValue::from_str(&percent_encode(&hints))
                .map_err(|err| format!("Invalid vocabulary hints header: {err}"))?,
        );
    }
    if wants_host_super_mode(settings) {
        headers.insert("x-fairspoken-super-mode", HeaderValue::from_static("1"));
    }
    Ok(headers)
}

/// Super mode on a remote host costs this device nothing, so `auto` asks
/// too and the host sheds it when busy. Fairspoken Cloud has no super mode.
fn wants_host_super_mode(settings: &Settings) -> bool {
    settings.transcription_location == crate::settings::TranscriptionLocation::RemoteHost
        && settings.super_mode != crate::settings::SuperModeSetting::Off
}

fn percent_encode(input: &str) -> String {
    let mut encoded = String::new();
    for byte in input.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(*byte as char)
            }
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
}

fn validate_remote_base_url(raw_url: &str) -> Result<Url, String> {
    let trimmed = raw_url.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        return Err("Remote transcription host URL is not configured".to_string());
    }
    let url = Url::parse(trimmed).map_err(|err| format!("Invalid remote host URL: {err}"))?;
    if !url.username().is_empty() || url.password().is_some() {
        return Err("Remote host URL must not contain username or password".to_string());
    }

    let scheme = url.scheme();
    let host = url
        .host_str()
        .ok_or_else(|| "Remote host URL must include a host".to_string())?;
    if scheme != "https" && !(scheme == "http" && host_allows_plain_http(host)) {
        return Err(
            "Remote host URL must use HTTPS unless it is localhost or a private network address"
                .to_string(),
        );
    }

    Ok(url)
}

fn host_allows_plain_http(host: &str) -> bool {
    if matches!(host, "localhost" | "localhost.") {
        return true;
    }

    let Ok(ip) = host
        .trim_start_matches('[')
        .trim_end_matches(']')
        .parse::<IpAddr>()
    else {
        return host_is_private_name(host);
    };
    match ip {
        IpAddr::V4(ip) => {
            ip.is_loopback()
                || ip.is_private()
                || ip.is_link_local()
                || ip.octets()[0] == 169
                || is_tailscale_cgnat(ip)
        }
        IpAddr::V6(ip) => ip.is_loopback() || ip.is_unique_local() || ip.is_unicast_link_local(),
    }
}

/// Tailscale assigns node addresses from the 100.64.0.0/10 CGNAT range, and
/// tailnet traffic is already WireGuard-encrypted end to end.
fn is_tailscale_cgnat(ip: std::net::Ipv4Addr) -> bool {
    let [a, b, ..] = ip.octets();
    a == 100 && (64..=127).contains(&b)
}

/// Names that can only resolve inside a private network: Tailscale MagicDNS
/// (`box` or `box.tailnet-name.ts.net`) and mDNS (`box.local`).
fn host_is_private_name(host: &str) -> bool {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    !host.is_empty()
        && (!host.contains('.') || host.ends_with(".ts.net") || host.ends_with(".local"))
}

#[cfg(test)]
mod tests {
    use super::{
        decode_wav, encode_wav, format_remote_error, read_stream_frame, remote_connect_timeout,
        resolve_remote_target, resolve_remote_target_with, validate_remote_base_url,
        write_stream_frame,
    };
    use crate::audio::{AudioFrame, Recording};
    use crate::settings::{Settings, TranscriptionLocation};

    const TEST_CLOUD_URL: &str = "https://cloud.example.com";
    use std::time::Duration;

    #[test]
    fn wav_round_trip_preserves_mono_pcm() {
        let recording = Recording {
            pcm_i16: vec![0, 1000, -1000],
            sample_rate: 16_000,
            dropped_stream_frames: 0,
        };

        let wav = encode_wav(&recording).expect("encode wav");
        let decoded = decode_wav(&wav).expect("decode wav");

        assert_eq!(decoded.sample_rate, 16_000);
        assert_eq!(decoded.pcm_i16, recording.pcm_i16);
    }

    #[test]
    fn stream_frame_round_trip_preserves_audio_frame() {
        let frame = AudioFrame {
            pcm_i16: vec![1, -2, 3],
            sample_rate: 48_000,
        };
        let mut encoded = Vec::new();
        write_stream_frame(&frame, &mut encoded);

        let decoded = read_stream_frame(&mut encoded.as_slice())
            .expect("decode frame")
            .expect("frame exists");

        assert_eq!(decoded.sample_rate, frame.sample_rate);
        assert_eq!(decoded.pcm_i16, frame.pcm_i16);
    }

    #[test]
    fn remote_stream_uses_configured_connect_timeout_only() {
        let settings = Settings {
            remote_timeout_seconds: 15,
            max_recording_seconds: 120,
            ..Settings::default()
        };

        assert_eq!(remote_connect_timeout(&settings), Duration::from_secs(15));
    }

    #[test]
    fn stream_frame_rejects_unsupported_sample_rate() {
        let mut encoded = Vec::new();
        encoded.extend_from_slice(&0_u32.to_le_bytes());
        encoded.extend_from_slice(&1_u32.to_le_bytes());
        encoded.extend_from_slice(&1_i16.to_le_bytes());

        let err = read_stream_frame(&mut encoded.as_slice()).expect_err("zero sample rate rejects");

        assert!(err.contains("sample rate"));
    }

    #[test]
    fn remote_url_allows_http_for_private_home_network_hosts() {
        assert!(validate_remote_base_url("http://192.168.0.35:48173").is_ok());
        assert!(validate_remote_base_url("http://10.0.0.2:48173").is_ok());
        assert!(validate_remote_base_url("http://172.16.0.2:48173").is_ok());
        assert!(validate_remote_base_url("http://localhost:48173").is_ok());
    }

    #[test]
    fn remote_url_allows_http_for_tailnet_hosts() {
        assert!(validate_remote_base_url("http://100.101.102.103:48173").is_ok());
        assert!(validate_remote_base_url("http://studio-box:48173").is_ok());
        assert!(validate_remote_base_url("http://studio-box.tail1234.ts.net:48173").is_ok());
        assert!(validate_remote_base_url("http://studio-box.local:48173").is_ok());
        assert!(validate_remote_base_url("http://[fd7a:115c:a1e0::1]:48173").is_ok());
        // 100.0.0.0/8 outside the CGNAT /10 is public address space.
        assert!(validate_remote_base_url("http://100.128.0.1:48173").is_err());
        assert!(validate_remote_base_url("http://evil.ts.net.example.com:48173").is_err());
    }

    #[test]
    fn super_mode_header_goes_only_to_a_remote_host_when_asked_for() {
        let host = |mode| Settings {
            transcription_location: TranscriptionLocation::RemoteHost,
            remote_url: "https://host.example.com".to_string(),
            super_mode: mode,
            ..Settings::default()
        };
        let header = |settings: &Settings| {
            let target = resolve_remote_target(settings).expect("remote host target");
            super::transcription_headers(settings, &target)
                .unwrap()
                .get("x-fairspoken-super-mode")
                .map(|value| value.to_str().unwrap().to_string())
        };
        assert_eq!(header(&host(crate::settings::SuperModeSetting::On)).as_deref(), Some("1"));
        assert_eq!(header(&host(crate::settings::SuperModeSetting::Auto)).as_deref(), Some("1"));
        assert_eq!(header(&host(crate::settings::SuperModeSetting::Off)), None);
        let cloud = Settings {
            transcription_location: TranscriptionLocation::Cloud,
            super_mode: crate::settings::SuperModeSetting::On,
            ..Settings::default()
        };
        assert!(!super::wants_host_super_mode(&cloud));

        let old_host: super::RemoteTranscriptionResponse = serde_json::from_str(
            r#"{"text":"hi","durationSeconds":1,"backend":"parakeet","model":"m","serverVersion":"0.1.0"}"#,
        )
        .unwrap();
        assert_eq!(old_host.super_mode, None);
        assert!(!serde_json::to_string(&old_host).unwrap().contains("superMode"));
    }

    #[test]
    fn resolve_remote_target_maps_each_location() {
        let remote_host = Settings {
            transcription_location: TranscriptionLocation::RemoteHost,
            remote_url: "https://host.example.com".to_string(),
            remote_auth_token: "  host-token  ".to_string(),
            ..Settings::default()
        };
        let target = resolve_remote_target(&remote_host).expect("remote host target");
        assert!(!target.is_cloud);
        assert_eq!(target.auth_token, "host-token");
        assert_eq!(target.base_url.host_str(), Some("host.example.com"));
        assert_eq!(target.display_name(), "Remote transcription host");

        let cloud = Settings {
            transcription_location: TranscriptionLocation::Cloud,
            cloud_auth_token: "cloud-jwt".to_string(),
            // Cloud ignores the user's remote-host fields entirely.
            remote_url: "https://host.example.com".to_string(),
            remote_auth_token: "host-token".to_string(),
            ..Settings::default()
        };
        let target = resolve_remote_target_with(&cloud, Some(TEST_CLOUD_URL.into()))
            .expect("cloud target");
        assert!(target.is_cloud);
        assert_eq!(target.auth_token, "cloud-jwt");
        assert_ne!(target.base_url.host_str(), Some("host.example.com"));
        assert_eq!(target.display_name(), "Fairspoken Cloud");

        // Builds without a cloud URL never offer Cloud; a stale setting fails
        // with guidance instead of panicking.
        let unavailable = resolve_remote_target_with(&cloud, None).unwrap_err();
        assert!(unavailable.contains("not available in this build"));

        let local = Settings::default();
        assert!(resolve_remote_target(&local).is_err());
    }

    #[test]
    fn cloud_errors_get_actionable_copy() {
        let cloud = Settings {
            transcription_location: TranscriptionLocation::Cloud,
            ..Settings::default()
        };
        let target = resolve_remote_target_with(&cloud, Some(TEST_CLOUD_URL.into()))
            .expect("cloud target");

        let unauthorized = format_remote_error(&target, 401, "401 Unauthorized", None);
        assert!(unauthorized.contains("update your token in Settings"));

        let exhausted = format_remote_error(
            &target,
            402,
            "402 Payment Required",
            Some("Cloud transcription allowance used: 300 of 300 minutes this month".to_string()),
        );
        assert!(exhausted.contains("300 of 300 minutes"));
        assert!(exhausted.contains("switch to Local"));

        let backend_down = format_remote_error(&target, 502, "502 Bad Gateway", None);
        assert!(backend_down.contains("Fairspoken Cloud"));
    }

    #[test]
    fn remote_host_errors_surface_the_server_message() {
        let settings = Settings {
            transcription_location: TranscriptionLocation::RemoteHost,
            remote_url: "https://host.example.com".to_string(),
            ..Settings::default()
        };
        let target = resolve_remote_target(&settings).expect("remote host target");

        let with_body = format_remote_error(
            &target,
            429,
            "429 Too Many Requests",
            Some("queue full".to_string()),
        );
        assert_eq!(
            with_body,
            "Remote transcription host returned an error (429 Too Many Requests): queue full"
        );

        let without_body = format_remote_error(&target, 500, "500 Internal Server Error", None);
        assert_eq!(
            without_body,
            "Remote transcription host returned an error: 500 Internal Server Error"
        );
    }

    #[test]
    fn remote_url_still_requires_https_for_public_hosts() {
        let err = validate_remote_base_url("http://example.com:48173")
            .expect_err("public HTTP host should be rejected");

        assert!(err.contains("HTTPS"));
    }
}
