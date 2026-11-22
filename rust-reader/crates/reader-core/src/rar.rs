//! RAR/CBR page reading with a bounded in-memory chapter cache.
//!
//! RAR members are only ever decoded into memory. Nothing is written to the
//! filesystem from an archive name, so a hostile `../` entry has no path to
//! escape into. Every page and the whole cache have explicit byte limits.

use std::io::{self, Write};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use anyhow::{Result, bail};
use rars::{ArchiveMemberDetail, ArchiveReader};

use crate::pages::{MAX_ARCHIVE_ENTRIES, MAX_PAGE_BYTES, ScannedPage};
use crate::sort::natural_cmp;
use crate::util::{is_hidden_entry, is_supported_image_name, normalize_entry_name};

/// Largest total amount of decoded page bytes kept for one book.
pub(crate) const RAR_CACHE_MAX_BYTES: u64 = 128 * 1024 * 1024;
/// Largest number of pages kept in one chapter cache.
pub(crate) const RAR_CACHE_MAX_PAGES: usize = 2048;

pub(crate) fn scan_rar(path: &Path) -> Result<Vec<ScannedPage>> {
    let archive = ArchiveReader::read_path(path)
        .map_err(|error| anyhow::anyhow!("read RAR archive {}: {error}", path.display()))?;
    let mut pages = Vec::new();
    let mut members = 0usize;
    for (index, member) in archive.members().enumerate() {
        members += 1;
        if members > MAX_ARCHIVE_ENTRIES {
            bail!(
                "RAR archive {} has more than {} entries",
                path.display(),
                MAX_ARCHIVE_ENTRIES
            );
        }
        if member.meta.is_directory {
            continue;
        }
        reject_unsupported_member(&member.meta, path)?;
        let name = normalize_entry_name(&member.meta.name_lossy());
        if is_hidden_entry(&name) || !is_supported_image_name(&name) {
            continue;
        }
        let size = member.meta.unpacked_size;
        if size == 0 {
            continue;
        }
        if size > MAX_PAGE_BYTES {
            bail!(
                "page {} is {} bytes, above the {} byte page limit",
                name,
                size,
                MAX_PAGE_BYTES
            );
        }
        pages.push(ScannedPage {
            entry_index: index,
            entry_name: name,
            size,
            crc32: member_crc32(&member.detail),
        });
    }

    pages.sort_by(|left, right| natural_cmp(&left.entry_name, &right.entry_name));
    if pages.is_empty() {
        bail!(
            "no PNG, JPEG or WebP pages found in RAR archive {}",
            path.display()
        );
    }
    Ok(pages)
}

pub(crate) fn read_rar_page(path: &Path, page: &ScannedPage, maximum: u64) -> Result<Vec<u8>> {
    if page.size > maximum {
        bail!(
            "page {} is {} bytes, above the {} byte limit",
            page.entry_name,
            page.size,
            maximum
        );
    }
    if !path.is_file() {
        bail!("book archive is missing or unreadable: {}", path.display());
    }
    let archive = ArchiveReader::read_path(path)
        .map_err(|error| anyhow::anyhow!("read RAR archive {}: {error}", path.display()))?;
    let budget = Arc::new(Mutex::new(Budget {
        total: 0,
        total_limit: maximum,
        page_limit: maximum as usize,
    }));
    let mut index = 0usize;
    let mut found: Option<(String, Arc<Mutex<Vec<u8>>>)> = None;

    archive
        .extract_to(None, |meta| {
            let current = index;
            index += 1;
            if current != page.entry_index || meta.is_directory {
                return Ok(sink());
            }
            let name = normalize_entry_name(&meta.name_lossy());
            let buffer = Arc::new(Mutex::new(Vec::new()));
            found = Some((name, Arc::clone(&buffer)));
            Ok(Box::new(CappedWriter {
                buffer,
                budget: Arc::clone(&budget),
            }) as Box<dyn Write>)
        })
        .map_err(|error| {
            anyhow::anyhow!(
                "extract page {} from RAR archive {}: {error}",
                page.entry_name,
                path.display()
            )
        })?;

    let Some((name, buffer)) = found else {
        bail!(
            "page index {} is missing from RAR archive {}",
            page.entry_index,
            path.display()
        );
    };
    if name != page.entry_name {
        bail!(
            "book archive changed: page {} no longer matches its index",
            page.entry_name
        );
    }
    let bytes = take_buffer(buffer)?;
    validate_page_bytes(page, &bytes, maximum)?;
    Ok(bytes)
}

/// Keeps decoded pages for the most recently read RAR book.
#[derive(Default)]
pub(crate) struct RarPageCache {
    inner: Mutex<Option<CachedBook>>,
}

struct CachedBook {
    book_id: String,
    identity: CacheIdentity,
    /// `None` means the archive was checked and is too large to cache.
    pages: Option<Vec<CachedPage>>,
}

struct CachedPage {
    entry_index: usize,
    entry_name: String,
    bytes: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CacheIdentity {
    len: u64,
    modified: Option<SystemTime>,
}

impl CacheIdentity {
    fn read(path: &Path) -> Option<Self> {
        let metadata = std::fs::metadata(path).ok()?;
        Some(Self {
            len: metadata.len(),
            modified: metadata.modified().ok(),
        })
    }
}

impl RarPageCache {
    /// Returns a cached page, or `None` when the book is not cached.
    pub(crate) fn page(
        &self,
        book_id: &str,
        path: &Path,
        page: &ScannedPage,
    ) -> Result<Option<Vec<u8>>> {
        let Some(identity) = CacheIdentity::read(path) else {
            return Ok(None);
        };
        let mut guard = self
            .inner
            .lock()
            .map_err(|_| anyhow::anyhow!("RAR page cache lock poisoned"))?;
        let Some(cached) = guard.as_ref() else {
            return Ok(None);
        };
        if cached.book_id != book_id || cached.identity != identity {
            *guard = None;
            return Ok(None);
        }
        let Some(bytes) = cached
            .pages
            .as_ref()
            .and_then(|pages| find_page(pages, page))
        else {
            return Ok(None);
        };
        // Cached bytes came from the same archive identity, but validate them
        // against the indexed size and CRC before handing a page to the UI.
        validate_page_bytes(page, &bytes, MAX_PAGE_BYTES)?;
        Ok(Some(bytes))
    }

    /// Ensures the cache has been checked for `book_id`.
    ///
    /// Archives above either cache limit keep a negative entry, so later page
    /// reads do not retry a full extraction on every page.
    pub(crate) fn load(&self, book_id: &str, path: &Path) -> Result<()> {
        let Some(identity) = CacheIdentity::read(path) else {
            return Ok(());
        };
        {
            let guard = self
                .inner
                .lock()
                .map_err(|_| anyhow::anyhow!("RAR page cache lock poisoned"))?;
            if guard
                .as_ref()
                .is_some_and(|cached| cached.book_id == book_id && cached.identity == identity)
            {
                return Ok(());
            }
        }

        let pages = extract_all_rar_pages(path)?;
        let mut guard = self
            .inner
            .lock()
            .map_err(|_| anyhow::anyhow!("RAR page cache lock poisoned"))?;
        *guard = Some(CachedBook {
            book_id: book_id.to_string(),
            identity,
            pages,
        });
        Ok(())
    }
}

struct Budget {
    total: u64,
    total_limit: u64,
    page_limit: usize,
}

struct CappedWriter {
    buffer: Arc<Mutex<Vec<u8>>>,
    budget: Arc<Mutex<Budget>>,
}

impl Write for CappedWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let mut buffer = self
            .buffer
            .lock()
            .map_err(|_| io::Error::other("RAR page buffer lock poisoned"))?;
        let mut budget = self
            .budget
            .lock()
            .map_err(|_| io::Error::other("RAR page budget lock poisoned"))?;
        if buffer.len().saturating_add(bytes.len()) > budget.page_limit {
            return Err(io::Error::other("RAR page exceeds the page size limit"));
        }
        if budget.total.saturating_add(bytes.len() as u64) > budget.total_limit {
            return Err(io::Error::other(
                "RAR cache exceeds the chapter cache size limit",
            ));
        }
        budget.total += bytes.len() as u64;
        buffer.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn extract_all_rar_pages(path: &Path) -> Result<Option<Vec<CachedPage>>> {
    let archive = ArchiveReader::read_path(path)
        .map_err(|error| anyhow::anyhow!("read RAR archive {}: {error}", path.display()))?;

    let mut total = 0u64;
    let mut count = 0usize;
    let mut members = 0usize;
    for member in archive.members() {
        members += 1;
        if members > MAX_ARCHIVE_ENTRIES {
            bail!(
                "RAR archive {} has more than {} entries",
                path.display(),
                MAX_ARCHIVE_ENTRIES
            );
        }
        if member.meta.is_directory {
            continue;
        }
        reject_unsupported_member(&member.meta, path)?;
        let name = normalize_entry_name(&member.meta.name_lossy());
        if is_hidden_entry(&name) || !is_supported_image_name(&name) {
            continue;
        }
        total = total.saturating_add(member.meta.unpacked_size);
        count += 1;
    }
    if count == 0 {
        return Ok(None);
    }
    if count > RAR_CACHE_MAX_PAGES || total > RAR_CACHE_MAX_BYTES {
        return Ok(None);
    }

    let budget = Arc::new(Mutex::new(Budget {
        total: 0,
        total_limit: RAR_CACHE_MAX_BYTES,
        page_limit: MAX_PAGE_BYTES as usize,
    }));
    let mut collected: Vec<(usize, String, Arc<Mutex<Vec<u8>>>)> = Vec::new();
    let mut index = 0usize;
    let extraction = archive.extract_to(None, |meta| {
        let current = index;
        index += 1;
        if meta.is_directory {
            return Ok(sink());
        }
        let name = normalize_entry_name(&meta.name_lossy());
        if is_hidden_entry(&name) || !is_supported_image_name(&name) {
            return Ok(sink());
        }
        let buffer = Arc::new(Mutex::new(Vec::new()));
        collected.push((current, name, Arc::clone(&buffer)));
        Ok(Box::new(CappedWriter {
            buffer,
            budget: Arc::clone(&budget),
        }) as Box<dyn Write>)
    });
    if let Err(error) = extraction {
        if is_cache_limit_error(&error) {
            return Ok(None);
        }
        return Err(anyhow::anyhow!(
            "extract RAR archive {}: {error}",
            path.display()
        ));
    }

    let mut pages = Vec::with_capacity(collected.len());
    for (entry_index, entry_name, buffer) in collected {
        let bytes = take_buffer(buffer)?;
        if bytes.is_empty() {
            continue;
        }
        pages.push(CachedPage {
            entry_index,
            entry_name,
            bytes,
        });
    }
    if pages.is_empty() {
        return Ok(None);
    }
    pages.sort_by(|left, right| natural_cmp(&left.entry_name, &right.entry_name));
    Ok(Some(pages))
}

fn find_page(pages: &[CachedPage], page: &ScannedPage) -> Option<Vec<u8>> {
    pages
        .iter()
        .find(|cached| {
            cached.entry_index == page.entry_index && cached.entry_name == page.entry_name
        })
        .or_else(|| {
            pages
                .iter()
                .find(|cached| cached.entry_name == page.entry_name)
        })
        .map(|cached| cached.bytes.clone())
}

fn take_buffer(buffer: Arc<Mutex<Vec<u8>>>) -> Result<Vec<u8>> {
    match Arc::try_unwrap(buffer) {
        Ok(mutex) => mutex
            .into_inner()
            .map_err(|_| anyhow::anyhow!("RAR page buffer lock poisoned")),
        Err(shared) => Ok(shared
            .lock()
            .map_err(|_| anyhow::anyhow!("RAR page buffer lock poisoned"))?
            .clone()),
    }
}

fn validate_page_bytes(page: &ScannedPage, bytes: &[u8], maximum: u64) -> Result<()> {
    if bytes.len() as u64 != page.size {
        bail!(
            "damaged page {}: expected {} bytes, got {}",
            page.entry_name,
            page.size,
            bytes.len()
        );
    }
    if bytes.len() as u64 > maximum {
        bail!(
            "damaged page {}: {} bytes is above the {} byte limit",
            page.entry_name,
            bytes.len(),
            maximum
        );
    }
    if let Some(expected) = page.crc32 {
        let actual = crc32fast::hash(bytes);
        if actual != expected {
            bail!(
                "damaged page {}: CRC mismatch (expected {expected:08x}, got {actual:08x})",
                page.entry_name
            );
        }
    }
    Ok(())
}

fn member_crc32(detail: &ArchiveMemberDetail) -> Option<u32> {
    match detail {
        ArchiveMemberDetail::Rar15To40 { crc32, .. } => Some(*crc32),
        ArchiveMemberDetail::Rar50Plus { crc32, .. } => *crc32,
        _ => None,
    }
}

fn reject_unsupported_member(meta: &rars::ArchiveMemberMeta, path: &Path) -> Result<()> {
    if meta.is_encrypted {
        bail!(
            "encrypted RAR archives are not supported: {}",
            path.display()
        );
    }
    if meta.is_split_before || meta.is_split_after {
        bail!(
            "multi-volume or split RAR archives are not supported: {}",
            path.display()
        );
    }
    Ok(())
}

fn is_cache_limit_error(error: &rars::Error) -> bool {
    let message = error.to_string().to_ascii_lowercase();
    message.contains("rar page exceeds") || message.contains("rar cache exceeds")
}

fn sink() -> Box<dyn Write> {
    Box::new(io::sink())
}
