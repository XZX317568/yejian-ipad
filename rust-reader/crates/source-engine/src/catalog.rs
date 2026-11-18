//! Installed-source catalogue on disk.
//!
//! One installed source is one directory:
//!
//! ```text
//! <root>/<source-id>/
//!   source.json      (required, Aidoku metadata)
//!   main.wasm        (required, Aidoku source module)
//!   settings.json    (optional, static settings)
//!   filters.json     (optional, static filters)
//!   icon.png         (optional)
//! ```
//!
//! The layout matches an unpacked Aidoku `.aix` package and the Dart
//! `Source.loadFromDirectory` path, so an existing installed source can be
//! copied in without conversion.

use std::collections::HashMap;
use std::fmt;
use std::fs;
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;
use zip::ZipArchive;

use crate::wasm::models::{Listing, ListingKind};

const MAX_AIX_BYTES: usize = 64 * 1024 * 1024;
const MAX_ENTRY_BYTES: usize = 64 * 1024 * 1024;
const MAX_TOTAL_BYTES: usize = 128 * 1024 * 1024;
const WASM_MAGIC: &[u8] = b"\0asm";

/// Metadata needed by the UI before a module is instantiated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceManifest {
    pub id: String,
    pub name: String,
    pub version: i64,
    pub alt_names: Vec<String>,
    pub languages: Vec<String>,
    /// Base URLs declared by `source.json` (`url` plus `urls`).
    pub urls: Vec<String>,
    /// 0 = safe, 1 = contains NSFW, 2 = primarily NSFW.
    pub content_rating: i32,
    pub listings: Vec<Listing>,
    pub allows_base_url_select: bool,
    pub hides_filters_while_searching: bool,
}

impl SourceManifest {
    pub fn primary_url(&self) -> Option<&str> {
        self.urls.first().map(String::as_str)
    }
}

/// One source found in the catalogue directory.
#[derive(Debug, Clone)]
pub struct InstalledSource {
    pub manifest: SourceManifest,
    pub directory: PathBuf,
    pub wasm_path: PathBuf,
    pub settings_path: Option<PathBuf>,
    pub filters_path: Option<PathBuf>,
    pub icon_path: Option<PathBuf>,
}

impl InstalledSource {
    pub fn wasm_bytes(&self) -> Result<Vec<u8>, CatalogError> {
        fs::read(&self.wasm_path)
            .map_err(|e| CatalogError::Io(format!("reading {}: {e}", self.wasm_path.display())))
    }

    pub fn settings_json(&self) -> Option<String> {
        let path = self.settings_path.as_ref()?;
        fs::read_to_string(path).ok()
    }
}

/// Failures while scanning, installing, updating or removing local sources.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CatalogError {
    Io(String),
    InvalidArchive(String),
    InvalidMetadata(String),
    InvalidId(String),
    NotFound(String),
    TooLarge(String),
}

pub type InstallError = CatalogError;

impl fmt::Display for CatalogError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(m) => write!(f, "source storage error: {m}"),
            Self::InvalidArchive(m) => write!(f, "invalid .aix archive: {m}"),
            Self::InvalidMetadata(m) => write!(f, "invalid source metadata: {m}"),
            Self::InvalidId(m) => write!(f, "invalid source id: {m}"),
            Self::NotFound(m) => write!(f, "source \"{m}\" is not installed"),
            Self::TooLarge(m) => write!(f, "source package rejected: {m}"),
        }
    }
}

impl std::error::Error for CatalogError {}

/// Directory-backed catalogue of installed Aidoku sources.
pub struct SourceCatalog {
    root: PathBuf,
}

impl SourceCatalog {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Lists valid installed sources. Corrupt directories are skipped; use
    /// [`Self::get`] for a precise error on one id.
    pub fn list(&self) -> Result<Vec<InstalledSource>, CatalogError> {
        if !self.root.exists() {
            return Ok(Vec::new());
        }
        let entries = fs::read_dir(&self.root).map_err(|e| {
            CatalogError::Io(format!("reading catalogue {}: {e}", self.root.display()))
        })?;
        let mut installed = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if name.starts_with('.') || name.contains(".old-") || name.contains(".install-") {
                continue;
            }
            if let Ok(source) = read_installed(&path, name) {
                installed.push(source);
            }
        }
        installed.sort_by(|a, b| {
            a.manifest
                .name
                .to_lowercase()
                .cmp(&b.manifest.name.to_lowercase())
                .then_with(|| a.manifest.id.cmp(&b.manifest.id))
        });
        Ok(installed)
    }

    pub fn get(&self, id: &str) -> Result<InstalledSource, CatalogError> {
        validate_id(id)?;
        let dir = self.root.join(id);
        if !dir.is_dir() {
            return Err(CatalogError::NotFound(id.to_string()));
        }
        read_installed(&dir, id)
    }

    pub fn is_installed(&self, id: &str) -> bool {
        validate_id(id).is_ok() && self.root.join(id).join("main.wasm").is_file()
    }

    /// Installs or replaces a source from `.aix` bytes.
    ///
    /// The new version is written to a temporary directory and swapped in only
    /// after every required file has been validated, so a failed update leaves
    /// the previous installation intact.
    pub fn install_aix_bytes(&self, bytes: &[u8]) -> Result<InstalledSource, CatalogError> {
        if bytes.len() > MAX_AIX_BYTES {
            return Err(CatalogError::TooLarge(format!(
                ".aix is {} bytes (limit {})",
                bytes.len(),
                MAX_AIX_BYTES
            )));
        }
        let mut archive = ZipArchive::new(Cursor::new(bytes))
            .map_err(|e| CatalogError::InvalidArchive(e.to_string()))?;
        let mut wanted = read_aix_entries(&mut archive)?;

        let source_json = wanted
            .remove("source.json")
            .ok_or_else(|| CatalogError::InvalidArchive("source.json not found".into()))?;
        let wasm = wanted
            .remove("main.wasm")
            .ok_or_else(|| CatalogError::InvalidArchive("main.wasm not found".into()))?;
        if wasm.is_empty() || !wasm.starts_with(WASM_MAGIC) {
            return Err(CatalogError::InvalidMetadata(
                "main.wasm is empty or is not a WebAssembly module".into(),
            ));
        }

        let manifest = SourceManifest::from_json_bytes(&source_json)?;
        validate_id(&manifest.id)?;

        fs::create_dir_all(&self.root).map_err(|e| {
            CatalogError::Io(format!("creating catalogue {}: {e}", self.root.display()))
        })?;
        let target = self.root.join(&manifest.id);
        let stamp = unique_stamp();
        let temp = self.root.join(format!(".install-{}-{stamp}", manifest.id));
        let backup = self.root.join(format!(".old-{}-{stamp}", manifest.id));

        let write_result = (|| -> Result<(), CatalogError> {
            fs::create_dir_all(&temp)
                .map_err(|e| CatalogError::Io(format!("creating {}: {e}", temp.display())))?;
            write_file(&temp.join("source.json"), &source_json)?;
            write_file(&temp.join("main.wasm"), &wasm)?;
            for name in ["settings.json", "filters.json", "icon.png"] {
                if let Some(data) = wanted.get(name) {
                    write_file(&temp.join(name), data)?;
                }
            }
            Ok(())
        })();
        if let Err(error) = write_result {
            let _ = fs::remove_dir_all(&temp);
            return Err(error);
        }

        let had_previous = target.exists();
        if had_previous {
            fs::rename(&target, &backup).map_err(|e| {
                let _ = fs::remove_dir_all(&temp);
                CatalogError::Io(format!("moving previous installation aside: {e}"))
            })?;
        }
        if let Err(e) = fs::rename(&temp, &target) {
            if had_previous {
                let _ = fs::rename(&backup, &target);
            }
            let _ = fs::remove_dir_all(&temp);
            return Err(CatalogError::Io(format!(
                "activating source installation: {e}"
            )));
        }
        if had_previous {
            let _ = fs::remove_dir_all(&backup);
        }
        read_installed(&target, &manifest.id)
    }

    /// Reads metadata from `.aix` bytes without writing anything. Use this to
    /// show the real name/version and to compare an index entry with the
    /// package before installing an update.
    pub fn inspect_aix_bytes(bytes: &[u8]) -> Result<SourceManifest, CatalogError> {
        if bytes.len() > MAX_AIX_BYTES {
            return Err(CatalogError::TooLarge(format!(
                ".aix is {} bytes (limit {})",
                bytes.len(),
                MAX_AIX_BYTES
            )));
        }
        let mut archive = ZipArchive::new(Cursor::new(bytes))
            .map_err(|e| CatalogError::InvalidArchive(e.to_string()))?;
        let mut wanted = read_aix_entries(&mut archive)?;
        let source_json = wanted
            .remove("source.json")
            .ok_or_else(|| CatalogError::InvalidArchive("source.json not found".into()))?;
        let wasm = wanted
            .remove("main.wasm")
            .ok_or_else(|| CatalogError::InvalidArchive("main.wasm not found".into()))?;
        if wasm.is_empty() || !wasm.starts_with(WASM_MAGIC) {
            return Err(CatalogError::InvalidMetadata(
                "main.wasm is empty or is not a WebAssembly module".into(),
            ));
        }
        SourceManifest::from_json_bytes(&source_json)
    }

    pub fn install_aix_file(&self, path: &Path) -> Result<InstalledSource, CatalogError> {
        let metadata = fs::metadata(path)
            .map_err(|e| CatalogError::Io(format!("reading {}: {e}", path.display())))?;
        if metadata.len() as usize > MAX_AIX_BYTES {
            return Err(CatalogError::TooLarge(format!(
                "{} is {} bytes (limit {})",
                path.display(),
                metadata.len(),
                MAX_AIX_BYTES
            )));
        }
        let bytes = fs::read(path)
            .map_err(|e| CatalogError::Io(format!("reading {}: {e}", path.display())))?;
        self.install_aix_bytes(&bytes)
    }

    pub fn remove(&self, id: &str) -> Result<(), CatalogError> {
        validate_id(id)?;
        let dir = self.root.join(id);
        if !dir.is_dir() {
            return Err(CatalogError::NotFound(id.to_string()));
        }
        fs::remove_dir_all(&dir)
            .map_err(|e| CatalogError::Io(format!("removing {}: {e}", dir.display())))
    }
}

impl SourceManifest {
    pub fn from_json_bytes(bytes: &[u8]) -> Result<Self, CatalogError> {
        let value: Value = serde_json::from_slice(bytes)
            .map_err(|e| CatalogError::InvalidMetadata(format!("source.json: {e}")))?;
        Self::from_value(&value)
    }

    pub fn from_json_str(text: &str) -> Result<Self, CatalogError> {
        let value: Value = serde_json::from_str(text)
            .map_err(|e| CatalogError::InvalidMetadata(format!("source.json: {e}")))?;
        Self::from_value(&value)
    }

    fn from_value(root: &Value) -> Result<Self, CatalogError> {
        let info = root.get("info").filter(|v| v.is_object()).unwrap_or(root);
        let id = info
            .get("id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                CatalogError::InvalidMetadata("source.json has no non-empty info.id".into())
            })?
            .to_string();
        let name = info
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or(&id)
            .to_string();
        let version = info
            .get("version")
            .and_then(|v| {
                v.as_i64()
                    .or_else(|| v.as_str().and_then(|s| s.parse::<i64>().ok()))
            })
            .unwrap_or(1);
        let alt_names = string_list(info.get("altNames"));
        let mut languages = string_list(info.get("languages"));
        if languages.is_empty() {
            languages.push("en".to_string());
        }
        let mut urls = Vec::new();
        if let Some(url) = info.get("url").and_then(Value::as_str) {
            if !url.is_empty() {
                urls.push(url.to_string());
            }
        }
        for url in string_list(info.get("urls")) {
            if !url.is_empty() && !urls.contains(&url) {
                urls.push(url);
            }
        }
        let content_rating = info
            .get("contentRating")
            .and_then(Value::as_i64)
            .unwrap_or(0) as i32;
        let listings = parse_listings(root.get("listings"));
        let config = root.get("config");
        let allows_base_url_select = config
            .and_then(|c| c.get("allowsBaseUrlSelect"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let hides_filters_while_searching = config
            .and_then(|c| c.get("hidesFiltersWhileSearching"))
            .and_then(Value::as_bool)
            .unwrap_or(false);

        Ok(Self {
            id,
            name,
            version,
            alt_names,
            languages,
            urls,
            content_rating,
            listings,
            allows_base_url_select,
            hides_filters_while_searching,
        })
    }
}

fn parse_listings(value: Option<&Value>) -> Vec<Listing> {
    let Some(items) = value.and_then(Value::as_array) else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|item| match item {
            Value::String(id) => Some(Listing {
                id: id.clone(),
                name: id.clone(),
                kind: ListingKind::Default,
            }),
            Value::Object(map) => {
                let id = map.get("id").and_then(Value::as_str)?.to_string();
                let name = map
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or(&id)
                    .to_string();
                let kind = map
                    .get("kind")
                    .and_then(Value::as_i64)
                    .map(|v| ListingKind::from_value(v as u8))
                    .unwrap_or(ListingKind::Default);
                Some(Listing { id, name, kind })
            }
            _ => None,
        })
        .collect()
}

fn string_list(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

fn read_installed(dir: &Path, id: &str) -> Result<InstalledSource, CatalogError> {
    let source_json = dir.join("source.json");
    let wasm_path = dir.join("main.wasm");
    if !source_json.is_file() {
        return Err(CatalogError::InvalidMetadata(format!(
            "{} is missing source.json",
            dir.display()
        )));
    }
    if !wasm_path.is_file() {
        return Err(CatalogError::InvalidMetadata(format!(
            "{} is missing main.wasm",
            dir.display()
        )));
    }
    let bytes = fs::read(&source_json)
        .map_err(|e| CatalogError::Io(format!("reading {}: {e}", source_json.display())))?;
    let manifest = SourceManifest::from_json_bytes(&bytes)?;
    if manifest.id != id {
        return Err(CatalogError::InvalidMetadata(format!(
            "directory \"{id}\" contains source id \"{}\"",
            manifest.id
        )));
    }
    Ok(InstalledSource {
        manifest,
        settings_path: existing(&dir.join("settings.json")),
        filters_path: existing(&dir.join("filters.json")),
        icon_path: existing(&dir.join("icon.png")),
        directory: dir.to_path_buf(),
        wasm_path,
    })
}

fn existing(path: &Path) -> Option<PathBuf> {
    path.is_file().then(|| path.to_path_buf())
}

fn read_aix_entries(
    archive: &mut ZipArchive<Cursor<&[u8]>>,
) -> Result<HashMap<String, Vec<u8>>, CatalogError> {
    const WANTED: [&str; 5] = [
        "source.json",
        "main.wasm",
        "settings.json",
        "filters.json",
        "icon.png",
    ];
    let mut entries = HashMap::new();
    let mut total = 0usize;
    for index in 0..archive.len() {
        let mut file = archive
            .by_index(index)
            .map_err(|e| CatalogError::InvalidArchive(e.to_string()))?;
        if file.is_dir() {
            continue;
        }
        let name = file.name().replace('\\', "/");
        let Some(wanted) = WANTED
            .iter()
            .find(|wanted| name == **wanted || name.ends_with(&format!("/{wanted}")))
        else {
            continue;
        };
        if entries.contains_key(*wanted) {
            continue;
        }
        let declared = file.size() as usize;
        if declared > MAX_ENTRY_BYTES {
            return Err(CatalogError::TooLarge(format!(
                "{wanted} declares {declared} bytes (limit {MAX_ENTRY_BYTES})"
            )));
        }
        total = total.saturating_add(declared);
        if total > MAX_TOTAL_BYTES {
            return Err(CatalogError::TooLarge(format!(
                "uncompressed package exceeds {MAX_TOTAL_BYTES} bytes"
            )));
        }
        let mut data = Vec::with_capacity(declared.min(MAX_ENTRY_BYTES));
        file.by_ref()
            .take((MAX_ENTRY_BYTES + 1) as u64)
            .read_to_end(&mut data)
            .map_err(|e| CatalogError::InvalidArchive(format!("reading {wanted}: {e}")))?;
        if data.len() > MAX_ENTRY_BYTES {
            return Err(CatalogError::TooLarge(format!(
                "{wanted} exceeds {MAX_ENTRY_BYTES} bytes"
            )));
        }
        entries.insert((*wanted).to_string(), data);
    }
    Ok(entries)
}

fn write_file(path: &Path, data: &[u8]) -> Result<(), CatalogError> {
    fs::write(path, data).map_err(|e| CatalogError::Io(format!("writing {}: {e}", path.display())))
}

fn validate_id(id: &str) -> Result<(), CatalogError> {
    let valid = !id.is_empty()
        && id.len() <= 128
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        && id != "."
        && id != "..";
    if valid {
        Ok(())
    } else {
        Err(CatalogError::InvalidId(format!(
            "\"{id}\" must be 1-128 characters of A-Z, a-z, 0-9, '.', '_' or '-'"
        )))
    }
}

fn unique_stamp() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{}-{nanos}", std::process::id())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use zip::CompressionMethod;
    use zip::write::SimpleFileOptions;

    fn aix(source_json: &str, wasm: &[u8]) -> Vec<u8> {
        let mut buffer = Cursor::new(Vec::new());
        {
            let mut writer = zip::ZipWriter::new(&mut buffer);
            let options =
                SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
            writer.start_file("Payload/source.json", options).unwrap();
            writer.write_all(source_json.as_bytes()).unwrap();
            writer.start_file("Payload/main.wasm", options).unwrap();
            writer.write_all(wasm).unwrap();
            writer.finish().unwrap();
        }
        buffer.into_inner()
    }

    const MANIFEST: &str = r#"{
        "info": {
            "id": "test.source",
            "name": "Test Source",
            "version": 3,
            "languages": ["en"],
            "urls": ["https://example.test"]
        },
        "listings": ["popular", {"id": "latest", "name": "Latest", "kind": 1}],
        "config": {"allowsBaseUrlSelect": false}
    }"#;

    #[test]
    fn installs_lists_and_removes_a_source() {
        let dir = tempfile::tempdir().unwrap();
        let catalog = SourceCatalog::new(dir.path());
        let installed = catalog
            .install_aix_bytes(&aix(MANIFEST, b"\0asm\x01\0\0\0"))
            .unwrap();
        assert_eq!(installed.manifest.id, "test.source");
        assert_eq!(installed.manifest.name, "Test Source");
        assert_eq!(installed.manifest.version, 3);
        assert_eq!(installed.manifest.listings.len(), 2);
        assert!(catalog.is_installed("test.source"));

        let listed = catalog.list().unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].manifest.id, "test.source");

        catalog.remove("test.source").unwrap();
        assert!(!catalog.is_installed("test.source"));
        assert!(matches!(
            catalog.get("test.source"),
            Err(CatalogError::NotFound(_))
        ));
    }

    #[test]
    fn rejects_missing_wasm_and_bad_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let catalog = SourceCatalog::new(dir.path());
        let mut buffer = Cursor::new(Vec::new());
        {
            let mut writer = zip::ZipWriter::new(&mut buffer);
            writer
                .start_file("source.json", SimpleFileOptions::default())
                .unwrap();
            writer.write_all(MANIFEST.as_bytes()).unwrap();
            writer.finish().unwrap();
        }
        let error = catalog.install_aix_bytes(&buffer.into_inner()).unwrap_err();
        assert!(matches!(error, CatalogError::InvalidArchive(_)));

        let error = catalog
            .install_aix_bytes(&aix("{\"info\":{}}", b"\0asm\x01\0\0\0"))
            .unwrap_err();
        assert!(matches!(error, CatalogError::InvalidMetadata(_)));
    }

    #[test]
    fn rejects_path_traversal_ids() {
        let dir = tempfile::tempdir().unwrap();
        let catalog = SourceCatalog::new(dir.path());
        let manifest = r#"{"info":{"id":"../escape","name":"x"}}"#;
        let error = catalog
            .install_aix_bytes(&aix(manifest, b"\0asm\x01\0\0\0"))
            .unwrap_err();
        assert!(matches!(error, CatalogError::InvalidId(_)));
        assert!(!dir.path().parent().unwrap().join("escape").exists());
    }
}
