//! Storage accounting and file purge for the managed library.
//!
//! Deletion is deliberately strict: every candidate path is resolved and
//! canonicalised and must land inside the library root before anything is
//! removed, so stale or external paths (and symlink escapes) are reported
//! instead of followed.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use rusqlite::params;

use crate::book::Book;
use crate::db::{self, StoredBook};
use crate::Library;

/// Bytes and row counts held by the library, split by purpose.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StorageReport {
    /// Archive bytes used by books still on the shelf.
    pub shelf_bytes: u64,
    /// Number of on-shelf books with an archive present on disk.
    pub shelf_count: usize,
    /// Archive bytes used by removed-but-present books.
    pub removed_bytes: u64,
    /// Number of removed books with an archive present on disk.
    pub removed_count: usize,
    /// Bytes used by managed cover files.
    pub cover_bytes: u64,
    /// Number of managed cover files.
    pub cover_count: usize,
}

impl StorageReport {
    pub fn total_bytes(&self) -> u64 {
        self.shelf_bytes + self.removed_bytes + self.cover_bytes
    }
}

/// Size of a file or directory tree; missing paths contribute 0.
fn path_size(path: &Path) -> u64 {
    let meta = match std::fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(_) => return 0,
    };
    if meta.is_file() {
        meta.len()
    } else if meta.is_dir() {
        walk_size(path)
    } else {
        0
    }
}

fn walk_size(dir: &Path) -> u64 {
    let mut total = 0;
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let meta = match entry.metadata() {
                Ok(meta) => meta,
                Err(_) => continue,
            };
            if meta.is_file() {
                total += meta.len();
            } else if meta.is_dir() {
                total += walk_size(&entry.path());
            }
        }
    }
    total
}

fn dir_stats(dir: &Path) -> (u64, usize) {
    let mut bytes = 0;
    let mut count = 0;
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let meta = match std::fs::symlink_metadata(entry.path()) {
                Ok(meta) => meta,
                Err(_) => continue,
            };
            if meta.is_file() {
                bytes += meta.len();
                count += 1;
            } else if meta.is_dir() {
                bytes += walk_size(&entry.path());
                count += 1;
            }
        }
    }
    (bytes, count)
}

/// Deletes `path` only after proving it sits inside `root`. Anything that
/// does not exist yet is a no-op; anything outside is an error.
fn delete_inside_root(root: &Path, path: &Path) -> Result<()> {
    if std::fs::symlink_metadata(path).is_err() {
        return Ok(());
    }
    let real_root = std::fs::canonicalize(root)
        .with_context(|| format!("resolve library root {}", root.display()))?;
    let real = std::fs::canonicalize(path)
        .with_context(|| format!("resolve managed path {}", path.display()))?;
    if !real.starts_with(&real_root) {
        bail!("refusing to delete {}: outside the library", path.display());
    }
    if real.is_dir() {
        std::fs::remove_dir_all(&real)
    } else {
        std::fs::remove_file(&real)
    }
    .with_context(|| format!("delete {}", real.display()))
}

/// Deletes a book's archive and cover files; the row and progress stay.
fn purge_files(root: &Path, book: &Book) -> Result<()> {
    delete_inside_root(root, Path::new(&book.archive_path))?;
    if let Some(cover) = &book.cover_path {
        let cover = PathBuf::from(cover);
        if cover != PathBuf::from(&book.archive_path) {
            delete_inside_root(root, &cover)?;
        }
    }
    Ok(())
}

impl Library {
    /// Byte accounting for managed archives and covers.
    pub fn storage_report(&self) -> Result<StorageReport> {
        let connection = self.lock_db()?;
        let mut report = StorageReport::default();
        let mut statement = connection
            .prepare("SELECT archive_path, removed FROM books")
            .context("prepare storage report")?;
        let rows = statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })
            .context("read storage report rows")?
            .collect::<rusqlite::Result<Vec<_>>>()
            .context("collect storage report rows")?;
        drop(statement);
        for (stored, removed) in rows {
            let path = crate::book::resolve_stored_path(self.root(), &stored);
            let bytes = path_size(&path);
            if bytes == 0 {
                continue;
            }
            if removed == 0 {
                report.shelf_bytes += bytes;
                report.shelf_count += 1;
            } else {
                report.removed_bytes += bytes;
                report.removed_count += 1;
            }
        }
        let (cover_bytes, cover_count) = dir_stats(&self.covers_dir());
        report.cover_bytes = cover_bytes;
        report.cover_count = cover_count;
        Ok(report)
    }

    /// Deletes one book's archive and cover files and marks the row removed.
    /// The row — and with it the reading progress — is kept, so re-importing
    /// the same content repairs the archive via the normal import path.
    pub fn purge_book(&self, book_id: &str) -> Result<()> {
        let stored = {
            let connection = self.lock_db()?;
            let stored =
                db::require_book(&connection, self.root(), book_id, true)?;
            // Two rows must never share an archive; make sure the file is
            // not referenced by a row that stays on the shelf.
            let shared: i64 = connection
                .query_row(
                    "SELECT COUNT(*) FROM books
                     WHERE archive_path = ?1 AND id <> ?2 AND removed = 0",
                    params![stored_archive(&connection, book_id)?, book_id],
                    |row| row.get(0),
                )
                .context("check archive sharing")?;
            if shared > 0 {
                bail!(
                    "archive of {book_id} is still referenced by a book on the shelf"
                );
            }
            stored
        };
        purge_files(self.root(), &stored.book)?;
        let connection = self.lock_db()?;
        connection
            .execute(
                "UPDATE books SET removed = 1, updated_at = ?1 WHERE id = ?2",
                params![crate::util::now_millis(), book_id],
            )
            .context("mark book removed")?;
        Ok(())
    }

    /// Deletes archive/cover files for every removed book; rows and
    /// progress stay for a later re-import.
    pub fn purge_removed(&self) -> Result<usize> {
        let books: Vec<StoredBook> = {
            let connection = self.lock_db()?;
            let mut statement = connection
                .prepare(&format!(
                    "SELECT {} FROM books WHERE removed = 1",
                    db::BOOK_COLUMNS
                ))
                .context("prepare removed book list")?;
            let rows = statement
                .query_map([], |row| db::stored_from_row(self.root(), row))
                .context("read removed books")?
                .collect::<rusqlite::Result<Vec<_>>>()
                .context("collect removed books")?;
            rows
        };
        let mut purged = 0;
        for stored in books {
            purge_files(self.root(), &stored.book)?;
            purged += 1;
        }
        Ok(purged)
    }
}

/// Raw `archive_path` column value for a book (may be relative).
fn stored_archive(connection: &rusqlite::Connection, book_id: &str) -> Result<String> {
    connection
        .query_row(
            "SELECT archive_path FROM books WHERE id = ?1",
            params![book_id],
            |row| row.get(0),
        )
        .context("read stored archive path")
}
