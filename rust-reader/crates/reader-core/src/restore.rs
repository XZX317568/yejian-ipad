//! Unified backup restore: detects a native YeJian backup or a legacy
//! Mangayomi JSON/ZIP backup and reports every book outcome.

use std::fs::File;
use std::io::Read;
use std::path::Path;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use zip::ZipArchive;

use crate::Library;
use crate::backup::{BACKUP_FORMAT, BackupImportReport, MAX_MANIFEST_BYTES, read_reader_limited};
use crate::legacy::LegacyImportReport;

/// Which backup dialect `restore()` detected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RestoreSource {
    /// A ZIP written by `export_backup()` with `manifest.json`.
    Native,
    /// A Mangayomi JSON file or a ZIP containing `*.backup.db`/`*.json`.
    LegacyMangayomi,
}

/// One book or legacy record handled by `restore()`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RestoreEntry {
    pub title: String,
    /// New YeJian library id, when the book was imported or found.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub book_id: Option<String>,
    /// Old Mangayomi id for legacy backups.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub legacy_id: Option<String>,
    /// Why the book was skipped or failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Complete outcome of one `restore()` call.
///
/// `skipped` means "not written because the local copy was already newer or
/// because a legacy local archive is unavailable on this device". `failures`
/// means "an error occurred while reading or importing this book". A damaged
/// book never rolls back books restored before or after it. `link_failures`
/// lists chapter links that were not written (for example because the book is
/// already linked to another chapter); they also count against the completion
/// checks so a partial restore never reports a full success.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RestoreSummary {
    pub source: RestoreSource,
    pub imported: Vec<RestoreEntry>,
    pub updated: Vec<RestoreEntry>,
    pub skipped: Vec<RestoreEntry>,
    pub failures: Vec<RestoreEntry>,
    pub settings_restored: usize,
    /// Chapter links written by a native backup restore.
    #[serde(default)]
    pub links_restored: usize,
    /// Chapter links that could not be restored; the books themselves stay
    /// untouched. A link can fail because its chapter already belongs to a
    /// different book on this device.
    #[serde(default)]
    pub link_failures: Vec<RestoreEntry>,
    /// True when the legacy raw JSON was preserved in the library database.
    pub raw_preserved: bool,
    /// `app-state.json` bytes carried by a native backup (cloud snapshot);
    /// the caller applies it through the UI-side owners.
    #[serde(default)]
    pub app_state: Option<Vec<u8>>,
}

impl RestoreSummary {
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

    /// True when no book failed with an error and every chapter link was
    /// restored.
    pub fn is_complete(&self) -> bool {
        self.failures.is_empty() && self.link_failures.is_empty()
    }

    /// True when every book was imported or updated, with nothing skipped and
    /// every chapter link restored.
    pub fn is_fully_restored(&self) -> bool {
        self.failures.is_empty() && self.skipped.is_empty() && self.link_failures.is_empty()
    }
}

impl Library {
    /// Restores either a native YeJian backup or a legacy Mangayomi JSON/ZIP
    /// backup, choosing by content rather than only by file extension.
    ///
    /// Native backups restore books, covers and settings. Legacy Mangayomi
    /// backups migrate readable local archives and preserve the original JSON;
    /// legacy books whose local archive no longer exists appear in `skipped`
    /// with a readable reason. Unsupported formats return an explicit error.
    pub fn restore(&self, path: &Path) -> Result<RestoreSummary> {
        match detect_backup_source(path)? {
            RestoreSource::Native => Ok(self.summary_from_native(self.import_backup(path)?)),
            RestoreSource::LegacyMangayomi => Ok(summary_from_legacy(
                self.import_legacy_mangayomi_backup(path)?,
            )),
        }
    }

    fn summary_from_native(&self, report: BackupImportReport) -> RestoreSummary {
        let imported = report
            .imported
            .into_iter()
            .map(|id| RestoreEntry {
                title: self
                    .get_book(&id)
                    .map(|book| book.title)
                    .unwrap_or_default(),
                book_id: Some(id),
                legacy_id: None,
                reason: None,
            })
            .collect();
        let updated = report
            .updated
            .into_iter()
            .map(|id| RestoreEntry {
                title: self
                    .get_book(&id)
                    .map(|book| book.title)
                    .unwrap_or_default(),
                book_id: Some(id),
                legacy_id: None,
                reason: None,
            })
            .collect();
        let skipped = report
            .skipped
            .into_iter()
            .map(|skip| RestoreEntry {
                title: skip.title,
                book_id: Some(skip.book_id),
                legacy_id: None,
                reason: Some(skip.reason),
            })
            .collect();
        let failures = report
            .failures
            .into_iter()
            .map(|failure| RestoreEntry {
                title: failure.title,
                book_id: Some(failure.book_id),
                legacy_id: None,
                reason: Some(failure.error),
            })
            .collect();
        let link_failures = report
            .link_failures
            .into_iter()
            .map(|failure| RestoreEntry {
                title: failure.title,
                book_id: Some(failure.book_id),
                legacy_id: None,
                reason: Some(failure.error),
            })
            .collect();

        RestoreSummary {
            source: RestoreSource::Native,
            imported,
            updated,
            skipped,
            failures,
            settings_restored: report.settings_restored,
            links_restored: report.links_restored,
            link_failures,
            raw_preserved: false,
            app_state: report.app_state,
        }
    }
}

fn summary_from_legacy(report: LegacyImportReport) -> RestoreSummary {
    let mut imported = Vec::new();
    let mut updated = Vec::new();
    let mut skipped = Vec::new();
    let mut failures = Vec::new();

    for item in report.imported {
        let entry = RestoreEntry {
            title: item.title,
            book_id: Some(item.book_id),
            legacy_id: item.legacy_id,
            reason: None,
        };
        if !item.already_present {
            imported.push(entry);
        } else if item.state_applied {
            updated.push(entry);
        } else {
            skipped.push(RestoreEntry {
                reason: Some(
                    "the book already existed and its local reading state is newer; kept local state"
                        .to_string(),
                ),
                ..entry
            });
        }
    }

    for item in report.missing_archives {
        let reason = match item.archive_path {
            Some(path) => format!("{}: {}", item.reason, path.display()),
            None => item.reason,
        };
        skipped.push(RestoreEntry {
            title: item.title,
            book_id: None,
            legacy_id: item.legacy_id,
            reason: Some(reason),
        });
    }

    for item in report.failures {
        let reason = match item.archive_path {
            Some(path) => format!("{}: {}", item.reason, path.display()),
            None => item.reason,
        };
        failures.push(RestoreEntry {
            title: item.title,
            book_id: None,
            legacy_id: item.legacy_id,
            reason: Some(reason),
        });
    }

    RestoreSummary {
        source: RestoreSource::LegacyMangayomi,
        imported,
        updated,
        skipped,
        failures,
        settings_restored: 0,
        links_restored: 0,
        link_failures: Vec::new(),
        raw_preserved: report.raw_preserved,
        app_state: None,
    }
}

fn detect_backup_source(path: &Path) -> Result<RestoreSource> {
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .map(|value| value.to_ascii_lowercase())
        .unwrap_or_default();
    if matches!(extension.as_str(), "tachibk" | "gz" | "tgz" | "tar.gz") {
        bail!(
            "unsupported backup format: Tachiyomi/Mihon .tachibk and compressed backups cannot be read; only YeJian native backups and Mangayomi JSON/ZIP backups are supported"
        );
    }

    let mut file = File::open(path).with_context(|| format!("open backup {}", path.display()))?;
    let mut magic = [0u8; 4];
    let read = file
        .read(&mut magic)
        .with_context(|| format!("read backup header {}", path.display()))?;
    drop(file);

    if read >= 2 && magic[0] == b'P' && magic[1] == b'K' {
        return detect_zip_source(path);
    }

    match first_non_whitespace(path)? {
        Some(b'{') => Ok(RestoreSource::LegacyMangayomi),
        Some(_) => bail!(
            "unsupported backup format: {} is neither a ZIP backup nor a Mangayomi JSON file",
            path.display()
        ),
        None => bail!("backup file is empty: {}", path.display()),
    }
}

fn detect_zip_source(path: &Path) -> Result<RestoreSource> {
    let file = File::open(path).with_context(|| format!("open backup {}", path.display()))?;
    let mut archive =
        ZipArchive::new(file).with_context(|| format!("read backup ZIP {}", path.display()))?;
    let names = archive.file_names().map(str::to_string).collect::<Vec<_>>();

    if names.iter().any(|name| name == "manifest.json") {
        let mut entry = archive
            .by_name("manifest.json")
            .context("read backup manifest")?;
        if entry.size() > MAX_MANIFEST_BYTES {
            bail!("backup manifest is above the size limit");
        }
        let bytes =
            read_reader_limited(&mut entry, MAX_MANIFEST_BYTES).context("read backup manifest")?;
        let value: serde_json::Value =
            serde_json::from_slice(&bytes).context("parse backup manifest")?;
        return match value.get("format").and_then(|format| format.as_str()) {
            Some(BACKUP_FORMAT) => Ok(RestoreSource::Native),
            Some(other) => bail!("unsupported backup manifest format: {other}"),
            None => bail!("unsupported backup: manifest.json has no format field"),
        };
    }

    if names.iter().any(|name| is_legacy_backup_entry(name)) {
        return Ok(RestoreSource::LegacyMangayomi);
    }

    bail!(
        "unsupported ZIP backup: neither a YeJian manifest.json nor a Mangayomi *.backup.db entry was found"
    )
}

fn first_non_whitespace(path: &Path) -> Result<Option<u8>> {
    let mut file = File::open(path).with_context(|| format!("open backup {}", path.display()))?;
    let mut buffer = [0u8; 4096];
    let mut scanned = 0u64;
    loop {
        let read = file
            .read(&mut buffer)
            .with_context(|| format!("read backup {}", path.display()))?;
        if read == 0 {
            return Ok(None);
        }
        for byte in &buffer[..read] {
            if !byte.is_ascii_whitespace() {
                return Ok(Some(*byte));
            }
        }
        scanned += read as u64;
        if scanned > 1024 * 1024 {
            bail!(
                "unsupported backup format: {} starts with more than 1 MiB of whitespace",
                path.display()
            );
        }
    }
}

pub(crate) fn is_legacy_backup_entry(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.ends_with(".backup.db") || lower.ends_with(".json")
}
