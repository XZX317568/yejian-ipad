use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use rusqlite::params;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use zip::ZipArchive;

use crate::import::ImportStatus;
use crate::util::{now_millis, read_file_limited, sha256_hex};
use crate::{Book, DEFAULT_CATEGORY, Library};

const MAX_LEGACY_BACKUP_BYTES: u64 = 256 * 1024 * 1024;
const LEGACY_SETTINGS_KEY: &str = "legacy.mangayomi.settings_json";

/// Result of migrating one old Mangayomi JSON/ZIP backup.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LegacyImportReport {
    pub source: PathBuf,
    pub total_manga: usize,
    pub imported: Vec<LegacyImportedBook>,
    pub missing_archives: Vec<LegacySkippedBook>,
    pub failures: Vec<LegacySkippedBook>,
    pub raw_preserved: bool,
}

/// A legacy manga whose local archive was imported.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LegacyImportedBook {
    pub legacy_id: Option<String>,
    pub title: String,
    pub book_id: String,
    pub already_present: bool,
    /// Whether the backup's reading state was written over the existing row.
    #[serde(default)]
    pub state_applied: bool,
}

/// A legacy manga that could not be imported, with a readable reason.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LegacySkippedBook {
    pub legacy_id: Option<String>,
    pub title: String,
    pub archive_path: Option<PathBuf>,
    pub reason: String,
}

struct LoadedLegacy {
    raw: String,
    value: Value,
}

#[derive(Debug, Clone)]
struct LegacyChapter {
    value: Value,
    archive_path: Option<String>,
    last_page_read: Option<String>,
    is_read: bool,
    name: Option<String>,
}

impl Library {
    /// Migrates metadata and readable local archives from an old Mangayomi
    /// JSON or `*.backup.db` ZIP backup.
    ///
    /// The original JSON is stored verbatim in `legacy_backups`; per-record
    /// payloads and any import error are stored in `legacy_records`. The old
    /// Isar database is never opened. Tachiyomi/Mihon `.tachibk`, encrypted
    /// ZIP backups and unknown formats return an explicit error.
    pub fn import_legacy_mangayomi_backup(&self, path: &Path) -> Result<LegacyImportReport> {
        let loaded = load_legacy_backup(path)?;
        let object = loaded
            .value
            .as_object()
            .ok_or_else(|| anyhow::anyhow!("unsupported backup: expected a JSON object"))?;
        if !object.contains_key("manga") && !object.contains_key("categories") {
            bail!(
                "unsupported backup: not a Mangayomi JSON backup; Tachiyomi/Mihon .tachibk and Isar databases cannot be read"
            );
        }

        let raw_hash = sha256_hex(loaded.raw.as_bytes());
        let backup_id = {
            let connection = self.lock_db()?;
            connection
                .execute(
                    "INSERT INTO legacy_backups(source_name, sha256, raw_json, imported_at)
                     VALUES(?1, ?2, ?3, ?4)
                     ON CONFLICT(sha256) DO UPDATE SET
                         source_name = excluded.source_name,
                         imported_at = excluded.imported_at",
                    params![
                        path.file_name()
                            .map(|name| name.to_string_lossy().into_owned())
                            .unwrap_or_else(|| "mangayomi.backup".to_string()),
                        &raw_hash,
                        &loaded.raw,
                        now_millis(),
                    ],
                )
                .context("store raw legacy backup")?;
            connection
                .query_row(
                    "SELECT id FROM legacy_backups WHERE sha256 = ?1",
                    params![&raw_hash],
                    |row| row.get::<_, i64>(0),
                )
                .context("read legacy backup id")?
        };

        if let Some(settings) = object.get("settings") {
            let json = serde_json::to_string(settings).context("serialize legacy settings")?;
            self.set_setting(LEGACY_SETTINGS_KEY, &json)?;
        }

        let categories = category_names(object.get("categories"));
        let chapters = chapters_by_manga(object.get("chapters"))?;
        let manga_values = match object.get("manga") {
            None | Some(Value::Null) => Vec::new(),
            Some(Value::Array(values)) => values.clone(),
            Some(_) => bail!("unsupported Mangayomi backup: manga field is not a list"),
        };

        let mut report = LegacyImportReport {
            source: path.to_path_buf(),
            total_manga: manga_values.len(),
            imported: Vec::new(),
            missing_archives: Vec::new(),
            failures: Vec::new(),
            raw_preserved: true,
        };

        for manga in &manga_values {
            let legacy_id = id_string(manga, "id");
            let title = string_field(manga, "name")
                .filter(|value| !value.trim().is_empty())
                .unwrap_or_else(|| "未命名".to_string());
            let author = string_field(manga, "author").unwrap_or_default();
            let category = manga
                .get("categories")
                .and_then(Value::as_array)
                .and_then(|ids| ids.first())
                .and_then(value_id)
                .and_then(|id| categories.get(&id).cloned())
                .unwrap_or_else(|| DEFAULT_CATEGORY.to_string());
            let updated_at = i64_field(manga, "updatedAt")
                .or_else(|| i64_field(manga, "lastUpdate"))
                .filter(|value| *value > 0)
                .unwrap_or_else(now_millis);
            let last_read_at = i64_field(manga, "lastRead").filter(|value| *value > 0);

            let manga_chapters = legacy_id
                .as_ref()
                .and_then(|id| chapters.get(id))
                .cloned()
                .unwrap_or_default();
            let archive_count = manga_chapters
                .iter()
                .filter(|chapter| chapter.archive_path.is_some())
                .count();
            let had_archive_path = archive_count > 0;
            let mut seen_archives = HashSet::new();
            let mut seen_missing = HashSet::new();
            let mut local_chapters = Vec::new();
            let mut missing_chapters = Vec::new();
            for (index, chapter) in manga_chapters.iter().enumerate() {
                let Some(path) = chapter.archive_path.as_deref() else {
                    continue;
                };
                if Path::new(path).exists() {
                    if seen_archives.insert(path.to_string()) {
                        local_chapters.push((index, chapter));
                    }
                } else if seen_missing.insert(path.to_string()) {
                    missing_chapters.push((index, chapter));
                }
            }

            if local_chapters.is_empty() {
                let reason = if had_archive_path {
                    "local archive path is not readable on this device"
                } else {
                    "backup has no local archive path"
                };
                let skipped = LegacySkippedBook {
                    legacy_id: legacy_id.clone(),
                    title: title.clone(),
                    archive_path: manga_chapters
                        .iter()
                        .find_map(|chapter| chapter.archive_path.clone())
                        .map(PathBuf::from),
                    reason: reason.to_string(),
                };
                report.missing_archives.push(skipped);
                self.insert_legacy_record(
                    backup_id,
                    &record_key(&legacy_id, "missing"),
                    manga,
                    None,
                    Some(reason),
                )?;
                continue;
            }

            for (index, chapter) in &local_chapters {
                let archive_path = PathBuf::from(
                    chapter
                        .archive_path
                        .as_deref()
                        .expect("local chapter has an archive path"),
                );
                let result = self.import_with_status(&archive_path);
                match result {
                    Ok((book, status)) => {
                        let chapter_title =
                            final_legacy_title(&title, chapter.name.as_deref(), archive_count);
                        let page = legacy_page(&book, chapter);
                        let state_applied = self.apply_legacy_state(
                            &book,
                            status,
                            &chapter_title,
                            &author,
                            &category,
                            page,
                            last_read_at,
                            updated_at,
                        )?;
                        report.imported.push(LegacyImportedBook {
                            legacy_id: legacy_id.clone(),
                            title: chapter_title,
                            book_id: book.id.clone(),
                            already_present: status == ImportStatus::Existing,
                            state_applied,
                        });
                        self.insert_legacy_record(
                            backup_id,
                            &record_key(&legacy_id, &format!("chapter:{index}")),
                            &chapter.value,
                            Some(&book.id),
                            None,
                        )?;
                    }
                    Err(error) => {
                        let message = format!("{error:#}");
                        report.failures.push(LegacySkippedBook {
                            legacy_id: legacy_id.clone(),
                            title: title.clone(),
                            archive_path: Some(archive_path),
                            reason: message.clone(),
                        });
                        self.insert_legacy_record(
                            backup_id,
                            &record_key(&legacy_id, &format!("chapter:{index}")),
                            &chapter.value,
                            None,
                            Some(&message),
                        )?;
                    }
                }
            }

            for (index, chapter) in &missing_chapters {
                let chapter_title =
                    final_legacy_title(&title, chapter.name.as_deref(), archive_count);
                let reason = "local archive path is not readable on this device".to_string();
                report.missing_archives.push(LegacySkippedBook {
                    legacy_id: legacy_id.clone(),
                    title: chapter_title,
                    archive_path: chapter.archive_path.as_deref().map(PathBuf::from),
                    reason: reason.clone(),
                });
                self.insert_legacy_record(
                    backup_id,
                    &record_key(&legacy_id, &format!("chapter:{index}")),
                    &chapter.value,
                    None,
                    Some(&reason),
                )?;
            }
        }

        Ok(report)
    }

    #[allow(clippy::too_many_arguments)]
    fn apply_legacy_state(
        &self,
        book: &Book,
        status: ImportStatus,
        title: &str,
        author: &str,
        category: &str,
        page: usize,
        last_read_at: Option<i64>,
        updated_at: i64,
    ) -> Result<bool> {
        let connection = self.lock_db()?;
        let progress_at = last_read_at.or_else(|| if page > 0 { Some(updated_at) } else { None });
        // Legacy backups carry a page but no in-page offset. Keep a local
        // offset when the page itself does not change.
        let page_fraction = if page == book.current_page {
            book.page_fraction
        } else {
            0.0
        };
        if status == ImportStatus::New {
            connection
                .execute(
                    "UPDATE books
                     SET title = ?1, author = ?2, category = ?3, current_page = ?4,
                         page_fraction = ?5, last_read_at = ?6, updated_at = ?7, progress_at = ?8,
                         added_at = ?7
                     WHERE id = ?9",
                    params![
                        title,
                        author,
                        category,
                        page as i64,
                        page_fraction as f64,
                        last_read_at,
                        updated_at,
                        progress_at,
                        &book.id
                    ],
                )
                .context("apply legacy book metadata")?;
            return Ok(true);
        }

        let Some(progress_at) = progress_at else {
            return Ok(false);
        };
        let local_progress_at = crate::db::find_by_id(&connection, &self.root, &book.id)?
            .and_then(|stored| stored.progress_at);
        if local_progress_at.is_some_and(|local| progress_at <= local) {
            return Ok(false);
        }
        connection
            .execute(
                "UPDATE books
                 SET category = ?1, current_page = ?2, page_fraction = ?3, last_read_at = ?4,
                     updated_at = ?5, progress_at = ?6
                 WHERE id = ?7",
                params![
                    category,
                    page as i64,
                    page_fraction as f64,
                    last_read_at,
                    updated_at,
                    progress_at,
                    &book.id
                ],
            )
            .context("merge legacy reading state")?;
        Ok(true)
    }

    fn insert_legacy_record(
        &self,
        backup_id: i64,
        key: &str,
        payload: &Value,
        book_id: Option<&str>,
        error: Option<&str>,
    ) -> Result<()> {
        let payload = serde_json::to_string(payload).context("serialize legacy record")?;
        let connection = self.lock_db()?;
        connection
            .execute(
                "INSERT INTO legacy_records(backup_id, record_key, payload, imported_book_id, error)
                 VALUES(?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(backup_id, record_key) DO UPDATE SET
                     payload = excluded.payload,
                     imported_book_id = excluded.imported_book_id,
                     error = excluded.error",
                params![backup_id, key, payload, book_id, error],
            )
            .context("store legacy record")?;
        Ok(())
    }
}

fn load_legacy_backup(path: &Path) -> Result<LoadedLegacy> {
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .map(|value| value.to_ascii_lowercase())
        .unwrap_or_default();
    if extension == "tachibk" || extension == "gz" {
        bail!(
            "unsupported backup format: Tachiyomi/Mihon .tachibk and compressed protobuf backups cannot be read; only Mangayomi JSON or ZIP backups are supported"
        );
    }

    let mut file =
        File::open(path).with_context(|| format!("open legacy backup {}", path.display()))?;
    let mut magic = [0u8; 4];
    let read = file
        .read(&mut magic)
        .with_context(|| format!("read legacy backup header {}", path.display()))?;
    drop(file);

    if read >= 2 && magic[0] == b'P' && magic[1] == b'K' {
        load_legacy_zip(path)
    } else {
        let bytes = read_file_limited(path, MAX_LEGACY_BACKUP_BYTES)?;
        let raw = String::from_utf8(bytes).context("legacy Mangayomi backup is not UTF-8 JSON")?;
        let value = parse_legacy_json(&raw)?;
        Ok(LoadedLegacy { raw, value })
    }
}

fn load_legacy_zip(path: &Path) -> Result<LoadedLegacy> {
    let file =
        File::open(path).with_context(|| format!("open legacy backup {}", path.display()))?;
    let mut archive = ZipArchive::new(file)
        .with_context(|| format!("read legacy backup ZIP {}", path.display()))?;
    let names = archive
        .file_names()
        .map(|name| name.to_string())
        .collect::<Vec<_>>();
    let entry_name = names
        .iter()
        .find(|name| name.to_ascii_lowercase().ends_with(".backup.db"))
        .or_else(|| {
            names
                .iter()
                .find(|name| name.to_ascii_lowercase().ends_with(".json"))
        })
        .cloned()
        .ok_or_else(|| {
            anyhow::anyhow!(
                "unsupported backup ZIP: expected a *.backup.db Mangayomi JSON entry; Tachiyomi/Mihon and Isar backups cannot be read"
            )
        })?;

    let mut entry = archive
        .by_name(&entry_name)
        .map_err(|error| legacy_zip_error(error, "read legacy backup entry"))?;
    if entry.size() > MAX_LEGACY_BACKUP_BYTES {
        bail!("legacy backup JSON is above the size limit");
    }
    let mut bytes = Vec::new();
    entry
        .by_ref()
        .take(MAX_LEGACY_BACKUP_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| legacy_io_error(error, "read legacy backup entry"))?;
    if bytes.len() as u64 > MAX_LEGACY_BACKUP_BYTES {
        bail!("legacy backup JSON is above the size limit");
    }
    let raw = String::from_utf8(bytes).context("legacy Mangayomi backup is not UTF-8 JSON")?;
    let value = parse_legacy_json(&raw)?;
    Ok(LoadedLegacy { raw, value })
}

fn parse_legacy_json(raw: &str) -> Result<Value> {
    serde_json::from_str(raw).context(
        "unsupported backup format: expected Mangayomi JSON or a ZIP containing *.backup.db; Tachiyomi/Mihon .tachibk and Isar databases cannot be read",
    )
}

fn legacy_zip_error(error: zip::result::ZipError, action: &str) -> anyhow::Error {
    if is_encryption_error(&format!("{error:?}")) {
        anyhow::anyhow!(
            "encrypted Mangayomi ZIP backups are not supported; export an unencrypted backup"
        )
    } else {
        anyhow::Error::new(error).context(action.to_string())
    }
}

fn legacy_io_error(error: std::io::Error, action: &str) -> anyhow::Error {
    if is_encryption_error(&format!("{error:?}")) {
        anyhow::anyhow!(
            "encrypted Mangayomi ZIP backups are not supported; export an unencrypted backup"
        )
    } else {
        anyhow::Error::new(error).context(action.to_string())
    }
}

fn is_encryption_error(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("password") || lower.contains("encrypt")
}

fn category_names(value: Option<&Value>) -> HashMap<String, String> {
    let mut categories = HashMap::new();
    if let Some(Value::Array(values)) = value {
        for category in values {
            if let (Some(id), Some(name)) = (
                id_string(category, "id"),
                string_field(category, "name").filter(|name| !name.trim().is_empty()),
            ) {
                categories.insert(id, name);
            }
        }
    }
    categories
}

fn chapters_by_manga(value: Option<&Value>) -> Result<HashMap<String, Vec<LegacyChapter>>> {
    let mut chapters = HashMap::<String, Vec<LegacyChapter>>::new();
    let Some(value) = value else {
        return Ok(chapters);
    };
    let Value::Array(values) = value else {
        bail!("unsupported Mangayomi backup: chapters field is not a list");
    };
    for chapter in values {
        let Some(manga_id) = id_string(chapter, "mangaId") else {
            continue;
        };
        let parsed = LegacyChapter {
            value: chapter.clone(),
            archive_path: string_field(chapter, "archivePath")
                .filter(|path| !path.trim().is_empty()),
            last_page_read: scalar_string(chapter.get("lastPageRead")),
            is_read: chapter
                .get("isRead")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            name: string_field(chapter, "name").filter(|name| !name.trim().is_empty()),
        };
        chapters.entry(manga_id).or_default().push(parsed);
    }
    Ok(chapters)
}

fn final_legacy_title(
    manga_title: &str,
    chapter_title: Option<&str>,
    archive_count: usize,
) -> String {
    match chapter_title {
        Some(chapter_title) if archive_count > 1 && !chapter_title.trim().is_empty() => {
            format!("{manga_title} - {chapter_title}")
        }
        _ => manga_title.to_string(),
    }
}

fn legacy_page(book: &Book, chapter: &LegacyChapter) -> usize {
    if book.page_count == 0 {
        return 0;
    }
    let parsed = chapter
        .last_page_read
        .as_deref()
        .and_then(|value| value.trim().parse::<usize>().ok());
    let page = match parsed {
        Some(value) => value.saturating_sub(1),
        None if chapter.is_read => book.page_count.saturating_sub(1),
        None => 0,
    };
    page.min(book.page_count - 1)
}

fn record_key(legacy_id: &Option<String>, suffix: &str) -> String {
    format!(
        "manga:{}:{suffix}",
        legacy_id.as_deref().unwrap_or("unknown")
    )
}

fn string_field(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_string)
}

fn scalar_string(value: Option<&Value>) -> Option<String> {
    match value? {
        Value::String(value) => Some(value.clone()),
        Value::Number(value) => Some(value.to_string()),
        _ => None,
    }
}

fn id_string(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(value_id)
}

fn value_id(value: &Value) -> Option<String> {
    match value {
        Value::String(value) => Some(value.clone()),
        Value::Number(value) => Some(value.to_string()),
        _ => None,
    }
}

fn i64_field(value: &Value, key: &str) -> Option<i64> {
    match value.get(key)? {
        Value::Number(number) => number.as_i64(),
        Value::String(value) => value.parse().ok(),
        _ => None,
    }
}
