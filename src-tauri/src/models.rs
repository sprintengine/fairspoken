use crate::settings::TranscriptionBackend;
use bzip2::read::BzDecoder;
use serde::{Deserialize, Serialize};
use sha1::{Digest, Sha1};
use sha2::Sha256;
use std::env;
use std::fs::{self, File};
use std::io::{BufReader, Read, Write};
use std::path::{Path, PathBuf};
use tar::Archive;

const MODEL_BASE_URL: &str = "https://huggingface.co/ggerganov/whisper.cpp/resolve/main";
const SHERPA_MODEL_BASE_URL: &str =
    "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models";

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum WhisperModel {
    Tiny,
    Base,
    Small,
    Medium,
    LargeV2,
    LargeV3,
    LargeV3Turbo,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum SherpaModel {
    #[serde(rename = "streaming-zipformer-en-2023-06-26-int8")]
    #[serde(alias = "streaming-zipformer-en20230626-int8")]
    StreamingZipformerEn20230626Int8,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelStatus {
    pub model: WhisperModel,
    pub cached: bool,
    pub message: String,
    pub model_path: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptionModelStatus {
    pub backend: TranscriptionBackend,
    pub model: String,
    pub cached: bool,
    pub message: String,
    pub model_path: String,
}

#[derive(Clone)]
pub struct ModelService {
    base_dir: PathBuf,
}

#[derive(Clone, Debug)]
pub struct ModelPrepareProgress {
    pub stage: &'static str,
    pub message: String,
    pub percentage: u8,
}

impl Default for ModelService {
    fn default() -> Self {
        Self {
            base_dir: default_model_dir(),
        }
    }
}

impl ModelService {
    pub fn status(&self, model: WhisperModel) -> ModelStatus {
        let path = self.path_for(model);
        let cached = path.is_file() && file_hash_matches(&path, model.hash()).unwrap_or(false);
        ModelStatus {
            model,
            cached,
            message: if cached {
                "Model file is available".to_string()
            } else if path.is_file() {
                format!("Model file failed checksum validation: {}", path.display())
            } else {
                format!("Missing model file: {}", path.display())
            },
            model_path: path.display().to_string(),
        }
    }

    pub fn transcription_status(
        &self,
        backend: TranscriptionBackend,
        whisper_model: WhisperModel,
        sherpa_model: SherpaModel,
    ) -> TranscriptionModelStatus {
        match backend {
            TranscriptionBackend::Whisper => {
                let status = self.status(whisper_model);
                TranscriptionModelStatus {
                    backend,
                    model: whisper_model.model_id().to_string(),
                    cached: status.cached,
                    message: status.message,
                    model_path: status.model_path,
                }
            }
            TranscriptionBackend::SherpaStreaming => {
                let status = self.sherpa_status(sherpa_model);
                TranscriptionModelStatus {
                    backend,
                    model: sherpa_model.model_id().to_string(),
                    cached: status.cached,
                    message: status.message,
                    model_path: status.model_path,
                }
            }
        }
    }

    pub fn prepare_transcription_model(
        &self,
        backend: TranscriptionBackend,
        whisper_model: WhisperModel,
        sherpa_model: SherpaModel,
    ) -> Result<TranscriptionModelStatus, String> {
        self.prepare_transcription_model_with_progress(backend, whisper_model, sherpa_model, |_| {})
    }

    pub fn prepare_transcription_model_with_progress(
        &self,
        backend: TranscriptionBackend,
        whisper_model: WhisperModel,
        sherpa_model: SherpaModel,
        mut progress: impl FnMut(ModelPrepareProgress),
    ) -> Result<TranscriptionModelStatus, String> {
        match backend {
            TranscriptionBackend::Whisper => {
                let status = self.prepare_with_progress(whisper_model, |event| progress(event))?;
                Ok(TranscriptionModelStatus {
                    backend,
                    model: whisper_model.model_id().to_string(),
                    cached: status.cached,
                    message: status.message,
                    model_path: status.model_path,
                })
            }
            TranscriptionBackend::SherpaStreaming => {
                let status =
                    self.prepare_sherpa_with_progress(sherpa_model, |event| progress(event))?;
                Ok(TranscriptionModelStatus {
                    backend,
                    model: sherpa_model.model_id().to_string(),
                    cached: status.cached,
                    message: status.message,
                    model_path: status.model_path,
                })
            }
        }
    }

    pub fn prepare(&self, model: WhisperModel) -> Result<ModelStatus, String> {
        self.prepare_with_progress(model, |_| {})
    }

    fn prepare_with_progress(
        &self,
        model: WhisperModel,
        mut progress: impl FnMut(ModelPrepareProgress),
    ) -> Result<ModelStatus, String> {
        let status = self.status(model);
        if status.cached {
            progress(ModelPrepareProgress {
                stage: "ready",
                message: status.message.clone(),
                percentage: 100,
            });
            return Ok(status);
        }

        fs::create_dir_all(&self.base_dir)
            .map_err(|err| format!("Failed to create model directory: {err}"))?;

        let url = format!("{MODEL_BASE_URL}/{}", model.file_name());
        let target = self.path_for(model);
        let tmp = target.with_extension("download");
        download_to_file_with_progress(&url, &tmp, |percentage| {
            progress(ModelPrepareProgress {
                stage: "downloading",
                message: format!("Downloading {}", model.model_id()),
                percentage,
            })
        })?;

        progress(ModelPrepareProgress {
            stage: "validating",
            message: "Validating model checksum".to_string(),
            percentage: 95,
        });
        if !file_hash_matches(&tmp, model.hash())? {
            let _ = fs::remove_file(&tmp);
            return Err("Downloaded model failed checksum validation".to_string());
        }

        fs::rename(&tmp, &target).map_err(|err| format!("Failed to install model file: {err}"))?;
        let status = self.status(model);
        progress(ModelPrepareProgress {
            stage: "ready",
            message: status.message.clone(),
            percentage: 100,
        });
        Ok(status)
    }

    pub fn path_for(&self, model: WhisperModel) -> PathBuf {
        self.base_dir.join(model.file_name())
    }

    pub fn sherpa_paths(&self, model: SherpaModel) -> SherpaModelPaths {
        sherpa_paths_for_dir(self.base_dir.join(model.directory_name()), model)
    }

    fn sherpa_status_for_dir(&self, model: SherpaModel, dir: PathBuf) -> ModelStatusLike {
        let paths = sherpa_paths_for_dir(dir, model);
        let validation = validate_sherpa_model_dir(model, &paths.dir);
        ModelStatusLike {
            cached: validation.is_ok(),
            message: validation.unwrap_or_else(|err| err),
            model_path: paths.dir.display().to_string(),
        }
    }

    fn sherpa_status(&self, model: SherpaModel) -> ModelStatusLike {
        self.sherpa_status_for_dir(model, self.base_dir.join(model.directory_name()))
    }

    fn prepare_sherpa_with_progress(
        &self,
        model: SherpaModel,
        mut progress: impl FnMut(ModelPrepareProgress),
    ) -> Result<ModelStatusLike, String> {
        let status = self.sherpa_status(model);
        if status.cached {
            progress(ModelPrepareProgress {
                stage: "ready",
                message: status.message.clone(),
                percentage: 100,
            });
            return Ok(status);
        }

        fs::create_dir_all(&self.base_dir)
            .map_err(|err| format!("Failed to create model directory: {err}"))?;

        let url = format!("{SHERPA_MODEL_BASE_URL}/{}", model.archive_name());
        let archive_path = self.base_dir.join(model.archive_name());
        let temp_dir = self
            .base_dir
            .join(format!("{}.download", model.directory_name()));
        let final_dir = self.base_dir.join(model.directory_name());

        let _ = fs::remove_dir_all(&temp_dir);
        fs::create_dir_all(&temp_dir)
            .map_err(|err| format!("Failed to create Sherpa model unpack directory: {err}"))?;
        if !archive_path.is_file() {
            download_to_file_with_progress(&url, &archive_path, |percentage| {
                progress(ModelPrepareProgress {
                    stage: "downloading",
                    message: "Downloading Sherpa model archive".to_string(),
                    percentage,
                })
            })?;
        } else {
            progress(ModelPrepareProgress {
                stage: "downloading",
                message: "Using existing Sherpa model archive".to_string(),
                percentage: 70,
            });
        }
        progress(ModelPrepareProgress {
            stage: "unpacking",
            message: "Unpacking Sherpa model archive".to_string(),
            percentage: 75,
        });
        if let Err(err) = unpack_tar_bz2(&archive_path, &temp_dir) {
            let _ = fs::remove_file(&archive_path);
            let _ = fs::remove_dir_all(&temp_dir);
            return Err(err);
        }
        let _ = fs::remove_file(&archive_path);

        let unpacked_dir = temp_dir.join(model.directory_name());
        progress(ModelPrepareProgress {
            stage: "validating",
            message: "Validating Sherpa model files".to_string(),
            percentage: 90,
        });
        let temp_status = self.sherpa_status_for_dir(model, unpacked_dir.clone());
        if !temp_status.cached {
            let _ = fs::remove_dir_all(&temp_dir);
            return Err(temp_status.message);
        }

        let _ = fs::remove_dir_all(&final_dir);
        fs::rename(&unpacked_dir, &final_dir)
            .map_err(|err| format!("Failed to install Sherpa model files: {err}"))?;
        let _ = fs::remove_dir_all(&temp_dir);

        let status = self.sherpa_status(model);
        progress(ModelPrepareProgress {
            stage: "ready",
            message: status.message.clone(),
            percentage: 100,
        });
        Ok(status)
    }
}

pub struct SherpaModelPaths {
    pub dir: PathBuf,
    pub encoder: PathBuf,
    pub decoder: PathBuf,
    pub joiner: PathBuf,
    pub tokens: PathBuf,
}

struct ModelStatusLike {
    cached: bool,
    message: String,
    model_path: String,
}

struct SherpaRequiredFile {
    name: &'static str,
    min_bytes: u64,
}

fn sherpa_paths_for_dir(dir: PathBuf, model: SherpaModel) -> SherpaModelPaths {
    SherpaModelPaths {
        dir: dir.clone(),
        encoder: dir.join(model.encoder_file()),
        decoder: dir.join(model.decoder_file()),
        joiner: dir.join(model.joiner_file()),
        tokens: dir.join("tokens.txt"),
    }
}

fn validate_sherpa_model_dir(model: SherpaModel, dir: &Path) -> Result<String, String> {
    for file in model.required_files() {
        let path = dir.join(file.name);
        if !path.is_file() {
            return Err(format!("Missing Sherpa model file: {}", path.display()));
        }

        let len = fs::metadata(&path)
            .map_err(|err| format!("Failed to inspect Sherpa model file: {err}"))?
            .len();
        if len < file.min_bytes {
            return Err(format!(
                "Sherpa model file failed size validation: {}",
                path.display()
            ));
        }
    }

    Ok("Sherpa model files are available".to_string())
}

impl WhisperModel {
    pub fn from_model_id(model_id: &str) -> Option<Self> {
        match model_id {
            "tiny" => Some(Self::Tiny),
            "base" => Some(Self::Base),
            "small" => Some(Self::Small),
            "medium" => Some(Self::Medium),
            "large-v2" => Some(Self::LargeV2),
            "large-v3" => Some(Self::LargeV3),
            "large-v3-turbo" => Some(Self::LargeV3Turbo),
            _ => None,
        }
    }

    pub fn model_id(self) -> &'static str {
        match self {
            Self::Tiny => "tiny",
            Self::Base => "base",
            Self::Small => "small",
            Self::Medium => "medium",
            Self::LargeV2 => "large-v2",
            Self::LargeV3 => "large-v3",
            Self::LargeV3Turbo => "large-v3-turbo",
        }
    }

    fn file_name(self) -> &'static str {
        match self {
            Self::Tiny => "ggml-tiny.bin",
            Self::Base => "ggml-base.bin",
            Self::Small => "ggml-small.bin",
            Self::Medium => "ggml-medium.bin",
            Self::LargeV2 => "ggml-large-v2.bin",
            Self::LargeV3 => "ggml-large-v3.bin",
            Self::LargeV3Turbo => "ggml-large-v3-turbo.bin",
        }
    }

    fn hash(self) -> ModelHash {
        match self {
            Self::Tiny => ModelHash::Sha1("bd577a113a864445d4c299885e0cb97d4ba92b5f"),
            Self::Base => ModelHash::Sha1("465707469ff3a37a2b9b8d8f89f2f99de7299dac"),
            Self::Small => ModelHash::Sha1("55356645c2b361a969dfd0ef2c5a50d530afd8d5"),
            Self::Medium => ModelHash::Sha1("fd9727b6e1217c2f614f9b698455c4ffd82463b4"),
            Self::LargeV2 => ModelHash::Sha1("0f4c8e34f21cf1a914c59d8b3ce882345ad349d6"),
            Self::LargeV3 => ModelHash::Sha1("ad82bf6a9043ceed055076d0fd39f5f186ff8062"),
            Self::LargeV3Turbo => ModelHash::Sha256(
                "1fc70f774d38eb169993ac391eea357ef47c88757ef72ee5943879b7e8e2bc69",
            ),
        }
    }
}

impl SherpaModel {
    pub fn from_model_id(model_id: &str) -> Option<Self> {
        match model_id {
            "streaming-zipformer-en-2023-06-26-int8" | "streaming-zipformer-en20230626-int8" => {
                Some(Self::StreamingZipformerEn20230626Int8)
            }
            _ => None,
        }
    }

    pub fn model_id(self) -> &'static str {
        "streaming-zipformer-en-2023-06-26-int8"
    }

    fn directory_name(self) -> &'static str {
        "sherpa-onnx-streaming-zipformer-en-2023-06-26"
    }

    fn archive_name(self) -> &'static str {
        "sherpa-onnx-streaming-zipformer-en-2023-06-26.tar.bz2"
    }

    fn encoder_file(self) -> &'static str {
        "encoder-epoch-99-avg-1-chunk-16-left-128.int8.onnx"
    }

    fn decoder_file(self) -> &'static str {
        "decoder-epoch-99-avg-1-chunk-16-left-128.onnx"
    }

    fn joiner_file(self) -> &'static str {
        "joiner-epoch-99-avg-1-chunk-16-left-128.int8.onnx"
    }

    fn required_files(self) -> &'static [SherpaRequiredFile] {
        &[
            SherpaRequiredFile {
                name: "encoder-epoch-99-avg-1-chunk-16-left-128.int8.onnx",
                min_bytes: 60 * 1024 * 1024,
            },
            SherpaRequiredFile {
                name: "decoder-epoch-99-avg-1-chunk-16-left-128.onnx",
                min_bytes: 1 * 1024 * 1024,
            },
            SherpaRequiredFile {
                name: "joiner-epoch-99-avg-1-chunk-16-left-128.int8.onnx",
                min_bytes: 200 * 1024,
            },
            SherpaRequiredFile {
                name: "tokens.txt",
                min_bytes: 4 * 1024,
            },
        ]
    }
}

fn default_model_dir() -> PathBuf {
    if let Some(path) = env::var_os("MULTIVOICE_TAURI_MODEL_DIR") {
        return PathBuf::from(path);
    }

    if let Some(local_app_data) = env::var_os("LOCALAPPDATA") {
        return PathBuf::from(local_app_data)
            .join("Multivoice Tauri")
            .join("models");
    }

    if let Some(home) = env::var_os("HOME") {
        return PathBuf::from(home)
            .join(".local")
            .join("share")
            .join("multivoice-tauri")
            .join("models");
    }

    PathBuf::from("models")
}

fn download_to_file_with_progress(
    url: &str,
    target: &Path,
    mut progress: impl FnMut(u8),
) -> Result<(), String> {
    let mut response = reqwest::blocking::get(url)
        .map_err(|err| format!("Model download failed: {err}"))?
        .error_for_status()
        .map_err(|err| format!("Model download returned an error: {err}"))?;
    let mut file = File::create(target)
        .map_err(|err| format!("Failed to create model download file: {err}"))?;

    let total = response.content_length();
    let mut downloaded = 0_u64;
    let mut last_percentage = 0_u8;
    let mut chunk = [0_u8; 1024 * 64];
    progress(1);
    loop {
        let read = response
            .read(&mut chunk)
            .map_err(|err| format!("Failed to read model download: {err}"))?;
        if read == 0 {
            break;
        }
        file.write_all(&chunk[..read])
            .map_err(|err| format!("Failed to write model download: {err}"))?;
        downloaded += read as u64;
        if let Some(total) = total.filter(|value| *value > 0) {
            let percentage = ((downloaded.saturating_mul(70) / total).min(70)) as u8;
            if percentage > last_percentage {
                last_percentage = percentage;
                progress(percentage.max(1));
            }
        }
    }
    progress(70);
    Ok(())
}

fn unpack_tar_bz2(archive_path: &Path, target_dir: &Path) -> Result<(), String> {
    fs::create_dir_all(target_dir)
        .map_err(|err| format!("Failed to create archive unpack directory: {err}"))?;
    let file = File::open(archive_path)
        .map_err(|err| format!("Failed to open Sherpa model archive: {err}"))?;
    let decoder = BzDecoder::new(file);
    let mut archive = Archive::new(decoder);

    for entry in archive
        .entries()
        .map_err(|err| format!("Failed to read Sherpa model archive: {err}"))?
    {
        let mut entry = entry.map_err(|err| format!("Failed to read archive entry: {err}"))?;
        let path = entry
            .path()
            .map_err(|err| format!("Failed to read archive entry path: {err}"))?;
        if path
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
        {
            return Err("Sherpa model archive contains an unsafe path".to_string());
        }
        entry
            .unpack_in(target_dir)
            .map_err(|err| format!("Failed to unpack Sherpa model archive: {err}"))?;
    }

    Ok(())
}

#[derive(Clone, Copy)]
enum ModelHash {
    Sha1(&'static str),
    Sha256(&'static str),
}

fn file_hash_matches(path: &Path, expected: ModelHash) -> Result<bool, String> {
    let file = File::open(path).map_err(|err| format!("Failed to open model file: {err}"))?;
    let mut reader = BufReader::new(file);
    let mut chunk = [0_u8; 1024 * 64];
    match expected {
        ModelHash::Sha1(expected) => {
            let mut hasher = Sha1::new();
            loop {
                let read = reader
                    .read(&mut chunk)
                    .map_err(|err| format!("Failed to hash model file: {err}"))?;
                if read == 0 {
                    break;
                }
                hasher.update(&chunk[..read]);
            }
            Ok(hex_digest(hasher.finalize().as_slice()) == expected)
        }
        ModelHash::Sha256(expected) => {
            let mut hasher = Sha256::new();
            loop {
                let read = reader
                    .read(&mut chunk)
                    .map_err(|err| format!("Failed to hash model file: {err}"))?;
                if read == 0 {
                    break;
                }
                hasher.update(&chunk[..read]);
            }
            Ok(hex_digest(hasher.finalize().as_slice()) == expected)
        }
    }
}

fn hex_digest(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>()
}

#[cfg(test)]
mod tests {
    use super::{file_hash_matches, ModelHash, ModelService, WhisperModel};
    use std::fs;

    #[test]
    fn validates_sha1_hashes() {
        let path =
            std::env::temp_dir().join(format!("multivoice-tauri-sha1-{}.txt", std::process::id()));
        fs::write(&path, b"abc").expect("write fixture");

        assert!(file_hash_matches(
            &path,
            ModelHash::Sha1("a9993e364706816aba3e25717850c26c9cd0d89d")
        )
        .expect("hash fixture"));
        assert!(!file_hash_matches(
            &path,
            ModelHash::Sha1("0000000000000000000000000000000000000000")
        )
        .expect("hash fixture"));

        let _ = fs::remove_file(path);
    }

    #[test]
    fn validates_sha256_hashes() {
        let path = std::env::temp_dir().join(format!(
            "multivoice-tauri-sha256-{}.txt",
            std::process::id()
        ));
        fs::write(&path, b"abc").expect("write fixture");

        assert!(file_hash_matches(
            &path,
            ModelHash::Sha256("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")
        )
        .expect("hash fixture"));

        let _ = fs::remove_file(path);
    }

    #[test]
    fn maps_large_v3_turbo_model_metadata() {
        let model = WhisperModel::from_model_id("large-v3-turbo").expect("model");

        assert_eq!(model.model_id(), "large-v3-turbo");
        assert_eq!(model.file_name(), "ggml-large-v3-turbo.bin");
    }

    #[test]
    fn reports_missing_model_status() {
        let service = ModelService {
            base_dir: std::env::temp_dir()
                .join(format!("multivoice-tauri-models-{}", std::process::id())),
        };
        let status = service.status(WhisperModel::Tiny);

        assert!(!status.cached);
        assert!(status.message.contains("Missing model file"));
        assert!(status.model_path.ends_with("ggml-tiny.bin"));
    }

    #[test]
    #[ignore = "downloads the tiny whisper.cpp model"]
    fn downloads_tiny_model() {
        let service = ModelService {
            base_dir: std::env::temp_dir().join(format!(
                "multivoice-tauri-model-download-{}",
                std::process::id()
            )),
        };
        let status = service
            .prepare(WhisperModel::Tiny)
            .expect("prepare tiny model");

        assert!(status.cached);
        let _ = fs::remove_dir_all(service.base_dir);
    }
}
