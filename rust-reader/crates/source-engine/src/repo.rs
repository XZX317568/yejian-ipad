//! Aidoku source-repository index parsing and installation.
//!
//! Only the real ecosystem formats are parsed:
//!
//! * the current Aidoku index object:
//!   `{ "name": "…", "sources": [ { "id", "name", "version", "downloadURL",
//!   "iconURL", "languages", "contentRating", "baseURL" } ] }`
//! * the legacy array form whose entries use `file` (ending in `.aix`).
//!
//! Nothing is bundled and no repository URL is baked in: the user supplies the
//! list URL, and this crate fetches only that list and the packages they choose
//! to install.

use std::fmt;

use serde_json::Value;

use crate::SourceError;
use crate::catalog::{CatalogError, InstalledSource, SourceCatalog};
use crate::http::{NativeHttp, NetRequestData, NetResponseData};

const MAX_INDEX_BYTES: usize = 16 * 1024 * 1024;
const MAX_INDEX_REDIRECTS: u8 = 2;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RepoError {
    Network(String),
    HttpStatus { url: String, status: u16 },
    Json(String),
    Redirect(String),
    NotFound(String),
    Catalog(CatalogError),
}

impl fmt::Display for RepoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Network(m) => write!(f, "source repository network error: {m}"),
            Self::HttpStatus { url, status } => {
                write!(f, "source repository returned HTTP {status} for {url}")
            }
            Self::Json(m) => write!(f, "source repository index is invalid: {m}"),
            Self::Redirect(url) => write!(f, "source repository redirects to {url}"),
            Self::NotFound(id) => write!(f, "source \"{id}\" is not in this repository"),
            Self::Catalog(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for RepoError {}

impl From<CatalogError> for RepoError {
    fn from(value: CatalogError) -> Self {
        Self::Catalog(value)
    }
}

/// One installable entry from a repository index.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RepositorySource {
    pub id: String,
    pub name: String,
    pub version: i64,
    /// Absolute `.aix` URL, resolved against the index URL.
    pub download_url: String,
    pub icon_url: Option<String>,
    pub languages: Vec<String>,
    pub content_rating: i32,
    pub base_url: Option<String>,
}

/// Parsed repository index.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RepositoryIndex {
    pub name: Option<String>,
    pub website: Option<String>,
    pub index_url: String,
    pub sources: Vec<RepositorySource>,
}

impl RepositoryIndex {
    /// Parses an index body fetched from `index_url`.
    ///
    /// A legacy `index_v2` pointer is reported as [`RepoError::Redirect`];
    /// [`SourceInstaller::fetch_index`] follows it automatically.
    pub fn parse(index_url: &str, body: &[u8]) -> Result<Self, RepoError> {
        if body.len() > MAX_INDEX_BYTES {
            return Err(RepoError::Json(format!(
                "index is {} bytes (limit {MAX_INDEX_BYTES})",
                body.len()
            )));
        }
        if body.starts_with(&[0x1f, 0x8b]) {
            return Err(RepoError::Json(
                "index is gzip-compressed without a Content-Encoding header; \
                 serve it as .json or ask the host to set the header"
                    .into(),
            ));
        }
        let text = std::str::from_utf8(body)
            .map_err(|e| RepoError::Json(format!("index is not UTF-8: {e}")))?;
        let value: Value =
            serde_json::from_str(text).map_err(|e| RepoError::Json(format!("index JSON: {e}")))?;
        let base = url::Url::parse(index_url)
            .map_err(|e| RepoError::Json(format!("index URL \"{index_url}\": {e}")))?;

        match &value {
            Value::Array(items) => {
                let sources = items
                    .iter()
                    .filter_map(|item| parse_legacy_entry(&base, item))
                    .collect();
                Ok(Self {
                    name: None,
                    website: Some(index_url.to_string()),
                    index_url: index_url.to_string(),
                    sources,
                })
            }
            Value::Object(map) => {
                if let Some(redirect) = map.get("index_v2").and_then(Value::as_str) {
                    let target = resolve_url(&base, redirect)?;
                    return Err(RepoError::Redirect(target));
                }
                let name = map
                    .get("name")
                    .or_else(|| map.get("meta").and_then(|m| m.get("name")))
                    .and_then(Value::as_str)
                    .map(str::to_string);
                let website = map
                    .get("website")
                    .or_else(|| map.get("contact").and_then(|c| c.get("website")))
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .or_else(|| Some(index_url.to_string()));
                let items = map
                    .get("sources")
                    .and_then(Value::as_array)
                    .ok_or_else(|| {
                        RepoError::Json(
                            "object index has no \"sources\" array (expected an Aidoku source list)"
                                .into(),
                        )
                    })?;
                let sources = items
                    .iter()
                    .filter_map(|item| parse_index_entry(&base, item))
                    .collect();
                Ok(Self {
                    name,
                    website,
                    index_url: index_url.to_string(),
                    sources,
                })
            }
            _ => Err(RepoError::Json("index must be an object or array".into())),
        }
    }

    pub fn find(&self, id: &str) -> Option<&RepositorySource> {
        self.sources.iter().find(|source| source.id == id)
    }
}

/// Fetches indexes/packages and installs them into a [`SourceCatalog`].
pub struct SourceInstaller {
    http: NativeHttp,
    catalog: SourceCatalog,
}

impl SourceInstaller {
    pub fn new(catalog: SourceCatalog) -> Result<Self, SourceError> {
        Ok(Self {
            http: NativeHttp::new()?,
            catalog,
        })
    }

    pub fn catalog(&self) -> &SourceCatalog {
        &self.catalog
    }

    /// Fetches and parses a repository index, following one legacy
    /// `index_v2` redirect level (at most two hops).
    pub fn fetch_index(&self, index_url: &str) -> Result<RepositoryIndex, RepoError> {
        self.fetch_index_inner(index_url, 0)
    }

    fn fetch_index_inner(&self, index_url: &str, depth: u8) -> Result<RepositoryIndex, RepoError> {
        let response = self.get(index_url)?;
        match RepositoryIndex::parse(index_url, &response.body) {
            Ok(index) => Ok(index),
            Err(RepoError::Redirect(target)) if depth < MAX_INDEX_REDIRECTS => {
                self.fetch_index_inner(&target, depth + 1)
            }
            Err(error) => Err(error),
        }
    }

    /// Downloads and installs one entry from a parsed index.
    pub fn install(
        &self,
        index: &RepositoryIndex,
        source_id: &str,
    ) -> Result<InstalledSource, RepoError> {
        let entry = index
            .find(source_id)
            .ok_or_else(|| RepoError::NotFound(source_id.to_string()))?;
        self.install_entry(entry)
    }

    pub fn install_entry(&self, entry: &RepositorySource) -> Result<InstalledSource, RepoError> {
        let response = self.get(&entry.download_url)?;
        let package = SourceCatalog::inspect_aix_bytes(&response.body)?;
        if package.id != entry.id {
            return Err(RepoError::Json(format!(
                "repository entry \"{}\" contains package \"{}\"",
                entry.id, package.id
            )));
        }
        Ok(self.catalog.install_aix_bytes(&response.body)?)
    }

    fn get(&self, url: &str) -> Result<NetResponseData, RepoError> {
        let parsed = url::Url::parse(url)
            .map_err(|e| RepoError::Network(format!("invalid URL \"{url}\": {e}")))?;
        if !matches!(parsed.scheme(), "http" | "https") {
            return Err(RepoError::Network(format!(
                "unsupported URL scheme \"{}\"",
                parsed.scheme()
            )));
        }
        let response = self.http.send(NetRequestData {
            method: "GET".into(),
            url: url.to_string(),
            headers: vec![(
                "Accept".into(),
                "application/json, application/zip, */*".into(),
            )],
            body: None,
            timeout_ms: 30_000,
        });
        if let Some(error) = response.error.as_deref() {
            return Err(RepoError::Network(error.to_string()));
        }
        if !(200..=299).contains(&response.status_code) {
            return Err(RepoError::HttpStatus {
                url: url.to_string(),
                status: response.status_code,
            });
        }
        Ok(response)
    }
}

fn parse_index_entry(base: &url::Url, value: &Value) -> Option<RepositorySource> {
    let map = value.as_object()?;
    let id = map.get("id").and_then(Value::as_str)?.trim();
    if id.is_empty() {
        return None;
    }
    let download = map
        .get("downloadURL")
        .or_else(|| map.get("file"))
        .and_then(Value::as_str)?;
    let download_url = resolve_url(base, download).ok()?;
    let name = map
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or(id)
        .to_string();
    let version = parse_version(map.get("version"));
    let icon_url = map
        .get("iconURL")
        .or_else(|| map.get("icon"))
        .and_then(Value::as_str)
        .and_then(|icon| resolve_url(base, icon).ok())
        .or_else(|| resolve_url(base, &format!("icons/{id}.png")).ok());
    let mut languages = language_list(map.get("languages"));
    if languages.is_empty() {
        languages = language_list(map.get("lang"));
    }
    if languages.is_empty() {
        languages.push("all".into());
    }
    let content_rating = map
        .get("contentRating")
        .and_then(Value::as_i64)
        .unwrap_or_else(|| match map.get("nsfw") {
            Some(Value::Bool(true)) => 2,
            Some(Value::Number(n)) if n.as_i64().unwrap_or(0) != 0 => 2,
            _ => 0,
        }) as i32;
    let base_url = map
        .get("baseURL")
        .or_else(|| map.get("url"))
        .and_then(Value::as_str)
        .map(str::to_string);
    Some(RepositorySource {
        id: id.to_string(),
        name,
        version,
        download_url,
        icon_url,
        languages,
        content_rating,
        base_url,
    })
}

fn parse_legacy_entry(base: &url::Url, value: &Value) -> Option<RepositorySource> {
    let map = value.as_object()?;
    let file = map.get("file").and_then(Value::as_str)?;
    if !file.ends_with(".aix") {
        return None;
    }
    let id = map.get("id").and_then(Value::as_str)?.trim();
    if id.is_empty() {
        return None;
    }
    let download_url = resolve_url(base, file).ok()?;
    let name = map
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or(id)
        .to_string();
    let icon_url = map
        .get("icon")
        .and_then(Value::as_str)
        .and_then(|icon| resolve_url(base, icon).ok())
        .or_else(|| resolve_url(base, &format!("icons/{id}.png")).ok());
    let mut languages = language_list(map.get("languages"));
    if languages.is_empty() {
        languages = language_list(map.get("lang"));
    }
    if languages.is_empty() {
        languages.push("all".into());
    }
    let base_url = map
        .get("urls")
        .and_then(Value::as_array)
        .and_then(|urls| urls.first())
        .and_then(Value::as_str)
        .or_else(|| map.get("url").and_then(Value::as_str))
        .map(str::to_string);
    Some(RepositorySource {
        id: id.to_string(),
        name,
        version: parse_version(map.get("version")),
        download_url,
        icon_url,
        languages,
        content_rating: if map.get("nsfw").and_then(Value::as_i64).unwrap_or(0) != 0 {
            2
        } else {
            0
        },
        base_url,
    })
}

fn parse_version(value: Option<&Value>) -> i64 {
    match value {
        Some(Value::Number(number)) => number.as_i64().unwrap_or(1),
        Some(Value::String(text)) => text
            .split('.')
            .next()
            .and_then(|first| first.parse::<i64>().ok())
            .unwrap_or(1),
        _ => 1,
    }
}

/// Repository indexes in the wild use both `["en", "zh"]` and `"en, zh"` for
/// `languages`/`lang`. Accept either shape and split comma/space separated
/// strings; a language tag such as `pt-BR` stays intact.
fn language_list(value: Option<&Value>) -> Vec<String> {
    let mut languages = Vec::new();
    match value {
        Some(Value::String(text)) => push_languages(&mut languages, text),
        Some(Value::Array(items)) => {
            for item in items {
                if let Some(text) = item.as_str() {
                    push_languages(&mut languages, text);
                }
            }
        }
        _ => {}
    }
    languages
}

fn push_languages(out: &mut Vec<String>, text: &str) {
    for part in text.split([';', ',']) {
        for token in part.split_whitespace() {
            if !token.is_empty()
                && !out
                    .iter()
                    .any(|existing| existing.eq_ignore_ascii_case(token))
            {
                out.push(token.to_string());
            }
        }
    }
}

fn resolve_url(base: &url::Url, raw: &str) -> Result<String, RepoError> {
    let resolved = base
        .join(raw)
        .map_err(|e| RepoError::Json(format!("bad URL \"{raw}\": {e}")))?;
    if !matches!(resolved.scheme(), "http" | "https") {
        return Err(RepoError::Json(format!(
            "unsupported URL scheme \"{}\" in repository entry",
            resolved.scheme()
        )));
    }
    Ok(resolved.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const INDEX: &str = r#"{
        "name": "Example Sources",
        "sources": [
            {
                "id": "example.en",
                "name": "Example",
                "version": 4,
                "downloadURL": "sources/example.en.aix",
                "iconURL": "icons/example.en.png",
                "languages": ["en", "zh"],
                "contentRating": 1,
                "baseURL": "https://example.test"
            }
        ]
    }"#;

    #[test]
    fn parses_aidoku_index_and_resolves_relative_urls() {
        let index =
            RepositoryIndex::parse("https://repo.test/index.json", INDEX.as_bytes()).unwrap();
        assert_eq!(index.name.as_deref(), Some("Example Sources"));
        assert_eq!(index.sources.len(), 1);
        let source = &index.sources[0];
        assert_eq!(source.id, "example.en");
        assert_eq!(source.version, 4);
        assert_eq!(
            source.download_url,
            "https://repo.test/sources/example.en.aix"
        );
        assert_eq!(
            source.icon_url.as_deref(),
            Some("https://repo.test/icons/example.en.png")
        );
        assert_eq!(source.languages, vec!["en", "zh"]);
        assert_eq!(source.content_rating, 1);
    }

    #[test]
    fn parses_legacy_array_index() {
        let body = r#"[
            {"id": "old", "name": "Old", "version": 2, "file": "Old.aix", "lang": "zh", "nsfw": 1}
        ]"#;
        let index =
            RepositoryIndex::parse("https://repo.test/index.min.json", body.as_bytes()).unwrap();
        assert_eq!(index.sources.len(), 1);
        assert_eq!(index.sources[0].download_url, "https://repo.test/Old.aix");
        assert_eq!(index.sources[0].languages, vec!["zh"]);
        assert_eq!(index.sources[0].content_rating, 2);
    }

    #[test]
    fn parses_comma_separated_language_strings() {
        let body = r#"[
            {"id": "old2", "name": "Old 2", "version": 2, "file": "Old2.aix", "lang": "zh, en"}
        ]"#;
        let index =
            RepositoryIndex::parse("https://repo.test/index.min.json", body.as_bytes()).unwrap();
        assert_eq!(index.sources[0].languages, vec!["zh", "en"]);
    }

    #[test]
    fn reports_legacy_index_v2_redirect() {
        let body = r#"{"index_v2": "v2/index.json"}"#;
        let error = RepositoryIndex::parse("https://repo.test/index.min.json", body.as_bytes())
            .unwrap_err();
        assert_eq!(
            error,
            RepoError::Redirect("https://repo.test/v2/index.json".into())
        );
    }

    #[test]
    fn rejects_non_source_json() {
        let error =
            RepositoryIndex::parse("https://repo.test/index.json", b"{\"foo\":1}").unwrap_err();
        assert!(matches!(error, RepoError::Json(_)));
    }
}
