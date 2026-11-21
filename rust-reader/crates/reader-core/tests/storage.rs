use std::fs::File;
use std::io::{Cursor, Write};
use std::path::Path;

use tempfile::TempDir;
use yejian_core::Library;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

fn sample_png(color: [u8; 3]) -> Vec<u8> {
    let image = image::RgbImage::from_pixel(4, 4, image::Rgb(color));
    let mut bytes = Vec::new();
    let mut cursor = Cursor::new(&mut bytes);
    image::DynamicImage::ImageRgb8(image)
        .write_to(&mut cursor, image::ImageFormat::Png)
        .expect("encode fixture PNG");
    bytes
}

fn write_cbz(path: &Path, entries: &[(&str, &[u8])]) {
    let file = File::create(path).expect("create fixture archive");
    let mut writer = ZipWriter::new(file);
    for (name, bytes) in entries {
        writer
            .start_file(
                *name,
                SimpleFileOptions::default().compression_method(CompressionMethod::Stored),
            )
            .expect("start fixture entry");
        writer.write_all(bytes).expect("write fixture entry");
    }
    writer.finish().expect("finish fixture archive");
}

fn file_size(path: &Path) -> u64 {
    std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}

#[test]
fn storage_report_splits_shelf_removed_and_covers() {
    let tmp = TempDir::new().unwrap();
    let library = Library::open(&tmp.path().join("library")).unwrap();
    let source = tmp.path().join("one.cbz");
    write_cbz(&source, &[("1.png", &sample_png([10, 0, 0]))]);
    let first = library.import(&source).unwrap();
    let source = tmp.path().join("two.cbz");
    write_cbz(&source, &[("1.png", &sample_png([0, 20, 0]))]);
    let second = library.import(&source).unwrap();
    library.remove_from_library(&second.id).unwrap();

    let report = library.storage_report().unwrap();
    assert_eq!(report.shelf_count, 1);
    assert_eq!(report.shelf_bytes, file_size(Path::new(&first.archive_path)));
    assert_eq!(report.removed_count, 1);
    assert_eq!(
        report.removed_bytes,
        file_size(Path::new(&second.archive_path))
    );
    assert!(report.cover_bytes > 0);
    assert!(report.cover_count >= 2);
    assert_eq!(
        report.total_bytes(),
        report.shelf_bytes + report.removed_bytes + report.cover_bytes
    );
}

#[test]
fn purge_book_deletes_files_and_keeps_row_and_progress() {
    let tmp = TempDir::new().unwrap();
    let library = Library::open(&tmp.path().join("library")).unwrap();
    let source = tmp.path().join("comic.cbz");
    write_cbz(
        &source,
        &[
            ("1.png", &sample_png([1, 0, 0])),
            ("2.png", &sample_png([2, 0, 0])),
        ],
    );
    let book = library.import(&source).unwrap();
    library.save_progress(&book.id, 1, false).unwrap();
    let archive = book.archive_path.clone();
    let cover = book.cover_path.clone().unwrap();
    assert!(Path::new(&archive).exists());
    assert!(Path::new(&cover).exists());

    library.purge_book(&book.id).unwrap();
    assert!(!Path::new(&archive).exists());
    assert!(!Path::new(&cover).exists());
    assert!(library.list_books("").unwrap().is_empty());

    // Re-importing the same source repairs the archive and keeps progress.
    let repaired = library.import(&source).unwrap();
    assert_eq!(repaired.id, book.id);
    assert!(Path::new(&repaired.archive_path).exists());
    assert_eq!(repaired.current_page, 1);
    assert_eq!(library.list_books("").unwrap().len(), 1);
}

#[test]
fn purge_removed_clears_only_removed_books() {
    let tmp = TempDir::new().unwrap();
    let library = Library::open(&tmp.path().join("library")).unwrap();
    let source = tmp.path().join("keep.cbz");
    write_cbz(&source, &[("1.png", &sample_png([3, 0, 0]))]);
    let kept = library.import(&source).unwrap();
    let source = tmp.path().join("gone.cbz");
    write_cbz(&source, &[("1.png", &sample_png([4, 0, 0]))]);
    let gone = library.import(&source).unwrap();
    library.remove_from_library(&gone.id).unwrap();

    assert_eq!(library.purge_removed().unwrap(), 1);
    assert!(!Path::new(&gone.archive_path).exists());
    assert!(Path::new(&kept.archive_path).exists());
    // Progress survives: re-import restores the row.
    let source = tmp.path().join("gone.cbz");
    let restored = library.import(&source).unwrap();
    assert_eq!(restored.id, gone.id);
}

#[test]
fn purge_book_rejects_paths_outside_the_library() {
    let tmp = TempDir::new().unwrap();
    let library = Library::open(&tmp.path().join("library")).unwrap();
    let source = tmp.path().join("comic.cbz");
    write_cbz(&source, &[("1.png", &sample_png([5, 0, 0]))]);
    let book = library.import(&source).unwrap();

    // Point the row at an external file; the purge must refuse and the
    // external file must survive.
    let outside = tmp.path().join("outside.cbz");
    write_cbz(&outside, &[("1.png", &sample_png([6, 0, 0]))]);
    let connection =
        rusqlite::Connection::open(library.root().join("library.sqlite3")).unwrap();
    connection
        .execute(
            "UPDATE books SET archive_path = ?1 WHERE id = ?2",
            rusqlite::params![outside.to_string_lossy().to_string(), book.id],
        )
        .unwrap();
    drop(connection);

    assert!(library.purge_book(&book.id).is_err());
    assert!(outside.exists());
    // The row stays on the shelf since nothing was deleted.
    assert_eq!(library.list_books("").unwrap().len(), 1);
}

#[cfg(unix)]
#[test]
fn purge_book_rejects_symlink_escapes() {
    use std::os::unix::fs::symlink;

    let tmp = TempDir::new().unwrap();
    let library = Library::open(&tmp.path().join("library")).unwrap();
    let source = tmp.path().join("comic.cbz");
    write_cbz(&source, &[("1.png", &sample_png([7, 0, 0]))]);
    let book = library.import(&source).unwrap();

    // Replace the managed archive with a symlink out of the library; the
    // canonicalised path escapes the root, so nothing may be deleted.
    let secret = tmp.path().join("secret.cbz");
    write_cbz(&secret, &[("1.png", &sample_png([8, 0, 0]))]);
    fs::remove_file(&book.archive_path).unwrap();
    symlink(&secret, &book.archive_path).unwrap();

    assert!(library.purge_book(&book.id).is_err());
    assert!(secret.exists());
    assert_eq!(library.list_books("").unwrap().len(), 1);
}

#[test]
fn purge_book_never_deletes_an_archive_shared_with_a_shelf_book() {
    let tmp = TempDir::new().unwrap();
    let library = Library::open(&tmp.path().join("library")).unwrap();
    let source = tmp.path().join("comic.cbz");
    write_cbz(&source, &[("1.png", &sample_png([9, 0, 0]))]);
    let book = library.import(&source).unwrap();

    // A second row pointing at the same archive while still on the shelf
    // must block the delete even though the target row is removed.
    let twin_id = "zzzzzzzzzzzzzzzz";
    let stored_archive: String = {
        let connection =
            rusqlite::Connection::open(library.root().join("library.sqlite3")).unwrap();
        let stored: String = connection
            .query_row(
                "SELECT archive_path FROM books WHERE id = ?1",
                rusqlite::params![book.id],
                |row| row.get(0),
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO books (id, title, author, archive_path, cover_path,
                                   page_count, current_page, page_fraction,
                                   updated_at, last_read_at, progress_at, category,
                                   content_hash, kind, removed)
                 VALUES (?1, 'twin', '', ?2, NULL, 1, 0, 0.0,
                         0, NULL, NULL, '未分类', ?3, 'archive', 0)",
                rusqlite::params![twin_id, stored, "other-hash"],
            )
            .unwrap();
        stored
    };
    let _ = stored_archive;

    library.remove_from_library(&book.id).unwrap();
    assert!(library.purge_book(&book.id).is_err());
    // The shared file is untouched because the live twin still needs it.
    assert!(Path::new(&book.archive_path).exists());
}
