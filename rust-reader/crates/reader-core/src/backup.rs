use std::collections::{BTreeMap, HashMap};
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use rusqlite::params;
use serde::{Deserialize, Serialize};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

use crate::book::Book;
use crate::db::{self, StoredBook};
use crate::pages::{MAX_COVER_BYTES, thumbnail_jpeg};
use crate::util::{
    atomic_write, now_millis, remove_path, safe_join, unique_suffix, validate_book_id, walk_files,
};
use crate::{ChapterLink, chapter};
use crate::{DEFAULT_CATEGORY, Library};

pub(crate) const BACKUP_FORMAT: &str = "yejian-library-backup";
const BACKUP_VERSION: u32 = 1;
pub(crate) const MAX_MANIFEST_BYTES: u64 = 64 * 1024 * 1024;
const MAX_BACKUP_ENTRY_BYTES: u64 = 8 * 1024 * 1024 * 1024;
const MAX_BACKUP_ENTRIES: usize = 100_000;

/// Summary of a completed library export.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackupReport {
    pub path: PathBuf,
    pub books_exported: usize,
    pub skipped: Vec<BackupSkip>,
}

impl BackupReport {
    /// True when every visible book could be written into the backup.
    pub fn is_complete(&self) -> bool {
        self.skipped.is_empty()
    }
}

/// A book that could not be exported because its archive copy is unreadable.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackupSkip {
    pub book_id: String,
    pub title: String,
    pub reason: String,
}

/// Summary of a completed library import.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackupImportReport {
    pub imported: Vec<String>,
    pub updated: Vec<String>,
    #[serde(default)]
    pub skipped: Vec<BackupSkip>,
    pub failures: Vec<BackupFailure>,
    pub settings_restored: usize,
    /// Chapter links written for restored or existing visible books.
    #[serde(default)]
    pub links_restored: usize,
    /// Links that could not be restored, for example because their chapter
    /// already belongs to a different book on this device.
    #[serde(default)]
    pub link_failures: Vec<BackupFailure>,
    /// Raw `app-state.json` bytes when the backup carried one — the caller
    /// applies follows/bookmarks/stats etc. through its own owners.
    #[serde(default)]
    pub app_state: Option<Vec<u8>>,
}

impl BackupImportReport {
    pub fn imported_count(&self) -> usize {
        self.imported.len()
    }

    pub fn updated_count(&self) -> usize {
        self.updated.len()
    }

    pub fn skipped_count(&self) -> usize {
        self.skipped.len()
    }

    pub fn failed_count(&self) -> usize {
        self.failures.len()
    }

    /// True when every book in the backup was restored or already present and
    /// every chapter link was written.
    pub fn is_complete(&self) -> bool {
        self.failures.is_empty() && self.link_failures.is_empty()
    }
}

/// One book that could not be restored; the other books are still restored.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackupFailure {
    pub book_id: String,
    pub title: String,
    pub error: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct BackupManifest {
    format: String,
    version: u32,
    created_at: i64,
    books: Vec<BackupBook>,
    #[serde(default)]
    settings: BTreeMap<String, String>,
    /// Added after the first backup format. Old backups have no links.
    #[serde(default)]
    links: Vec<ChapterLink>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct BackupBook {
    id: String,
    title: String,
    author: String,
    page_count: usize,
    current_page: usize,
    /// `None` in backups written before in-page positions existed.
    #[serde(default)]
    page_fraction: Option<f32>,
    updated_at: i64,
    /// `None` in backups written before join dates existed.
    #[serde(default)]
    added_at: Option<i64>,
    last_read_at: Option<i64>,
    category: String,
    content_hash: String,
    kind: String,
    archive_entry: String,
    cover_entry: Option<String>,
}

struct ExportPlan {
    stored: StoredBook,
    kind: &'static str,
    archive_entry: String,
    cover_entry: Option<String>,
    directory_files: Option<Vec<(PathBuf, String)>>,
}

enum NativeOutcome {
    Imported(Book),
    Updated(Book),
    Skipped(BackupSkip),
}

impl Library {
    /// Exports the visible library to one ZIP with a manifest, archive copies,
    /// covers and settings.
    pub fn export_backup(&self, path: &Path) -> Result<BackupReport> {
        self.export_backup_with_extras(path, &[])
    }

    /// Like [`export_backup`], plus extra files written verbatim at the zip
    /// root — used for `app-state.json` (the same JSON the cloud sync
    /// snapshot carries).
    pub fn export_backup_with_extras(
        &self,
        path: &Path,
        extras: &[(String, Vec<u8>)],
    ) -> Result<BackupReport> {
        if path.exists() {
            bail!("backup destination already exists: {}", path.display());
        }
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent)
                .with_context(|| format!("create backup directory {}", parent.display()))?;
        }

        let (books, settings, links) = {
            let connection = self.lock_db()?;
            let mut statement = connection
                .prepare(&format!(
                    "SELECT {} FROM books WHERE removed = 0 ORDER BY id",
                    db::BOOK_COLUMNS
                ))
                .context("prepare backup book list")?;
            let books = statement
                .query_map([], |row| db::stored_from_row(&self.root, row))
                .context("query backup books")?
                .collect::<rusqlite::Result<Vec<_>>>()
                .context("read backup books")?;

            let mut settings = BTreeMap::new();
            let mut setting_statement = connection
                .prepare("SELECT key, value FROM settings ORDER BY key")
                .context("prepare settings export")?;
            let rows = setting_statement
                .query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })
                .context("query settings")?;
            for row in rows {
                let (key, value) = row.context("read setting")?;
                settings.insert(key, value);
            }
            let links = chapter::query_links(&connection)?;
            (books, settings, links)
        };

        let mut plans = Vec::new();
        let mut skipped = Vec::new();
        for stored in books {
            if !stored.book.archive_path.exists() {
                skipped.push(BackupSkip {
                    book_id: stored.book.id.clone(),
                    title: stored.book.title.clone(),
                    reason: format!("archive is missing: {}", stored.book.archive_path.display()),
                });
                continue;
            }

            let (kind, archive_entry, directory_files) = if stored.kind == "directory" {
                match walk_files(&stored.book.archive_path) {
                    Ok(files) => (
                        "directory",
                        format!("archives/{}/", stored.book.id),
                        Some(files),
                    ),
                    Err(error) => {
                        skipped.push(BackupSkip {
                            book_id: stored.book.id.clone(),
                            title: stored.book.title.clone(),
                            reason: format!("{error:#}"),
                        });
                        continue;
                    }
                }
            } else {
                let extension = stored_archive_extension(&stored.book.archive_path);
                (
                    "archive",
                    format!("archives/{}.{}", stored.book.id, extension),
                    None,
                )
            };

            let cover_entry = stored
                .book
                .cover_path
                .as_ref()
                .filter(|cover| cover.is_file())
                .map(|_| format!("covers/{}.jpg", stored.book.id));
            plans.push(ExportPlan {
                stored,
                kind,
                archive_entry,
                cover_entry,
                directory_files,
            });
        }
        let exported_ids = plans
            .iter()
            .map(|plan| plan.stored.book.id.as_str())
            .collect::<std::collections::HashSet<_>>();
        let links = links
            .into_iter()
            .filter(|link| exported_ids.contains(link.book_id.as_str()))
            .collect::<Vec<_>>();

        let manifest = BackupManifest {
            format: BACKUP_FORMAT.to_string(),
            version: BACKUP_VERSION,
            created_at: now_millis(),
            books: plans
                .iter()
                .map(|plan| backup_book(plan))
                .collect::<Vec<_>>(),
            settings,
            links,
        };
        let manifest_bytes =
            serde_json::to_vec_pretty(&manifest).context("serialize backup manifest")?;

        let file_name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "library.backup".to_string());
        let temporary = path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(format!(".{file_name}.{}.tmp", unique_suffix()));

        let result = (|| -> Result<()> {
            let file = File::create(&temporary)
                .with_context(|| format!("create {}", temporary.display()))?;
            let mut writer = ZipWriter::new(file);
            writer
                .start_file(
                    "manifest.json",
                    SimpleFileOptions::default().compression_method(CompressionMethod::Deflated),
                )
                .context("start backup manifest")?;
            writer
                .write_all(&manifest_bytes)
                .context("write backup manifest")?;

            for (name, bytes) in extras {
                writer
                    .start_file(
                        name,
                        SimpleFileOptions::default()
                            .compression_method(CompressionMethod::Deflated),
                    )
                    .with_context(|| format!("start extra file {name}"))?;
                writer
                    .write_all(bytes)
                    .with_context(|| format!("write extra file {name}"))?;
            }

            for plan in &plans {
                if let Some(files) = &plan.directory_files {
                    for (source, relative) in files {
                        let entry_name =
                            format!("{}{}", plan.archive_entry, relative.trim_start_matches('/'));
                        writer
                            .start_file(
                                entry_name,
                                SimpleFileOptions::default()
                                    .compression_method(CompressionMethod::Stored)
                                    .large_file(true),
                            )
                            .context("start directory archive entry")?;
                        let mut source_file = File::open(source).with_context(|| {
                            format!("open directory archive entry {}", source.display())
                        })?;
                        std::io::copy(&mut source_file, &mut writer)
                            .context("write directory archive entry")?;
                    }
                } else {
                    writer
                        .start_file(
                            &plan.archive_entry,
                            SimpleFileOptions::default()
                                .compression_method(CompressionMethod::Stored)
                                .large_file(true),
                        )
                        .context("start archive entry")?;
                    let mut source_file =
                        File::open(&plan.stored.book.archive_path).with_context(|| {
                            format!("open archive {}", plan.stored.book.archive_path.display())
                        })?;
                    std::io::copy(&mut source_file, &mut writer).context("write archive entry")?;
                }

                if let Some(cover_entry) = &plan.cover_entry {
                    if let Some(cover_path) = &plan.stored.book.cover_path {
                        writer
                            .start_file(
                                cover_entry,
                                SimpleFileOptions::default()
                                    .compression_method(CompressionMethod::Stored),
                            )
                            .context("start cover entry")?;
                        let mut cover_file = File::open(cover_path)
                            .with_context(|| format!("open cover {}", cover_path.display()))?;
                        std::io::copy(&mut cover_file, &mut writer).context("write cover entry")?;
                    }
                }
            }

            let file = writer.finish().context("finish backup archive")?;
            file.sync_all().context("sync backup archive")?;
            fs::rename(&temporary, path).with_context(|| {
                format!(
                    "rename backup {} to {}",
                    temporary.display(),
                    path.display()
                )
            })?;
            Ok(())
        })();

        if result.is_err() {
            remove_path(&temporary);
        }
        result?;
        Ok(BackupReport {
            path: path.to_path_buf(),
            books_exported: plans.len(),
            skipped,
        })
    }

    /// Alias kept for callers that use the shorter verb.
    pub fn backup(&self, path: &Path) -> Result<BackupReport> {
        self.export_backup(path)
    }

    /// Restores a library export. Books are independent: a damaged entry is
    /// reported and does not roll back the books that restored before it.
    pub fn import_backup(&self, path: &Path) -> Result<BackupImportReport> {
        let mut file =
            File::open(path).with_context(|| format!("open backup {}", path.display()))?;
        if !has_zip_magic(&mut file)? {
            bail!(
                "{} is not a YeJian native library backup; use Library::restore() for Mangayomi JSON/ZIP backups",
                path.display()
            );
        }
        let mut archive =
            ZipArchive::new(file).with_context(|| format!("read backup ZIP {}", path.display()))?;

        let manifest: BackupManifest = {
            let mut entry = match archive.index_for_name("manifest.json") {
                Some(index) => archive.by_index(index).context("read backup manifest")?,
                None => {
                    let names = archive.file_names().map(str::to_string).collect::<Vec<_>>();
                    if names
                        .iter()
                        .any(|name| crate::restore::is_legacy_backup_entry(name))
                    {
                        bail!(
                            "{} is a legacy Mangayomi backup, not a YeJian native backup; use Library::restore()",
                            path.display()
                        );
                    }
                    bail!(
                        "{} is not a YeJian library backup (missing manifest.json)",
                        path.display()
                    );
                }
            };
            if entry.size() > MAX_MANIFEST_BYTES {
                bail!("backup manifest is above the size limit");
            }
            let bytes = read_reader_limited(&mut entry, MAX_MANIFEST_BYTES)
                .context("read backup manifest")?;
            serde_json::from_slice(&bytes).context("parse backup manifest")?
        };
        if manifest.format != BACKUP_FORMAT {
            bail!("unsupported backup format: {}", manifest.format);
        }
        if manifest.version > BACKUP_VERSION {
            bail!(
                "backup version {} is newer than this application supports ({BACKUP_VERSION})",
                manifest.version
            );
        }
        let _ = manifest.created_at;

        let app_state = match archive.index_for_name("app-state.json") {
            Some(index) => {
                let mut entry = archive.by_index(index).context("read app-state.json")?;
                let bytes = read_reader_limited(&mut entry, MAX_MANIFEST_BYTES)
                    .context("read app-state.json")?;
                Some(bytes)
            }
            None => None,
        };

        let temporary_root = self
            .archives_dir()
            .join(format!(".restore-{}", unique_suffix()));
        fs::create_dir_all(&temporary_root)
            .with_context(|| format!("create {}", temporary_root.display()))?;

        let mut report = BackupImportReport {
            imported: Vec::new(),
            updated: Vec::new(),
            skipped: Vec::new(),
            failures: Vec::new(),
            settings_restored: 0,
            links_restored: 0,
            link_failures: Vec::new(),
            app_state,
        };

        let result = (|| -> Result<()> {
            for entry in &manifest.books {
                match self.restore_book(&mut archive, entry, &temporary_root) {
                    Ok(NativeOutcome::Imported(book)) => report.imported.push(book.id),
                    Ok(NativeOutcome::Updated(book)) => report.updated.push(book.id),
                    Ok(NativeOutcome::Skipped(skip)) => report.skipped.push(skip),
                    Err(error) => report.failures.push(BackupFailure {
                        book_id: entry.id.clone(),
                        title: entry.title.clone(),
                        error: format!("{error:#}"),
                    }),
                }
            }

            let existing_links = {
                let connection = self.lock_db()?;
                chapter::query_links(&connection)?
            };
            let mut existing_by_book = HashMap::new();
            for link in existing_links {
                existing_by_book.insert(
                    link.book_id,
                    (
                        link.metadata.source_id,
                        link.metadata.manga_id,
                        link.metadata.chapter_id,
                    ),
                );
            }
            for link in &manifest.links {
                if let Some((source_id, manga_id, chapter_id)) = existing_by_book.get(&link.book_id)
                {
                    let same_chapter = source_id.as_str() == link.metadata.source_id.trim()
                        && manga_id.as_str() == link.metadata.manga_id.trim()
                        && chapter_id.as_str() == link.metadata.chapter_id.trim();
                    if !same_chapter {
                        report.link_failures.push(BackupFailure {
                            book_id: link.book_id.clone(),
                            title: format!(
                                "{} {}",
                                link.metadata.manga_title, link.metadata.chapter_title
                            )
                            .trim()
                            .to_string(),
                            error: format!(
                                "the book is already linked to another chapter ({source_id}/{manga_id}/{chapter_id}); kept the local link"
                            ),
                        });
                        continue;
                    }
                }
                match self.link_chapter(&link.book_id, &link.metadata) {
                    Ok(()) => report.links_restored += 1,
                    Err(error) => report.link_failures.push(BackupFailure {
                        book_id: link.book_id.clone(),
                        title: format!(
                            "{} {}",
                            link.metadata.manga_title, link.metadata.chapter_title
                        )
                        .trim()
                        .to_string(),
                        error: format!("{error:#}"),
                    }),
                }
            }

            let mut connection = self.lock_db()?;
            let transaction = connection.transaction().context("start settings restore")?;
            for (key, value) in &manifest.settings {
                transaction
                    .execute(
                        "INSERT INTO settings(key, value) VALUES(?1, ?2)
                         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                        params![key, value],
                    )
                    .context("restore setting")?;
            }
            transaction.commit().context("commit settings restore")?;
            report.settings_restored = manifest.settings.len();
            Ok(())
        })();

        remove_path(&temporary_root);
        result?;
        Ok(report)
    }

    fn restore_book(
        &self,
        archive: &mut ZipArchive<File>,
        entry: &BackupBook,
        temporary_root: &Path,
    ) -> Result<NativeOutcome> {
        validate_book_id(&entry.id)?;
        validate_book_id(&entry.content_hash)?;
        if entry.id != entry.content_hash {
            bail!("backup book id does not match its content hash");
        }
        if entry.kind != "directory" && entry.kind != "archive" {
            bail!("unknown backup book kind: {}", entry.kind);
        }

        let temporary = temporary_root.join(&entry.id);
        let result = (|| -> Result<NativeOutcome> {
            let (source_path, is_directory) = if entry.kind == "directory" {
                let expected_prefix = format!("archives/{}/", entry.id);
                if entry.archive_entry != expected_prefix {
                    bail!("invalid directory archive entry: {}", entry.archive_entry);
                }
                fs::create_dir_all(&temporary)
                    .with_context(|| format!("create {}", temporary.display()))?;
                extract_directory_entries(archive, &expected_prefix, &temporary)?;
                (temporary.clone(), true)
            } else {
                let expected_prefix = format!("archives/{}.", entry.id);
                let suffix = entry.archive_entry.strip_prefix("archives/").unwrap_or("");
                if !entry.archive_entry.starts_with(&expected_prefix)
                    || entry.archive_entry.contains("..")
                    || suffix.contains('/')
                {
                    bail!("invalid archive entry: {}", entry.archive_entry);
                }
                let extension = Path::new(&entry.archive_entry)
                    .extension()
                    .and_then(|value| value.to_str())
                    .filter(|value| {
                        !value.is_empty()
                            && value.len() <= 10
                            && value.bytes().all(|byte| byte.is_ascii_alphanumeric())
                    })
                    .unwrap_or("bin");
                let temporary_file = temporary_root.join(format!("{}.{}", entry.id, extension));
                extract_file_entry(archive, &entry.archive_entry, &temporary_file)?;
                (temporary_file, false)
            };

            let actual_hash = if is_directory {
                crate::pages::hash_directory_payload(&source_path)?
            } else {
                crate::util::hash_file(&source_path)?
            };
            if actual_hash != entry.content_hash {
                bail!(
                    "backup content for {} does not match its recorded id (expected {}, got {})",
                    entry.title,
                    entry.content_hash,
                    actual_hash
                );
            }

            let (book, status) = self.import_with_status(&source_path)?;
            let _ = self.restore_cover(archive, entry, &book);
            let local_progress_at = {
                let connection = self.lock_db()?;
                db::find_by_id(&connection, &self.root, &book.id)?
                    .and_then(|stored| stored.progress_at)
            };
            let backup_progress_at = entry.last_read_at.or_else(|| {
                if entry.current_page > 0 {
                    Some(entry.updated_at.max(0))
                } else {
                    None
                }
            });
            let outcome = match status {
                crate::import::ImportStatus::New => {
                    self.restore_book_state(&book, entry)?;
                    NativeOutcome::Imported(self.get_book(&book.id)?)
                }
                crate::import::ImportStatus::Existing
                    if backup_progress_at
                        .zip(local_progress_at)
                        .is_some_and(|(backup, local)| backup <= local)
                        || (backup_progress_at.is_none() && local_progress_at.is_some()) =>
                {
                    NativeOutcome::Skipped(BackupSkip {
                        book_id: book.id.clone(),
                        title: book.title.clone(),
                        reason:
                            "the library copy is newer than the backup; kept local reading state"
                                .to_string(),
                    })
                }
                crate::import::ImportStatus::Existing if same_reading_state(&book, entry) => {
                    NativeOutcome::Skipped(BackupSkip {
                        book_id: book.id.clone(),
                        title: book.title.clone(),
                        reason: "the book is already present with the same reading state"
                            .to_string(),
                    })
                }
                crate::import::ImportStatus::Existing => {
                    self.restore_book_state(&book, entry)?;
                    NativeOutcome::Updated(self.get_book(&book.id)?)
                }
            };
            Ok(outcome)
        })();

        remove_path(&temporary);
        result
    }

    fn restore_cover(
        &self,
        archive: &mut ZipArchive<File>,
        entry: &BackupBook,
        book: &Book,
    ) -> Result<()> {
        let Some(cover_entry) = &entry.cover_entry else {
            return Ok(());
        };
        let prefix = format!("covers/{}.", entry.id);
        let suffix = cover_entry.strip_prefix("covers/").unwrap_or("");
        if !cover_entry.starts_with(&prefix) || cover_entry.contains("..") || suffix.contains('/') {
            bail!("invalid cover entry: {cover_entry}");
        }
        let mut zip_entry = archive
            .by_name(cover_entry)
            .with_context(|| format!("read cover {cover_entry}"))?;
        if zip_entry.size() > MAX_COVER_BYTES {
            bail!("cover {cover_entry} is above the size limit");
        }
        let bytes = read_reader_limited(&mut zip_entry, MAX_COVER_BYTES)
            .with_context(|| format!("read cover {cover_entry}"))?;
        let thumbnail = thumbnail_jpeg(&bytes)?;
        let cover_path = self.covers_dir().join(format!("{}.jpg", book.id));
        atomic_write(&cover_path, &thumbnail)?;
        let cover_path = crate::book::managed_relative(&cover_path, &self.covers_dir())
            .unwrap_or_else(|| cover_path.to_string_lossy().into_owned());
        let connection = self.lock_db()?;
        connection
            .execute(
                "UPDATE books SET cover_path = ?1 WHERE id = ?2",
                params![cover_path, &book.id],
            )
            .context("restore cover path")?;
        Ok(())
    }

    fn restore_book_state(&self, book: &Book, entry: &BackupBook) -> Result<()> {
        let page = if book.page_count == 0 {
            0
        } else {
            entry
                .current_page
                .min(entry.page_count.saturating_sub(1))
                .min(book.page_count - 1)
        };
        let category = entry.category.trim();
        let category = if category.is_empty() {
            DEFAULT_CATEGORY
        } else {
            category
        };
        let updated_at = if entry.updated_at > 0 {
            entry.updated_at
        } else {
            now_millis()
        };
        let progress_at = entry
            .last_read_at
            .or_else(|| if page > 0 { Some(updated_at) } else { None });
        // A backup written before in-page positions existed has no fraction.
        // Keep a newer local offset when the page itself did not change
        // instead of resetting the reader to the top of that page.
        let page_fraction = match entry.page_fraction {
            Some(value) => crate::book::sanitize_fraction(value),
            None if page == book.current_page => crate::book::sanitize_fraction(book.page_fraction),
            None => 0.0,
        };
        let connection = self.lock_db()?;
        connection
            .execute(
                "UPDATE books
                 SET title = ?1, author = ?2, current_page = ?3, page_fraction = ?4,
                     last_read_at = ?5, updated_at = ?6, category = ?7, progress_at = ?8,
                     added_at = ?9
                 WHERE id = ?10",
                params![
                    &entry.title,
                    &entry.author,
                    page as i64,
                    page_fraction as f64,
                    entry.last_read_at,
                    updated_at,
                    category,
                    progress_at,
                    entry.added_at.unwrap_or(updated_at),
                    &book.id
                ],
            )
            .context("restore reading state")?;
        Ok(())
    }
}

fn backup_book(plan: &ExportPlan) -> BackupBook {
    BackupBook {
        id: plan.stored.book.id.clone(),
        title: plan.stored.book.title.clone(),
        author: plan.stored.book.author.clone(),
        page_count: plan.stored.book.page_count,
        current_page: plan.stored.book.current_page,
        page_fraction: Some(plan.stored.book.page_fraction),
        updated_at: plan.stored.book.updated_at,
        added_at: Some(plan.stored.book.added_at),
        last_read_at: plan.stored.book.last_read_at,
        category: plan.stored.book.category.clone(),
        content_hash: plan.stored.content_hash.clone(),
        kind: plan.kind.to_string(),
        archive_entry: plan.archive_entry.clone(),
        cover_entry: plan.cover_entry.clone(),
    }
}

fn stored_archive_extension(path: &Path) -> String {
    path.extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| extension.to_ascii_lowercase())
        .filter(|extension| {
            !extension.is_empty()
                && extension.len() <= 10
                && extension.bytes().all(|byte| byte.is_ascii_alphanumeric())
        })
        .unwrap_or_else(|| "bin".to_string())
}

fn same_reading_state(book: &Book, entry: &BackupBook) -> bool {
    book.title == entry.title
        && book.author == entry.author
        && book.current_page == entry.current_page
        && entry
            .page_fraction
            .is_none_or(|fraction| crate::book::sanitize_fraction(fraction) == book.page_fraction)
        && book.last_read_at == entry.last_read_at
        && book.category == entry.category
}

fn has_zip_magic(file: &mut File) -> Result<bool> {
    file.seek(SeekFrom::Start(0))
        .context("rewind backup file")?;
    let mut magic = [0u8; 4];
    let read = file.read(&mut magic).context("read backup header")?;
    file.seek(SeekFrom::Start(0))
        .context("rewind backup file")?;
    Ok(read >= 2 && magic[0] == b'P' && magic[1] == b'K')
}

pub(crate) fn read_reader_limited<R: Read>(mut reader: R, maximum: u64) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader
        .by_ref()
        .take(maximum + 1)
        .read_to_end(&mut bytes)
        .context("read backup entry")?;
    if bytes.len() as u64 > maximum {
        bail!("backup entry is above the size limit");
    }
    Ok(bytes)
}

fn extract_file_entry(
    archive: &mut ZipArchive<File>,
    entry_name: &str,
    destination: &Path,
) -> Result<()> {
    let mut entry = archive
        .by_name(entry_name)
        .with_context(|| format!("read backup entry {entry_name}"))?;
    if entry.size() > MAX_BACKUP_ENTRY_BYTES {
        bail!("backup entry {entry_name} is above the size limit");
    }
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let mut output =
        File::create(destination).with_context(|| format!("create {}", destination.display()))?;
    let mut limited = entry.by_ref().take(MAX_BACKUP_ENTRY_BYTES + 1);
    let copied = std::io::copy(&mut limited, &mut output)
        .with_context(|| format!("extract backup entry {entry_name}"))?;
    if copied > MAX_BACKUP_ENTRY_BYTES {
        bail!("backup entry {entry_name} is above the size limit");
    }
    output
        .sync_all()
        .with_context(|| format!("sync {}", destination.display()))?;
    Ok(())
}

fn extract_directory_entries(
    archive: &mut ZipArchive<File>,
    prefix: &str,
    destination: &Path,
) -> Result<()> {
    if archive.len() > MAX_BACKUP_ENTRIES {
        bail!("backup has too many entries");
    }
    let mut extracted = 0usize;
    for index in 0..archive.len() {
        let name = {
            let entry = archive
                .by_index(index)
                .with_context(|| format!("read backup entry {index}"))?;
            entry.name().to_string()
        };
        let Some(relative) = name.strip_prefix(prefix) else {
            continue;
        };
        if relative.is_empty() {
            continue;
        }
        let output_path = safe_join(destination, relative)?;
        let mut entry = archive
            .by_index(index)
            .with_context(|| format!("read backup entry {name}"))?;
        if entry.is_dir() {
            fs::create_dir_all(&output_path)
                .with_context(|| format!("create {}", output_path.display()))?;
            continue;
        }
        if !entry.is_file() || entry.is_symlink() {
            continue;
        }
        if entry.size() > MAX_BACKUP_ENTRY_BYTES {
            bail!("backup entry {name} is above the size limit");
        }
        if let Some(parent) = output_path.parent() {
            fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
        }
        let mut output = File::create(&output_path)
            .with_context(|| format!("create {}", output_path.display()))?;
        let mut limited = entry.by_ref().take(MAX_BACKUP_ENTRY_BYTES + 1);
        let copied = std::io::copy(&mut limited, &mut output)
            .with_context(|| format!("extract backup entry {name}"))?;
        if copied > MAX_BACKUP_ENTRY_BYTES {
            bail!("backup entry {name} is above the size limit");
        }
        output
            .sync_all()
            .with_context(|| format!("sync {}", output_path.display()))?;
        extracted += 1;
    }
    if extracted == 0 {
        bail!("directory backup entry is empty");
    }
    Ok(())
}
