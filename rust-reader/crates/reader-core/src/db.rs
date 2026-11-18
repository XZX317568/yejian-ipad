use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension, Row};

use crate::book::Book;

const SCHEMA_VERSION: i64 = 3;

pub(crate) const BOOK_COLUMNS: &str = "id, title, author, archive_path, cover_path, page_count, \
     current_page, page_fraction, updated_at, last_read_at, category, content_hash, kind, removed, \
     progress_at";

#[derive(Debug, Clone)]
pub(crate) struct StoredBook {
    pub book: Book,
    pub content_hash: String,
    pub kind: String,
    pub removed: bool,
    /// Unix milliseconds of the last real reading-state change. Import time
    /// and category-only edits do not advance this value.
    pub progress_at: Option<i64>,
}

pub(crate) fn migrate(connection: &Connection) -> Result<()> {
    let version: i64 = connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .context("read library schema version")?;
    if version > SCHEMA_VERSION {
        bail!(
            "library database schema {version} is newer than this application supports ({SCHEMA_VERSION})"
        );
    }

    connection
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS books (
                 id TEXT PRIMARY KEY,
                 title TEXT NOT NULL,
                 author TEXT NOT NULL DEFAULT '',
                 archive_path TEXT NOT NULL,
                 cover_path TEXT,
                 page_count INTEGER NOT NULL,
                 current_page INTEGER NOT NULL DEFAULT 0,
                 page_fraction REAL NOT NULL DEFAULT 0,
                 updated_at INTEGER NOT NULL,
                 last_read_at INTEGER,
                 progress_at INTEGER,
                 category TEXT NOT NULL,
                 content_hash TEXT NOT NULL UNIQUE,
                 kind TEXT NOT NULL,
                 removed INTEGER NOT NULL DEFAULT 0
             );
             CREATE INDEX IF NOT EXISTS books_visible_order
                 ON books(removed, last_read_at, updated_at);
             CREATE TABLE IF NOT EXISTS chapter_links (
                 book_id TEXT PRIMARY KEY REFERENCES books(id) ON DELETE CASCADE,
                 source_id TEXT NOT NULL,
                 manga_id TEXT NOT NULL,
                 manga_title TEXT NOT NULL,
                 chapter_id TEXT NOT NULL,
                 chapter_title TEXT NOT NULL,
                 chapter_number REAL,
                 volume_number REAL,
                 linked_at INTEGER NOT NULL,
                 UNIQUE(source_id, manga_id, chapter_id)
             );
             CREATE INDEX IF NOT EXISTS chapter_links_order
                 ON chapter_links(source_id, manga_id, volume_number, chapter_number);
             CREATE TABLE IF NOT EXISTS pages (
                 book_id TEXT NOT NULL REFERENCES books(id) ON DELETE CASCADE,
                 page_index INTEGER NOT NULL,
                 entry_index INTEGER NOT NULL,
                 entry_name TEXT NOT NULL,
                 size INTEGER NOT NULL,
                 crc32 INTEGER,
                 PRIMARY KEY (book_id, page_index)
             );
             CREATE TABLE IF NOT EXISTS settings (
                 key TEXT PRIMARY KEY,
                 value TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS legacy_backups (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 source_name TEXT NOT NULL,
                 sha256 TEXT NOT NULL UNIQUE,
                 raw_json TEXT NOT NULL,
                 imported_at INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS legacy_records (
                 backup_id INTEGER NOT NULL REFERENCES legacy_backups(id) ON DELETE CASCADE,
                 record_key TEXT NOT NULL,
                 payload TEXT NOT NULL,
                 imported_book_id TEXT,
                 error TEXT,
                 PRIMARY KEY (backup_id, record_key)
             );",
        )
        .context("create library schema")?;

    if version < 2 {
        if !column_exists(connection, "books", "progress_at")? {
            connection
                .execute_batch("ALTER TABLE books ADD COLUMN progress_at INTEGER;")
                .context("add reading-state timestamp column")?;
        }
        connection
            .execute_batch(
                "UPDATE books
                 SET progress_at = COALESCE(
                     last_read_at,
                     CASE WHEN current_page > 0 THEN updated_at ELSE NULL END
                 )
                 WHERE progress_at IS NULL;",
            )
            .context("backfill reading-state timestamps")?;
    }
    if version < 3 && !column_exists(connection, "books", "page_fraction")? {
        connection
            .execute_batch("ALTER TABLE books ADD COLUMN page_fraction REAL NOT NULL DEFAULT 0;")
            .context("add page position column")?;
    }
    if version < SCHEMA_VERSION {
        connection
            .execute_batch(&format!("PRAGMA user_version = {SCHEMA_VERSION};"))
            .context("store library schema version")?;
    }
    Ok(())
}

pub(crate) fn stored_from_row(row: &Row<'_>) -> rusqlite::Result<StoredBook> {
    let cover_path: Option<String> = row.get(4)?;
    let page_count: i64 = row.get(5)?;
    let current_page: i64 = row.get(6)?;
    let removed: i64 = row.get(13)?;
    Ok(StoredBook {
        book: Book {
            id: row.get(0)?,
            title: row.get(1)?,
            author: row.get(2)?,
            archive_path: PathBuf::from(row.get::<_, String>(3)?),
            cover_path: cover_path.map(PathBuf::from),
            page_count: page_count.max(0) as usize,
            current_page: current_page.max(0) as usize,
            page_fraction: crate::book::sanitize_fraction(row.get::<_, f64>(7)? as f32),
            updated_at: row.get(8)?,
            last_read_at: row.get(9)?,
            category: row.get(10)?,
        },
        content_hash: row.get(11)?,
        kind: row.get(12)?,
        removed: removed != 0,
        progress_at: row.get(14)?,
    })
}

fn column_exists(connection: &Connection, table: &str, column: &str) -> Result<bool> {
    let mut statement = connection
        .prepare(&format!("PRAGMA table_info({table})"))
        .context("inspect library table columns")?;
    let mut rows = statement.query([]).context("query library table columns")?;
    while let Some(row) = rows.next().context("read library table columns")? {
        let name: String = row.get(1).context("read library column name")?;
        if name == column {
            return Ok(true);
        }
    }
    Ok(false)
}

pub(crate) fn find_by_hash(
    connection: &Connection,
    content_hash: &str,
) -> Result<Option<StoredBook>> {
    let mut statement = connection
        .prepare(&format!(
            "SELECT {BOOK_COLUMNS} FROM books WHERE content_hash = ?1"
        ))
        .context("prepare book lookup by content hash")?;
    statement
        .query_row(rusqlite::params![content_hash], stored_from_row)
        .optional()
        .context("look up book by content hash")
}

pub(crate) fn find_by_id(connection: &Connection, book_id: &str) -> Result<Option<StoredBook>> {
    let mut statement = connection
        .prepare(&format!("SELECT {BOOK_COLUMNS} FROM books WHERE id = ?1"))
        .context("prepare book lookup by id")?;
    statement
        .query_row(rusqlite::params![book_id], stored_from_row)
        .optional()
        .context("look up book by id")
}

pub(crate) fn require_book(
    connection: &Connection,
    book_id: &str,
    include_removed: bool,
) -> Result<StoredBook> {
    let stored = find_by_id(connection, book_id)?
        .ok_or_else(|| anyhow::anyhow!("book not found: {book_id}"))?;
    if stored.removed && !include_removed {
        bail!("book is not in the library: {book_id}");
    }
    Ok(stored)
}

pub(crate) fn page_count(connection: &Connection, book_id: &str) -> Result<usize> {
    let count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM pages WHERE book_id = ?1",
            rusqlite::params![book_id],
            |row| row.get(0),
        )
        .context("count indexed pages")?;
    Ok(count.max(0) as usize)
}
