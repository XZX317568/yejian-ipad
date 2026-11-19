use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// One book in the visible library.
///
/// `current_page` is zero-based. `last_read_at` is Unix milliseconds and stays
/// `None` until the first non-private progress save.
///
/// `page_fraction` is the reading position inside `current_page`: the top edge
/// of the viewport relative to the full image height, clamped to `0.0..=1.0`.
/// It makes tall, continuously scrolled pages reopen where the reader left
/// off. Values written by readers that predate this field decode as `0.0`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Book {
    pub id: String,
    pub title: String,
    pub author: String,
    pub archive_path: PathBuf,
    pub cover_path: Option<PathBuf>,
    pub page_count: usize,
    pub current_page: usize,
    #[serde(default)]
    pub page_fraction: f32,
    pub updated_at: i64,
    pub last_read_at: Option<i64>,
    pub category: String,
}

/// Repairs a position fraction coming from storage or another device.
///
/// A stored value is always finite and inside `0.0..=1.0`; invalid input is
/// mapped to the top of the page instead of poisoning later comparisons.
pub(crate) fn sanitize_fraction(value: f32) -> f32 {
    if value.is_finite() {
        value.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

/// Resolves a stored `archive_path`/`cover_path` value against the library
/// root that owns it.
///
/// New rows store managed files as canonical forward-slash values
/// (`archives/<name>`, `covers/<name>`), so a library directory moved to a
/// new data container keeps working when it is reopened at the new root.
/// Absolute rows written by older builds and any other relative value keep
/// their original contract: they are returned literally and resolve against
/// the process working directory, exactly as the reader that wrote them did.
pub(crate) fn resolve_stored_path(root: &Path, stored: &str) -> PathBuf {
    if is_canonical_managed(stored) {
        let mut resolved = root.to_path_buf();
        for part in stored.split('/') {
            resolved.push(part);
        }
        return resolved;
    }
    PathBuf::from(stored)
}

/// True when `stored` is exactly `<dirname>/<file>` with a managed directory
/// name (`archives` or `covers`) and a single plain file component — the only
/// relative form this library joins to its root.
pub(crate) fn is_canonical_managed(stored: &str) -> bool {
    if stored.contains('\\') {
        return false;
    }
    let mut parts = stored.split('/');
    match parts.next() {
        Some("archives") | Some("covers") => {}
        _ => return false,
    }
    match parts.next() {
        Some(file) if !file.is_empty() && file != "." && file != ".." => {}
        _ => return false,
    }
    parts.next().is_none()
}

/// The stored form of a managed path: `<dirname>/<name>` joined with forward
/// slashes, where `dirname` is the managed directory's own name (`archives`
/// or `covers`). Returns `None` when `path` is not a plain file path directly
/// inside `directory`, so nested folders or unrelated roots are never
/// claimed.
pub(crate) fn managed_relative(path: &Path, directory: &Path) -> Option<String> {
    let parent = path.parent()?;
    if parent != directory {
        return None;
    }
    let file_name = path.file_name()?.to_string_lossy();
    if file_name.is_empty() {
        return None;
    }
    let directory_name = directory.file_name()?.to_string_lossy();
    Some(format!("{directory_name}/{file_name}"))
}
