use crate::audio::{AudioFrame, Recording};
use crate::settings::{Settings, TranscriptionBackend};
use reqwest::blocking::Client;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use reqwest::Url;
use serde::{Deserialize, Serialize};
use std::io::{Cursor, Read};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::thread::{self, JoinHandle};
use std::time::Duration;

const STREAM_CONTENT_TYPE: &str = "application/vnd.multivoice.pcm-stream";
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
}

pub struct RemoteStreamingSession {
    audio_tx: SyncSender<AudioFrame>,
    result_rx: Receiver<Result<RemoteTranscriptionResponse, String>>,
    worker: Option<JoinHandle<()>>,
}

pub fn test_remote_transcription_host(settings: &Settings) -> Result<RemoteHealth, String> {
    let base_url = validate_remote_base_url(&settings.remote_url)?;
    let timeout_seconds = u64::from(
        settings
            .remote_timeout_seconds
            .saturating_add(settings.max_recording_seconds),
    );
    let client = Client::builder()
        .timeout(Duration::from_secs(timeout_seconds))
        .build()
        .map_err(|err| format!("Failed to create remote transcription client: {err}"))?;
    let url = base_url
        .join("v1/health")
        .map_err(|err| format!("Invalid remote health URL: {err}"))?;
    let response = client
        .get(url)
        .headers(auth_headers(settings)?)
        .send()
        .map_err(|err| format!("Remote transcription host is unreachable: {err}"))?
        .error_for_status()
        .map_err(|err| format!("Remote transcription host returned an error: {err}"))?;

    response
        .json::<RemoteHealth>()
        .map_err(|err| format!("Remote transcription health response was invalid: {err}"))
}

pub fn start_remote_streaming_session(
    settings: &Settings,
) -> Result<(RemoteStreamingSession, SyncSender<AudioFrame>), String> {
    let settings = settings.clone();
    let (audio_tx, audio_rx) = mpsc::sync_channel::<AudioFrame>(STREAM_CHANNEL_DEPTH);
    let (result_tx, result_rx) = mpsc::channel::<Result<RemoteTranscriptionResponse, String>>();
    let worker = thread::Builder::new()
        .name("remote-streaming-transcription".to_string())
        .spawn(move || {
            let result = transcribe_remote_stream(audio_rx, &settings);
            let _ = result_tx.send(result);
        })
        .map_err(|err| format!("Failed to start remote streaming worker: {err}"))?;

    let session = RemoteStreamingSession {
        audio_tx: audio_tx.clone(),
        result_rx,
        worker: Some(worker),
    };
    Ok((session, audio_tx))
}

impl RemoteStreamingSession {
    pub fn finish(mut self) -> Result<RemoteTranscriptionResponse, String> {
        drop(self.audio_tx);
        let result = self
            .result_rx
            .recv()
            .map_err(|_| "Remote streaming worker stopped without a transcript".to_string())?;
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
    let base_url = validate_remote_base_url(&settings.remote_url)?;
    let client = Client::builder()
        .timeout(Duration::from_secs(u64::from(
            settings.remote_timeout_seconds,
        )))
        .build()
        .map_err(|err| format!("Failed to create remote transcription client: {err}"))?;
    let url = base_url
        .join("v1/transcriptions/stream")
        .map_err(|err| format!("Invalid remote streaming transcription URL: {err}"))?;
    let mut headers = transcription_headers(settings)?;
    headers.insert(CONTENT_TYPE, HeaderValue::from_static(STREAM_CONTENT_TYPE));

    let response = client
        .post(url)
        .headers(headers)
        .body(reqwest::blocking::Body::new(AudioFrameReader::new(
            audio_rx,
        )))
        .send()
        .map_err(|err| format!("Remote streaming transcription request failed: {err}"))?
        .error_for_status()
        .map_err(|err| format!("Remote streaming transcription host returned an error: {err}"))?;
    let result = response
        .json::<RemoteTranscriptionResponse>()
        .map_err(|err| format!("Remote streaming transcription response was invalid: {err}"))?;

    if result.text.trim().is_empty() {
        return Err("Remote streaming transcription returned no speech".to_string());
    }

    Ok(result)
}

#[allow(dead_code)]
pub fn transcribe_remote(
    recording: &Recording,
    settings: &Settings,
) -> Result<RemoteTranscriptionResponse, String> {
    let base_url = validate_remote_base_url(&settings.remote_url)?;
    let wav = encode_wav(recording)?;
    let client = Client::builder()
        .timeout(Duration::from_secs(u64::from(
            settings.remote_timeout_seconds,
        )))
        .build()
        .map_err(|err| format!("Failed to create remote transcription client: {err}"))?;
    let url = base_url
        .join("v1/transcriptions")
        .map_err(|err| format!("Invalid remote transcription URL: {err}"))?;
    let mut headers = transcription_headers(settings)?;
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("audio/wav"));

    let response = client
        .post(url)
        .headers(headers)
        .body(wav)
        .send()
        .map_err(|err| format!("Remote transcription request failed: {err}"))?
        .error_for_status()
        .map_err(|err| format!("Remote transcription host returned an error: {err}"))?;
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
    })
}

pub fn backend_id(backend: TranscriptionBackend) -> &'static str {
    match backend {
        TranscriptionBackend::Whisper => "whisper",
        TranscriptionBackend::SherpaStreaming => "sherpa-streaming",
    }
}

pub fn selected_model_id(settings: &Settings) -> &'static str {
    match settings.transcription_backend {
        TranscriptionBackend::Whisper => settings.model.model_id(),
        TranscriptionBackend::SherpaStreaming => settings.sherpa_model.model_id(),
    }
}

fn auth_headers(settings: &Settings) -> Result<HeaderMap, String> {
    let mut headers = HeaderMap::new();
    let token = settings.remote_auth_token.trim();
    if !token.is_empty() {
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {token}"))
                .map_err(|err| format!("Invalid remote auth token: {err}"))?,
        );
    }
    Ok(headers)
}

fn transcription_headers(settings: &Settings) -> Result<HeaderMap, String> {
    let mut headers = auth_headers(settings)?;
    headers.insert(
        "x-multivoice-client",
        HeaderValue::from_static("multivoice-tauri"),
    );
    headers.insert(
        "x-multivoice-backend",
        HeaderValue::from_static(backend_id(settings.transcription_backend)),
    );
    headers.insert(
        "x-multivoice-model",
        HeaderValue::from_str(selected_model_id(settings))
            .map_err(|err| format!("Invalid selected model header: {err}"))?,
    );
    headers.insert(
        "x-multivoice-language",
        HeaderValue::from_str(&settings.language)
            .map_err(|err| format!("Invalid language header: {err}"))?,
    );
    Ok(headers)
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
    let local_host = matches!(host, "localhost" | "127.0.0.1" | "::1");
    if scheme != "https" && !(scheme == "http" && local_host) {
        return Err("Remote host URL must use HTTPS unless it is localhost".to_string());
    }

    Ok(url)
}

#[cfg(test)]
mod tests {
    use super::{decode_wav, encode_wav, read_stream_frame, write_stream_frame};
    use crate::audio::{AudioFrame, Recording};

    #[test]
    fn wav_round_trip_preserves_mono_pcm() {
        let recording = Recording {
            pcm_i16: vec![0, 1000, -1000],
            sample_rate: 16_000,
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
}
