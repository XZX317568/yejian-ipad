//! Progress-only synchronization records.
//!
//! Records identify books by their content hash/id, never by a file path. They
//! deliberately do not carry archives or images: transport only moves reading
//! state. Deletion is not propagated because there is no reliable tombstone
//! format yet, so a remote `removed` flag never removes a local book.

use anyhow::{Context, Result};
use rusqlite::params;
use serde::{Deserialize, Serialize};

use crate::book::sanitize_fraction;
use crate::db;
use crate::util::validate_book_id;
use crate::{DEFAULT_CATEGORY, Library};

/// Reading state for one book, keyed by the book's content id.
///
/// `updated_at` is the reading-state timestamp (the moment the page changed),
/// not the book's import or category-edit time. `updated_at` and
/// `last_read_at` are Unix milliseconds and are preserved exactly when a
/// record wins a merge.
///
/// `page_fraction` is the viewport offset inside `current_page`, `0.0..=1.0`.
/// Records written before this field existed decode as `0.0`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProgressRecord {
    pub book_id: String,
    pub current_page: usize,
    #[serde(default)]
    pub page_fraction: f32,
    #[serde(default)]
    pub last_read_at: Option<i64>,
    pub updated_at: i64,
    pub category: String,
    /// Received for compatibility, but never applied as a delete.
    #[serde(default)]
    pub removed: bool,
}

/// Outcome of merging remote progress records.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProgressApplyReport {
    /// Book ids whose page/category were updated.
    pub applied: Vec<String>,
    /// Records that could not be applied, with a readable reason.
    pub skipped: Vec<ProgressSkip>,
    /// Records whose local timestamp is the same or newer.
    pub unchanged: usize,
}

impl ProgressApplyReport {
    pub fn applied_count(&self) -> usize {
        self.applied.len()
    }

    pub fn skipped_count(&self) -> usize {
        self.skipped.len()
    }

    pub fn unchanged_count(&self) -> usize {
        self.unchanged
    }
}

/// One record that was not applied, and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProgressSkip {
    pub book_id: String,
    pub reason: String,
}

impl Library {
    /// Exports progress and category for every visible book that has been
    /// read at least once.
    ///
    /// A book imported on a new device is not exported before its first real
    /// page change: its `updated_at` is an import time, not a reading time.
    /// A category-only edit on a never-read book is therefore also not synced
    /// yet; reading and category changes on a book that has been read do use
    /// the reading-state record.
    /// Private reading never writes a page, so it cannot appear here. Hidden
    /// books are excluded; deletion is not part of the synchronization format.
    pub fn export_progress(&self) -> Result<Vec<ProgressRecord>> {
        let connection = self.lock_db()?;
        let mut statement = connection
            .prepare(
                "SELECT id, current_page, page_fraction, last_read_at, progress_at, category
                 FROM books
                 WHERE removed = 0 AND progress_at IS NOT NULL
                 ORDER BY id",
            )
            .context("prepare progress export")?;
        let records = statement
            .query_map([], |row| {
                Ok(ProgressRecord {
                    book_id: row.get(0)?,
                    current_page: row.get::<_, i64>(1)?.max(0) as usize,
                    page_fraction: sanitize_fraction(row.get::<_, f64>(2)? as f32),
                    last_read_at: row.get(3)?,
                    updated_at: row.get(4)?,
                    category: row.get(5)?,
                    removed: false,
                })
            })
            .context("query progress export")?
            .collect::<rusqlite::Result<Vec<_>>>()
            .context("read progress export")?;
        Ok(records)
    }

    /// Merges remote progress records in one transaction.
    ///
    /// A record wins only when its `updated_at` is strictly newer than the
    /// local reading-state timestamp; the incoming timestamps are stored
    /// unchanged. Import time and category-only edits do not count as reading
    /// state. Pages may move backwards during this merge. Unknown books and
    /// out-of-range pages are reported in `skipped` instead of being treated as
    /// page 0. A `removed` record is skipped, and no local book is ever deleted
    /// or hidden here.
    pub fn apply_progress(&self, records: &[ProgressRecord]) -> Result<ProgressApplyReport> {
        let mut connection = self.lock_db()?;
        let transaction = connection
            .transaction()
            .context("start progress synchronization")?;
        let mut report = ProgressApplyReport {
            applied: Vec::new(),
            skipped: Vec::new(),
            unchanged: 0,
        };

        for record in records {
            apply_one(&transaction, record, &mut report)?;
        }

        transaction
            .commit()
            .context("commit progress synchronization")?;
        Ok(report)
    }
}

fn apply_one(
    connection: &rusqlite::Connection,
    record: &ProgressRecord,
    report: &mut ProgressApplyReport,
) -> Result<()> {
    if validate_book_id(&record.book_id).is_err() {
        report.skipped.push(ProgressSkip {
            book_id: record.book_id.clone(),
            reason: "invalid book id".to_string(),
        });
        return Ok(());
    }
    if record.removed {
        report.skipped.push(ProgressSkip {
            book_id: record.book_id.clone(),
            reason: "delete propagation is not supported; kept the local book".to_string(),
        });
        return Ok(());
    }
    if record.updated_at <= 0 {
        report.skipped.push(ProgressSkip {
            book_id: record.book_id.clone(),
            reason: "record has no valid updated_at timestamp".to_string(),
        });
        return Ok(());
    }

    let Some(stored) =
        db::find_by_id(connection, &record.book_id).context("look up progress book")?
    else {
        report.skipped.push(ProgressSkip {
            book_id: record.book_id.clone(),
            reason: "book is not in this library".to_string(),
        });
        return Ok(());
    };
    if stored.removed {
        report.skipped.push(ProgressSkip {
            book_id: record.book_id.clone(),
            reason: "book is not visible in this library".to_string(),
        });
        return Ok(());
    }
    if record.last_read_at.is_none() && record.current_page == 0 {
        report.skipped.push(ProgressSkip {
            book_id: record.book_id.clone(),
            reason: "record has no reading state".to_string(),
        });
        return Ok(());
    }
    if stored
        .progress_at
        .is_some_and(|local| record.updated_at <= local)
    {
        report.unchanged += 1;
        return Ok(());
    }
    if stored.book.page_count == 0 || record.current_page >= stored.book.page_count {
        report.skipped.push(ProgressSkip {
            book_id: record.book_id.clone(),
            reason: format!(
                "page {} is out of range for a {}-page book",
                record.current_page, stored.book.page_count
            ),
        });
        return Ok(());
    }

    let category = record.category.trim();
    let category = if category.is_empty() {
        DEFAULT_CATEGORY
    } else {
        category
    };
    connection
        .execute(
            "UPDATE books
             SET current_page = ?1, page_fraction = ?2, last_read_at = ?3,
                 updated_at = ?4, category = ?5, progress_at = ?4
             WHERE id = ?6 AND removed = 0",
            params![
                record.current_page as i64,
                sanitize_fraction(record.page_fraction) as f64,
                record.last_read_at,
                record.updated_at,
                category,
                &record.book_id
            ],
        )
        .context("apply progress record")?;
    report.applied.push(record.book_id.clone());
    Ok(())
}
