//! LoRA adapters for the local polish model: which ones may load, how
//! llama-server is started with them, and how a request selects them.
//!
//! A vocabulary pack may declare a `polish_adapter`: a GGUF LoRA trained
//! (training/polish) for one exact base model. An adapter for another base
//! of the same architecture and size loads without complaint and quietly
//! changes the output, so nothing loads unless its manifest names the base
//! model being served by SHA-256 and the adapter still matches its own
//! recorded hash. Any doubt, and polish runs without that adapter.
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::ffi::OsString;
use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

/// A pack's request for an adapter: a local `.gguf` path or a catalog id.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdapterRequest {
    pub pack: String,
    pub source: String,
}

/// An adapter that passed validation, in the order it is passed to
/// llama-server (its position is its id there).
#[derive(Clone, Debug, PartialEq)]
pub struct ValidAdapter {
    pub pack: String,
    pub path: PathBuf,
    pub scale: f32,
}

/// The base model being served, as an adapter manifest must name it.
pub struct BaseModel<'a> {
    pub path: &'a Path,
    pub sha256: &'a str,
}

/// Published adapters, pinned like `MODELS`. Empty until one ships: a
/// catalog id resolves to `<polish dir>/<file>` and loads only on `base`.
pub struct AdapterSpec {
    pub id: &'static str,
    pub base: &'static str,
    pub file: &'static str,
    pub sha256: &'static str,
}
pub const ADAPTERS: &[AdapterSpec] = &[];

/// `<adapter>.json`, written by training/polish/write_adapter_manifest.py.
#[derive(Debug, Deserialize)]
struct Manifest {
    format: String,
    version: u32,
    #[serde(default)]
    pack: String,
    base_model_sha256: String,
    adapter_sha256: String,
    #[serde(default = "default_scale")]
    scale: f32,
}
fn default_scale() -> f32 {
    1.0
}

pub fn sha256_file(path: &Path) -> Result<String, String> {
    let mut file = File::open(path).map_err(|e| e.to_string())?;
    let mut hash = Sha256::new();
    let mut buffer = vec![0u8; 1 << 20];
    loop {
        let n = file.read(&mut buffer).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
    }
    Ok(hash.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// Validates one request against the base model. `catalog_base` is the
/// catalog id of the base model when it is a catalog model.
pub fn validate(
    request: &AdapterRequest,
    base: &BaseModel,
    catalog_base: Option<&str>,
    root: &Path,
) -> Result<ValidAdapter, String> {
    let source = request.source.trim();
    if let Some(spec) = ADAPTERS.iter().find(|a| a.id == source) {
        if catalog_base != Some(spec.base) {
            return Err(format!("adapter {} is for {}, not this model", spec.id, spec.base));
        }
        let path = root.join(spec.file);
        if sha256_file(&path)? != spec.sha256 {
            return Err(format!("adapter {} is missing or damaged", spec.id));
        }
        return Ok(ValidAdapter { pack: request.pack.clone(), path, scale: 1.0 });
    }
    let path = PathBuf::from(source);
    if !path.is_absolute() || path.extension().and_then(|e| e.to_str()) != Some("gguf") {
        return Err(format!("\"{source}\" is neither a catalog adapter nor an absolute .gguf path"));
    }
    let manifest: Manifest = serde_json::from_str(
        &std::fs::read_to_string(path.with_extension("json"))
            .map_err(|_| "adapter has no manifest next to it".to_string())?,
    )
    .map_err(|e| format!("adapter manifest is invalid: {e}"))?;
    if manifest.format != "fairspoken-polish-adapter" || manifest.version != 1 {
        return Err("adapter manifest is not a Fairspoken polish adapter (version 1)".into());
    }
    if !manifest.base_model_sha256.eq_ignore_ascii_case(base.sha256) {
        return Err("adapter was trained for a different base model".into());
    }
    if !(0.0..=2.0).contains(&manifest.scale) {
        return Err("adapter scale is out of range".into());
    }
    let meta = gguf_strings(&path, &["general.type", "adapter.type", "general.architecture"])?;
    if meta.get("general.type").map(String::as_str) != Some("adapter")
        || meta.get("adapter.type").map(String::as_str) != Some("lora")
    {
        return Err("file is not a LoRA adapter".into());
    }
    let base_arch = gguf_strings(base.path, &["general.architecture"])?;
    if meta.get("general.architecture") != base_arch.get("general.architecture") {
        return Err("adapter architecture does not match the base model".into());
    }
    if !sha256_file(&path)?.eq_ignore_ascii_case(&manifest.adapter_sha256) {
        return Err("adapter file does not match its manifest".into());
    }
    Ok(ValidAdapter {
        pack: if manifest.pack.is_empty() { request.pack.clone() } else { manifest.pack },
        path,
        scale: manifest.scale,
    })
}

/// llama-server flags for the adapters: each loaded, none applied until a
/// request asks (`--lora-init-without-apply`, present in the pinned b10930
/// runtime), so a request without a `lora` field gets the bare base model.
pub fn server_args(adapters: &[ValidAdapter]) -> Vec<OsString> {
    let mut args = Vec::new();
    for adapter in adapters {
        args.push(OsString::from("--lora"));
        args.push(adapter.path.clone().into_os_string());
    }
    if !adapters.is_empty() {
        args.push(OsString::from("--lora-init-without-apply"));
    }
    args
}

/// The per-request `lora` field (`[{id, scale}]`, b10930's server API) for
/// the adapters of the packs that apply to this dictation. Adapters not
/// listed run at scale 0. None when the server has no adapters loaded.
pub fn request_field(loaded: &[ValidAdapter], packs: &[String]) -> Option<serde_json::Value> {
    if loaded.is_empty() {
        return None;
    }
    Some(serde_json::Value::Array(
        loaded
            .iter()
            .enumerate()
            .filter(|(_, a)| packs.contains(&a.pack))
            .map(|(id, a)| serde_json::json!({"id": id, "scale": a.scale}))
            .collect(),
    ))
}

/// Whether `GET /lora-adapters` lists exactly our adapters, in order.
pub fn listing_matches(listing: &serde_json::Value, loaded: &[ValidAdapter]) -> bool {
    let Some(items) = listing.as_array() else {
        return false;
    };
    items.len() == loaded.len()
        && items.iter().zip(loaded).enumerate().all(|(i, (item, adapter))| {
            item["id"].as_u64() == Some(i as u64)
                && item["path"]
                    .as_str()
                    .map(|p| Path::new(p) == adapter.path)
                    .unwrap_or(false)
        })
}

const GGUF_MAX_STRING: u64 = 1 << 20;

/// Reads the string values of `keys` from a GGUF file's metadata (format v2
/// and v3), stopping as soon as all are found. Never loads tensor data, and
/// skips the large tokenizer arrays without reading them into memory.
pub fn gguf_strings(path: &Path, keys: &[&str]) -> Result<HashMap<String, String>, String> {
    let file = File::open(path).map_err(|e| format!("cannot open {}: {e}", path.display()))?;
    let mut r = BufReader::new(file);
    let bad = || "not a GGUF file".to_string();
    let mut magic = [0u8; 4];
    r.read_exact(&mut magic).map_err(|_| bad())?;
    if &magic != b"GGUF" {
        return Err(bad());
    }
    let version = read_u32(&mut r)?;
    if !(2..=3).contains(&version) {
        return Err(format!("unsupported GGUF version {version}"));
    }
    let _tensors = read_u64(&mut r)?;
    let kv = read_u64(&mut r)?;
    let mut found = HashMap::new();
    for _ in 0..kv {
        if found.len() == keys.len() {
            break;
        }
        let key = read_string(&mut r)?;
        let kind = read_u32(&mut r)?;
        if kind == 8 && keys.contains(&key.as_str()) {
            found.insert(key, read_string(&mut r)?);
        } else {
            skip_value(&mut r, kind)?;
        }
    }
    Ok(found)
}

fn read_u32(r: &mut impl Read) -> Result<u32, String> {
    let mut b = [0u8; 4];
    r.read_exact(&mut b).map_err(|_| "truncated GGUF header".to_string())?;
    Ok(u32::from_le_bytes(b))
}
fn read_u64(r: &mut impl Read) -> Result<u64, String> {
    let mut b = [0u8; 8];
    r.read_exact(&mut b).map_err(|_| "truncated GGUF header".to_string())?;
    Ok(u64::from_le_bytes(b))
}
fn read_string(r: &mut impl Read) -> Result<String, String> {
    let len = read_u64(r)?;
    if len > GGUF_MAX_STRING {
        return Err("GGUF string too long".into());
    }
    let mut b = vec![0u8; len as usize];
    r.read_exact(&mut b).map_err(|_| "truncated GGUF header".to_string())?;
    String::from_utf8(b).map_err(|_| "GGUF string is not UTF-8".to_string())
}
fn scalar_size(kind: u32) -> Option<i64> {
    match kind {
        0 | 1 | 7 => Some(1),
        2 | 3 => Some(2),
        4..=6 => Some(4),
        10..=12 => Some(8),
        _ => None,
    }
}
fn skip_value<R: Read + Seek>(r: &mut R, kind: u32) -> Result<(), String> {
    let skip = |r: &mut R, n: i64| r.seek(SeekFrom::Current(n)).map(|_| ()).map_err(|e| e.to_string());
    match kind {
        8 => {
            let len = read_u64(r)?;
            skip(r, len as i64)
        }
        9 => {
            let inner = read_u32(r)?;
            let count = read_u64(r)?;
            if let Some(size) = scalar_size(inner) {
                return skip(r, size * count as i64);
            }
            for _ in 0..count {
                skip_value(r, inner)?;
            }
            Ok(())
        }
        other => match scalar_size(other) {
            Some(size) => skip(r, size),
            None => Err(format!("unknown GGUF value type {other}")),
        },
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::io::Write;

    /// A metadata-only GGUF (v3) with string KVs plus a large string array,
    /// enough for the header reader.
    pub fn write_gguf(path: &Path, kvs: &[(&str, &str)]) {
        let mut f = File::create(path).unwrap();
        let s = |f: &mut File, v: &str| {
            f.write_all(&(v.len() as u64).to_le_bytes()).unwrap();
            f.write_all(v.as_bytes()).unwrap();
        };
        f.write_all(b"GGUF").unwrap();
        f.write_all(&3u32.to_le_bytes()).unwrap();
        f.write_all(&0u64.to_le_bytes()).unwrap();
        f.write_all(&((kvs.len() + 2) as u64).to_le_bytes()).unwrap();
        // Values the reader must skip: a u32 and an array of strings.
        s(&mut f, "general.alignment");
        f.write_all(&4u32.to_le_bytes()).unwrap();
        f.write_all(&32u32.to_le_bytes()).unwrap();
        s(&mut f, "tokenizer.ggml.tokens");
        f.write_all(&9u32.to_le_bytes()).unwrap();
        f.write_all(&8u32.to_le_bytes()).unwrap();
        f.write_all(&3u64.to_le_bytes()).unwrap();
        for t in ["a", "bb", "ccc"] {
            s(&mut f, t);
        }
        for (k, v) in kvs {
            s(&mut f, k);
            f.write_all(&8u32.to_le_bytes()).unwrap();
            s(&mut f, v);
        }
    }

    pub struct Fixture {
        pub dir: tempfile::TempDir,
        pub base: PathBuf,
        pub base_sha: String,
        pub adapter: PathBuf,
    }
    pub fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join("model.gguf");
        write_gguf(&base, &[("general.architecture", "qwen35"), ("general.type", "model")]);
        let adapter = dir.path().join("ie-general-practice.lora.gguf");
        write_gguf(
            &adapter,
            &[("general.architecture", "qwen35"), ("general.type", "adapter"), ("adapter.type", "lora")],
        );
        let base_sha = sha256_file(&base).unwrap();
        write_manifest(&adapter, &base_sha, None);
        Fixture { dir, base, base_sha, adapter }
    }
    pub fn write_manifest(adapter: &Path, base_sha: &str, adapter_sha: Option<&str>) {
        let sha = adapter_sha.map(str::to_string).unwrap_or_else(|| sha256_file(adapter).unwrap());
        std::fs::write(
            adapter.with_extension("json"),
            serde_json::json!({"format":"fairspoken-polish-adapter","version":1,"pack":"ie-general-practice",
                "base_model_sha256":base_sha,"adapter_sha256":sha,"scale":1.0})
            .to_string(),
        )
        .unwrap();
    }

    fn request(source: &Path) -> AdapterRequest {
        AdapterRequest { pack: "ie-general-practice".into(), source: source.display().to_string() }
    }

    #[test]
    fn reads_gguf_strings_and_skips_arrays() {
        let f = fixture();
        let meta = gguf_strings(&f.adapter, &["general.type", "adapter.type", "general.architecture"]).unwrap();
        assert_eq!(meta["general.type"], "adapter");
        assert_eq!(meta["adapter.type"], "lora");
        assert_eq!(meta["general.architecture"], "qwen35");
        std::fs::write(f.dir.path().join("x.gguf"), b"nope").unwrap();
        assert!(gguf_strings(&f.dir.path().join("x.gguf"), &["general.type"]).is_err());
    }

    #[test]
    fn loads_only_adapters_trained_for_the_served_model() {
        let f = fixture();
        let base = BaseModel { path: &f.base, sha256: &f.base_sha };
        let ok = validate(&request(&f.adapter), &base, None, f.dir.path()).unwrap();
        assert_eq!(ok, ValidAdapter { pack: "ie-general-practice".into(), path: f.adapter.clone(), scale: 1.0 });

        // Another base model.
        let other = BaseModel { path: &f.base, sha256: &"0".repeat(64) };
        assert!(validate(&request(&f.adapter), &other, None, f.dir.path()).unwrap_err().contains("different base"));

        // The adapter changed after its manifest was written.
        write_manifest(&f.adapter, &f.base_sha, Some(&"1".repeat(64)));
        assert!(validate(&request(&f.adapter), &base, None, f.dir.path()).unwrap_err().contains("does not match its manifest"));

        // No manifest at all.
        std::fs::remove_file(f.adapter.with_extension("json")).unwrap();
        assert!(validate(&request(&f.adapter), &base, None, f.dir.path()).unwrap_err().contains("no manifest"));

        // A full model passed off as an adapter.
        let model = f.dir.path().join("not-an-adapter.gguf");
        write_gguf(&model, &[("general.architecture", "qwen35"), ("general.type", "model")]);
        write_manifest(&model, &f.base_sha, None);
        assert!(validate(&request(&model), &base, None, f.dir.path()).unwrap_err().contains("not a LoRA"));

        // A LoRA for another architecture.
        let llama = f.dir.path().join("llama.gguf");
        write_gguf(&llama, &[("general.architecture", "llama"), ("general.type", "adapter"), ("adapter.type", "lora")]);
        write_manifest(&llama, &f.base_sha, None);
        assert!(validate(&request(&llama), &base, None, f.dir.path()).unwrap_err().contains("architecture"));

        // Relative paths and unknown catalog ids.
        for source in ["adapters/x.gguf", "speakoflow-gp", ""] {
            let r = AdapterRequest { pack: "p".into(), source: source.into() };
            assert!(validate(&r, &base, Some("speakoflow-mini"), f.dir.path()).is_err(), "{source}");
        }
    }

    #[test]
    fn server_loads_adapters_disabled_and_requests_select_them() {
        assert!(server_args(&[]).is_empty());
        assert_eq!(request_field(&[], &["ie-general-practice".into()]), None);
        let loaded = vec![
            ValidAdapter { pack: "ie-general-practice".into(), path: "/a/gp.gguf".into(), scale: 1.0 },
            ValidAdapter { pack: "developer".into(), path: "/a/dev.gguf".into(), scale: 0.5 },
        ];
        assert_eq!(
            server_args(&loaded),
            ["--lora", "/a/gp.gguf", "--lora", "/a/dev.gguf", "--lora-init-without-apply"]
                .map(OsString::from)
                .to_vec()
        );
        assert_eq!(
            request_field(&loaded, &["developer".into()]),
            Some(serde_json::json!([{"id":1,"scale":0.5}]))
        );
        assert_eq!(
            request_field(&loaded, &["ie-general-practice".into(), "developer".into()]),
            Some(serde_json::json!([{"id":0,"scale":1.0},{"id":1,"scale":0.5}]))
        );
        // No pack applies: an explicit empty list keeps every adapter off.
        assert_eq!(request_field(&loaded, &[]), Some(serde_json::json!([])));
        let listing = serde_json::json!([{"id":0,"path":"/a/gp.gguf","scale":0.0},{"id":1,"path":"/a/dev.gguf","scale":0.0}]);
        assert!(listing_matches(&listing, &loaded));
        assert!(!listing_matches(&serde_json::json!([{"id":0,"path":"/a/gp.gguf"}]), &loaded));
        assert!(!listing_matches(&serde_json::json!({"error":"x"}), &loaded));
    }
}
