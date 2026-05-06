use serde::{Deserialize, Serialize};
use sha1::{Digest, Sha1};
use std::env;
use std::fs::{self, File};
use std::io::{self, BufReader, Read};
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
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelStatus {
    pub model: WhisperModel,
    pub cached: bool,
    pub message: String,
    pub model_path: String,
}

pub struct ModelService {
    base_dir: PathBuf,
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
        let cached = path.is_file() && file_sha1_matches(&path, model.sha1()).unwrap_or(false);
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
        let status = self.status(model);
        if status.cached {
            return Ok(status);
        }

        fs::create_dir_all(&self.base_dir)
            .map_err(|err| format!("Failed to create model directory: {err}"))?;

        let url = format!("{MODEL_BASE_URL}/{}", model.file_name());
        let target = self.path_for(model);
        let tmp = target.with_extension("download");
        download_to_file(&url, &tmp)?;

        if !file_sha1_matches(&tmp, model.sha1())? {
            let _ = fs::remove_file(&tmp);
            return Err("Downloaded model failed checksum validation".to_string());
        }

        fs::rename(&tmp, &target).map_err(|err| format!("Failed to install model file: {err}"))?;
        Ok(self.status(model))
    }

    pub fn path_for(&self, model: WhisperModel) -> PathBuf {
        self.base_dir.join(model.file_name())
    }
}

impl WhisperModel {
    fn file_name(self) -> &'static str {
        match self {
            Self::Tiny => "ggml-tiny.bin",
            Self::Base => "ggml-base.bin",
            Self::Small => "ggml-small.bin",
            Self::Medium => "ggml-medium.bin",
            Self::LargeV2 => "ggml-large-v2.bin",
            Self::LargeV3 => "ggml-large-v3.bin",
        }
    }

    fn sha1(self) -> &'static str {
        match self {
            Self::Tiny => "bd577a113a864445d4c299885e0cb97d4ba92b5f",
            Self::Base => "465707469ff3a37a2b9b8d8f89f2f99de7299dac",
            Self::Small => "55356645c2b361a969dfd0ef2c5a50d530afd8d5",
            Self::Medium => "fd9727b6e1217c2f614f9b698455c4ffd82463b4",
            Self::LargeV2 => "0f4c8e34f21cf1a914c59d8b3ce882345ad349d6",
            Self::LargeV3 => "ad82bf6a9043ceed055076d0fd39f5f186ff8062",
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

fn download_to_file(url: &str, target: &Path) -> Result<(), String> {
    let mut response = reqwest::blocking::get(url)
        .map_err(|err| format!("Model download failed: {err}"))?
        .error_for_status()
        .map_err(|err| format!("Model download returned an error: {err}"))?;
    let mut file = File::create(target)
        .map_err(|err| format!("Failed to create model download file: {err}"))?;

    io::copy(&mut response, &mut file)
        .map_err(|err| format!("Failed to write model download: {err}"))?;
    Ok(())
}

fn file_sha1_matches(path: &Path, expected: &str) -> Result<bool, String> {
    let file = File::open(path).map_err(|err| format!("Failed to open model file: {err}"))?;
    let mut reader = BufReader::new(file);
    let mut hasher = Sha1::new();
    let mut chunk = [0_u8; 1024 * 64];
    loop {
        let read = reader
            .read(&mut chunk)
            .map_err(|err| format!("Failed to hash model file: {err}"))?;
        if read == 0 {
            break;
        }
        hasher.update(&chunk[..read]);
    }
    let actual = hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    Ok(actual == expected)
}

#[cfg(test)]
mod tests {
    use super::{file_sha1_matches, ModelService, WhisperModel};
    use std::fs;

    #[test]
    fn validates_sha1_hashes() {
        let path =
            std::env::temp_dir().join(format!("multivoice-tauri-sha1-{}.txt", std::process::id()));
        fs::write(&path, b"abc").expect("write fixture");

        assert!(
            file_sha1_matches(&path, "a9993e364706816aba3e25717850c26c9cd0d89d")
                .expect("hash fixture")
        );
        assert!(
            !file_sha1_matches(&path, "0000000000000000000000000000000000000000")
                .expect("hash fixture")
        );

        let _ = fs::remove_file(path);
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
