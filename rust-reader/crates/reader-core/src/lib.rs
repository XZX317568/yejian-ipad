//! Real local library storage for the Yejian Rust reader.
//!
//! The crate owns the imported archive copies, page indexes, reading progress,
//! settings, backups and the legacy Mangayomi metadata migration. It never
//! opens or edits the old Isar database.
//!
//! Page numbers in the public API are zero-based. Imported books are
//! identified by the SHA-256 hash of their copied content, so importing the
//! same file or image folder again returns the existing book and keeps its
//! progress.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use anyhow::{Context, Result, bail};
use rusqlite::Connection;

mod backup;
mod book;
mod chapter;
mod db;
mod import;
mod legacy;
mod library;
mod pages;
mod progress;
mod rar;
mod restore;
mod sort;
mod util;

pub use backup::{BackupFailure, BackupImportReport, BackupReport, BackupSkip};
pub use book::Book;
pub use chapter::{ChapterLink, ChapterMetadata};
pub use legacy::{LegacyImportReport, LegacyImportedBook, LegacySkippedBook};
pub use progress::{ProgressApplyReport, ProgressRecord, ProgressSkip};
pub use restore::{RestoreEntry, RestoreSource, RestoreSummary};
pub use sort::natural_cmp;

/// Category used when neither the caller nor a legacy backup supplies one.
pub const DEFAULT_CATEGORY: &str = "未分类";

/// Archive and image formats that `import()` and `page_bytes()` can read.
pub const SUPPORTED_FORMATS: &[&str] = &[
    "cbz", "zip", "cbt", "tar", "cbr", "rar", "png", "jpg", "jpeg", "webp",
];

/// A library rooted at one directory.
///
/// `Library` keeps one SQLite connection behind a mutex, so one instance can
/// be shared between the render thread and background import work. Opening a
/// second `Library` on the same root is allowed; SQLite WAL and the busy
/// timeout coordinate the writers.
pub struct Library {
    root: PathBuf,
    conn: Mutex<Connection>,
    rar_cache: crate::rar::RarPageCache,
}

impl Library {
    /// Opens or creates the library under `root`.
    ///
    /// The directory layout is:
    /// - `library.sqlite3` for metadata
    /// - `archives/` for imported archive copies and image folders
    /// - `covers/` for generated JPEG thumbnails
    pub fn open(root: &Path) -> Result<Self> {
        if root.exists() && !root.is_dir() {
            bail!("library root is not a directory: {}", root.display());
        }
        std::fs::create_dir_all(root)
            .with_context(|| format!("create library root {}", root.display()))?;
        std::fs::create_dir_all(root.join("archives"))
            .with_context(|| format!("create archive directory under {}", root.display()))?;
        std::fs::create_dir_all(root.join("covers"))
            .with_context(|| format!("create cover directory under {}", root.display()))?;

        let database = root.join("library.sqlite3");
        let connection = Connection::open(&database)
            .with_context(|| format!("open library database {}", database.display()))?;
        connection
            .busy_timeout(std::time::Duration::from_secs(10))
            .context("configure SQLite busy timeout")?;
        connection
            .execute_batch(
                "PRAGMA journal_mode = WAL;
                 PRAGMA synchronous = NORMAL;
                 PRAGMA foreign_keys = ON;",
            )
            .context("configure library database")?;
        db::migrate(&connection)?;

        Ok(Self {
            root: root.to_path_buf(),
            conn: Mutex::new(connection),
            rar_cache: crate::rar::RarPageCache::default(),
        })
    }

    /// The directory that owns this library.
    pub fn root(&self) -> &Path {
        &self.root
    }

    pub(crate) fn archives_dir(&self) -> PathBuf {
        self.root.join("archives")
    }

    pub(crate) fn covers_dir(&self) -> PathBuf {
        self.root.join("covers")
    }

    pub(crate) fn lock_db(&self) -> Result<MutexGuard<'_, Connection>> {
        self.conn
            .lock()
            .map_err(|_| anyhow::anyhow!("library database lock poisoned"))
    }
}
