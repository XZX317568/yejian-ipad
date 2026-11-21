//! Reader bookmarks persisted atomically at `<root>/bookmarks.json`.
//!
//! `work` groups chapters of one title: chapters with source metadata use
//! `source\u{1f}manga` so local downloaded chapters and online chapters of
//! the same work share one bookmark list; plain local books use the book id.
//! `chapter` is the chapter id when known, otherwise the book/task id.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

const FILE: &str = "bookmarks.json";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Bookmark {
    pub work: String,
    pub chapter: String,
    pub page: usize,
    /// Chapter title shown in the list.
    pub title: String,
    pub created_at: u64,
}

#[derive(Debug, Default)]
pub struct Bookmarks {
    entries: Vec<Bookmark>,
}

impl Bookmarks {
    pub fn load(root: &Path) -> Self {
        let path = path(root);
        let Ok(bytes) = std::fs::read(&path) else {
            return Self::default();
        };
        let entries = serde_json::from_slice(&bytes).unwrap_or_default();
        Self { entries }
    }

    pub fn save(&self, root: &Path) -> anyhow::Result<()> {
        let bytes = serde_json::to_vec(&self.entries)?;
        std::fs::create_dir_all(root)?;
        let path = path(root);
        let tmp = root.join(format!("{FILE}.tmp"));
        std::fs::write(&tmp, bytes)?;
        std::fs::rename(&tmp, &path).map_err(|error| {
            let _ = std::fs::remove_file(&tmp);
            anyhow::anyhow!("写入 {} 失败：{error}", path.display())
        })
    }

    #[cfg(any(test, debug_assertions))]
    pub fn entries(&self) -> &[Bookmark] {
        &self.entries
    }

    /// Bookmarks of one work, oldest first for list display.
    pub fn for_work(&self, work: &str) -> Vec<&Bookmark> {
        self.entries
            .iter()
            .filter(|entry| entry.work == work)
            .collect()
    }

    /// Returns true when the current page now has a bookmark, false when an
    /// existing bookmark on the same page was removed.
    pub fn toggle(&mut self, bookmark: Bookmark) -> bool {
        if let Some(index) = self.entries.iter().position(|entry| {
            entry.work == bookmark.work
                && entry.chapter == bookmark.chapter
                && entry.page == bookmark.page
        }) {
            self.entries.remove(index);
            false
        } else {
            self.entries.push(bookmark);
            true
        }
    }

    /// Removes the bookmark matching `index` inside `for_work(work)`.
    pub fn remove(&mut self, work: &str, index: usize) {
        let mut seen = 0;
        self.entries.retain(|entry| {
            if entry.work == work {
                let keep = seen != index;
                seen += 1;
                keep
            } else {
                true
            }
        });
    }

    pub fn has(&self, work: &str, chapter: &str, page: usize) -> bool {
        self.entries.iter().any(|entry| {
            entry.work == work && entry.chapter == chapter && entry.page == page
        })
    }
}

fn path(root: &Path) -> PathBuf {
    root.join(FILE)
}

/// Bookmark work key: `source\u{1f}manga` when chapter metadata exists so a
/// work's local and online chapters share the list; the book id otherwise.
pub fn work_key(book_id: &str, metadata: Option<&yejian_core::ChapterMetadata>) -> String {
    metadata
        .map(|meta| format!("{}\u{1f}{}", meta.source_id, meta.manga_id))
        .unwrap_or_else(|| book_id.to_string())
}

/// Bookmark chapter key: the chapter id when known, the book/task id otherwise.
pub fn chapter_key(book_id: &str, metadata: Option<&yejian_core::ChapterMetadata>) -> String {
    metadata
        .map(|meta| meta.chapter_id.clone())
        .unwrap_or_else(|| book_id.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mark(work: &str, chapter: &str, page: usize) -> Bookmark {
        Bookmark {
            work: work.into(),
            chapter: chapter.into(),
            page,
            title: format!("{chapter} · 第 {} 页", page + 1),
            created_at: 1,
        }
    }

    #[test]
    fn toggle_adds_then_removes_the_same_page() {
        let mut bookmarks = Bookmarks::default();
        assert!(bookmarks.toggle(mark("w", "c1", 3)));
        assert!(bookmarks.has("w", "c1", 3));
        assert!(!bookmarks.toggle(mark("w", "c1", 3)));
        assert!(!bookmarks.has("w", "c1", 3));
        assert!(bookmarks.entries().is_empty());
    }

    #[test]
    fn remove_targets_one_work_only() {
        let mut bookmarks = Bookmarks::default();
        bookmarks.toggle(mark("a", "c1", 0));
        bookmarks.toggle(mark("b", "c1", 0));
        bookmarks.remove("a", 0);
        assert!(bookmarks.for_work("a").is_empty());
        assert_eq!(bookmarks.for_work("b").len(), 1);
    }

    #[test]
    fn round_trips_atomically() {
        let dir = tempfile::tempdir().unwrap();
        let mut bookmarks = Bookmarks::default();
        bookmarks.toggle(mark("w", "c1", 2));
        bookmarks.save(dir.path()).unwrap();
        let loaded = Bookmarks::load(dir.path());
        assert!(loaded.has("w", "c1", 2));
        assert!(!dir.path().join("bookmarks.json.tmp").exists());
    }

    #[test]
    fn online_and_local_chapters_of_one_work_share_the_list() {
        let meta = |chapter: &str| yejian_core::ChapterMetadata {
            source_id: "src".into(),
            manga_id: "m".into(),
            manga_title: "作品".into(),
            chapter_id: chapter.into(),
            chapter_title: chapter.into(),
            chapter_number: None,
            volume_number: None,
        };
        let local = work_key("book-1", Some(&meta("c1")));
        let online = work_key("stream:task", Some(&meta("c1")));
        assert_eq!(local, online);
        assert_eq!(chapter_key("book-1", Some(&meta("c1"))), "c1");
        assert_eq!(work_key("book-1", None), "book-1");
        assert_eq!(chapter_key("book-1", None), "book-1");
    }
}

impl Bookmarks {
    /// Replaces every entry after a cloud merge and persists.
    pub fn replace_all(&mut self, root: &Path, entries: Vec<Bookmark>) -> anyhow::Result<()> {
        self.entries = entries;
        self.save(root)
    }
}
