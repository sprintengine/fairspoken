//! Where model files come from: Hugging Face (the default), a Hugging
//! Face–compatible mirror, or a local or network folder. Organisations that
//! block huggingface.co host the models themselves and point the app (the
//! `modelSource` setting) or the transcription host (`--model-source`,
//! `FAIRSPOKEN_MODEL_SOURCE`, the host config's `modelSource`) at their copy.
//! The Swift apps read the same setting the same way; docs/model-sources.md
//! is the admin guide.
//!
//! - `""`: Hugging Face, `https://huggingface.co`.
//! - `http://` or `https://`: a mirror's base URL. A file is fetched from
//!   `{base}/{repo}/resolve/{revision}/{file}`, the Hub's own layout.
//! - Anything else (an absolute path, `~/…`, a `file://` URL): a folder
//!   holding `{folder}/{owner}/{repo-name}/{file}`, what
//!   `hf download owner/repo-name --local-dir {folder}/owner/repo-name`
//!   writes. Revisions are ignored; files are copied, never linked.

use reqwest::blocking::{Client, Response};
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

pub const HUGGING_FACE: &str = "https://huggingface.co";
/// The longest model source accepted, in characters.
pub const MAX_LEN: usize = 2048;

/// One file in a Hugging Face model repository.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HubFile {
    /// `owner/name`.
    pub repo: &'static str,
    /// A branch (`main`) or a pinned commit.
    pub revision: &'static str,
    /// The file's path inside the repository, `/`-separated.
    pub path: String,
}

impl HubFile {
    pub fn new(repo: &'static str, revision: &'static str, path: impl Into<String>) -> Self {
        Self {
            repo,
            revision,
            path: path.into(),
        }
    }

    /// The file's own name, without any folder inside the repository.
    pub fn name(&self) -> &str {
        self.path.rsplit('/').next().unwrap_or(&self.path)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum ModelSource {
    #[default]
    HuggingFace,
    /// A mirror's base URL, without a trailing slash.
    Mirror(String),
    /// A folder laid out as `{owner}/{repo-name}/{file}`.
    Folder(PathBuf),
}

/// A model file opened for reading, from the network or a folder.
pub struct Opened {
    pub reader: Box<dyn Read + Send>,
    /// The size, when the source reports it.
    pub len: Option<u64>,
}

impl ModelSource {
    /// Reads a `modelSource` value. Errors are user-facing.
    pub fn parse(raw: &str) -> Result<Self, String> {
        let value = raw.trim();
        if value.is_empty() {
            return Ok(Self::HuggingFace);
        }
        if value.chars().count() > MAX_LEN {
            return Err(format!(
                "The model source is too long: use at most {MAX_LEN} characters."
            ));
        }
        let scheme = url_scheme(value).map(|scheme| scheme.to_ascii_lowercase());
        if scheme.is_some() && has_credentials(value) {
            return Err(
                "The model source URL must not contain a user name or password.".to_string(),
            );
        }
        match scheme.as_deref() {
            Some("http" | "https") => {
                let url = reqwest::Url::parse(value)
                    .map_err(|err| format!("The model source is not a valid URL ({err}): {value}"))?;
                if !url.username().is_empty() || url.password().is_some() {
                    return Err(
                        "The model source URL must not contain a user name or password."
                            .to_string(),
                    );
                }
                if url.host_str().is_none_or(str::is_empty) {
                    return Err(format!("The model source URL has no host: {value}"));
                }
                Ok(Self::Mirror(value.trim_end_matches('/').to_string()))
            }
            Some("file") => {
                let url = reqwest::Url::parse(value).map_err(|err| {
                    format!("The model source is not a valid file URL ({err}): {value}")
                })?;
                let path = url.to_file_path().map_err(|()| {
                    format!("The model source file URL does not name a folder on this computer: {value}")
                })?;
                Ok(Self::Folder(path))
            }
            Some(other) => Err(format!(
                "The model source must be an http:// or https:// URL, a file:// URL or an absolute folder path, not a {other}: URL."
            )),
            None => {
                let path = expand_home(value)?;
                if !path.is_absolute() {
                    return Err(format!(
                        "The model source folder must be an absolute path, not {value}"
                    ));
                }
                Ok(Self::Folder(path))
            }
        }
    }

    pub fn is_hugging_face(&self) -> bool {
        matches!(self, Self::HuggingFace)
    }

    fn base_url(&self) -> Option<&str> {
        match self {
            Self::HuggingFace => Some(HUGGING_FACE),
            Self::Mirror(base) => Some(base),
            Self::Folder(_) => None,
        }
    }

    /// `{base}/{repo}/resolve/{revision}/{file}`; `None` for a folder.
    pub fn file_url(&self, file: &HubFile) -> Option<String> {
        self.base_url().map(|base| {
            format!(
                "{base}/{}/resolve/{}/{}",
                file.repo, file.revision, file.path
            )
        })
    }

    /// The Hub's model metadata endpoint, `{base}/api/models/{repo}`; `None`
    /// for a folder, which has no metadata.
    pub fn model_api_url(&self, repo: &str) -> Option<String> {
        self.base_url()
            .map(|base| format!("{base}/api/models/{repo}"))
    }

    /// `{folder}/{owner}/{repo-name}/{file}`; `None` for a URL source.
    pub fn folder_file(&self, file: &HubFile) -> Option<PathBuf> {
        let Self::Folder(folder) = self else {
            return None;
        };
        let mut path = folder.clone();
        path.extend(file.repo.split('/').filter(|part| !part.is_empty()));
        path.extend(file.path.split('/').filter(|part| !part.is_empty()));
        Some(path)
    }

    /// "Downloading" from a URL, "Copying" from a folder.
    pub fn verb(&self) -> &'static str {
        match self {
            Self::Folder(_) => "Copying",
            _ => "Downloading",
        }
    }

    /// Opens `file` for reading: a GET for a URL source, the file itself in a
    /// folder. The error says what failed and where.
    pub fn open(&self, client: &Client, file: &HubFile) -> Result<Opened, String> {
        if let Some(url) = self.file_url(file) {
            return open_url(client, &url, file.name());
        }
        let path = self.folder_file(file).unwrap_or_default();
        if !path.is_file() {
            return Err(not_found_in_folder(file, &path));
        }
        let handle = File::open(&path).map_err(|err| {
            format!(
                "Could not read {} in {}: {err}",
                file.name(),
                parent_display(&path)
            )
        })?;
        let len = handle.metadata().ok().map(|meta| meta.len());
        Ok(Opened {
            reader: Box::new(handle),
            len,
        })
    }

    /// Whether the source can serve `file`, without fetching it: a one-byte
    /// ranged GET for a URL (servers that ignore the range still answer
    /// before the body is read), the file's existence for a folder. The
    /// success message names where it was found.
    pub fn check(&self, client: &Client, file: &HubFile) -> Result<String, String> {
        match self.file_url(file) {
            Some(url) => {
                let response = client
                    .get(&url)
                    .header(reqwest::header::RANGE, "bytes=0-0")
                    .send()
                    .map_err(|err| download_error(file.name(), &url, &error_chain(&err)))?;
                ensure_success(&response, file.name(), &url)?;
                Ok(format!("Found {} at {url}", file.name()))
            }
            None => {
                let path = self.folder_file(file).unwrap_or_default();
                if path.is_file() {
                    Ok(format!(
                        "Found {} in {}",
                        file.name(),
                        parent_display(&path)
                    ))
                } else {
                    Err(not_found_in_folder(file, &path))
                }
            }
        }
    }
}

impl std::fmt::Display for ModelSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::HuggingFace => write!(f, "Hugging Face ({HUGGING_FACE})"),
            Self::Mirror(base) => write!(f, "{base}"),
            Self::Folder(path) => write!(f, "{}", path.display()),
        }
    }
}

/// GETs `url`, failing with "Could not download {name} from {url}: …" on a
/// network error or a non-success status.
pub fn open_url(client: &Client, url: &str, name: &str) -> Result<Opened, String> {
    let response = client
        .get(url)
        .send()
        .map_err(|err| download_error(name, url, &error_chain(&err)))?;
    ensure_success(&response, name, url)?;
    let len = response.content_length();
    Ok(Opened {
        reader: Box::new(response),
        len,
    })
}

/// The stored form of a `modelSource` value: trimmed, and a mirror URL or
/// folder path without trailing slashes (a root such as `/` or `C:\` keeps
/// its own). A value that does not parse is kept (trimmed) so the error
/// stays visible where it is used instead of the app quietly falling back
/// to Hugging Face.
pub fn normalize_setting(raw: &str) -> String {
    let value = raw.replace('\0', "");
    let value = value.trim();
    match ModelSource::parse(value) {
        Ok(ModelSource::Mirror(base)) => base,
        Ok(ModelSource::Folder(_)) if url_scheme(value).is_none() => {
            let trimmed = value.trim_end_matches(['/', '\\']);
            match ModelSource::parse(trimmed) {
                Ok(ModelSource::Folder(_)) => trimmed.to_string(),
                _ => value.to_string(),
            }
        }
        _ => value.to_string(),
    }
}

fn ensure_success(response: &Response, name: &str, url: &str) -> Result<(), String> {
    let status = response.status();
    if status.is_success() {
        Ok(())
    } else {
        Err(download_error(name, url, &status.as_u16().to_string()))
    }
}

fn download_error(name: &str, url: &str, reason: &str) -> String {
    format!("Could not download {name} from {url}: {reason}")
}

fn not_found_in_folder(file: &HubFile, path: &Path) -> String {
    format!("{} not found in {}", file.name(), parent_display(path))
}

fn parent_display(path: &Path) -> String {
    path.parent().unwrap_or(path).display().to_string()
}

/// A reqwest error and its causes ("error sending request: dns error: …"),
/// without the URL, which the message already names.
fn error_chain(err: &reqwest::Error) -> String {
    use std::error::Error;
    let mut message = String::new();
    let mut current: Option<&dyn Error> = Some(err);
    while let Some(error) = current {
        let text = error.to_string();
        let text = text
            .split_once(" for url (")
            .map_or(text.as_str(), |(head, _)| head)
            .to_string();
        if !text.is_empty() && !message.contains(&text) {
            if !message.is_empty() {
                message.push_str(": ");
            }
            message.push_str(&text);
        }
        current = error.source();
    }
    message
}

/// The scheme of a URL-like value (`https`, `file`, `smb`…), or `None` for a
/// path. A single letter before the colon is a Windows drive (`C:\models`),
/// not a scheme.
fn url_scheme(value: &str) -> Option<&str> {
    let (scheme, _) = value.split_once(':')?;
    let mut chars = scheme.chars();
    let first = chars.next()?;
    (scheme.len() > 1
        && first.is_ascii_alphabetic()
        && chars.all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c)))
    .then_some(scheme)
}

/// Whether a URL's authority (between `//` and the path) holds `user@` or
/// `user:password@`.
fn has_credentials(value: &str) -> bool {
    value
        .split_once("//")
        .map(|(_, rest)| rest.split(['/', '?', '#']).next().unwrap_or(""))
        .is_some_and(|authority| authority.contains('@'))
}

/// `~` and `~/…` (or `~\…`) relative to the user's home folder.
fn expand_home(value: &str) -> Result<PathBuf, String> {
    let rest = match value.strip_prefix('~') {
        Some("") => "",
        Some(rest) if rest.starts_with('/') || rest.starts_with('\\') => &rest[1..],
        _ => return Ok(PathBuf::from(value)),
    };
    let home = home_dir().ok_or_else(|| {
        format!("The model source starts with ~ but the home folder is unknown: {value}")
    })?;
    Ok(if rest.is_empty() {
        home
    } else {
        home.join(rest)
    })
}

fn home_dir() -> Option<PathBuf> {
    let names: &[&str] = if cfg!(windows) {
        &["USERPROFILE", "HOME"]
    } else {
        &["HOME"]
    };
    names
        .iter()
        .filter_map(std::env::var_os)
        .find(|value| !value.is_empty())
        .map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parakeet_v3() -> HubFile {
        HubFile::new(
            "istupakov/parakeet-tdt-0.6b-v3-onnx",
            "main",
            "encoder-model.onnx",
        )
    }

    fn ultra() -> HubFile {
        HubFile::new(
            "altunenes/parakeet-rs",
            "4d2a8bc71f5c896ec40faa59732e6716295edaf2",
            "parakeet-ultra/encoder-model.onnx",
        )
    }

    /// An absolute folder on this platform.
    fn absolute_folder() -> PathBuf {
        std::env::temp_dir().join("fairspoken-models")
    }

    #[test]
    fn empty_is_hugging_face_with_todays_urls() {
        for raw in ["", "   ", "\n"] {
            assert_eq!(ModelSource::parse(raw), Ok(ModelSource::HuggingFace));
        }
        let source = ModelSource::default();
        assert_eq!(
            source.file_url(&parakeet_v3()).unwrap(),
            "https://huggingface.co/istupakov/parakeet-tdt-0.6b-v3-onnx/resolve/main/encoder-model.onnx"
        );
        assert_eq!(
            source.model_api_url("ggml-org/Qwen3.5-0.8B-GGUF").unwrap(),
            "https://huggingface.co/api/models/ggml-org/Qwen3.5-0.8B-GGUF"
        );
        assert_eq!(source.folder_file(&parakeet_v3()), None);
    }

    #[test]
    fn mirror_urls_keep_repo_revision_and_path() {
        let source = ModelSource::parse("https://mirror.example/hf").unwrap();
        assert_eq!(
            source,
            ModelSource::Mirror("https://mirror.example/hf".into())
        );
        assert_eq!(
            source.file_url(&parakeet_v3()).unwrap(),
            "https://mirror.example/hf/istupakov/parakeet-tdt-0.6b-v3-onnx/resolve/main/encoder-model.onnx"
        );
        // A revision-pinned file in a subfolder of its repository.
        assert_eq!(
            source.file_url(&ultra()).unwrap(),
            "https://mirror.example/hf/altunenes/parakeet-rs/resolve/4d2a8bc71f5c896ec40faa59732e6716295edaf2/parakeet-ultra/encoder-model.onnx"
        );
        assert_eq!(
            source.model_api_url("unsloth/Qwen3.5-4B-GGUF").unwrap(),
            "https://mirror.example/hf/api/models/unsloth/Qwen3.5-4B-GGUF"
        );
        assert_eq!(source.folder_file(&parakeet_v3()), None);
    }

    #[test]
    fn mirror_trailing_slashes_and_whitespace_are_trimmed() {
        for raw in [
            "https://mirror.example/hf/",
            "  https://mirror.example/hf//  ",
            "https://mirror.example/hf///\t",
        ] {
            assert_eq!(
                ModelSource::parse(raw),
                Ok(ModelSource::Mirror("https://mirror.example/hf".into())),
                "{raw:?}"
            );
            assert_eq!(normalize_setting(raw), "https://mirror.example/hf");
        }
        assert_eq!(
            ModelSource::parse("HTTP://Mirror.Example:8081"),
            Ok(ModelSource::Mirror("HTTP://Mirror.Example:8081".into()))
        );
        assert_eq!(
            ModelSource::parse("https://mirror.example/").unwrap().file_url(&parakeet_v3()).unwrap(),
            "https://mirror.example/istupakov/parakeet-tdt-0.6b-v3-onnx/resolve/main/encoder-model.onnx"
        );
    }

    #[test]
    fn folders_resolve_owner_repo_and_file() {
        let folder = absolute_folder();
        let source = ModelSource::parse(&folder.display().to_string()).unwrap();
        assert_eq!(source, ModelSource::Folder(folder.clone()));
        assert_eq!(source.file_url(&parakeet_v3()), None);
        assert_eq!(source.model_api_url("a/b"), None);
        assert_eq!(
            source.folder_file(&parakeet_v3()).unwrap(),
            folder
                .join("istupakov")
                .join("parakeet-tdt-0.6b-v3-onnx")
                .join("encoder-model.onnx")
        );
        assert_eq!(
            source.folder_file(&ultra()).unwrap(),
            folder
                .join("altunenes")
                .join("parakeet-rs")
                .join("parakeet-ultra")
                .join("encoder-model.onnx")
        );
        assert_eq!(source.verb(), "Copying");
    }

    #[test]
    fn home_relative_folders_expand() {
        let Some(home) = home_dir() else { return };
        assert_eq!(
            ModelSource::parse("~/models"),
            Ok(ModelSource::Folder(home.join("models")))
        );
        assert_eq!(ModelSource::parse("~"), Ok(ModelSource::Folder(home)));
    }

    #[cfg(unix)]
    #[test]
    fn file_urls_become_paths() {
        assert_eq!(
            ModelSource::parse("file:///Volumes/models"),
            Ok(ModelSource::Folder(PathBuf::from("/Volumes/models")))
        );
        assert_eq!(
            ModelSource::parse("FILE:///srv/AI%20models/"),
            Ok(ModelSource::Folder(PathBuf::from("/srv/AI models/")))
        );
        assert_eq!(
            ModelSource::parse("/Volumes/models"),
            Ok(ModelSource::Folder(PathBuf::from("/Volumes/models")))
        );
        // A host other than this computer cannot be a path here.
        assert!(ModelSource::parse("file://fileserver/models").is_err());
    }

    #[cfg(windows)]
    #[test]
    fn file_urls_become_paths() {
        assert_eq!(
            ModelSource::parse("file:///C:/models"),
            Ok(ModelSource::Folder(PathBuf::from(r"C:\models")))
        );
        assert_eq!(
            ModelSource::parse("file://fileserver/share/models"),
            Ok(ModelSource::Folder(PathBuf::from(
                r"\\fileserver\share\models"
            )))
        );
        assert_eq!(
            ModelSource::parse(r"C:\models"),
            Ok(ModelSource::Folder(PathBuf::from(r"C:\models")))
        );
        assert_eq!(
            ModelSource::parse(r"\\fileserver\share\models"),
            Ok(ModelSource::Folder(PathBuf::from(
                r"\\fileserver\share\models"
            )))
        );
    }

    #[test]
    fn rejects_other_schemes_credentials_and_relative_paths() {
        for (raw, needle) in [
            ("ftp://mirror.example/models", "not a ftp: URL"),
            ("smb://server/share", "not a smb: URL"),
            ("s3://bucket/models", "not a s3: URL"),
            (
                "https://user:secret@mirror.example",
                "user name or password",
            ),
            ("https://token@mirror.example", "user name or password"),
            (
                "file://user:secret@localhost/models",
                "user name or password",
            ),
            ("models", "absolute path"),
            ("./models", "absolute path"),
            ("../shared/models", "absolute path"),
            ("~someone/models", "absolute path"),
            ("https://", "not a valid URL"),
        ] {
            let err = ModelSource::parse(raw).expect_err(raw);
            assert!(err.contains(needle), "{raw}: {err}");
        }
        let long = format!("https://mirror.example/{}", "a".repeat(MAX_LEN));
        assert!(ModelSource::parse(&long).unwrap_err().contains("too long"));
        // Exactly the limit is fine.
        let limit = format!("https://m.example/{}", "a".repeat(MAX_LEN - 18));
        assert_eq!(limit.chars().count(), MAX_LEN);
        assert!(ModelSource::parse(&limit).is_ok());
    }

    #[test]
    fn invalid_values_are_kept_trimmed_for_a_visible_error() {
        assert_eq!(normalize_setting("  models  "), "models");
        assert_eq!(normalize_setting(""), "");
        let folder = absolute_folder().display().to_string();
        assert_eq!(normalize_setting(&format!(" {folder} ")), folder);
        assert_eq!(normalize_setting(&format!("{folder}/")), folder);
        assert_eq!(normalize_setting("file:///srv/models/"), "file:///srv/models/");
        assert_eq!(normalize_setting("~/"), "~");
        #[cfg(unix)]
        {
            assert_eq!(normalize_setting("/Volumes/Models//"), "/Volumes/Models");
            assert_eq!(normalize_setting("/"), "/");
        }
        #[cfg(windows)]
        {
            assert_eq!(normalize_setting(r"D:\Models\"), r"D:\Models");
            assert_eq!(normalize_setting(r"C:\"), r"C:\");
        }
    }

    #[test]
    fn opening_from_a_mirror_requests_the_hub_path_and_reports_failures() {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let base = format!("http://{}/hf/", server.server_addr());
        let worker = std::thread::spawn(move || {
            let mut paths = Vec::new();
            for _ in 0..3 {
                let request = server.recv().unwrap();
                paths.push(request.url().to_string());
                let response = if request.url().ends_with("missing.onnx") {
                    tiny_http::Response::from_string("no").with_status_code(404)
                } else {
                    tiny_http::Response::from_string("model")
                };
                request.respond(response).unwrap();
            }
            paths
        });
        let source = ModelSource::parse(&base).unwrap();
        let client = Client::new();
        let mut opened = source.open(&client, &ultra()).unwrap();
        let mut body = String::new();
        opened.reader.read_to_string(&mut body).unwrap();
        assert_eq!(body, "model");
        assert_eq!(opened.len, Some(5));

        let missing = HubFile::new(
            "istupakov/parakeet-tdt-0.6b-v3-onnx",
            "main",
            "missing.onnx",
        );
        let err = source.open(&client, &missing).err().unwrap();
        let base = base.trim_end_matches('/');
        assert_eq!(
            err,
            format!("Could not download missing.onnx from {base}/istupakov/parakeet-tdt-0.6b-v3-onnx/resolve/main/missing.onnx: 404")
        );
        let found = source.check(&client, &parakeet_v3()).unwrap();
        assert!(found.starts_with("Found encoder-model.onnx at "), "{found}");

        assert_eq!(
            worker.join().unwrap(),
            [
                "/hf/altunenes/parakeet-rs/resolve/4d2a8bc71f5c896ec40faa59732e6716295edaf2/parakeet-ultra/encoder-model.onnx",
                "/hf/istupakov/parakeet-tdt-0.6b-v3-onnx/resolve/main/missing.onnx",
                "/hf/istupakov/parakeet-tdt-0.6b-v3-onnx/resolve/main/encoder-model.onnx",
            ]
        );
    }

    #[test]
    fn an_unreachable_mirror_names_the_file_and_url() {
        // Port 9 (discard) on loopback refuses connections.
        let source = ModelSource::parse("http://127.0.0.1:9").unwrap();
        let err = source.check(&Client::new(), &parakeet_v3()).unwrap_err();
        assert!(
            err.starts_with("Could not download encoder-model.onnx from http://127.0.0.1:9/istupakov/parakeet-tdt-0.6b-v3-onnx/resolve/main/encoder-model.onnx: "),
            "{err}"
        );
    }

    #[test]
    fn folder_check_and_open_report_the_folder() {
        let dir = tempfile::tempdir().unwrap();
        let source = ModelSource::Folder(dir.path().to_path_buf());
        let repo = dir
            .path()
            .join("istupakov")
            .join("parakeet-tdt-0.6b-v3-onnx");
        let client = Client::new();
        let err = source.check(&client, &parakeet_v3()).unwrap_err();
        assert_eq!(
            err,
            format!("encoder-model.onnx not found in {}", repo.display())
        );
        assert_eq!(source.open(&client, &parakeet_v3()).err().unwrap(), err);

        std::fs::create_dir_all(&repo).unwrap();
        std::fs::write(repo.join("encoder-model.onnx"), b"weights").unwrap();
        assert_eq!(
            source.check(&client, &parakeet_v3()).unwrap(),
            format!("Found encoder-model.onnx in {}", repo.display())
        );
        let mut opened = source.open(&client, &parakeet_v3()).unwrap();
        assert_eq!(opened.len, Some(7));
        let mut body = Vec::new();
        opened.reader.read_to_end(&mut body).unwrap();
        assert_eq!(body, b"weights");
    }
}
