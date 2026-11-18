use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use rusqlite::{TransactionBehavior, params};
use sha2::{Digest, Sha256};

use crate::db::{self, StoredBook};
use crate::pages::{ScannedPage, SourceKind, build_cover, copy_directory_payload, scan_source};
use crate::util::{
    atomic_write, copy_file_hashed, hash_file, now_millis, remove_path, unique_suffix,
};
use crate::{Book, DEFAULT_CATEGORY, Library};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ImportStatus {
    New,
    Existing,
}

struct PreparedImport {
    content_hash: String,
    title: String,
    author: String,
    kind: SourceKind,
    archive_path: PathBuf,
    cover_path: Option<PathBuf>,
    pages: Vec<ScannedPage>,
    updated_at: i64,
}

impl Library {
    /// Imports one CBZ/ZIP/CBT/TAR file or image folder.
    ///
    /// The source is copied into `archives/` through a temporary path and
    /// renamed into place. Re-importing the same content returns the existing
    /// book and keeps its reading progress. Callers importing a batch should
    /// call this once per file so one damaged file cannot roll back the rest.
    pub fn import(&self, path: &Path) -> Result<Book> {
        Ok(self.import_with_status(path)?.0)
    }

    pub(crate) fn import_with_status(&self, path: &Path) -> Result<(Book, ImportStatus)> {
        let metadata = fs::symlink_metadata(path)
            .with_context(|| format!("read import path {}", path.display()))?;
        if metadata.file_type().is_symlink() {
            bail!("refusing to import a symlink: {}", path.display());
        }
        if metadata.is_dir() {
            self.import_directory(path)
        } else if metadata.is_file() {
            self.import_archive(path)
        } else {
            bail!(
                "import path is neither a file nor a folder: {}",
                path.display()
            )
        }
    }

    fn import_archive(&self, path: &Path) -> Result<(Book, ImportStatus)> {
        let kind = detect_archive_kind(path)?;
        let initial_hash = hash_file(path)?;
        if let Some(existing) = self.find_by_hash(&initial_hash)? {
            return self.finish_existing(existing, Some((path, kind)));
        }

        let temporary = self
            .archives_dir()
            .join(format!(".import-{}.tmp", unique_suffix()));
        let mut hasher = Sha256::new();
        let mut created = Vec::new();
        let result = (|| -> Result<(Book, ImportStatus)> {
            let copied = copy_file_hashed(path, &temporary, &mut hasher)?;
            if copied == 0 {
                bail!("archive is empty: {}", path.display());
            }
            let content_hash = hex::encode(hasher.finalize());
            if content_hash != initial_hash {
                if let Some(existing) = self.find_by_hash(&content_hash)? {
                    remove_path(&temporary);
                    return self.finish_existing(existing, Some((path, kind)));
                }
            }

            let pages = scan_source(kind, &temporary)?;
            let cover = build_cover(kind, &temporary, &pages)?;
            let extension = archive_extension(path, kind);
            let final_path = self
                .archives_dir()
                .join(format!("{content_hash}.{extension}"));
            if final_path.exists() {
                remove_path(&final_path);
            }
            fs::rename(&temporary, &final_path).with_context(|| {
                format!(
                    "move imported archive {} to {}",
                    temporary.display(),
                    final_path.display()
                )
            })?;
            created.push(final_path.clone());

            let cover_path = self.covers_dir().join(format!("{content_hash}.jpg"));
            atomic_write(&cover_path, &cover)?;
            created.push(cover_path.clone());

            let prepared = PreparedImport {
                content_hash,
                title: display_title(path),
                author: String::new(),
                kind,
                archive_path: final_path.clone(),
                cover_path: Some(cover_path),
                pages,
                updated_at: now_millis(),
            };
            let outcome = self.insert_prepared(&prepared)?;
            if outcome.1 == ImportStatus::Existing && outcome.0.archive_path != final_path {
                remove_path(&final_path);
            }
            Ok(outcome)
        })();

        if result.is_err() {
            remove_path(&temporary);
            self.remove_unreferenced(created);
        }
        result
    }

    fn import_directory(&self, path: &Path) -> Result<(Book, ImportStatus)> {
        let temporary = self
            .archives_dir()
            .join(format!(".import-{}.tmp", unique_suffix()));
        let mut hasher = Sha256::new();
        let mut created = Vec::new();
        let result = (|| -> Result<(Book, ImportStatus)> {
            copy_directory_payload(path, &temporary, &mut hasher)?;
            let content_hash = hex::encode(hasher.finalize());
            if let Some(existing) = self.find_by_hash(&content_hash)? {
                remove_path(&temporary);
                return self.finish_existing(existing, Some((path, SourceKind::Directory)));
            }

            let pages = scan_source(SourceKind::Directory, &temporary)?;
            let cover = build_cover(SourceKind::Directory, &temporary, &pages)?;
            let final_path = self.archives_dir().join(&content_hash);
            if final_path.exists() {
                remove_path(&final_path);
            }
            fs::rename(&temporary, &final_path).with_context(|| {
                format!(
                    "move imported image folder {} to {}",
                    temporary.display(),
                    final_path.display()
                )
            })?;
            created.push(final_path.clone());

            let cover_path = self.covers_dir().join(format!("{content_hash}.jpg"));
            atomic_write(&cover_path, &cover)?;
            created.push(cover_path.clone());

            let prepared = PreparedImport {
                content_hash,
                title: display_title(path),
                author: String::new(),
                kind: SourceKind::Directory,
                archive_path: final_path.clone(),
                cover_path: Some(cover_path),
                pages,
                updated_at: now_millis(),
            };
            let outcome = self.insert_prepared(&prepared)?;
            if outcome.1 == ImportStatus::Existing && outcome.0.archive_path != final_path {
                remove_path(&final_path);
            }
            Ok(outcome)
        })();

        if result.is_err() {
            remove_path(&temporary);
            self.remove_unreferenced(created);
        }
        result
    }

    /// Removes files created by a failed import unless a book row already
    /// points at the same content-addressed path.
    ///
    /// Two callers may import identical content at the same time. The loser of
    /// that race must not delete the winner's archive or cover just because its
    /// own database transaction rolled back.
    fn remove_unreferenced(&self, created: Vec<PathBuf>) {
        if created.is_empty() {
            return;
        }
        let Ok(connection) = self.lock_db() else {
            // Without the database, keeping a file is safer than guessing.
            return;
        };
        for path in created {
            let text = path.to_string_lossy();
            let referenced: i64 = connection
                .query_row(
                    "SELECT EXISTS(
                         SELECT 1 FROM books WHERE archive_path = ?1 OR cover_path = ?1
                     )",
                    params![&*text],
                    |row| row.get(0),
                )
                .unwrap_or(1);
            if referenced == 0 {
                remove_path(&path);
            }
        }
    }

    fn find_by_hash(&self, content_hash: &str) -> Result<Option<StoredBook>> {
        let connection = self.lock_db()?;
        db::find_by_hash(&connection, content_hash)
    }

    fn finish_existing(
        &self,
        stored: StoredBook,
        source: Option<(&Path, SourceKind)>,
    ) -> Result<(Book, ImportStatus)> {
        if !stored.book.archive_path.exists() {
            let (source_path, kind) = source.ok_or_else(|| {
                anyhow::anyhow!(
                    "book archive is missing and the source is unavailable: {}",
                    stored.book.archive_path.display()
                )
            })?;
            self.repair_existing_archive(&stored, source_path, kind)?;
        }

        {
            let connection = self.lock_db()?;
            if stored.removed {
                connection
                    .execute(
                        "UPDATE books SET removed = 0, updated_at = ?1 WHERE id = ?2",
                        params![now_millis(), &stored.book.id],
                    )
                    .context("restore removed book")?;
            }
        }

        let _ = self.ensure_cover(&stored);
        let connection = self.lock_db()?;
        let current = db::require_book(&connection, &stored.book.id, false)?;
        Ok((current.book, ImportStatus::Existing))
    }

    fn repair_existing_archive(
        &self,
        stored: &StoredBook,
        source: &Path,
        kind: SourceKind,
    ) -> Result<()> {
        let target = &stored.book.archive_path;
        if target.exists() {
            return Ok(());
        }
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
        }

        let temporary = self
            .archives_dir()
            .join(format!(".repair-{}.tmp", unique_suffix()));
        let result = (|| -> Result<()> {
            match kind {
                SourceKind::Directory => {
                    let mut hasher = Sha256::new();
                    copy_directory_payload(source, &temporary, &mut hasher)?;
                }
                SourceKind::Zip | SourceKind::Tar | SourceKind::Rar => {
                    let mut hasher = Sha256::new();
                    copy_file_hashed(source, &temporary, &mut hasher)?;
                }
            }
            fs::rename(&temporary, target).with_context(|| {
                format!(
                    "restore archive {} from {}",
                    target.display(),
                    temporary.display()
                )
            })?;
            Ok(())
        })();
        if result.is_err() {
            remove_path(&temporary);
        }
        result?;

        let pages_missing = {
            let connection = self.lock_db()?;
            db::page_count(&connection, &stored.book.id)? == 0
        };
        if pages_missing {
            let pages = scan_source(kind, target)?;
            let mut connection = self.lock_db()?;
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .context("start page repair")?;
            transaction
                .execute(
                    "DELETE FROM pages WHERE book_id = ?1",
                    params![&stored.book.id],
                )
                .context("clear old page index")?;
            insert_pages(&transaction, &stored.book.id, &pages)?;
            transaction
                .execute(
                    "UPDATE books SET page_count = ?1 WHERE id = ?2",
                    params![pages.len() as i64, &stored.book.id],
                )
                .context("update repaired page count")?;
            transaction.commit().context("commit page repair")?;
        }
        Ok(())
    }

    fn ensure_cover(&self, stored: &StoredBook) -> Result<()> {
        let cover_missing = stored
            .book
            .cover_path
            .as_ref()
            .map_or(true, |path| !path.exists());
        if !cover_missing {
            return Ok(());
        }
        let kind = SourceKind::from_db(&stored.kind)?;
        let pages = scan_source(kind, &stored.book.archive_path)?;
        let cover = build_cover(kind, &stored.book.archive_path, &pages)?;
        let cover_path = self.covers_dir().join(format!("{}.jpg", stored.book.id));
        atomic_write(&cover_path, &cover)?;
        let cover_path = cover_path.to_string_lossy().into_owned();
        let connection = self.lock_db()?;
        connection
            .execute(
                "UPDATE books SET cover_path = ?1 WHERE id = ?2",
                params![cover_path, &stored.book.id],
            )
            .context("store repaired cover path")?;
        Ok(())
    }

    fn insert_prepared(&self, prepared: &PreparedImport) -> Result<(Book, ImportStatus)> {
        let mut connection = self.lock_db()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context("start book import")?;
        if let Some(existing) = db::find_by_hash(&transaction, &prepared.content_hash)? {
            transaction.commit().context("finish duplicate import")?;
            return Ok((existing.book, ImportStatus::Existing));
        }

        let cover = prepared
            .cover_path
            .as_ref()
            .map(|path| path.to_string_lossy().into_owned());
        let archive_path = prepared.archive_path.to_string_lossy().into_owned();
        transaction
            .execute(
                "INSERT INTO books(
                     id, title, author, archive_path, cover_path, page_count,
                     current_page, updated_at, last_read_at, category,
                     content_hash, kind, removed
                 ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, 0, ?7, NULL, ?8, ?9, ?10, 0)",
                params![
                    &prepared.content_hash,
                    &prepared.title,
                    &prepared.author,
                    archive_path,
                    cover,
                    prepared.pages.len() as i64,
                    prepared.updated_at,
                    DEFAULT_CATEGORY,
                    &prepared.content_hash,
                    prepared.kind.as_str(),
                ],
            )
            .context("insert book")?;
        insert_pages(&transaction, &prepared.content_hash, &prepared.pages)?;
        transaction.commit().context("commit book import")?;
        Ok((prepared_book(prepared), ImportStatus::New))
    }
}

fn prepared_book(prepared: &PreparedImport) -> Book {
    Book {
        id: prepared.content_hash.clone(),
        title: prepared.title.clone(),
        author: prepared.author.clone(),
        archive_path: prepared.archive_path.clone(),
        cover_path: prepared.cover_path.clone(),
        page_count: prepared.pages.len(),
        current_page: 0,
        page_fraction: 0.0,
        updated_at: prepared.updated_at,
        last_read_at: None,
        category: DEFAULT_CATEGORY.to_string(),
    }
}

fn insert_pages(
    connection: &rusqlite::Connection,
    book_id: &str,
    pages: &[ScannedPage],
) -> Result<()> {
    let mut statement = connection
        .prepare(
            "INSERT INTO pages(book_id, page_index, entry_index, entry_name, size, crc32)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
        )
        .context("prepare page index insert")?;
    for (index, page) in pages.iter().enumerate() {
        statement
            .execute(params![
                book_id,
                index as i64,
                page.entry_index as i64,
                &page.entry_name,
                page.size as i64,
                page.crc32.map(|value| value as i64),
            ])
            .context("insert page index")?;
    }
    Ok(())
}

fn display_title(path: &Path) -> String {
    path.file_stem()
        .or_else(|| path.file_name())
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| "未命名".to_string())
}

fn archive_extension(path: &Path, kind: SourceKind) -> String {
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| extension.to_ascii_lowercase());
    if let Some(extension) = extension.as_deref() {
        if matches!(extension, "cbz" | "zip" | "cbt" | "tar" | "cbr" | "rar") {
            return extension.to_string();
        }
    }
    match kind {
        SourceKind::Zip => "cbz".to_string(),
        SourceKind::Tar => "cbt".to_string(),
        SourceKind::Rar => "cbr".to_string(),
        SourceKind::Directory => String::new(),
    }
}

fn detect_archive_kind(path: &Path) -> Result<SourceKind> {
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| extension.to_ascii_lowercase());
    match extension.as_deref() {
        Some("cbz" | "zip") => return Ok(SourceKind::Zip),
        Some("cbt" | "tar") => return Ok(SourceKind::Tar),
        Some("cbr" | "rar") => return Ok(SourceKind::Rar),
        Some("gz" | "tgz" | "tar.gz") => {
            bail!("gzip-compressed TAR is not supported; extract it to .tar first")
        }
        _ => {}
    }

    let mut file = File::open(path).with_context(|| format!("open archive {}", path.display()))?;
    let mut header = [0u8; 512];
    let read = file
        .read(&mut header)
        .with_context(|| format!("read archive header {}", path.display()))?;
    if read >= 4 && header.starts_with(b"PK\x03\x04") {
        return Ok(SourceKind::Zip);
    }
    if read >= 2 && header[0] == 0x1f && header[1] == 0x8b {
        bail!("gzip-compressed TAR is not supported; extract it to .tar first");
    }
    if read >= 262 && &header[257..262] == b"ustar".as_slice() {
        return Ok(SourceKind::Tar);
    }
    if read >= 7 && header.starts_with(b"Rar!\x1a\x07") {
        return Ok(SourceKind::Rar);
    }
    bail!(
        "unsupported archive format: {} (supported: CBZ, ZIP, CBT, TAR, CBR, RAR)",
        path.display()
    )
}
