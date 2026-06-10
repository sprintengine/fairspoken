use serde::{Deserialize, Serialize};
use sha1::{Digest, Sha1};
use sha2::Sha256;
use std::env;
use std::fs::{self, File};
use std::io::{BufReader, Read, Write};
use std::path::{Path, PathBuf};

const MODEL_BASE_URL: &str = "https://huggingface.co/ggerganov/whisper.cpp/resolve/main";

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

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelStatus {
    pub model: WhisperModel,
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

    pub fn prepare(&self, model: WhisperModel) -> Result<ModelStatus, String> {
        self.prepare_with_progress(model, |_| {})
    }

    pub fn prepare_with_progress(
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
