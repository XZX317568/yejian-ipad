use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{Cursor, Read};
use std::path::Path;

use anyhow::{Context, Result, bail};
use image::codecs::jpeg::JpegEncoder;
use image::{DynamicImage, ImageReader};
use rusqlite::OptionalExtension;
use sha2::{Digest, Sha256};
use zip::ZipArchive;

use crate::Library;
use crate::db;
use crate::rar::{read_rar_page, scan_rar};
use crate::sort::natural_cmp;
use crate::util::{
    copy_file_hashed, ensure_supported_image, is_hidden_entry, is_supported_image_name,
    normalize_entry_name, read_file_limited, safe_join,
};

/// Largest encoded page returned by `page_bytes`.
pub const MAX_PAGE_BYTES: u64 = 64 * 1024 * 1024;
/// Largest decoded cover source considered during import.
pub const MAX_COVER_BYTES: u64 = 32 * 1024 * 1024;
/// Largest pixel count accepted by the cover decoder.
pub const MAX_IMAGE_PIXELS: u64 = 120_000_000;
/// Largest number of archive entries inspected during import.
pub const MAX_ARCHIVE_ENTRIES: usize = 20_000;

const COVER_MAX_WIDTH: u32 = 480;
const COVER_MAX_HEIGHT: u32 = 720;
const COVER_CANDIDATES: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SourceKind {
    Zip,
    Tar,
    Rar,
    Directory,
}

impl SourceKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Zip => "zip",
            Self::Tar => "tar",
            Self::Rar => "rar",
            Self::Directory => "directory",
        }
    }

    pub(crate) fn from_db(value: &str) -> Result<Self> {
        match value {
            "zip" => Ok(Self::Zip),
            "tar" => Ok(Self::Tar),
            "rar" | "cbr" => Ok(Self::Rar),
            "directory" => Ok(Self::Directory),
            other => bail!("unknown archive kind in library: {other}"),
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ScannedPage {
    pub entry_index: usize,
    pub entry_name: String,
    pub size: u64,
    pub crc32: Option<u32>,
}

#[derive(Debug, Clone)]
pub(crate) struct DirImage {
    pub source: std::path::PathBuf,
    pub relative: String,
    pub size: u64,
}

pub(crate) fn scan_source(kind: SourceKind, path: &Path) -> Result<Vec<ScannedPage>> {
    match kind {
        SourceKind::Zip => scan_zip(path),
        SourceKind::Tar => scan_tar(path),
        SourceKind::Rar => scan_rar(path),
        SourceKind::Directory => scan_directory(path),
    }
}

fn scan_zip(path: &Path) -> Result<Vec<ScannedPage>> {
    let file = File::open(path).with_context(|| format!("open ZIP archive {}", path.display()))?;
    let mut archive =
        ZipArchive::new(file).with_context(|| format!("read ZIP archive {}", path.display()))?;
    if archive.len() > MAX_ARCHIVE_ENTRIES {
        bail!(
            "ZIP archive {} has {} entries, above the {} entry limit",
            path.display(),
            archive.len(),
            MAX_ARCHIVE_ENTRIES
        );
    }

    let mut pages = Vec::new();
    for index in 0..archive.len() {
        let entry = archive
            .by_index(index)
            .with_context(|| format!("read ZIP entry {index} in {}", path.display()))?;
        if entry.is_dir() || !entry.is_file() || entry.is_symlink() {
            continue;
        }
        let name = normalize_entry_name(entry.name());
        if is_hidden_entry(&name) || !is_supported_image_name(&name) {
            continue;
        }
        let size = entry.size();
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
            crc32: Some(entry.crc32()),
        });
    }

    pages.sort_by(|left, right| natural_cmp(&left.entry_name, &right.entry_name));
    if pages.is_empty() {
        bail!("no PNG, JPEG or WebP pages found in {}", path.display());
    }
    Ok(pages)
}

fn scan_tar(path: &Path) -> Result<Vec<ScannedPage>> {
    let file = File::open(path).with_context(|| format!("open TAR archive {}", path.display()))?;
    let mut archive = tar::Archive::new(file);
    let entries = archive
        .entries()
        .with_context(|| format!("read TAR archive {}", path.display()))?;

    let mut pages = Vec::new();
    for (index, entry) in entries.enumerate() {
        if index >= MAX_ARCHIVE_ENTRIES {
            bail!(
                "TAR archive {} has more than {} entries",
                path.display(),
                MAX_ARCHIVE_ENTRIES
            );
        }
        let entry = entry.with_context(|| format!("read TAR entry {index}"))?;
        if !entry.header().entry_type().is_file() {
            continue;
        }
        let entry_path = entry
            .path()
            .with_context(|| format!("read TAR entry name {index}"))?;
        let name = normalize_entry_name(&entry_path.to_string_lossy());
        if is_hidden_entry(&name) || !is_supported_image_name(&name) {
            continue;
        }
        let size = entry.size();
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
            crc32: None,
        });
    }

    pages.sort_by(|left, right| natural_cmp(&left.entry_name, &right.entry_name));
    if pages.is_empty() {
        bail!("no PNG, JPEG or WebP pages found in {}", path.display());
    }
    Ok(pages)
}

fn scan_directory(path: &Path) -> Result<Vec<ScannedPage>> {
    let images = collect_directory_images(path)?;
    let mut pages = images
        .into_iter()
        .enumerate()
        .map(|(index, image)| ScannedPage {
            entry_index: index,
            entry_name: image.relative,
            size: image.size,
            crc32: None,
        })
        .collect::<Vec<_>>();
    pages.sort_by(|left, right| natural_cmp(&left.entry_name, &right.entry_name));
    for (index, page) in pages.iter_mut().enumerate() {
        page.entry_index = index;
    }
    if pages.is_empty() {
        bail!("no PNG, JPEG or WebP pages found in {}", path.display());
    }
    Ok(pages)
}

pub(crate) fn collect_directory_images(root: &Path) -> Result<Vec<DirImage>> {
    fn visit(root: &Path, directory: &Path, output: &mut Vec<DirImage>) -> Result<()> {
        let mut entries = fs::read_dir(directory)
            .with_context(|| format!("read image folder {}", directory.display()))?
            .collect::<std::io::Result<Vec<_>>>()
            .with_context(|| format!("list image folder {}", directory.display()))?;
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') || name == "__MACOSX" {
                continue;
            }
            let file_type = entry
                .file_type()
                .with_context(|| format!("inspect {}", entry.path().display()))?;
            if file_type.is_symlink() {
                bail!(
                    "image folder contains a symlink, which is not imported: {}",
                    entry.path().display()
                );
            }
            let path = entry.path();
            if file_type.is_dir() {
                visit(root, &path, output)?;
            } else if file_type.is_file() {
                let relative = path
                    .strip_prefix(root)
                    .with_context(|| format!("relative path for {}", path.display()))?
                    .to_string_lossy()
                    .replace('\\', "/");
                if !is_supported_image_name(&relative) {
                    continue;
                }
                let size = entry
                    .metadata()
                    .with_context(|| format!("read size of {}", path.display()))?
                    .len();
                if size == 0 {
                    continue;
                }
                if size > MAX_PAGE_BYTES {
                    bail!(
                        "page {} is {} bytes, above the {} byte page limit",
                        relative,
                        size,
                        MAX_PAGE_BYTES
                    );
                }
                output.push(DirImage {
                    source: path,
                    relative,
                    size,
                });
            }
        }
        Ok(())
    }

    if !root.is_dir() {
        bail!("image folder is missing: {}", root.display());
    }
    let mut output = Vec::new();
    visit(root, root, &mut output)?;
    output.sort_by(|left, right| natural_cmp(&left.relative, &right.relative));
    if output.is_empty() {
        bail!("no PNG, JPEG or WebP pages found in {}", root.display());
    }
    if output.len() > MAX_ARCHIVE_ENTRIES {
        bail!(
            "image folder has {} pages, above the {} page limit",
            output.len(),
            MAX_ARCHIVE_ENTRIES
        );
    }
    Ok(output)
}

pub(crate) fn copy_directory_payload(
    root: &Path,
    destination: &Path,
    hasher: &mut Sha256,
) -> Result<()> {
    let images = collect_directory_images(root)?;
    hasher.update(b"directory\0");
    for image in images {
        hasher.update(image.relative.as_bytes());
        hasher.update(image.size.to_le_bytes());
        let target = safe_join(destination, &image.relative)?;
        copy_file_hashed(&image.source, &target, hasher)?;
    }
    Ok(())
}

/// Computes the same content hash as `copy_directory_payload` without copying.
pub(crate) fn hash_directory_payload(root: &Path) -> Result<String> {
    let images = collect_directory_images(root)?;
    let mut hasher = Sha256::new();
    hasher.update(b"directory\0");
    for image in images {
        hasher.update(image.relative.as_bytes());
        hasher.update(image.size.to_le_bytes());
        let bytes = read_file_limited(&image.source, MAX_PAGE_BYTES)?;
        if bytes.len() as u64 != image.size {
            bail!(
                "image folder changed while reading {}",
                image.source.display()
            );
        }
        hasher.update(&bytes);
    }
    Ok(hex::encode(hasher.finalize()))
}

pub(crate) fn read_source_page(
    kind: SourceKind,
    path: &Path,
    page: &ScannedPage,
    maximum: u64,
) -> Result<Vec<u8>> {
    match kind {
        SourceKind::Zip => read_zip_page(path, page, maximum),
        SourceKind::Tar => read_tar_page(path, page, maximum),
        SourceKind::Rar => read_rar_page(path, page, maximum),
        SourceKind::Directory => read_directory_page(path, page, maximum),
    }
}

fn read_zip_page(path: &Path, page: &ScannedPage, maximum: u64) -> Result<Vec<u8>> {
    let file = File::open(path)
        .with_context(|| format!("book archive is missing or unreadable: {}", path.display()))?;
    let mut archive = ZipArchive::new(file)
        .with_context(|| format!("book archive is damaged: {}", path.display()))?;
    let entry = archive.by_index(page.entry_index).with_context(|| {
        format!(
            "page index {} is missing from {}",
            page.entry_index,
            path.display()
        )
    })?;
    if normalize_entry_name(entry.name()) != page.entry_name {
        bail!(
            "book archive changed: page {} no longer matches its index",
            page.entry_name
        );
    }
    if entry.size() != page.size {
        bail!("damaged page {}: archive size changed", page.entry_name);
    }
    if page.size > maximum {
        bail!(
            "damaged page {}: {} bytes is above the {} byte limit",
            page.entry_name,
            page.size,
            maximum
        );
    }

    let mut data = Vec::with_capacity(page.size as usize);
    entry
        .take(maximum + 1)
        .read_to_end(&mut data)
        .with_context(|| format!("decompress page {}", page.entry_name))?;
    if data.len() as u64 != page.size {
        bail!(
            "damaged page {}: expected {} bytes, got {}",
            page.entry_name,
            page.size,
            data.len()
        );
    }
    if let Some(expected) = page.crc32 {
        let actual = crc32fast::hash(&data);
        if actual != expected {
            bail!(
                "damaged page {}: CRC mismatch (expected {expected:08x}, got {actual:08x})",
                page.entry_name
            );
        }
    }
    Ok(data)
}

fn read_tar_page(path: &Path, page: &ScannedPage, maximum: u64) -> Result<Vec<u8>> {
    let file = File::open(path)
        .with_context(|| format!("book archive is missing or unreadable: {}", path.display()))?;
    let mut archive = tar::Archive::new(file);
    let entries = archive
        .entries()
        .with_context(|| format!("book archive is damaged: {}", path.display()))?;
    for (index, entry) in entries.enumerate() {
        let entry = entry.with_context(|| format!("read TAR entry {index}"))?;
        if index != page.entry_index {
            continue;
        }
        let entry_path = entry
            .path()
            .with_context(|| format!("read TAR page name {}", page.entry_name))?;
        if normalize_entry_name(&entry_path.to_string_lossy()) != page.entry_name {
            bail!(
                "book archive changed: page {} no longer matches its index",
                page.entry_name
            );
        }
        if entry.size() != page.size {
            bail!("damaged page {}: archive size changed", page.entry_name);
        }
        if page.size > maximum {
            bail!(
                "damaged page {}: {} bytes is above the {} byte limit",
                page.entry_name,
                page.size,
                maximum
            );
        }
        let mut data = Vec::with_capacity(page.size as usize);
        entry
            .take(maximum + 1)
            .read_to_end(&mut data)
            .with_context(|| format!("read page {}", page.entry_name))?;
        if data.len() as u64 != page.size {
            bail!(
                "damaged page {}: expected {} bytes, got {}",
                page.entry_name,
                page.size,
                data.len()
            );
        }
        return Ok(data);
    }
    bail!(
        "page index {} is missing from {}",
        page.entry_index,
        path.display()
    )
}

fn read_directory_page(path: &Path, page: &ScannedPage, maximum: u64) -> Result<Vec<u8>> {
    let page_path = safe_join(path, &page.entry_name)?;
    let metadata = fs::symlink_metadata(&page_path)
        .with_context(|| format!("book page is missing: {}", page_path.display()))?;
    if metadata.file_type().is_symlink() {
        bail!("refusing to read symlink page: {}", page_path.display());
    }
    if metadata.len() != page.size {
        bail!("damaged page {}: file size changed", page.entry_name);
    }
    read_file_limited(&page_path, maximum)
}

pub(crate) fn build_cover(kind: SourceKind, path: &Path, pages: &[ScannedPage]) -> Result<Vec<u8>> {
    let mut ordered = Vec::with_capacity(pages.len());
    ordered.extend(
        pages
            .iter()
            .filter(|page| page.entry_name.to_ascii_lowercase().contains("cover")),
    );
    ordered.extend(
        pages
            .iter()
            .filter(|page| !page.entry_name.to_ascii_lowercase().contains("cover")),
    );

    let mut seen = HashSet::new();
    for page in ordered.into_iter().take(COVER_CANDIDATES) {
        if !seen.insert(page.entry_index) {
            continue;
        }
        if let Ok(bytes) = read_source_page(kind, path, page, MAX_COVER_BYTES) {
            if let Ok(thumbnail) = thumbnail_jpeg(&bytes) {
                return Ok(thumbnail);
            }
        }
    }
    bail!(
        "no decodable PNG, JPEG or WebP cover image was found in {}",
        path.display()
    )
}

pub(crate) fn thumbnail_jpeg(bytes: &[u8]) -> Result<Vec<u8>> {
    ensure_supported_image(bytes)?;
    let dimensions = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .context("guess cover image format")?
        .into_dimensions()
        .context("read cover image dimensions")?;
    if dimensions.0 == 0 || dimensions.1 == 0 {
        bail!("cover image is empty");
    }
    let pixels = u64::from(dimensions.0) * u64::from(dimensions.1);
    if pixels > MAX_IMAGE_PIXELS {
        bail!(
            "cover image is {}x{}, above the {} pixel limit",
            dimensions.0,
            dimensions.1,
            MAX_IMAGE_PIXELS
        );
    }

    let image = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .context("guess cover image format")?
        .decode()
        .context("decode cover image")?;
    let thumbnail = image.thumbnail(COVER_MAX_WIDTH, COVER_MAX_HEIGHT).to_rgb8();
    let mut output = Vec::new();
    JpegEncoder::new_with_quality(&mut output, 82)
        .encode_image(&DynamicImage::ImageRgb8(thumbnail))
        .context("encode cover thumbnail")?;
    Ok(output)
}

impl Library {
    /// Returns the encoded page bytes for a visible book.
    pub fn page_bytes(&self, book_id: &str, page: usize) -> Result<Vec<u8>> {
        let stored = {
            let connection = self.lock_db()?;
            db::require_book(&connection, &self.root, book_id, false)?
        };
        if page >= stored.book.page_count {
            bail!(
                "page {page} is out of range for a {}-page book",
                stored.book.page_count
            );
        }

        let page_row = {
            let connection = self.lock_db()?;
            connection
                .query_row(
                    "SELECT entry_index, entry_name, size, crc32
                     FROM pages WHERE book_id = ?1 AND page_index = ?2",
                    rusqlite::params![book_id, page as i64],
                    |row| {
                        Ok((
                            row.get::<_, i64>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, i64>(2)?,
                            row.get::<_, Option<i64>>(3)?,
                        ))
                    },
                )
                .optional()
                .context("read page index")?
                .ok_or_else(|| anyhow::anyhow!("page {page} is not indexed for book {book_id}"))?
        };

        let scanned = ScannedPage {
            entry_index: page_row.0.max(0) as usize,
            entry_name: page_row.1,
            size: page_row.2.max(0) as u64,
            crc32: page_row.3.map(|value| value as u32),
        };
        let kind = SourceKind::from_db(&stored.kind)?;
        let bytes = if kind == SourceKind::Rar {
            match self
                .rar_cache
                .page(book_id, &stored.book.archive_path, &scanned)?
            {
                Some(bytes) => bytes,
                None => {
                    // The whole-chapter cache is an optimization. A damaged
                    // unrelated member must not make a readable page fail, so
                    // a cache failure falls through to one-page extraction.
                    let _ = self.rar_cache.load(book_id, &stored.book.archive_path);
                    match self
                        .rar_cache
                        .page(book_id, &stored.book.archive_path, &scanned)?
                    {
                        Some(bytes) => bytes,
                        None => read_source_page(
                            kind,
                            &stored.book.archive_path,
                            &scanned,
                            MAX_PAGE_BYTES,
                        )?,
                    }
                }
            }
        } else {
            read_source_page(kind, &stored.book.archive_path, &scanned, MAX_PAGE_BYTES)?
        };
        ensure_supported_image(&bytes)?;
        Ok(bytes)
    }
}
