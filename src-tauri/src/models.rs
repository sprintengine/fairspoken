use crate::model_link::{self, ModelLink};
use serde::de::Deserializer;
use serde::ser::Serializer;
use serde::{Deserialize, Serialize};
use sha1::{Digest, Sha1};
use sha2::Sha256;
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::SystemTime;

#[cfg(feature = "whisper")]
const WHISPER_BASE_URL: &str = "https://huggingface.co/ggerganov/whisper.cpp/resolve/main";

/// Source repo for the ONNX export of NVIDIA Parakeet TDT 0.6B v3 that
/// `parakeet-rs` loads. The four files below use the exact names the
/// `ParakeetTDT` loader expects in its model directory.
const PARAKEET_BASE_URL: &str =
    "https://huggingface.co/istupakov/parakeet-tdt-0.6b-v3-onnx/resolve/main";
const PARAKEET_DIR: &str = "parakeet-tdt-0.6b-v3";
const PARAKEET_MODEL_ID: &str = "parakeet-tdt-0.6b-v3";
const PARAKEET_V2_MODEL_ID: &str = "parakeet-tdt-0.6b-v2";
const PARAKEET_V2_BASE_URL: &str = "https://huggingface.co/istupakov/parakeet-tdt-0.6b-v2-onnx/resolve/0bbb45a3365852604aef28b538a8f066f4ccaa85";
/// Moondream's Parakeet Ultra, a post-trained Parakeet TDT 0.6B v3, as an
/// ONNX export laid out exactly like the istupakov TDT folders.
const PARAKEET_ULTRA_MODEL_ID: &str = "parakeet-ultra";
const PARAKEET_ULTRA_BASE_URL: &str = "https://huggingface.co/altunenes/parakeet-rs/resolve/4d2a8bc71f5c896ec40faa59732e6716295edaf2/parakeet-ultra";
const PARAKEET_FILES: [&str; 4] = [
    "encoder-model.onnx",
    "encoder-model.onnx.data",
    "decoder_joint-model.onnx",
    "vocab.txt",
];

/// The set of speech-to-text models the app can run. Parakeet is the default
/// model. Default builds include Whisper too; `--no-default-features` excludes it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SttModel {
    Parakeet,
    ParakeetV2,
    ParakeetUltra,
    #[cfg(feature = "whisper")]
    Whisper(WhisperModel),
}

impl Default for SttModel {
    fn default() -> Self {
        Self::Parakeet
    }
}

impl SttModel {
    pub fn from_model_id(model_id: &str) -> Option<Self> {
        match model_id {
            PARAKEET_MODEL_ID | "parakeet" => Some(Self::Parakeet),
            PARAKEET_V2_MODEL_ID => Some(Self::ParakeetV2),
            PARAKEET_ULTRA_MODEL_ID => Some(Self::ParakeetUltra),
            #[cfg(feature = "whisper")]
            other => WhisperModel::from_model_id(other).map(Self::Whisper),
            #[cfg(not(feature = "whisper"))]
            _ => None,
        }
    }

    /// Every model this build can run, in the order the picker lists them.
    pub fn all() -> Vec<Self> {
        #[cfg_attr(not(feature = "whisper"), allow(unused_mut))]
        let mut models = vec![Self::Parakeet, Self::ParakeetUltra, Self::ParakeetV2];
        #[cfg(feature = "whisper")]
        models.extend(
            [
                WhisperModel::Tiny,
                WhisperModel::Base,
                WhisperModel::Small,
                WhisperModel::Medium,
                WhisperModel::LargeV2,
                WhisperModel::LargeV3,
                WhisperModel::LargeV3Turbo,
            ]
            .map(Self::Whisper),
        );
        models
    }

    pub fn model_id(self) -> &'static str {
        match self {
            Self::Parakeet => PARAKEET_MODEL_ID,
            Self::ParakeetV2 => PARAKEET_V2_MODEL_ID,
            Self::ParakeetUltra => PARAKEET_ULTRA_MODEL_ID,
            #[cfg(feature = "whisper")]
            Self::Whisper(model) => model.model_id(),
        }
    }

    /// The model's name in messages.
    pub fn display_name(self) -> String {
        match self {
            Self::Parakeet => "Parakeet TDT 0.6B v3".to_string(),
            Self::ParakeetV2 => "Parakeet TDT 0.6B v2".to_string(),
            Self::ParakeetUltra => "Parakeet Ultra".to_string(),
            #[cfg(feature = "whisper")]
            Self::Whisper(model) => format!("Whisper {}", model.model_id()),
        }
    }

    /// True when this model runs on the Whisper engine (and therefore honours
    /// the language and vocabulary-prompt settings).
    pub fn is_whisper(self) -> bool {
        match self {
            Self::Parakeet | Self::ParakeetV2 | Self::ParakeetUltra => false,
            #[cfg(feature = "whisper")]
            Self::Whisper(_) => true,
        }
    }
}

// A flat string wire format ("parakeet-tdt-0.6b-v3", "base", ...) keeps the
// settings file, the host config, and the frontend simple. Unknown ids fall
// back to the default model so an upgraded build that no longer recognises a
// persisted Whisper id (e.g. the default Parakeet-only build) migrates
// gracefully instead of failing to load the whole settings file.
impl Serialize for SttModel {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.model_id())
    }
}

impl<'de> Deserialize<'de> for SttModel {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Ok(Self::from_model_id(&raw).unwrap_or_default())
    }
}

/// The Whisper model family. Kept compiled even without the `whisper` feature
/// (it is pure metadata with no dependencies); it is only *reachable* through
/// `SttModel::Whisper`, which is feature-gated, so a Parakeet-only build can never
/// select it without that feature.
#[cfg_attr(not(feature = "whisper"), allow(dead_code))]
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
    pub model: SttModel,
    pub cached: bool,
    pub message: String,
    pub model_path: String,
}

#[derive(Clone)]
pub struct ModelService {
    base_dir: PathBuf,
    /// Download links by model id (`model_link`): a model with one installs
    /// from it instead of its usual download.
    links: Arc<BTreeMap<String, ModelLink>>,
}

#[derive(Clone, Debug)]
pub struct ModelPrepareProgress {
    pub stage: &'static str,
    pub message: String,
    pub percentage: u8,
}

/// One downloadable artefact belonging to a model. Multi-file models (Parakeet)
/// list several; single-file models (Whisper) list one. `hash` is optional
/// because the published checksums for the Parakeet ONNX export are not pinned
/// yet — when absent, presence on disk is the only validation.
struct ModelFile {
    name: &'static str,
    url: String,
    hash: Option<ModelHash>,
}

impl Default for ModelService {
    fn default() -> Self {
        Self {
            base_dir: default_model_dir(),
            links: Arc::default(),
        }
    }
}

impl ModelService {
    /// Models stored under `base_dir`, for tests.
    #[cfg(test)]
    pub(crate) fn at(base_dir: PathBuf) -> Self {
        Self {
            base_dir,
            links: Arc::default(),
        }
    }

    /// The same models, each one in `links` (by model id) installing from
    /// its link.
    pub fn with_links(mut self, links: BTreeMap<String, ModelLink>) -> Self {
        self.links = Arc::new(links);
        self
    }

    /// The files `model` needs in its folder, by name: what an archive at
    /// its download link must contain.
    #[cfg(test)]
    pub fn required_files(&self, model: SttModel) -> Vec<&'static str> {
        self.storage(model).1.iter().map(|file| file.name).collect()
    }

    pub fn status(&self, model: SttModel) -> ModelStatus {
        let (dir, files) = self.storage(model);
        let mut cached = true;
        let mut first_problem: Option<String> = None;
        for file in &files {
            let path = dir.join(file.name);
            if !path.is_file() {
                cached = false;
                first_problem
                    .get_or_insert_with(|| format!("Missing model file: {}", path.display()));
                continue;
            }
            if let Some(hash) = file.hash {
                if !file_hash_matches(&path, hash).unwrap_or(false) {
                    cached = false;
                    first_problem.get_or_insert_with(|| {
                        format!("Model file failed checksum validation: {}", path.display())
                    });
                }
            }
        }

        ModelStatus {
            model,
            cached,
            message: if cached {
                "Model file is available".to_string()
            } else {
                first_problem.unwrap_or_else(|| "Model files are missing".to_string())
            },
            model_path: self.path_for(model).display().to_string(),
        }
    }

    #[cfg(all(test, feature = "whisper"))]
    pub fn prepare(&self, model: SttModel) -> Result<ModelStatus, String> {
        self.prepare_with_progress(model, |_| {})
    }

    pub fn prepare_with_progress(
        &self,
        model: SttModel,
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

        if let Some(link) = self.links.get(model.model_id()) {
            self.install_from_link(model, link, &mut progress)?;
            let status = self.status(model);
            progress(ModelPrepareProgress {
                stage: "ready",
                message: status.message.clone(),
                percentage: 100,
            });
            return Ok(status);
        }

        let (dir, files) = self.storage(model);
        fs::create_dir_all(&dir)
            .map_err(|err| format!("Failed to create model directory: {err}"))?;

        let file_count = files.len().max(1) as u32;
        for (index, file) in files.iter().enumerate() {
            let target = dir.join(file.name);
            // Skip artefacts already present (and valid, when a checksum is
            // known) so an interrupted multi-file download resumes cheaply.
            let already_valid = target.is_file()
                && file
                    .hash
                    .map(|hash| file_hash_matches(&target, hash).unwrap_or(false))
                    .unwrap_or(true);
            if already_valid {
                continue;
            }

            let tmp = target.with_extension("download");
            let label = format!(
                "Downloading {} ({}/{})",
                model.model_id(),
                index + 1,
                file_count
            );
            let file_index = index as u32;
            download_to_file_with_progress(&file.url, &tmp, |file_percentage| {
                // Map this file's 0-100 onto its slice of the overall 0-95 band.
                let overall =
                    ((file_index * 100 + u32::from(file_percentage)) / file_count).min(95) as u8;
                progress(ModelPrepareProgress {
                    stage: "downloading",
                    message: label.clone(),
                    percentage: overall.max(1),
                });
            })?;

            if let Some(hash) = file.hash {
                progress(ModelPrepareProgress {
                    stage: "validating",
                    message: "Validating model checksum".to_string(),
                    percentage: 96,
                });
                if !file_hash_matches(&tmp, hash)? {
                    let _ = fs::remove_file(&tmp);
                    return Err(format!(
                        "Downloaded model file failed checksum validation: {}",
                        file.name
                    ));
                }
            }

            fs::rename(&tmp, &target)
                .map_err(|err| format!("Failed to install model file: {err}"))?;
        }

        let status = self.status(model);
        progress(ModelPrepareProgress {
            stage: "ready",
            message: status.message.clone(),
            percentage: 100,
        });
        Ok(status)
    }

    /// Downloads `model` from its link and installs exactly the files it
    /// needs, checked against their pinned checksums where there are any.
    fn install_from_link(
        &self,
        model: SttModel,
        link: &ModelLink,
        progress: &mut impl FnMut(ModelPrepareProgress),
    ) -> Result<(), String> {
        let (dir, files) = self.storage(model);
        let names: Vec<&str> = files.iter().map(|file| file.name).collect();
        let name = model.display_name();
        fs::create_dir_all(&self.base_dir)
            .map_err(|err| format!("Failed to create model directory: {err}"))?;
        let client = model_link::client()?;
        model_link::Install {
            link,
            model: &name,
            files: &names,
            dest: &dir,
            work_dir: &self.base_dir,
        }
        .run(
            &client,
            &AtomicBool::new(false),
            |file_name, path| {
                match files.iter().find(|file| file.name == file_name).and_then(|file| file.hash) {
                    Some(hash) => file_hash_matches(path, hash),
                    None => Ok(true),
                }
            },
            |stage, done, total| progress(link_progress(&name, stage, done, total)),
        )
    }

    /// The path handed to the inference engine: the model *directory* for
    /// Parakeet (its loader reads several files from it) and the single model
    /// *file* for Whisper.
    pub fn path_for(&self, model: SttModel) -> PathBuf {
        match model {
            SttModel::Parakeet => self.base_dir.join(PARAKEET_DIR),
            SttModel::ParakeetV2 => self.base_dir.join(PARAKEET_V2_MODEL_ID),
            SttModel::ParakeetUltra => self.base_dir.join(PARAKEET_ULTRA_MODEL_ID),
            #[cfg(feature = "whisper")]
            SttModel::Whisper(whisper) => self.base_dir.join(whisper.file_name()),
        }
    }

    /// Cheap existence check used where loading would be too expensive (host
    /// worker availability, dashboard snapshot): every artefact present, no
    /// checksum verification.
    pub fn files_present(&self, model: SttModel) -> bool {
        let (dir, files) = self.storage(model);
        files.iter().all(|file| dir.join(file.name).is_file())
    }

    /// Newest modification time across the model's installed files, used by the
    /// host worker to decide whether a previously failed load is worth
    /// retrying after the files on disk changed.
    pub fn installed_mtime(&self, model: SttModel) -> Option<SystemTime> {
        let (dir, files) = self.storage(model);
        files
            .iter()
            .filter_map(|file| {
                fs::metadata(dir.join(file.name))
                    .and_then(|meta| meta.modified())
                    .ok()
            })
            .max()
    }

    /// The on-disk directory holding a model's files, plus the file manifest.
    fn storage(&self, model: SttModel) -> (PathBuf, Vec<ModelFile>) {
        match model {
            SttModel::ParakeetV2 => {
                // Same TDT graph interface as v3: 128 mel features, 2×640
                // decoder states, vocabulary-derived blank and five durations.
                // V2 is English-only; v3 remains the multilingual default.
                let hashes = [
                    "3987bcd28175d829d12888a996a84e8f62a0e374d9ffd640662c1515adc679d3",
                    "4dab7362d4874d85965045b1e41b2d61dd2cc0fb25671a7f6b3dc47bf120cc41",
                    "cbb52a07bd70ab5b67f8439d4b3cd8704b18467b4430bcacb5adabe154b8d191",
                    "ec182b70dd42113aff6c5372c75cac58c952443eb22322f57bbd7f53977d497d",
                ];
                let files = PARAKEET_FILES
                    .iter()
                    .zip(hashes)
                    .map(|(name, hash)| ModelFile {
                        name,
                        url: format!("{PARAKEET_V2_BASE_URL}/{name}"),
                        hash: Some(ModelHash::Sha256(hash)),
                    })
                    .collect();
                (self.base_dir.join(PARAKEET_V2_MODEL_ID), files)
            }
            SttModel::ParakeetUltra => {
                // v3's architecture, tokenizer (identical vocab.txt) and 25
                // languages with post-trained weights, so the same TDT loader
                // runs it unchanged. Only the fp32 export is published.
                let hashes = [
                    "76f835e57d62d82f1485c7a84706782e44a123a69f4efa86ed3b4ad56e236051",
                    "6aeb9438f1f45dafc17d27c61a12bc406c0c2ccb8c17219aeeb3f898c283a8e6",
                    "a5911fe202e8fba44251fce252a6c9c7c0a7c724c882a13f81d96611fa2d7ccb",
                    "d58544679ea4bc6ac563d1f545eb7d474bd6cfa467f0a6e2c1dc1c7d37e3c35d",
                ];
                let files = PARAKEET_FILES
                    .iter()
                    .zip(hashes)
                    .map(|(name, hash)| ModelFile {
                        name,
                        url: format!("{PARAKEET_ULTRA_BASE_URL}/{name}"),
                        hash: Some(ModelHash::Sha256(hash)),
                    })
                    .collect();
                (self.base_dir.join(PARAKEET_ULTRA_MODEL_ID), files)
            }
            SttModel::Parakeet => {
                let files = PARAKEET_FILES
                    .iter()
                    .map(|name| ModelFile {
                        name,
                        url: format!("{PARAKEET_BASE_URL}/{name}"),
                        // TODO: pin SHA256 for each Parakeet file once the
                        // checksums can be fetched from Hugging Face.
                        hash: None,
                    })
                    .collect();
                (self.base_dir.join(PARAKEET_DIR), files)
            }
            #[cfg(feature = "whisper")]
            SttModel::Whisper(whisper) => {
                let file = ModelFile {
                    name: whisper.file_name(),
                    url: format!("{WHISPER_BASE_URL}/{}", whisper.file_name()),
                    hash: Some(whisper.hash()),
                };
                (self.base_dir.clone(), vec![file])
            }
        }
    }
}

impl WhisperModel {
    #[cfg_attr(not(feature = "whisper"), allow(dead_code))]
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

    #[cfg_attr(not(feature = "whisper"), allow(dead_code))]
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

    #[cfg_attr(not(feature = "whisper"), allow(dead_code))]
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

    #[cfg_attr(not(feature = "whisper"), allow(dead_code))]
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

/// A link install's progress as the app and host show it: the download is
/// 1-80%, unpacking 80-95%, checking 96%.
fn link_progress(
    name: &str,
    stage: model_link::Stage,
    done: u64,
    total: Option<u64>,
) -> ModelPrepareProgress {
    let fraction = |from: u64, to: u64| match total.filter(|total| *total > 0) {
        Some(total) => (from + (to - from) * done.min(total) / total) as u8,
        None => from as u8,
    };
    match stage {
        model_link::Stage::Downloading => ModelPrepareProgress {
            stage: "downloading",
            message: match total {
                Some(_) => format!("Downloading {name}"),
                None => format!("Downloading {name} · {:.1} MB", done as f64 / 1e6),
            },
            percentage: fraction(1, 80).max(1),
        },
        model_link::Stage::Unpacking => ModelPrepareProgress {
            stage: "downloading",
            message: format!("Unpacking {name}"),
            percentage: fraction(80, 95),
        },
        model_link::Stage::Checking => ModelPrepareProgress {
            stage: "validating",
            message: format!("Checking {name}"),
            percentage: 96,
        },
    }
}

pub(crate) fn default_model_dir() -> PathBuf {
    if let Some(path) = crate::app_dirs::env_var_os("FAIRSPOKEN_MODEL_DIR") {
        return PathBuf::from(path);
    }

    crate::app_dirs::data_dir()
        .map(|dir| dir.join("models"))
        .unwrap_or_else(|| PathBuf::from("models"))
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
            let percentage = ((downloaded.saturating_mul(100) / total).min(100)) as u8;
            if percentage > last_percentage {
                last_percentage = percentage;
                progress(percentage.max(1));
            }
        }
    }
    progress(100);
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
    use super::{file_hash_matches, ModelHash, ModelService, SttModel};
    use std::fs;

    #[test]
    fn validates_sha1_hashes() {
        let path =
            std::env::temp_dir().join(format!("fairspoken-sha1-{}.txt", std::process::id()));
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
            "fairspoken-sha256-{}.txt",
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
    fn parakeet_is_the_default_model() {
        assert_eq!(SttModel::default(), SttModel::Parakeet);
        assert_eq!(SttModel::default().model_id(), "parakeet-tdt-0.6b-v3");
        assert_eq!(
            SttModel::from_model_id("parakeet-tdt-0.6b-v3"),
            Some(SttModel::Parakeet)
        );
        assert_eq!(
            SttModel::from_model_id("parakeet"),
            Some(SttModel::Parakeet)
        );
        assert!(!SttModel::Parakeet.is_whisper());
    }

    #[test]
    fn english_v2_has_its_own_pinned_verified_manifest() {
        let model: SttModel = serde_json::from_str("\"parakeet-tdt-0.6b-v2\"").unwrap();
        assert_eq!(model, SttModel::ParakeetV2);
        assert!(!model.is_whisper());
        assert_eq!(
            serde_json::to_string(&model).unwrap(),
            "\"parakeet-tdt-0.6b-v2\""
        );
        let service = ModelService::default();
        let (path, files) = service.storage(model);
        assert_ne!(path, service.path_for(SttModel::default()));
        assert_eq!(files.len(), 4);
        assert_eq!(
            files.iter().map(|f| f.name).collect::<Vec<_>>(),
            super::PARAKEET_FILES
        );
        assert!(files.iter().all(
            |f| f.url.contains("0bbb45a3365852604aef28b538a8f066f4ccaa85") && f.hash.is_some()
        ));
    }

    #[test]
    fn parakeet_ultra_has_its_own_pinned_verified_manifest() {
        let model: SttModel = serde_json::from_str("\"parakeet-ultra\"").unwrap();
        assert_eq!(model, SttModel::ParakeetUltra);
        assert!(!model.is_whisper());
        assert_eq!(serde_json::to_string(&model).unwrap(), "\"parakeet-ultra\"");
        let service = ModelService::default();
        let (path, files) = service.storage(model);
        assert!(path.ends_with("parakeet-ultra"));
        assert_ne!(path, service.path_for(SttModel::default()));
        assert_eq!(
            files.iter().map(|f| f.name).collect::<Vec<_>>(),
            super::PARAKEET_FILES
        );
        assert!(files.iter().all(|f| f
            .url
            .contains("4d2a8bc71f5c896ec40faa59732e6716295edaf2/parakeet-ultra/")
            && f.hash.is_some()));
    }

    #[test]
    fn stt_model_serde_round_trips_and_migrates_unknown_ids() {
        // Flat string wire format.
        let json = serde_json::to_string(&SttModel::Parakeet).expect("serialize");
        assert_eq!(json, "\"parakeet-tdt-0.6b-v3\"");
        let back: SttModel = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, SttModel::Parakeet);

        // Unknown / legacy ids migrate to the default rather than failing the
        // whole settings/host-config load.
        let migrated: SttModel =
            serde_json::from_str("\"totally-unknown-model\"").expect("graceful migration");
        assert_eq!(migrated, SttModel::default());
    }

    #[cfg(feature = "whisper")]
    #[test]
    fn whisper_model_serde_round_trips_when_compiled_in() {
        use super::WhisperModel;
        let model = SttModel::Whisper(WhisperModel::LargeV3Turbo);
        let json = serde_json::to_string(&model).expect("serialize");
        assert_eq!(json, "\"large-v3-turbo\"");
        assert_eq!(
            serde_json::from_str::<SttModel>(&json).expect("deserialize"),
            model
        );
        assert!(model.is_whisper());
    }

    #[cfg(feature = "whisper")]
    #[test]
    fn maps_whisper_model_ids_when_compiled_in() {
        use super::WhisperModel;
        assert_eq!(
            SttModel::from_model_id("large-v3-turbo"),
            Some(SttModel::Whisper(WhisperModel::LargeV3Turbo))
        );
        assert_eq!(SttModel::from_model_id("base").unwrap().model_id(), "base");
    }

    #[test]
    fn every_listed_model_round_trips_through_its_id() {
        let ids: Vec<&str> = SttModel::all().into_iter().map(SttModel::model_id).collect();
        assert_eq!(&ids[..3], ["parakeet-tdt-0.6b-v3", "parakeet-ultra", "parakeet-tdt-0.6b-v2"]);
        #[cfg(feature = "whisper")]
        assert_eq!(
            &ids[3..],
            ["tiny", "base", "small", "medium", "large-v2", "large-v3", "large-v3-turbo"]
        );
        for id in ids {
            assert_eq!(SttModel::from_model_id(id).map(SttModel::model_id), Some(id));
        }
    }

    #[cfg(not(feature = "whisper"))]
    #[test]
    fn whisper_ids_are_unknown_in_a_parakeet_only_build() {
        assert_eq!(SttModel::from_model_id("base"), None);
    }

    #[test]
    fn reports_missing_model_status_for_parakeet() {
        let service = ModelService::at(
            std::env::temp_dir().join(format!("fairspoken-models-{}", std::process::id())),
        );
        let status = service.status(SttModel::Parakeet);

        assert!(!status.cached);
        assert!(status.message.contains("Missing model file"));
        assert!(status.model_path.ends_with("parakeet-tdt-0.6b-v3"));
    }

    #[test]
    fn a_model_with_a_link_installs_from_its_zip() {
        use crate::model_link::{tests::serve, tests::zip_bytes, ModelLink};
        let names = ModelService::default().required_files(SttModel::Parakeet);
        assert_eq!(names, super::PARAKEET_FILES);
        let entries: Vec<(String, Vec<u8>)> = names
            .iter()
            .map(|name| (format!("parakeet-tdt-0.6b-v3/{name}"), format!("contents of {name}").into_bytes()))
            .collect();
        let entries: Vec<(&str, &[u8])> = entries.iter().map(|(n, b)| (n.as_str(), b.as_slice())).collect();
        let (base, server) = serve(vec![(200, zip_bytes(&entries))]);
        let models = tempfile::tempdir().unwrap();
        let link = ModelLink::parse(&format!("{base}/artifactory/models/parakeet-v3.zip?token=abc")).unwrap();
        let service = ModelService::at(models.path().to_path_buf())
            .with_links([("parakeet-tdt-0.6b-v3".to_string(), link)].into());

        let mut stages = Vec::new();
        let status = service
            .prepare_with_progress(SttModel::Parakeet, |progress| {
                stages.push((progress.stage, progress.message))
            })
            .expect("install from the link");
        assert!(status.cached, "{}", status.message);
        assert_eq!(server.join().unwrap(), ["/artifactory/models/parakeet-v3.zip?token=abc"]);
        for message in ["Downloading Parakeet TDT 0.6B v3", "Unpacking Parakeet TDT 0.6B v3"] {
            assert!(stages.iter().any(|(_, m)| m == message), "{message}: {stages:?}");
        }
        assert_eq!(stages.last().unwrap().0, "ready");
        let installed = models.path().join("parakeet-tdt-0.6b-v3");
        for name in super::PARAKEET_FILES {
            assert_eq!(fs::read_to_string(installed.join(name)).unwrap(), format!("contents of {name}"));
        }
        // Only the model folder is left.
        let left: Vec<_> = fs::read_dir(models.path()).unwrap().flatten().map(|e| e.file_name()).collect();
        assert_eq!(left, ["parakeet-tdt-0.6b-v3"]);

        // Other models keep their usual download: no link, no request.
        assert!(!service.links.contains_key("parakeet-tdt-0.6b-v2"));
    }

    #[test]
    fn a_link_download_is_held_to_the_pinned_checksums() {
        use crate::model_link::{tests::zip_bytes, ModelLink};
        let shared = tempfile::tempdir().unwrap();
        let archive = shared.path().join("v2.zip");
        let names = ModelService::default().required_files(SttModel::ParakeetV2);
        let entries: Vec<(&str, &[u8])> = names.iter().map(|name| (*name, &b"not the model"[..])).collect();
        fs::write(&archive, zip_bytes(&entries)).unwrap();
        let models = tempfile::tempdir().unwrap();
        let service = ModelService::at(models.path().to_path_buf()).with_links(
            [("parakeet-tdt-0.6b-v2".to_string(), ModelLink::Path(archive.clone()))].into(),
        );
        let err = service.prepare_with_progress(SttModel::ParakeetV2, |_| {}).unwrap_err();
        assert_eq!(
            err,
            format!("encoder-model.onnx in the download from {} isn't the file Parakeet TDT 0.6B v2 needs: it failed its size or checksum check.", archive.display())
        );
        assert!(!models.path().join("parakeet-tdt-0.6b-v2").join("encoder-model.onnx").exists());
        assert_eq!(fs::read_dir(models.path()).unwrap().count(), 0);
    }

    // Run explicitly with FAIRSPOKEN_TEST_MODEL_LINK naming a real model zip
    // (from scripts/models/make-model-bundle.sh) and FAIRSPOKEN_TEST_MODEL its
    // model id (default parakeet-tdt-0.6b-v3). Installs into a temporary
    // folder.
    #[test]
    #[ignore = "installs a real model zip named by FAIRSPOKEN_TEST_MODEL_LINK"]
    fn installs_a_real_model_zip() {
        let link = crate::app_dirs::env_var("FAIRSPOKEN_TEST_MODEL_LINK").expect("FAIRSPOKEN_TEST_MODEL_LINK");
        let id = crate::app_dirs::env_var("FAIRSPOKEN_TEST_MODEL").unwrap_or_else(|| "parakeet-tdt-0.6b-v3".into());
        let model = SttModel::from_model_id(&id).expect("a model id");
        let link = crate::model_link::ModelLink::parse(&link).unwrap();
        let models = tempfile::tempdir().unwrap();
        let service = ModelService::at(models.path().to_path_buf()).with_links([(id, link)].into());
        let mut last = String::new();
        let status = service
            .prepare_with_progress(model, |p| {
                if p.message != last {
                    println!("{} {}% {}", p.stage, p.percentage, p.message);
                    last = p.message;
                }
            })
            .unwrap();
        assert!(status.cached, "{}", status.message);
        println!("{}", status.model_path);
    }

    #[cfg(feature = "whisper")]
    #[test]
    #[ignore = "downloads the tiny whisper.cpp model"]
    fn downloads_tiny_model() {
        use super::WhisperModel;
        let service = ModelService::at(std::env::temp_dir().join(format!(
            "fairspoken-model-download-{}",
            std::process::id()
        )));
        let status = service
            .prepare(SttModel::Whisper(WhisperModel::Tiny))
            .expect("prepare tiny model");

        assert!(status.cached);
        let _ = fs::remove_dir_all(service.base_dir);
    }
}
