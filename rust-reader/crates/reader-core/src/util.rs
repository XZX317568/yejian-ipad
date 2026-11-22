use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};

pub(crate) fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or(0)
}

pub(crate) fn unique_suffix() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    format!(
        "{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    )
}

pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

pub(crate) fn hash_file(path: &Path) -> Result<String> {
    let mut file =
        File::open(path).with_context(|| format!("open file for hashing {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .with_context(|| format!("read {} while hashing", path.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex::encode(hasher.finalize()))
}

pub(crate) fn copy_file_hashed(
    source: &Path,
    destination: &Path,
    hasher: &mut Sha256,
) -> Result<u64> {
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let mut input = File::open(source).with_context(|| format!("open {}", source.display()))?;
    let mut output =
        File::create(destination).with_context(|| format!("create {}", destination.display()))?;
    let mut total = 0u64;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = input
            .read(&mut buffer)
            .with_context(|| format!("read {}", source.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        output
            .write_all(&buffer[..read])
            .with_context(|| format!("write {}", destination.display()))?;
        total += read as u64;
    }
    output
        .sync_all()
        .with_context(|| format!("sync {}", destination.display()))?;
    Ok(total)
}

pub(crate) fn read_file_limited(path: &Path, maximum: u64) -> Result<Vec<u8>> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("read metadata for {}", path.display()))?;
    if metadata.file_type().is_symlink() {
        bail!("refusing to read symlink: {}", path.display());
    }
    if !metadata.is_file() {
        bail!("not a regular file: {}", path.display());
    }
    if metadata.len() > maximum {
        bail!(
            "file {} is {} bytes, above the {} byte limit",
            path.display(),
            metadata.len(),
            maximum
        );
    }
    let file = File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut data = Vec::with_capacity(metadata.len() as usize);
    file.take(maximum + 1)
        .read_to_end(&mut data)
        .with_context(|| format!("read {}", path.display()))?;
    if data.len() as u64 > maximum {
        bail!(
            "file {} is larger than the {} byte limit",
            path.display(),
            maximum
        );
    }
    Ok(data)
}

pub(crate) fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .with_context(|| format!("path has no parent: {}", path.display()))?;
    fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    let file_name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file".to_string());
    let temporary = parent.join(format!(".{file_name}.{}.tmp", unique_suffix()));

    let result = (|| -> Result<()> {
        let mut file =
            File::create(&temporary).with_context(|| format!("create {}", temporary.display()))?;
        file.write_all(bytes)
            .with_context(|| format!("write {}", temporary.display()))?;
        file.sync_all()
            .with_context(|| format!("sync {}", temporary.display()))?;
        if path.exists() {
            remove_path(path);
        }
        fs::rename(&temporary, path)
            .with_context(|| format!("rename {} to {}", temporary.display(), path.display()))?;
        Ok(())
    })();

    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

/// Joins a relative name without allowing absolute paths, drive prefixes or
/// parent traversal. Archive entry names are data, never output paths.
pub(crate) fn safe_join(root: &Path, relative: &str) -> Result<PathBuf> {
    let normalized = relative.replace('\\', "/");
    let path = Path::new(&normalized);
    if path.is_absolute() {
        bail!("unsafe absolute archive path: {relative}");
    }
    let mut output = root.to_path_buf();
    for component in path.components() {
        match component {
            Component::Normal(part) => {
                if part.is_empty() {
                    continue;
                }
                output.push(part);
            }
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                bail!("unsafe archive path: {relative}");
            }
        }
    }
    if output == root {
        bail!("archive path has no file name: {relative}");
    }
    Ok(output)
}

pub(crate) fn normalize_entry_name(name: &str) -> String {
    name.replace('\\', "/")
        .trim_start_matches("./")
        .trim_start_matches('/')
        .to_string()
}

pub(crate) fn is_hidden_entry(name: &str) -> bool {
    let normalized = normalize_entry_name(name);
    normalized
        .split('/')
        .any(|part| part.is_empty() || part == "__MACOSX" || part.starts_with('.'))
}

pub(crate) fn is_supported_image_name(name: &str) -> bool {
    let base = name.rsplit('/').next().unwrap_or(name).to_ascii_lowercase();
    let extension = base.rsplit_once('.').map(|(_, extension)| extension);
    matches!(extension, Some("png" | "jpg" | "jpeg" | "webp"))
}

pub(crate) fn ensure_supported_image(bytes: &[u8]) -> Result<()> {
    let png = bytes.starts_with(b"\x89PNG\r\n\x1a\n");
    let jpeg = bytes.starts_with(&[0xff, 0xd8, 0xff]);
    let webp = bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP";
    if png || jpeg || webp {
        Ok(())
    } else {
        bail!("damaged page: expected PNG, JPEG or WebP image data")
    }
}

pub(crate) fn validate_book_id(id: &str) -> Result<()> {
    if id.len() == 64
        && id
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        Ok(())
    } else {
        bail!("invalid book id in backup: {id}")
    }
}

pub(crate) fn remove_path(path: &Path) {
    if path.is_dir() {
        let _ = fs::remove_dir_all(path);
    } else {
        let _ = fs::remove_file(path);
    }
}

pub(crate) fn walk_files(root: &Path) -> Result<Vec<(PathBuf, String)>> {
    fn visit(root: &Path, directory: &Path, output: &mut Vec<(PathBuf, String)>) -> Result<()> {
        let mut entries = fs::read_dir(directory)
            .with_context(|| format!("read directory {}", directory.display()))?
            .collect::<std::io::Result<Vec<_>>>()
            .with_context(|| format!("list directory {}", directory.display()))?;
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let file_type = entry
                .file_type()
                .with_context(|| format!("inspect {}", entry.path().display()))?;
            if file_type.is_symlink() {
                continue;
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
                output.push((path, relative));
            }
        }
        Ok(())
    }

    if !root.is_dir() {
        bail!("image folder is missing: {}", root.display());
    }
    let mut output = Vec::new();
    visit(root, root, &mut output)?;
    Ok(output)
}
