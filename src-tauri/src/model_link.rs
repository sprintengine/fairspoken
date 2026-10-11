//! Per-model download links. Organisations that can't reach the usual model
//! hosts put a zip of each model on their own server (an Artifactory generic
//! repository, an intranet web server, a network share) and give the app a
//! link to it: the app's `modelLinks` setting, set from the Models screen, or
//! the transcription host's `--model-link`, `FAIRSPOKEN_MODEL_LINKS` and
//! `modelLinks` in its config. A model without a link downloads as it always
//! has. docs/model-sources.md is the admin guide.
//!
//! A link is an `http://` or `https://` URL (a query string is kept, a
//! `#fragment` dropped, a `user:password@` refused) or a path or `file://`
//! URL to a file. What it fetches is a zip, tar or tar.gz holding the
//! model's files in any folder (or, for a one-file model, the file itself).
//! Installing unpacks it into a staging folder next to the model, finds the
//! folder with every file the model needs, checks them and moves them into
//! place.
//!
//! A link's query string may carry an access token, so messages and logs
//! show a link as `scheme://host/path` only (`Display`).

use reqwest::blocking::Client;
use std::collections::VecDeque;
use std::fmt;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// The longest link accepted, in characters.
pub const MAX_LEN: usize = 2048;
/// The most entries (files and folders) an archive may have.
pub const MAX_ENTRIES: usize = 10_000;
/// The most an archive may unpack to, and the largest download accepted.
pub const MAX_BYTES: u64 = 8 * 1024 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ModelLink {
    /// An `http://` or `https://` URL, without a fragment.
    Url(reqwest::Url),
    /// A file on this computer or a network share.
    Path(PathBuf),
}

impl ModelLink {
    /// Reads a link as typed. Errors are user-facing and never repeat a
    /// query string.
    pub fn parse(raw: &str) -> Result<Self, String> {
        let value = raw.replace('\0', "");
        let value = value.trim();
        if value.is_empty() {
            return Err("Enter a link to a .zip of the model.".to_string());
        }
        if value.chars().count() > MAX_LEN {
            return Err(format!(
                "The link is too long: use at most {MAX_LEN} characters."
            ));
        }
        let scheme = url_scheme(value).map(|scheme| scheme.to_ascii_lowercase());
        if scheme.is_some() && has_credentials(value) {
            return Err(
                "The link must not contain a user name or password (user:password@). Use a link that downloads without signing in."
                    .to_string(),
            );
        }
        match scheme.as_deref() {
            Some("http" | "https") => {
                let mut url = reqwest::Url::parse(value).map_err(|err| {
                    format!("{} isn't a valid link: {err}.", without_query(value))
                })?;
                if !url.username().is_empty() || url.password().is_some() {
                    return Err(
                        "The link must not contain a user name or password (user:password@). Use a link that downloads without signing in."
                            .to_string(),
                    );
                }
                if url.host_str().is_none_or(str::is_empty) {
                    return Err(format!("{} has no server name.", without_query(value)));
                }
                url.set_fragment(None);
                Ok(Self::Url(url))
            }
            Some("file") => {
                let url = reqwest::Url::parse(value).map_err(|err| {
                    format!("{} isn't a valid file link: {err}.", without_query(value))
                })?;
                url.to_file_path().map(Self::Path).map_err(|()| {
                    format!(
                        "{} doesn't name a file this computer can open.",
                        without_query(value)
                    )
                })
            }
            Some(other) => Err(format!(
                "The link must start with http:// or https://, or be the full path to a file, not {other}:."
            )),
            None => {
                let path = expand_home(value)?;
                if path.is_absolute() {
                    Ok(Self::Path(path))
                } else {
                    Err(format!(
                        "{value} isn't a full path. Use an http:// or https:// link, or the full path to the file."
                    ))
                }
            }
        }
    }

    /// The form saved in settings: the URL without its fragment, or the
    /// path as typed (trimmed).
    pub fn normalize(raw: &str) -> Result<String, String> {
        Ok(match Self::parse(raw)? {
            Self::Url(url) => url.to_string(),
            Self::Path(_) => raw.replace('\0', "").trim().to_string(),
        })
    }

    /// Fetches the link into `dest`, reporting bytes so far and the total
    /// when known. Refuses more than `MAX_BYTES`.
    pub fn fetch(
        &self,
        client: &Client,
        dest: &Path,
        cancel: &AtomicBool,
        mut progress: impl FnMut(u64, Option<u64>),
    ) -> Result<(), String> {
        let (mut reader, total): (Box<dyn Read>, Option<u64>) = match self {
            Self::Url(url) => {
                let response = client
                    .get(url.clone())
                    .send()
                    .map_err(|err| format!("Could not download {self}: {}", self.error_chain(&err)))?;
                let status = response.status();
                if !status.is_success() {
                    return Err(format!("Could not download {self}: HTTP {}", status.as_u16()));
                }
                let total = response.content_length();
                (Box::new(response), total)
            }
            Self::Path(path) => {
                if path.is_dir() {
                    return Err(format!(
                        "{self} is a folder. Link to a .zip of the model instead."
                    ));
                }
                let file = File::open(path)
                    .map_err(|err| format!("Could not read {self}: {err}"))?;
                let total = file.metadata().ok().map(|meta| meta.len());
                (Box::new(file), total)
            }
        };
        if total.is_some_and(|total| total > MAX_BYTES) {
            return Err(too_large(self));
        }
        let mut file = File::create(dest)
            .map_err(|err| format!("Could not save the download from {self}: {err}"))?;
        let mut done = 0_u64;
        let mut buffer = vec![0_u8; 256 * 1024];
        let mut last = Instant::now();
        progress(0, total);
        loop {
            check_cancel(cancel)?;
            let read = reader.read(&mut buffer).map_err(|err| match self {
                Self::Url(_) => format!("Could not download {self}: {}", self.redact(&err.to_string())),
                Self::Path(_) => format!("Could not read {self}: {err}"),
            })?;
            if read == 0 {
                break;
            }
            done += read as u64;
            if done > MAX_BYTES {
                return Err(too_large(self));
            }
            file.write_all(&buffer[..read])
                .map_err(|err| format!("Could not save the download from {self}: {err}"))?;
            if last.elapsed() >= PROGRESS_INTERVAL {
                progress(done, total);
                last = Instant::now();
            }
        }
        if let Some(total) = total.filter(|total| *total != done) {
            return Err(format!(
                "Could not download {self}: it stopped after {done} of {total} bytes. Please retry."
            ));
        }
        file.sync_all()
            .map_err(|err| format!("Could not save the download from {self}: {err}"))?;
        progress(done, total.or(Some(done)));
        Ok(())
    }

    /// A reqwest error and its causes ("error sending request: dns error:
    /// …"), without the URL, which the message already names.
    fn error_chain(&self, err: &reqwest::Error) -> String {
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
        self.redact(&message)
    }

    /// `text` with this link's full URL and query string taken out.
    fn redact(&self, text: &str) -> String {
        match self {
            Self::Url(url) => {
                let mut text = text.replace(url.as_str(), &self.to_string());
                if let Some(query) = url.query().filter(|query| !query.is_empty()) {
                    text = text.replace(query, "…");
                }
                text
            }
            Self::Path(_) => text.to_string(),
        }
    }
}

/// `scheme://host[:port]/path` for a URL (never the query string), the path
/// for a file.
impl fmt::Display for ModelLink {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Url(url) => {
                write!(f, "{}://{}", url.scheme(), url.host_str().unwrap_or(""))?;
                if let Some(port) = url.port() {
                    write!(f, ":{port}")?;
                }
                write!(f, "{}", url.path())
            }
            Self::Path(path) => write!(f, "{}", path.display()),
        }
    }
}

/// How far an install has got: the bytes done and the total when known.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    Downloading,
    Unpacking,
    Checking,
}

/// One model to install from a link.
pub struct Install<'a> {
    pub link: &'a ModelLink,
    /// The model's name in messages, e.g. "Parakeet TDT 0.6B v3".
    pub model: &'a str,
    /// Every file the model needs, by name.
    pub files: &'a [&'a str],
    /// Where the files go.
    pub dest: &'a Path,
    /// Where the download and the unpacked files wait: on the same disk as
    /// `dest`, so installing is a rename.
    pub work_dir: &'a Path,
}

impl Install<'_> {
    /// Downloads, unpacks, finds, checks (`verify` says whether a file is
    /// the right one; it has the file's name and its unpacked path) and
    /// installs the model's files. The download and the unpacked files are
    /// removed whatever happens; the model's folder only changes once every
    /// file has passed `verify`.
    pub fn run(
        &self,
        client: &Client,
        cancel: &AtomicBool,
        mut verify: impl FnMut(&str, &Path) -> Result<bool, String>,
        mut progress: impl FnMut(Stage, u64, Option<u64>),
    ) -> Result<(), String> {
        let link = self.link;
        let work = WorkDir::create(self.work_dir)?;
        let download = work.0.join("download");
        link.fetch(client, &download, cancel, |done, total| {
            progress(Stage::Downloading, done, total)
        })?;

        let format = detect(&download)
            .map_err(|err| format!("Could not read the download from {link}: {err}"))?;
        let found: Vec<PathBuf> = match format {
            Format::Other if self.files.len() == 1 => vec![download.clone()],
            Format::Other => {
                return Err(format!(
                    "{link} isn't a zip or tar archive.{}",
                    web_page_hint(&download)
                ))
            }
            archive => {
                let unpacked = work.0.join("unpacked");
                fs::create_dir_all(&unpacked)
                    .map_err(|err| format!("Could not unpack {link}: {err}"))?;
                unpack(archive, &download, &unpacked, link, cancel, &mut |done, total| {
                    progress(Stage::Unpacking, done, total)
                })?;
                let _ = fs::remove_file(&download);
                let dir = find_dir(&unpacked, self.files)
                    .ok_or_else(|| missing_files(link, self.model, self.files))?;
                self.files.iter().map(|name| dir.join(name)).collect()
            }
        };

        progress(Stage::Checking, 0, None);
        for (name, path) in self.files.iter().zip(&found) {
            check_cancel(cancel)?;
            if !verify(name, path)? {
                return Err(if format == Format::Other {
                    format!(
                        "The download from {link} isn't the {} file: it failed its size or checksum check.{}",
                        self.model,
                        web_page_hint(path)
                    )
                } else {
                    format!(
                        "{name} in the download from {link} isn't the file {} needs: it failed its size or checksum check.",
                        self.model
                    )
                });
            }
        }

        fs::create_dir_all(self.dest)
            .map_err(|err| format!("Could not install {}: {err}", self.model))?;
        for (name, path) in self.files.iter().zip(&found) {
            move_file(path, &self.dest.join(name))
                .map_err(|err| format!("Could not install {name} for {}: {err}", self.model))?;
        }
        Ok(())
    }
}

const PROGRESS_INTERVAL: Duration = Duration::from_millis(150);

fn check_cancel(cancel: &AtomicBool) -> Result<(), String> {
    if cancel.load(Ordering::SeqCst) {
        Err("Cancelled".to_string())
    } else {
        Ok(())
    }
}

fn too_large(link: &ModelLink) -> String {
    format!("{link} is larger than 8 GB, more than any model needs.")
}

/// "The download from {link} doesn't contain the {model} files. It needs a,
/// b and c."
pub fn missing_files(link: &ModelLink, model: &str, files: &[&str]) -> String {
    let (noun, list) = match files {
        [one] => ("file", (*one).to_string()),
        [rest @ .., last] => ("files", format!("{} and {last}", rest.join(", "))),
        [] => ("files", String::new()),
    };
    format!("The download from {link} doesn't contain the {model} {noun}. It needs {list}.")
}

/// A note for a download that is a web page, which is what a link that
/// needs a sign-in usually returns.
fn web_page_hint(path: &Path) -> &'static str {
    let mut head = [0_u8; 256];
    let read = File::open(path)
        .and_then(|mut file| file.read(&mut head))
        .unwrap_or(0);
    let text = String::from_utf8_lossy(&head[..read]).trim_start().to_ascii_lowercase();
    if text.starts_with("<!doctype html") || text.starts_with("<html") {
        " It's a web page: the link may need a sign-in, or point to a page about the file rather than the file itself."
    } else {
        ""
    }
}

/// A uniquely named folder in `parent`, removed with everything in it when
/// dropped.
struct WorkDir(PathBuf);

impl WorkDir {
    fn create(parent: &Path) -> Result<Self, String> {
        let path = parent.join(format!(".link-download-{}", uuid::Uuid::new_v4().simple()));
        fs::create_dir_all(&path)
            .map_err(|err| format!("Could not create a download folder in {}: {err}", parent.display()))?;
        Ok(Self(path))
    }
}

impl Drop for WorkDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Format {
    Zip,
    Tar,
    TarGz,
    Other,
}

/// What a file is, from its first bytes: links often have no extension.
fn detect(path: &Path) -> io::Result<Format> {
    let mut head = [0_u8; 512];
    let read = read_up_to(&mut File::open(path)?, &mut head)?;
    let head = &head[..read];
    if head.starts_with(b"PK\x03\x04") || head.starts_with(b"PK\x05\x06") {
        return Ok(Format::Zip);
    }
    if is_tar_header(head) {
        return Ok(Format::Tar);
    }
    if head.starts_with(&[0x1f, 0x8b]) {
        // Gzip: a tar.gz only when what it holds is a tar.
        let mut inner = [0_u8; 512];
        let mut decoder = flate2::read::GzDecoder::new(File::open(path)?);
        let read = read_up_to(&mut decoder, &mut inner).unwrap_or(0);
        if is_tar_header(&inner[..read]) {
            return Ok(Format::TarGz);
        }
    }
    Ok(Format::Other)
}

fn is_tar_header(head: &[u8]) -> bool {
    head.len() >= 262 && &head[257..262] == b"ustar"
}

fn read_up_to(reader: &mut impl Read, buffer: &mut [u8]) -> io::Result<usize> {
    let mut filled = 0;
    while filled < buffer.len() {
        match reader.read(&mut buffer[filled..])? {
            0 => break,
            read => filled += read,
        }
    }
    Ok(filled)
}

/// `path` as a plain relative path, `..` resolved, or `None` when it is
/// absolute or climbs out of the folder it is unpacked into.
fn enclosed(path: &Path) -> Option<PathBuf> {
    let mut clean = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => clean.push(part),
            Component::CurDir => {}
            Component::ParentDir => {
                if !clean.pop() {
                    return None;
                }
            }
            Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    Some(clean)
}

/// Counts what an archive unpacks to against `MAX_BYTES`.
struct Budget<'a> {
    link: &'a ModelLink,
    entries: usize,
    bytes: u64,
}

impl Budget<'_> {
    fn entry(&mut self) -> Result<(), String> {
        self.entries += 1;
        if self.entries > MAX_ENTRIES {
            return Err(format!(
                "{} has more than {MAX_ENTRIES} files, more than any model needs.",
                self.link
            ));
        }
        Ok(())
    }

    /// Writes `reader` to `path`, stopping at the limit.
    fn write(
        &mut self,
        reader: &mut impl Read,
        path: &Path,
        cancel: &AtomicBool,
    ) -> Result<(), String> {
        let link = self.link;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|err| format!("Could not unpack {link}: {err}"))?;
        }
        let mut file = File::create(path).map_err(|err| format!("Could not unpack {link}: {err}"))?;
        let mut buffer = vec![0_u8; 256 * 1024];
        loop {
            check_cancel(cancel)?;
            let read = reader
                .read(&mut buffer)
                .map_err(|err| format!("Could not unpack {link}: {err}"))?;
            if read == 0 {
                return Ok(());
            }
            self.bytes += read as u64;
            if self.bytes > MAX_BYTES {
                return Err(format!(
                    "{link} unpacks to more than 8 GB, more than any model needs."
                ));
            }
            file.write_all(&buffer[..read])
                .map_err(|err| format!("Could not unpack {link}: {err}"))?;
        }
    }
}

fn unsafe_entry(link: &ModelLink, name: &str) -> String {
    format!("{link} contains {name}, which would unpack outside its folder.")
}

fn link_entry(link: &ModelLink, name: &str) -> String {
    format!("{link} contains a link ({name}). Model archives must hold plain files.")
}

/// Unpacks `archive` into `into`, which must be empty: plain files and
/// folders only, every one inside `into`.
fn unpack(
    format: Format,
    archive: &Path,
    into: &Path,
    link: &ModelLink,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(u64, Option<u64>),
) -> Result<(), String> {
    let mut budget = Budget {
        link,
        entries: 0,
        bytes: 0,
    };
    let unreadable = |err: &dyn fmt::Display| format!("{link} isn't a readable archive: {err}");
    let file = File::open(archive).map_err(|err| format!("Could not unpack {link}: {err}"))?;
    match format {
        Format::Zip => {
            let mut zip = zip::ZipArchive::new(file).map_err(|err| unreadable(&err))?;
            if zip.len() > MAX_ENTRIES {
                return Err(format!(
                    "{link} has more than {MAX_ENTRIES} files, more than any model needs."
                ));
            }
            // Progress against the sizes the archive declares; the budget
            // counts what is actually written.
            let mut declared = 0_u64;
            for index in 0..zip.len() {
                if let Ok(entry) = zip.by_index_raw(index) {
                    declared = declared.saturating_add(entry.size());
                }
            }
            progress(0, Some(declared));
            let mut last = Instant::now();
            for index in 0..zip.len() {
                check_cancel(cancel)?;
                let mut entry = zip.by_index(index).map_err(|err| unreadable(&err))?;
                budget.entry()?;
                let name = entry.name().to_string();
                if entry.is_symlink() {
                    return Err(link_entry(link, &name));
                }
                let relative = entry
                    .enclosed_name()
                    .and_then(|path| enclosed(&path))
                    .ok_or_else(|| unsafe_entry(link, &name))?;
                let path = into.join(&relative);
                if entry.is_dir() {
                    fs::create_dir_all(&path)
                        .map_err(|err| format!("Could not unpack {link}: {err}"))?;
                } else if !relative.as_os_str().is_empty() {
                    budget.write(&mut entry, &path, cancel)?;
                }
                if last.elapsed() >= PROGRESS_INTERVAL {
                    progress(budget.bytes, Some(declared.max(budget.bytes)));
                    last = Instant::now();
                }
            }
            progress(budget.bytes, Some(budget.bytes));
        }
        Format::Tar | Format::TarGz => {
            let total = file.metadata().ok().map(|meta| meta.len());
            let read = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
            let counting = CountingReader {
                inner: file,
                read: std::sync::Arc::clone(&read),
            };
            let reader: Box<dyn Read> = if format == Format::TarGz {
                Box::new(flate2::read::GzDecoder::new(counting))
            } else {
                Box::new(counting)
            };
            let mut tar = tar::Archive::new(reader);
            progress(0, total);
            let mut last = Instant::now();
            for entry in tar.entries().map_err(|err| unreadable(&err))? {
                check_cancel(cancel)?;
                let mut entry = entry.map_err(|err| unreadable(&err))?;
                let kind = entry.header().entry_type();
                if kind.is_pax_global_extensions()
                    || kind.is_pax_local_extensions()
                    || kind.is_gnu_longname()
                    || kind.is_gnu_longlink()
                {
                    continue;
                }
                budget.entry()?;
                let raw = entry.path().map_err(|err| unreadable(&err))?.into_owned();
                let name = raw.display().to_string();
                if kind.is_symlink() || kind.is_hard_link() {
                    return Err(link_entry(link, &name));
                }
                let relative = enclosed(&raw).ok_or_else(|| unsafe_entry(link, &name))?;
                let path = into.join(&relative);
                if kind.is_dir() {
                    fs::create_dir_all(&path)
                        .map_err(|err| format!("Could not unpack {link}: {err}"))?;
                } else if (kind.is_file() || kind.is_contiguous())
                    && !relative.as_os_str().is_empty()
                {
                    budget.write(&mut entry, &path, cancel)?;
                }
                // Devices, FIFOs and other special entries are skipped.
                if last.elapsed() >= PROGRESS_INTERVAL {
                    progress(read.load(Ordering::Relaxed), total);
                    last = Instant::now();
                }
            }
            progress(total.unwrap_or(0), total);
        }
        Format::Other => return Err(format!("{link} isn't a zip or tar archive.")),
    }
    Ok(())
}

struct CountingReader<R> {
    inner: R,
    read: std::sync::Arc<std::sync::atomic::AtomicU64>,
}

impl<R: Read> Read for CountingReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let read = self.inner.read(buf)?;
        self.read.fetch_add(read as u64, Ordering::Relaxed);
        Ok(read)
    }
}

/// The shallowest folder under `root` (itself included) holding every one
/// of `files`; among folders at the same depth, the first by name. macOS
/// zip metadata (`__MACOSX`) is skipped.
fn find_dir(root: &Path, files: &[&str]) -> Option<PathBuf> {
    let mut queue = VecDeque::from([root.to_path_buf()]);
    while let Some(dir) = queue.pop_front() {
        let has_all = files.iter().all(|name| {
            fs::symlink_metadata(dir.join(name)).is_ok_and(|meta| meta.file_type().is_file())
        });
        if has_all {
            return Some(dir);
        }
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        let mut children: Vec<PathBuf> = entries
            .flatten()
            .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
            .filter(|entry| entry.file_name() != "__MACOSX")
            .map(|entry| entry.path())
            .collect();
        children.sort();
        queue.extend(children);
    }
    None
}

/// Moves a file, copying when a rename can't (another disk).
fn move_file(from: &Path, to: &Path) -> io::Result<()> {
    if to.is_file() {
        fs::remove_file(to)?;
    }
    if fs::rename(from, to).is_ok() {
        return Ok(());
    }
    let partial = to.with_extension("partial");
    let copied = fs::copy(from, &partial).and_then(|_| fs::rename(&partial, to));
    if copied.is_err() {
        let _ = fs::remove_file(&partial);
    }
    copied?;
    let _ = fs::remove_file(from);
    Ok(())
}

/// `value` up to its query string or fragment, for messages about a link
/// that didn't parse.
fn without_query(value: &str) -> &str {
    value.split(['?', '#']).next().unwrap_or(value)
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
    let home = home_dir()
        .ok_or_else(|| format!("{value} starts with ~ but the home folder is unknown."))?;
    Ok(if rest.is_empty() { home } else { home.join(rest) })
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

/// A client for link downloads: a bounded connect, and a generous wait
/// between reads for slow servers.
pub fn client() -> Result<Client, String> {
    Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(120))
        .build()
        .map_err(|err| format!("Failed to start the download: {err}"))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;

    const FILES: [&str; 3] = ["encoder.onnx", "decoder.onnx", "vocab.txt"];

    /// An absolute path on this platform.
    fn absolute(name: &str) -> PathBuf {
        std::env::temp_dir().join(name)
    }

    fn url(link: &ModelLink) -> &reqwest::Url {
        match link {
            ModelLink::Url(url) => url,
            ModelLink::Path(path) => panic!("not a URL: {}", path.display()),
        }
    }

    pub(crate) fn zip_bytes(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut zip = zip::ZipWriter::new(io::Cursor::new(Vec::new()));
        for (name, body) in entries {
            if name.ends_with('/') {
                zip.add_directory(*name, zip::write::SimpleFileOptions::default())
                    .unwrap();
            } else {
                zip.start_file(*name, zip::write::SimpleFileOptions::default())
                    .unwrap();
                zip.write_all(body).unwrap();
            }
        }
        zip.finish().unwrap().into_inner()
    }

    fn tar_gz_bytes(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let encoder =
            flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        let mut tar = tar::Builder::new(encoder);
        for (name, body) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(body.len() as u64);
            header.set_mode(0o644);
            header.set_entry_type(tar::EntryType::Regular);
            tar.append_data(&mut header, name, *body).unwrap();
        }
        tar.into_inner().unwrap().finish().unwrap()
    }

    /// Serves `body` once per request for `requests` requests, recording
    /// each request's path and query.
    pub(crate) fn serve(
        bodies: Vec<(u16, Vec<u8>)>,
    ) -> (String, std::thread::JoinHandle<Vec<String>>) {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let base = format!("http://{}", server.server_addr());
        let worker = std::thread::spawn(move || {
            let mut urls = Vec::new();
            for (status, body) in bodies {
                let request = server.recv().unwrap();
                urls.push(request.url().to_string());
                request
                    .respond(tiny_http::Response::from_data(body).with_status_code(status))
                    .unwrap();
            }
            urls
        });
        (base, worker)
    }

    fn install(
        link: &ModelLink,
        files: &[&str],
        dest: &Path,
        work: &Path,
    ) -> Result<Vec<Stage>, String> {
        let mut stages = Vec::new();
        Install {
            link,
            model: "Test Model",
            files,
            dest,
            work_dir: work,
        }
        .run(
            &client().unwrap(),
            &AtomicBool::new(false),
            |_, _| Ok(true),
            |stage, _, _| {
                if stages.last() != Some(&stage) {
                    stages.push(stage)
                }
            },
        )?;
        Ok(stages)
    }

    fn leftovers(work: &Path) -> Vec<String> {
        fs::read_dir(work)
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with(".link-download-"))
            .collect()
    }

    #[test]
    fn urls_keep_their_query_drop_the_fragment_and_never_show_the_query() {
        let link = ModelLink::parse(
            "  https://artifactory.example.com/artifactory/models/parakeet.zip?X-Token=s3cret&a=b#section  ",
        )
        .unwrap();
        assert_eq!(
            url(&link).as_str(),
            "https://artifactory.example.com/artifactory/models/parakeet.zip?X-Token=s3cret&a=b"
        );
        assert_eq!(
            link.to_string(),
            "https://artifactory.example.com/artifactory/models/parakeet.zip"
        );
        assert_eq!(
            ModelLink::normalize(" https://example.com/m.zip?sig=1#x ").unwrap(),
            "https://example.com/m.zip?sig=1"
        );
        let with_port = ModelLink::parse("HTTP://Files.Example:8081/models/v3").unwrap();
        assert_eq!(with_port.to_string(), "http://files.example:8081/models/v3");
        // A query on a link that doesn't parse stays out of the message too.
        let err = ModelLink::parse("https://exa mple.com/m.zip?token=s3cret").unwrap_err();
        assert!(!err.contains("s3cret"), "{err}");
    }

    #[test]
    fn rejects_credentials_other_schemes_relative_paths_and_long_links() {
        for (raw, needle) in [
            ("https://user:secret@example.com/m.zip", "user name or password"),
            ("https://token@example.com/m.zip", "user name or password"),
            ("ftp://example.com/m.zip", "not ftp:"),
            ("smb://server/share/m.zip", "not smb:"),
            ("models/m.zip", "isn't a full path"),
            ("./m.zip", "isn't a full path"),
            ("~someone/m.zip", "isn't a full path"),
            ("https://", "isn't a valid link"),
            ("", "Enter a link"),
            ("   ", "Enter a link"),
        ] {
            let err = ModelLink::parse(raw).expect_err(raw);
            assert!(err.contains(needle), "{raw}: {err}");
            assert!(!err.contains("secret"), "{raw}: {err}");
        }
        let long = format!("https://example.com/{}", "a".repeat(MAX_LEN));
        assert!(ModelLink::parse(&long).unwrap_err().contains("too long"));
        let limit = format!("https://e.example/{}", "a".repeat(MAX_LEN - 18));
        assert_eq!(limit.chars().count(), MAX_LEN);
        assert!(ModelLink::parse(&limit).is_ok());
    }

    #[test]
    fn paths_and_file_urls_name_a_file() {
        let path = absolute("parakeet.zip");
        assert_eq!(
            ModelLink::parse(&format!(" {} ", path.display())).unwrap(),
            ModelLink::Path(path.clone())
        );
        let file_url = reqwest::Url::from_file_path(&path).unwrap();
        assert_eq!(
            ModelLink::parse(file_url.as_str()).unwrap(),
            ModelLink::Path(path.clone())
        );
        assert_eq!(ModelLink::Path(path.clone()).to_string(), path.display().to_string());
        if let Some(home) = home_dir() {
            assert_eq!(
                ModelLink::parse("~/models/m.zip").unwrap(),
                ModelLink::Path(home.join("models/m.zip"))
            );
        }
        #[cfg(windows)]
        {
            assert_eq!(
                ModelLink::parse(r"C:\models\m.zip").unwrap(),
                ModelLink::Path(PathBuf::from(r"C:\models\m.zip"))
            );
            assert_eq!(
                ModelLink::parse(r"\\fileserver\share\m.zip").unwrap(),
                ModelLink::Path(PathBuf::from(r"\\fileserver\share\m.zip"))
            );
            assert_eq!(
                ModelLink::parse("file://fileserver/share/m.zip").unwrap(),
                ModelLink::Path(PathBuf::from(r"\\fileserver\share\m.zip"))
            );
        }
        #[cfg(unix)]
        {
            assert_eq!(
                ModelLink::parse("file:///Volumes/Models/AI%20models/m.zip").unwrap(),
                ModelLink::Path(PathBuf::from("/Volumes/Models/AI models/m.zip"))
            );
            assert!(ModelLink::parse("file://fileserver/share/m.zip").is_err());
        }
    }

    #[test]
    fn detects_archives_by_their_first_bytes_not_their_name() {
        let dir = tempfile::tempdir().unwrap();
        let write = |name: &str, body: &[u8]| {
            let path = dir.path().join(name);
            fs::write(&path, body).unwrap();
            path
        };
        let entries: &[(&str, &[u8])] = &[("a.txt", b"a")];
        assert_eq!(detect(&write("download", &zip_bytes(entries))).unwrap(), Format::Zip);
        assert_eq!(detect(&write("model.bin", &tar_gz_bytes(entries))).unwrap(), Format::TarGz);
        let mut tar = tar::Builder::new(Vec::new());
        let mut header = tar::Header::new_ustar();
        header.set_size(1);
        tar.append_data(&mut header, "a.txt", &b"a"[..]).unwrap();
        assert_eq!(detect(&write("x.zip", &tar.into_inner().unwrap())).unwrap(), Format::Tar);
        assert_eq!(detect(&write("x.tar.gz", b"GGUF\x03\0\0\0weights")).unwrap(), Format::Other);
        // Gzip that isn't a tar is not an archive this installs.
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        gz.write_all(b"plain text, not a tar").unwrap();
        assert_eq!(detect(&write("y", &gz.finish().unwrap())).unwrap(), Format::Other);
        assert_eq!(detect(&write("empty", b"")).unwrap(), Format::Other);
    }

    #[test]
    fn installs_from_a_zip_found_at_any_depth_over_http_with_the_query() {
        let zip = zip_bytes(&[
            ("bundle/", b""),
            ("bundle/README.txt", b"read me"),
            ("bundle/parakeet/encoder.onnx", b"enc"),
            ("bundle/parakeet/decoder.onnx", b"dec"),
            ("bundle/parakeet/vocab.txt", b"voc"),
            ("bundle/parakeet/extra.bin", b"not needed"),
            ("__MACOSX/bundle/parakeet/._vocab.txt", b"junk"),
        ]);
        let (base, server) = serve(vec![(200, zip)]);
        let link = ModelLink::parse(&format!("{base}/models/parakeet?token=abc")).unwrap();
        let models = tempfile::tempdir().unwrap();
        let dest = models.path().join("parakeet");
        let stages = install(&link, &FILES, &dest, models.path()).unwrap();
        assert_eq!(stages, [Stage::Downloading, Stage::Unpacking, Stage::Checking]);
        assert_eq!(server.join().unwrap(), ["/models/parakeet?token=abc"]);
        for (name, body) in [("encoder.onnx", "enc"), ("decoder.onnx", "dec"), ("vocab.txt", "voc")] {
            assert_eq!(fs::read_to_string(dest.join(name)).unwrap(), body);
        }
        // Exactly the needed files, and nothing left behind.
        assert_eq!(fs::read_dir(&dest).unwrap().count(), 3);
        assert!(leftovers(models.path()).is_empty());
    }

    #[test]
    fn installs_from_a_tar_gz_at_the_root_of_a_file_on_disk() {
        let shared = tempfile::tempdir().unwrap();
        let archive = shared.path().join("parakeet");
        fs::write(
            &archive,
            tar_gz_bytes(&[("encoder.onnx", b"enc"), ("decoder.onnx", b"dec"), ("vocab.txt", b"voc")]),
        )
        .unwrap();
        let link = ModelLink::parse(&archive.display().to_string()).unwrap();
        let models = tempfile::tempdir().unwrap();
        let dest = models.path().join("parakeet");
        install(&link, &FILES, &dest, models.path()).unwrap();
        assert_eq!(fs::read_to_string(dest.join("vocab.txt")).unwrap(), "voc");
        // The source file stays where it is.
        assert!(archive.is_file());
        assert!(leftovers(models.path()).is_empty());
    }

    #[test]
    fn a_download_without_the_files_names_the_link_and_what_it_needs() {
        let zip = zip_bytes(&[("encoder.onnx", b"enc"), ("other/vocab.txt", b"voc")]);
        let (base, server) = serve(vec![(200, zip)]);
        let link = ModelLink::parse(&format!("{base}/m.zip?sig=s3cret")).unwrap();
        let models = tempfile::tempdir().unwrap();
        let dest = models.path().join("model");
        let err = install(&link, &FILES, &dest, models.path()).unwrap_err();
        server.join().unwrap();
        assert_eq!(
            err,
            format!("The download from {base}/m.zip doesn't contain the Test Model files. It needs encoder.onnx, decoder.onnx and vocab.txt.")
        );
        assert!(!dest.exists());
        assert!(leftovers(models.path()).is_empty());
        assert_eq!(
            missing_files(&link, "Whisper base", &["ggml-base.bin"]),
            format!("The download from {base}/m.zip doesn't contain the Whisper base file. It needs ggml-base.bin.")
        );
    }

    #[test]
    fn http_errors_and_non_archives_name_the_link_without_its_query() {
        let (base, server) = serve(vec![
            (403, b"forbidden".to_vec()),
            (200, b"<!DOCTYPE html><html>Sign in</html>".to_vec()),
        ]);
        let link = ModelLink::parse(&format!("{base}/m.zip?token=s3cret")).unwrap();
        let models = tempfile::tempdir().unwrap();
        let dest = models.path().join("model");
        let err = install(&link, &FILES, &dest, models.path()).unwrap_err();
        assert_eq!(err, format!("Could not download {base}/m.zip: HTTP 403"));
        let err = install(&link, &FILES, &dest, models.path()).unwrap_err();
        assert!(err.starts_with(&format!("{base}/m.zip isn't a zip or tar archive. It's a web page")), "{err}");
        assert!(!err.contains("s3cret"));
        server.join().unwrap();
        assert!(leftovers(models.path()).is_empty());

        // Nothing listening: the cause, without the query.
        let link = ModelLink::parse("http://127.0.0.1:9/m.zip?token=s3cret").unwrap();
        let err = install(&link, &FILES, &dest, models.path()).unwrap_err();
        assert!(err.starts_with("Could not download http://127.0.0.1:9/m.zip: "), "{err}");
        assert!(!err.contains("s3cret"), "{err}");

        let missing = ModelLink::Path(models.path().join("missing.zip"));
        let err = install(&missing, &FILES, &dest, models.path()).unwrap_err();
        assert!(err.starts_with(&format!("Could not read {}: ", models.path().join("missing.zip").display())), "{err}");
    }

    #[test]
    fn a_one_file_model_may_be_the_bare_file() {
        let (base, server) = serve(vec![(200, b"GGUF weights".to_vec()), (200, b"<html>login</html>".to_vec())]);
        let link = ModelLink::parse(&format!("{base}/download?id=7")).unwrap();
        let models = tempfile::tempdir().unwrap();
        let mut checked = Vec::new();
        let run = |checked: &mut Vec<String>| {
            Install {
                link: &link,
                model: "Qwen3.5 2B",
                files: &["model.gguf"],
                dest: models.path(),
                work_dir: models.path(),
            }
            .run(
                &client().unwrap(),
                &AtomicBool::new(false),
                |name, path| {
                    checked.push(name.to_string());
                    Ok(fs::read(path).unwrap() == b"GGUF weights")
                },
                |_, _, _| {},
            )
        };
        run(&mut checked).unwrap();
        assert_eq!(fs::read(models.path().join("model.gguf")).unwrap(), b"GGUF weights");
        let err = run(&mut checked).unwrap_err();
        assert_eq!(
            err,
            format!("The download from {base}/download isn't the Qwen3.5 2B file: it failed its size or checksum check. It's a web page: the link may need a sign-in, or point to a page about the file rather than the file itself.")
        );
        assert_eq!(checked, ["model.gguf", "model.gguf"]);
        // The installed file is untouched by the failed attempt.
        assert_eq!(fs::read(models.path().join("model.gguf")).unwrap(), b"GGUF weights");
        server.join().unwrap();
        assert!(leftovers(models.path()).is_empty());
    }

    #[test]
    fn a_failed_check_names_the_file_and_installs_nothing() {
        let shared = tempfile::tempdir().unwrap();
        let archive = shared.path().join("m.zip");
        fs::write(&archive, zip_bytes(&[("encoder.onnx", b"enc"), ("decoder.onnx", b"dec"), ("vocab.txt", b"voc")])).unwrap();
        let link = ModelLink::Path(archive.clone());
        let models = tempfile::tempdir().unwrap();
        let dest = models.path().join("model");
        let err = Install { link: &link, model: "Parakeet Ultra", files: &FILES, dest: &dest, work_dir: models.path() }
            .run(&client().unwrap(), &AtomicBool::new(false), |name, _| Ok(name != "decoder.onnx"), |_, _, _| {})
            .unwrap_err();
        assert_eq!(
            err,
            format!("decoder.onnx in the download from {} isn't the file Parakeet Ultra needs: it failed its size or checksum check.", archive.display())
        );
        assert!(!dest.exists());
        assert!(leftovers(models.path()).is_empty());
    }

    #[test]
    fn rejects_entries_that_escape_links_and_oversized_archives() {
        let models = tempfile::tempdir().unwrap();
        let shared = tempfile::tempdir().unwrap();
        let dest = models.path().join("model");
        let archive = shared.path().join("bad");

        // Zip slip.
        fs::write(&archive, zip_bytes(&[("../escaped.txt", b"bad"), ("encoder.onnx", b"x")])).unwrap();
        let link = ModelLink::Path(archive.clone());
        let err = install(&link, &FILES, &dest, models.path()).unwrap_err();
        assert!(err.contains("../escaped.txt, which would unpack outside its folder"), "{err}");
        assert!(!models.path().join("escaped.txt").exists());
        assert!(!shared.path().join("escaped.txt").exists());

        // Tar slip, written by hand: the builder refuses `..` itself.
        let mut header = tar::Header::new_gnu();
        let body = b"bad";
        header.as_gnu_mut().unwrap().name[..17].copy_from_slice(b"../../escaped.txt");
        header.set_size(body.len() as u64);
        header.set_entry_type(tar::EntryType::Regular);
        header.set_cksum();
        let mut tar = tar::Builder::new(Vec::new());
        tar.append(&header, &body[..]).unwrap();
        fs::write(&archive, tar.into_inner().unwrap()).unwrap();
        let err = install(&link, &FILES, &dest, models.path()).unwrap_err();
        assert!(err.contains("would unpack outside its folder"), "{err}");

        // A symlink in a tar.
        let mut tar = tar::Builder::new(Vec::new());
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Symlink);
        header.set_size(0);
        tar.append_link(&mut header, "vocab.txt", "/etc/passwd").unwrap();
        fs::write(&archive, tar.into_inner().unwrap()).unwrap();
        let err = install(&link, &FILES, &dest, models.path()).unwrap_err();
        assert!(err.contains("contains a link (vocab.txt)"), "{err}");

        // A symlink in a zip.
        let mut zip = zip::ZipWriter::new(io::Cursor::new(Vec::new()));
        zip.add_symlink("vocab.txt", "/etc/passwd", zip::write::SimpleFileOptions::default()).unwrap();
        fs::write(&archive, zip.finish().unwrap().into_inner()).unwrap();
        let err = install(&link, &FILES, &dest, models.path()).unwrap_err();
        assert!(err.contains("contains a link (vocab.txt)"), "{err}");

        // Too many entries.
        let names: Vec<String> = (0..=MAX_ENTRIES).map(|i| format!("d/{i}")).collect();
        let entries: Vec<(&str, &[u8])> = names.iter().map(|name| (name.as_str(), &b""[..])).collect();
        fs::write(&archive, zip_bytes(&entries)).unwrap();
        let err = install(&link, &FILES, &dest, models.path()).unwrap_err();
        assert!(err.contains("more than 10000 files"), "{err}");

        assert!(!dest.exists());
        assert!(leftovers(models.path()).is_empty());
    }

    #[test]
    fn the_unpacked_size_is_capped() {
        let link = ModelLink::Path(PathBuf::from("bomb.zip"));
        let mut budget = Budget { link: &link, entries: 0, bytes: MAX_BYTES - 2 };
        let dir = tempfile::tempdir().unwrap();
        let err = budget
            .write(&mut &b"four"[..], &dir.path().join("f"), &AtomicBool::new(false))
            .unwrap_err();
        assert!(err.contains("unpacks to more than 8 GB"), "{err}");
    }

    #[test]
    fn enclosed_paths_stay_inside() {
        assert_eq!(enclosed(Path::new("a/./b/../c")), Some(PathBuf::from("a/c")));
        assert_eq!(enclosed(Path::new("./")), Some(PathBuf::new()));
        assert_eq!(enclosed(Path::new("a/../../b")), None);
        assert_eq!(enclosed(Path::new("/etc/passwd")), None);
        #[cfg(windows)]
        {
            assert_eq!(enclosed(Path::new(r"C:\Windows\x")), None);
            assert_eq!(enclosed(Path::new(r"..\x")), None);
        }
    }

    #[test]
    fn cancelling_stops_the_download() {
        let shared = tempfile::tempdir().unwrap();
        let archive = shared.path().join("m.zip");
        fs::write(&archive, zip_bytes(&[("encoder.onnx", b"enc")])).unwrap();
        let models = tempfile::tempdir().unwrap();
        let err = Install {
            link: &ModelLink::Path(archive),
            model: "M",
            files: &FILES,
            dest: &models.path().join("m"),
            work_dir: models.path(),
        }
        .run(&client().unwrap(), &AtomicBool::new(true), |_, _| Ok(true), |_, _, _| {})
        .unwrap_err();
        assert_eq!(err, "Cancelled");
        assert!(leftovers(models.path()).is_empty());
    }
}
