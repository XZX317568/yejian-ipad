//! Per-work reading settings, persisted as JSON next to follows.json.
//!
//! All chapters of one source work share a single entry, so picking a mode
//! once covers the whole series. Books without chapter metadata fall back to
//! their own id.

use serde::{Deserialize, Serialize};
use std::{collections::HashMap, path::Path};
use yejian_core::ChapterMetadata;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    #[default]
    Paged,
    Spread,
    Vertical,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ReaderPrefs {
    pub mode: Mode,
    pub rtl: bool,
    #[serde(default)]
    pub crop: bool,
    #[serde(default)]
    pub fit_width: bool,
    #[serde(default = "one")]
    pub brightness: f32,
    #[serde(default)]
    pub spread_offset: bool,
}
fn one() -> f32 {
    1.
}
impl Default for ReaderPrefs {
    fn default() -> Self {
        Self {
            mode: Mode::Paged,
            rtl: false,
            crop: false,
            fit_width: false,
            brightness: 1.,
            spread_offset: false,
        }
    }
}
impl ReaderPrefs {
    pub fn brightness(&self) -> f32 {
        if self.brightness.is_finite() {
            self.brightness.clamp(0.3, 1.)
        } else {
            1.
        }
    }
}

/// The preference key a book resolves to: one entry per source work when the
/// chapter metadata is known, otherwise the book itself.
pub fn key_for(metadata: Option<&ChapterMetadata>, book_id: &str) -> String {
    match metadata {
        Some(metadata) => format!("source:{}\u{1f}{}", metadata.source_id, metadata.manga_id),
        None => format!("book:{book_id}"),
    }
}

#[derive(Default, Serialize, Deserialize)]
struct Store {
    prefs: HashMap<String, ReaderPrefs>,
}

const FILE: &str = "reader-prefs.json";

pub fn load(root: &Path) -> anyhow::Result<HashMap<String, ReaderPrefs>> {
    let path = root.join(FILE);
    match std::fs::read(&path) {
        Ok(bytes) => Ok(serde_json::from_slice::<Store>(&bytes)
            .map_err(|error| anyhow::anyhow!("阅读设置损坏：{error}"))?
            .prefs),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(HashMap::new()),
        Err(error) => Err(anyhow::anyhow!("读取 {} 失败：{error}", path.display())),
    }
}

pub fn save(root: &Path, prefs: &HashMap<String, ReaderPrefs>) -> anyhow::Result<()> {
    let bytes = serde_json::to_vec(&Store {
        prefs: prefs.clone(),
    })?;
    std::fs::create_dir_all(root)?;
    let path = root.join(FILE);
    let tmp = root.join(format!("{FILE}.tmp"));
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, &path).map_err(|error| {
        let _ = std::fs::remove_file(&tmp);
        anyhow::anyhow!("写入 {} 失败：{error}", path.display())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metadata() -> ChapterMetadata {
        ChapterMetadata {
            source_id: "source.a".into(),
            manga_id: "story".into(),
            manga_title: "Story".into(),
            chapter_id: "c1".into(),
            chapter_title: "第 1 话".into(),
            chapter_number: Some(1.),
            volume_number: None,
        }
    }

    #[test]
    fn prefs_round_trip_through_an_atomic_json_file() {
        let dir = tempfile::tempdir().unwrap();
        let mut prefs = HashMap::new();
        let key = key_for(Some(&metadata()), "book-1");
        prefs.insert(
            key.clone(),
            ReaderPrefs {
                mode: Mode::Vertical,
                rtl: true,
                crop: true,
                fit_width: false,
                brightness: 0.5,
                spread_offset: true,
            },
        );
        save(dir.path(), &prefs).unwrap();
        assert!(dir.path().join(FILE).is_file());
        assert!(!dir.path().join(format!("{FILE}.tmp")).exists());
        let loaded = load(dir.path()).unwrap();
        assert_eq!(loaded, prefs);
        assert_eq!(loaded[&key].mode, Mode::Vertical);
        assert_eq!(loaded[&key].brightness(), 0.5);
        // Chapters of the same work share one entry.
        let mut other = metadata();
        other.chapter_id = "c2".into();
        assert_eq!(key_for(Some(&other), "book-2"), key);
        assert_eq!(key_for(None, "book-9"), "book:book-9");
    }

    #[test]
    fn missing_or_old_files_read_as_defaults() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load(dir.path()).unwrap().is_empty());
        std::fs::write(dir.path().join(FILE), br#"{"prefs":{"k":{"mode":"spread","rtl":true}}}"#)
            .unwrap();
        let loaded = load(dir.path()).unwrap();
        assert_eq!(loaded["k"].mode, Mode::Spread);
        assert_eq!(loaded["k"].brightness(), 1.);
        assert!(!loaded["k"].crop);
    }
}
