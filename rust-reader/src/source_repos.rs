//! Source-repository bookkeeping: saved repo URLs, the cached indexes that
//! back update badges, and the language filter used by the install list.
//!
//! `source-repos` itself is a JSON setting (whitelisted through the worker
//! settings path, so it survives backup/restore); the fetched indexes are
//! cached atomically at `<root>/source-repos-cache.json` so update badges
//! work offline and before the day's first check.

use chrono::Datelike;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use yejian_sources::{RepositoryIndex, RepositorySource};

/// The community index is always offered, even on a fresh install.
pub const COMMUNITY_INDEX: &str =
    "https://aidoku-community.github.io/sources/index.min.json";
const CACHE_FILE: &str = "source-repos-cache.json";

/// One fetched index, kept per repo URL.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedRepo {
    pub url: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub fetched_at: u64,
    #[serde(default)]
    pub sources: Vec<RepositorySource>,
}

/// Language bucket for the repo-list filter chips.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    #[default]
    All,
    Zh,
    Ja,
    En,
    Other,
}

impl Lang {
    pub const CHOICES: [(Lang, &'static str); 5] = [
        (Lang::All, "全部"),
        (Lang::Zh, "中文"),
        (Lang::Ja, "日本語"),
        (Lang::En, "English"),
        (Lang::Other, "其他"),
    ];

    /// A source belongs to the bucket when any of its declared languages
    /// matches; `Other` collects everything else (and undeclared).
    pub fn accepts(self, languages: &[String]) -> bool {
        match self {
            Lang::All => true,
            Lang::Zh => languages.iter().any(|l| l == "zh" || l.starts_with("zh-")),
            Lang::Ja => languages.iter().any(|l| l == "ja" || l.starts_with("ja-")),
            Lang::En => languages.iter().any(|l| l == "en" || l.starts_with("en-")),
            Lang::Other => {
                languages.is_empty()
                    || languages.iter().all(|l| {
                        !(l == "zh" || l.starts_with("zh-"))
                            && !(l == "ja" || l.starts_with("ja-"))
                            && !(l == "en" || l.starts_with("en-"))
                    })
            }
        }
    }
}

#[derive(Debug, Default)]
pub struct SourceRepos {
    /// Saved repo URLs (`source-repos` setting, JSON list). The community
    /// index is always present.
    pub urls: Vec<String>,
    /// Fetched indexes by repo URL.
    pub cache: BTreeMap<String, CachedRepo>,
    /// Epoch day of the last automatic check (`source-repos-checked-at`).
    pub checked_day: Option<i64>,
}

impl SourceRepos {
    /// Parses the `source-repos` setting; the community index is forced in
    /// so an empty/edited list can never lose it.
    pub fn apply_urls(&mut self, json: &str) {
        let mut urls: Vec<String> = serde_json::from_str(json).unwrap_or_default();
        urls.retain(|url| !url.trim().is_empty());
        if !urls.iter().any(|url| url == COMMUNITY_INDEX) {
            urls.insert(0, COMMUNITY_INDEX.into());
        }
        urls.dedup();
        self.urls = urls;
    }

    /// Serializes the saved URLs for the `source-repos` setting.
    pub fn urls_setting(&self) -> String {
        serde_json::to_string(&self.urls).unwrap_or_else(|_| "[]".into())
    }

    /// A successfully fetched repo joins the saved list. Returns true when
    /// the list changed (so the caller can persist the setting).
    pub fn record_repo(&mut self, url: &str) -> bool {
        let url = url.trim();
        if url.is_empty() || self.urls.iter().any(|saved| saved == url) {
            return false;
        }
        self.urls.push(url.to_string());
        true
    }

    /// Removes a saved repo and its cached index; the community index is
    /// not removable. Returns true when anything changed.
    pub fn remove_repo(&mut self, url: &str) -> bool {
        if url == COMMUNITY_INDEX {
            return false;
        }
        let before = self.urls.len();
        self.urls.retain(|saved| saved != url);
        self.cache.remove(url);
        self.urls.len() != before
    }

    pub fn merge_index(&mut self, url: &str, index: &RepositoryIndex, now: u64) {
        self.cache.insert(
            url.to_string(),
            CachedRepo {
                url: url.to_string(),
                name: index.name.clone(),
                fetched_at: now,
                sources: index.sources.clone(),
            },
        );
    }

    /// Highest repo version seen per source id, across all cached indexes.
    pub fn repo_versions(&self) -> BTreeMap<String, i64> {
        let mut versions = BTreeMap::new();
        for repo in self.cache.values() {
            for source in &repo.sources {
                versions
                    .entry(source.id.clone())
                    .and_modify(|version: &mut i64| *version = (*version).max(source.version))
                    .or_insert(source.version);
            }
        }
        versions
    }

    /// `id -> newest repo version` for installed sources that have a newer
    /// version in any cached index.
    pub fn updates<'a>(
        &self,
        installed: impl Iterator<Item = (&'a str, i64)>,
    ) -> BTreeMap<String, i64> {
        let versions = self.repo_versions();
        installed
            .filter_map(|(id, version)| {
                versions.get(id).and_then(|repo_version| {
                    (*repo_version > version).then(|| (id.to_string(), *repo_version))
                })
            })
            .collect()
    }

    /// The repo entry used to update `id` (highest version wins).
    pub fn update_entry(&self, id: &str) -> Option<RepositorySource> {
        self.cache
            .values()
            .flat_map(|repo| repo.sources.iter())
            .filter(|source| source.id == id)
            .max_by_key(|source| source.version)
            .cloned()
    }

    pub fn load_cache(&mut self, root: &Path) {
        let path = cache_path(root);
        if let Ok(bytes) = std::fs::read(&path)
            && let Ok(cache) = serde_json::from_slice::<Vec<CachedRepo>>(&bytes)
        {
            self.cache = cache.into_iter().map(|repo| (repo.url.clone(), repo)).collect();
        }
    }

    pub fn save_cache(&self, root: &Path) -> anyhow::Result<()> {
        let repos: Vec<&CachedRepo> = self
            .urls
            .iter()
            .filter_map(|url| self.cache.get(url))
            .collect();
        std::fs::create_dir_all(root)?;
        let tmp = root.join(format!("{CACHE_FILE}.tmp"));
        std::fs::write(&tmp, serde_json::to_vec(&repos)?)?;
        std::fs::rename(&tmp, cache_path(root)).map_err(|error| {
            let _ = std::fs::remove_file(&tmp);
            anyhow::anyhow!("写入 {} 失败：{error}", cache_path(root).display())
        })
    }
}

fn cache_path(root: &Path) -> PathBuf {
    root.join(CACHE_FILE)
}

/// Local epoch *day* (days since 1970-01-01) of an epoch-second timestamp —
/// used by the once-a-day auto check.
pub fn epoch_day(secs: u64) -> i64 {
    let date = chrono::DateTime::from_timestamp(secs as i64, 0)
        .map(|stamp| stamp.with_timezone(&chrono::Local).date_naive());
    // 1970-01-01 is `num_days_from_ce` 719_163, so epoch day = ce - 719_163.
    date.map(|d| d.num_days_from_ce() as i64 - 719_163).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo_source(id: &str, version: i64) -> RepositorySource {
        RepositorySource {
            id: id.into(),
            name: id.into(),
            version,
            download_url: format!("https://repo/{id}.aix"),
            icon_url: None,
            languages: vec!["zh".into()],
            content_rating: 0,
            base_url: None,
        }
    }

    #[test]
    fn urls_default_to_community_and_dedupe() {
        let mut repos = SourceRepos::default();
        repos.apply_urls("");
        assert_eq!(repos.urls, vec![COMMUNITY_INDEX.to_string()]);
        repos.apply_urls("[\"https://a\", \"https://a\"]");
        assert_eq!(repos.urls.len(), 2);
        assert_eq!(repos.urls[0], COMMUNITY_INDEX);
        assert_eq!(repos.urls[1], "https://a");
        // Community can never be removed.
        assert!(!repos.remove_repo(COMMUNITY_INDEX));
        assert!(repos.remove_repo("https://a"));
        assert_eq!(repos.urls, vec![COMMUNITY_INDEX.to_string()]);
    }

    #[test]
    fn updates_compare_the_highest_cached_version() {
        let mut repos = SourceRepos::default();
        repos.merge_index(
            "https://a",
            &RepositoryIndex {
                name: None,
                website: None,
                index_url: "https://a".into(),
                sources: vec![repo_source("s1", 3), repo_source("s2", 1)],
            },
            1,
        );
        repos.merge_index(
            "https://b",
            &RepositoryIndex {
                name: None,
                website: None,
                index_url: "https://b".into(),
                sources: vec![repo_source("s1", 2), repo_source("s2", 5)],
            },
            2,
        );
        let updates = repos.updates([("s1", 3), ("s2", 1), ("s3", 9)].into_iter());
        assert_eq!(updates.len(), 1, "s1 is up to date, s3 unknown");
        assert_eq!(updates["s2"], 5);
        assert_eq!(repos.update_entry("s2").unwrap().download_url, "https://repo/s2.aix");
        assert_eq!(repos.update_entry("s2").unwrap().version, 5);
    }

    #[test]
    fn language_buckets() {
        assert!(Lang::Zh.accepts(&["zh".into(), "en".into()]));
        assert!(Lang::Zh.accepts(&["zh-Hans".into()]));
        assert!(!Lang::Zh.accepts(&["en".into()]));
        assert!(Lang::Other.accepts(&["vi".into()]));
        assert!(Lang::Other.accepts(&[]));
        assert!(!Lang::Other.accepts(&["en".into()]));
        assert!(Lang::All.accepts(&["ko".into()]));
    }

    #[test]
    fn cache_round_trip_keeps_only_saved_repos() {
        let dir = tempfile::tempdir().unwrap();
        let mut repos = SourceRepos::default();
        repos.apply_urls("");
        repos.record_repo("https://b");
        repos.merge_index(
            "https://b",
            &RepositoryIndex {
                name: Some("B".into()),
                website: None,
                index_url: "https://b".into(),
                sources: vec![repo_source("s1", 2)],
            },
            7,
        );
        // A repo the user removed is pruned on save.
        repos.merge_index(
            "https://stale",
            &RepositoryIndex {
                name: None,
                website: None,
                index_url: "https://stale".into(),
                sources: vec![repo_source("s9", 1)],
            },
            8,
        );
        repos.save_cache(dir.path()).unwrap();
        let mut loaded = SourceRepos::default();
        loaded.apply_urls(&repos.urls_setting());
        loaded.load_cache(dir.path());
        assert!(loaded.cache.contains_key("https://b"));
        assert!(!loaded.cache.contains_key("https://stale"));
        assert!(!dir.path().join("source-repos-cache.json.tmp").exists());
    }
}
