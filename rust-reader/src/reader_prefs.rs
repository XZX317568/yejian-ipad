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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReaderBg {
    #[default]
    Oled,
    Charcoal,
    Gray,
    Paper,
}
impl ReaderBg {
    pub fn rgb(self) -> [u8; 3] {
        match self {
            Self::Oled => [0, 0, 0],
            Self::Charcoal => [22, 20, 18],
            Self::Gray => [36, 38, 41],
            Self::Paper => [234, 227, 213],
        }
    }
    #[allow(dead_code)]
    pub fn is_light_canvas(self) -> bool {
        matches!(self, Self::Paper)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GutterStyle {
    #[default]
    Book,
    None,
    Gap,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TapPreset {
    #[default]
    Split,
    Edges,
    Off,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PageAnim {
    #[default]
    Curl,
    Slide,
    None,
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
    #[serde(default)]
    pub bg: ReaderBg,
    #[serde(default)]
    pub warm_tint: bool,
    #[serde(default)]
    pub gutter: GutterStyle,
    #[serde(default)]
    pub tap_preset: TapPreset,
    #[serde(default = "yes")]
    pub show_corner_hud: bool,
    #[serde(default)]
    pub page_anim: PageAnim,
    #[serde(default)]
    pub bookmarks: Vec<usize>,
}
fn one() -> f32 {
    1.
}
fn yes() -> bool {
    true
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
            bg: ReaderBg::Oled,
            warm_tint: false,
            gutter: GutterStyle::Book,
            tap_preset: TapPreset::Split,
            show_corner_hud: true,
            page_anim: PageAnim::Curl,
            bookmarks: Vec::new(),
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
    #[allow(dead_code)]
    pub fn is_bookmarked(&self, page: usize) -> bool {
        self.bookmarks.contains(&page)
    }
    #[allow(dead_code)]
    pub fn toggle_bookmark(&mut self, page: usize) -> bool {
        if self.bookmarks.contains(&page) {
            self.bookmarks.retain(|&saved| saved != page);
            false
        } else {
            self.bookmarks.push(page);
            self.bookmarks.sort_unstable();
            self.bookmarks.dedup();
            true
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
                ..ReaderPrefs::default()
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

    #[test]
    fn old_json_loads_new_v4_defaults() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(FILE), br#"{"prefs":{"k":{"mode":"spread","rtl":true}}}"#)
            .unwrap();
        let loaded = load(dir.path()).unwrap();
        let prefs = &loaded["k"];
        assert_eq!(prefs.bg, ReaderBg::Oled);
        assert_eq!(prefs.gutter, GutterStyle::Book);
        assert_eq!(prefs.tap_preset, TapPreset::Split);
        assert!(prefs.show_corner_hud);
        assert_eq!(prefs.page_anim, PageAnim::Curl);
        assert!(!prefs.warm_tint);
        assert!(prefs.bookmarks.is_empty());
    }

    #[test]
    fn bookmark_toggle_sorts_dedups_and_persists() {
        let dir = tempfile::tempdir().unwrap();
        let mut prefs = ReaderPrefs {
            bookmarks: vec![4, 1, 4],
            ..ReaderPrefs::default()
        };
        assert!(prefs.is_bookmarked(4));
        assert!(prefs.toggle_bookmark(2));
        assert_eq!(prefs.bookmarks, vec![1, 2, 4]);
        assert!(!prefs.toggle_bookmark(2));
        assert_eq!(prefs.bookmarks, vec![1, 4]);
        assert!(prefs.toggle_bookmark(3));
        assert_eq!(prefs.bookmarks, vec![1, 3, 4]);

        let mut stored = HashMap::new();
        stored.insert("k".into(), prefs);
        save(dir.path(), &stored).unwrap();
        let loaded = load(dir.path()).unwrap();
        assert_eq!(loaded["k"].bookmarks, vec![1, 3, 4]);
        assert!(loaded["k"].is_bookmarked(3));
        assert!(!loaded["k"].is_bookmarked(2));
    }
}
