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
mod storage;
mod util;

pub use backup::{BackupFailure, BackupImportReport, BackupReport, BackupSkip};
pub use book::Book;
pub use chapter::{ChapterLink, ChapterMetadata};
pub use legacy::{LegacyImportReport, LegacyImportedBook, LegacySkippedBook};
pub use progress::{ProgressApplyReport, ProgressRecord, ProgressSkip};
pub use restore::{RestoreEntry, RestoreSource, RestoreSummary};
pub use sort::natural_cmp;
pub use storage::StorageReport;

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

        // Rows written before managed paths were stored relative to the
        // library root hold absolute paths from the container that imported
        // them (or working-directory-relative `data\archives\...` values).
        // Rewrite those rows once so a library moved to a new root keeps
        // resolving its own archives/ and covers/ copies; the rewrite touches
        // no other column, in particular not the reading timestamps.
        relocate_managed_paths(&connection)?;

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

/// Rewrites `archive_path`/`cover_path` values that name a managed copy
/// (`archives/<id>[.ext]`, `covers/<id>.jpg`) into the canonical
/// root-relative form. Values pointing elsewhere — a legacy external path,
/// an unrelated folder name, or a file that does not follow the
/// content-addressed convention — are left untouched.
fn relocate_managed_paths(connection: &Connection) -> Result<()> {
    let mut statement = connection
        .prepare("SELECT id, archive_path, cover_path FROM books")
        .context("prepare managed-path relocation")?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
            ))
        })
        .context("query managed paths")?
        .collect::<rusqlite::Result<Vec<_>>>()
        .context("read managed paths")?;
    drop(statement);

    let transaction = connection
        .unchecked_transaction()
        .context("start managed-path relocation")?;
    for (id, archive_path, cover_path) in rows {
        let new_archive = relocate_value(&archive_path, "archives", &id);
        let new_cover = cover_path
            .as_deref()
            .and_then(|value| relocate_value(value, "covers", &id));
        // Only a recognised managed value is rewritten; an unrecognised row
        // keeps its stored text untouched and is not even updated.
        let archive_update = new_archive.filter(|value| *value != archive_path);
        let cover_update = new_cover.filter(|value| Some(value.as_str()) != cover_path.as_deref());
        if archive_update.is_none() && cover_update.is_none() {
            continue;
        }
        transaction
            .execute(
                "UPDATE books SET archive_path = ?1, cover_path = ?2 WHERE id = ?3",
                rusqlite::params![
                    archive_update.unwrap_or(archive_path),
                    cover_update.or(cover_path),
                    id
                ],
            )
            .context("relocate managed path")?;
    }
    transaction
        .commit()
        .context("commit managed-path relocation")
}

/// Returns the canonical `<dirname>/<file>` stored value when `stored` names
/// a managed copy of `book_id`, else `None`.
///
/// A managed row is recognised by shape alone, never by probing the
/// filesystem: the last path component is the content-addressed file name
/// (`<id>[.ext]` for archives, `<id>.jpg` for covers) and the component
/// before it is the managed directory name (`archives`/`covers`). Anything
/// else — an external file, an unrelated folder, a `data/archives/...`
/// nesting that still ends in the managed pair — is either rewritten to that
/// same pair (the managed pair is what matters) or left untouched when the
/// file name does not match the book id.
fn relocate_value(stored: &str, directory: &str, book_id: &str) -> Option<String> {
    let normalized = stored.replace('\\', "/");
    let mut parts = normalized.split('/');
    let file_name = parts.next_back()?;
    let parent = parts.next_back()?;
    if parent != directory {
        return None;
    }
    // The stored stem must be this row's content-addressed id, not just any
    // 64-hex name and never a foreign basename.
    if !is_content_id(book_id) {
        return None;
    }
    let stem = file_name.split('.').next()?;
    if stem != book_id {
        return None;
    }
    // Archive copies keep the extension the importer recorded (`<id>` for an
    // image folder, `<id>.<ext>` for an archive file); covers are the
    // generated JPEG thumbnail `<id>.jpg`. Any other suffix is not a managed
    // name we may claim.
    let suffix = file_name.strip_prefix(book_id)?;
    let allowed = match directory {
        "archives" => {
            suffix.is_empty()
                || matches!(suffix, ".cbz" | ".zip" | ".cbt" | ".tar" | ".cbr" | ".rar")
        }
        "covers" => suffix == ".jpg",
        _ => false,
    };
    if !allowed {
        return None;
    }
    Some(format!("{directory}/{stem}{suffix}"))
}

fn is_content_id(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}
