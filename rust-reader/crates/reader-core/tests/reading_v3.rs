use std::fs::{self, File};
use std::io::{Cursor, Write};
use std::path::Path;

use tempfile::TempDir;
use yejian_core::{Book, ChapterMetadata, Library};
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

fn write_cbz(path: &Path, pages: usize, color: [u8; 3]) {
    let file = File::create(path).expect("create fixture archive");
    let mut writer = ZipWriter::new(file);
    let page = sample_png(color);
    for index in 0..pages {
        writer
            .start_file(
                format!("{}.png", index + 1),
                SimpleFileOptions::default().compression_method(CompressionMethod::Stored),
            )
            .expect("start fixture entry");
        writer.write_all(&page).expect("write fixture entry");
    }
    writer.finish().expect("finish fixture archive");
}

fn open(tmp: &TempDir, name: &str) -> Library {
    Library::open(&tmp.path().join(name)).expect("open fixture library")
}

fn import_book(library: &Library, tmp: &TempDir, name: &str, color: [u8; 3]) -> Book {
    let archive = tmp.path().join(format!("{name}.cbz"));
    write_cbz(&archive, 3, color);
    library.import(&archive).expect("import fixture book")
}

fn metadata(
    source: &str,
    chapter_id: &str,
    chapter_title: &str,
    chapter_number: Option<f64>,
    volume_number: Option<f64>,
) -> ChapterMetadata {
    ChapterMetadata {
        source_id: source.to_string(),
        manga_id: "manga-1".to_string(),
        manga_title: "同作品".to_string(),
        chapter_id: chapter_id.to_string(),
        chapter_title: chapter_title.to_string(),
        chapter_number,
        volume_number,
    }
}

#[test]
fn old_libraries_upgrade_with_zero_fraction() {
    let tmp = TempDir::new().unwrap();
    let v1 = tmp.path().join("v1");
    fs::create_dir_all(&v1).unwrap();
    {
        let connection = rusqlite::Connection::open(v1.join("library.sqlite3")).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE books (
                     id TEXT PRIMARY KEY,
                     title TEXT NOT NULL,
                     author TEXT NOT NULL DEFAULT '',
                     archive_path TEXT NOT NULL,
                     cover_path TEXT,
                     page_count INTEGER NOT NULL,
                     current_page INTEGER NOT NULL DEFAULT 0,
                     updated_at INTEGER NOT NULL,
                     last_read_at INTEGER,
                     category TEXT NOT NULL,
                     content_hash TEXT NOT NULL UNIQUE,
                     kind TEXT NOT NULL,
                     removed INTEGER NOT NULL DEFAULT 0
                 );
                 PRAGMA user_version = 1;",
            )
            .unwrap();
        let id = "a".repeat(64);
        connection
            .execute(
                "INSERT INTO books(
                     id, title, author, archive_path, cover_path, page_count, current_page,
                     updated_at, last_read_at, category, content_hash, kind, removed
                 ) VALUES(?1, '旧书', '', '/missing/old.cbz', NULL, 5, 3, 1000, NULL, '未分类', ?1, 'cbz', 0)",
                rusqlite::params![id],
            )
            .unwrap();
    }

    let v2 = tmp.path().join("v2");
    fs::create_dir_all(&v2).unwrap();
    {
        let connection = rusqlite::Connection::open(v2.join("library.sqlite3")).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE books (
                     id TEXT PRIMARY KEY,
                     title TEXT NOT NULL,
                     author TEXT NOT NULL DEFAULT '',
                     archive_path TEXT NOT NULL,
                     cover_path TEXT,
                     page_count INTEGER NOT NULL,
                     current_page INTEGER NOT NULL DEFAULT 0,
                     updated_at INTEGER NOT NULL,
                     last_read_at INTEGER,
                     progress_at INTEGER,
                     category TEXT NOT NULL,
                     content_hash TEXT NOT NULL UNIQUE,
                     kind TEXT NOT NULL,
                     removed INTEGER NOT NULL DEFAULT 0
                 );
                 PRAGMA user_version = 2;",
            )
            .unwrap();
        let id = "b".repeat(64);
        connection
            .execute(
                "INSERT INTO books(
                     id, title, author, archive_path, cover_path, page_count, current_page,
                     updated_at, last_read_at, progress_at, category, content_hash, kind, removed
                 ) VALUES(?1, '旧书', '', '/missing/old.cbz', NULL, 5, 2, 2000, 2000, 2000, '未分类', ?1, 'cbz', 0)",
                rusqlite::params![id],
            )
            .unwrap();
    }

    let v1_library = Library::open(&v1).unwrap();
    let v1_book = v1_library.get_book(&"a".repeat(64)).unwrap();
    assert_eq!(v1_book.current_page, 3);
    assert_eq!(v1_book.page_fraction, 0.0);
    let v1_record = v1_library.export_progress().unwrap().remove(0);
    assert_eq!(v1_record.page_fraction, 0.0);
    assert_eq!(v1_record.updated_at, 1000);

    let v2_library = Library::open(&v2).unwrap();
    let v2_book = v2_library.get_book(&"b".repeat(64)).unwrap();
    assert_eq!(v2_book.current_page, 2);
    assert_eq!(v2_book.page_fraction, 0.0);
    assert_eq!(v2_library.export_progress().unwrap()[0].page_fraction, 0.0);
}

#[test]
fn position_inside_page_survives_reopen_and_rejects_non_finite_values() {
    let tmp = TempDir::new().unwrap();
    let id = {
        let library = open(&tmp, "library");
        let book = import_book(&library, &tmp, "Scroll", [11, 22, 33]);
        library.save_position(&book.id, 1, 0.375, false).unwrap();
        library.save_position(&book.id, 0, 0.0, false).unwrap();
        library.save_position(&book.id, 1, 0.375, false).unwrap();
        book.id
    };

    let library = open(&tmp, "library");
    let book = library.get_book(&id).unwrap();
    assert_eq!(book.current_page, 1);
    assert_eq!(book.page_fraction, 0.375);

    library.save_position(&id, 1, 0.9, true).unwrap();
    let book = library.get_book(&id).unwrap();
    assert_eq!(book.page_fraction, 0.375, "private reading never writes");

    library.save_position(&id, 1, 1.0, false).unwrap();
    assert_eq!(library.get_book(&id).unwrap().page_fraction, 1.0);
    library.save_position(&id, 1, 0.0, false).unwrap();
    assert_eq!(library.get_book(&id).unwrap().page_fraction, 0.0);

    library.save_position(&id, 1, 0.5, false).unwrap();
    assert!(library.save_position(&id, 1, f32::NAN, false).is_err());
    assert!(library.save_position(&id, 1, f32::INFINITY, false).is_err());
    assert_eq!(library.get_book(&id).unwrap().page_fraction, 0.5);

    library.save_position(&id, 1, 1.7, false).unwrap();
    assert_eq!(library.get_book(&id).unwrap().page_fraction, 1.0);
    library.save_position(&id, 1, -0.4, false).unwrap();
    assert_eq!(library.get_book(&id).unwrap().page_fraction, 0.0);
}

#[test]
fn progress_records_carry_the_in_page_position() {
    let tmp = TempDir::new().unwrap();
    let left = open(&tmp, "left");
    let right = open(&tmp, "right");
    let book = import_book(&left, &tmp, "Sync", [40, 50, 60]);
    import_book(&right, &tmp, "Sync", [40, 50, 60]);

    left.save_position(&book.id, 1, 0.25, false).unwrap();
    let records = left.export_progress().unwrap();
    assert_eq!(records[0].page_fraction, 0.25);
    let report = right.apply_progress(&records).unwrap();
    assert_eq!(report.applied_count(), 1);
    let synced = right.get_book(&book.id).unwrap();
    assert_eq!(synced.current_page, 1);
    assert_eq!(synced.page_fraction, 0.25);

    std::thread::sleep(std::time::Duration::from_millis(50));
    left.save_position(&book.id, 0, 0.5, false).unwrap();
    let records = left.export_progress().unwrap();
    let report = right.apply_progress(&records).unwrap();
    assert_eq!(report.applied_count(), 1);
    let synced = right.get_book(&book.id).unwrap();
    assert_eq!(synced.current_page, 0);
    assert_eq!(synced.page_fraction, 0.5);
}

fn write_backup_without_fraction(
    path: &Path,
    book: &Book,
    archive_bytes: &[u8],
    page: usize,
    timestamp: i64,
) {
    let file = File::create(path).expect("create crafted backup");
    let mut writer = ZipWriter::new(file);
    let manifest = serde_json::json!({
        "format": "yejian-library-backup",
        "version": 1,
        "created_at": timestamp,
        "books": [{
            "id": book.id,
            "title": book.title,
            "author": book.author,
            "page_count": book.page_count,
            "current_page": page,
            "updated_at": timestamp,
            "last_read_at": timestamp,
            "category": book.category,
            "content_hash": book.id,
            "kind": "archive",
            "archive_entry": format!("archives/{}.cbz", book.id),
            "cover_entry": null
        }],
        "settings": {}
    });
    writer
        .start_file(
            "manifest.json",
            SimpleFileOptions::default().compression_method(CompressionMethod::Stored),
        )
        .unwrap();
    writer
        .write_all(&serde_json::to_vec(&manifest).unwrap())
        .unwrap();
    writer
        .start_file(
            format!("archives/{}.cbz", book.id),
            SimpleFileOptions::default().compression_method(CompressionMethod::Stored),
        )
        .unwrap();
    writer.write_all(archive_bytes).unwrap();
    writer.finish().unwrap();
}

#[test]
fn backups_round_trip_position_and_links_without_resetting_old_backups() {
    let tmp = TempDir::new().unwrap();
    let source = open(&tmp, "source");
    let book = import_book(&source, &tmp, "Backup", [70, 80, 90]);
    let archive_bytes = fs::read(&book.archive_path).unwrap();
    source.save_position(&book.id, 1, 0.4, false).unwrap();
    source
        .link_chapter(
            &book.id,
            &metadata("source-a", "chapter-1", "第1话", Some(1.0), Some(1.0)),
        )
        .unwrap();

    let backup = tmp.path().join("source.backup");
    source.export_backup(&backup).unwrap();
    let target = open(&tmp, "target");
    let summary = target.restore(&backup).unwrap();
    assert_eq!(summary.imported_count(), 1);
    assert_eq!(summary.links_restored, 1);
    assert!(summary.link_failures.is_empty());
    assert!(summary.is_complete());
    assert!(summary.is_fully_restored());
    let restored = target.get_book(&book.id).unwrap();
    assert_eq!(restored.current_page, 1);
    assert_eq!(restored.page_fraction, 0.4);
    let links = target.list_chapter_links().unwrap();
    assert_eq!(links.len(), 1);
    assert_eq!(links[0].book_id, book.id);
    assert_eq!(links[0].metadata.chapter_id, "chapter-1");

    // Restoring an older backup must not silently replace a chapter link the
    // local library already changed.
    target
        .link_chapter(
            &book.id,
            &metadata("source-a", "chapter-9", "第9话", Some(9.0), Some(1.0)),
        )
        .unwrap();
    let summary = target.restore(&backup).unwrap();
    assert_eq!(summary.links_restored, 0);
    assert_eq!(summary.link_failures.len(), 1);
    assert!(
        !summary.is_complete(),
        "a chapter link conflict must not report a complete restore"
    );
    assert!(!summary.is_fully_restored());
    assert_eq!(
        target.list_chapter_links().unwrap()[0].metadata.chapter_id,
        "chapter-9"
    );

    // A backup written before fractional positions existed only has a page.
    // It must not reset a newer local offset for that same page.
    let future = i64::MAX / 2;
    let old_same_page = tmp.path().join("old-same-page.backup");
    write_backup_without_fraction(&old_same_page, &book, &archive_bytes, 1, future);
    let summary = target.restore(&old_same_page).unwrap();
    assert_eq!(summary.updated_count(), 1);
    assert_eq!(target.get_book(&book.id).unwrap().page_fraction, 0.4);

    // When the old backup points at a different page, the offset is the top
    // of that page, not a stale value from the previous page.
    let old_new_page = tmp.path().join("old-new-page.backup");
    write_backup_without_fraction(&old_new_page, &book, &archive_bytes, 0, future + 1);
    target.restore(&old_new_page).unwrap();
    let moved = target.get_book(&book.id).unwrap();
    assert_eq!(moved.current_page, 0);
    assert_eq!(moved.page_fraction, 0.0);
}

#[test]
fn chapter_links_are_explicit_unique_and_hide_with_the_book() {
    let tmp = TempDir::new().unwrap();
    let library = open(&tmp, "library");
    let first = import_book(&library, &tmp, "First", [10, 20, 30]);
    let second = import_book(&library, &tmp, "Second", [30, 20, 10]);

    let chapter = metadata("source-a", "chapter-1", "第1话", Some(1.0), Some(1.0));
    library.link_chapter(&first.id, &chapter).unwrap();
    let links = library.list_chapter_links().unwrap();
    assert_eq!(links.len(), 1);
    assert_eq!(links[0].book_id, first.id);

    let error = library
        .link_chapter(&second.id, &chapter)
        .expect_err("one chapter cannot belong to two books");
    assert!(format!("{error:#}").contains(&first.id));
    assert_eq!(library.list_chapter_links().unwrap().len(), 1);

    library
        .link_chapter(
            &second.id,
            &metadata("source-a", "chapter-2", "第2话", Some(2.0), Some(1.0)),
        )
        .unwrap();
    assert_eq!(library.list_chapter_links().unwrap().len(), 2);

    // Relinking the same book replaces its own link instead of duplicating it.
    library
        .link_chapter(
            &second.id,
            &metadata("source-a", "chapter-2", "第2话", Some(2.0), Some(1.0)),
        )
        .unwrap();
    assert_eq!(library.list_chapter_links().unwrap().len(), 2);

    library.remove_from_library(&first.id).unwrap();
    let links = library.list_chapter_links().unwrap();
    assert_eq!(links.len(), 1);
    assert_eq!(links[0].book_id, second.id);
    assert!(library.link_chapter(&first.id, &chapter).is_err());

    let archive = tmp.path().join("First.cbz");
    let restored = library.import(&archive).unwrap();
    assert_eq!(restored.id, first.id);
    let links = library.list_chapter_links().unwrap();
    assert_eq!(links.len(), 2, "re-importing a hidden book keeps its link");

    let blank = metadata("source-a", "  ", "第3话", Some(3.0), Some(1.0));
    assert!(library.link_chapter(&first.id, &blank).is_err());
}

#[test]
fn chapter_order_keeps_decimal_extras_and_separates_sources() {
    let tmp = TempDir::new().unwrap();
    let library = open(&tmp, "library");
    let books = [
        import_book(&library, &tmp, "A", [1, 1, 1]),
        import_book(&library, &tmp, "B", [2, 2, 2]),
        import_book(&library, &tmp, "C", [3, 3, 3]),
        import_book(&library, &tmp, "D", [4, 4, 4]),
        import_book(&library, &tmp, "E", [5, 5, 5]),
        import_book(&library, &tmp, "F", [6, 6, 6]),
        import_book(&library, &tmp, "G", [7, 7, 7]),
    ];

    library
        .link_chapter(
            &books[0].id,
            &metadata("source-a", "c3", "第3话", Some(3.0), Some(1.0)),
        )
        .unwrap();
    library
        .link_chapter(
            &books[1].id,
            &metadata("source-a", "c25", "第2.5话", Some(2.5), Some(1.0)),
        )
        .unwrap();
    library
        .link_chapter(
            &books[2].id,
            &metadata("source-a", "c2", "第2话", Some(2.0), Some(1.0)),
        )
        .unwrap();
    library
        .link_chapter(
            &books[3].id,
            &metadata("source-a", "extra-b", "番外B", None, Some(1.0)),
        )
        .unwrap();
    library
        .link_chapter(
            &books[4].id,
            &metadata("source-a", "extra-a", "番外A", None, Some(1.0)),
        )
        .unwrap();
    library
        .link_chapter(
            &books[5].id,
            &metadata("source-a", "v2c1", "第1话", Some(1.0), Some(2.0)),
        )
        .unwrap();
    // Same manga title and chapter title on another source must stay separate.
    library
        .link_chapter(
            &books[6].id,
            &metadata("source-b", "c3", "第3话", Some(3.0), Some(1.0)),
        )
        .unwrap();

    let links = library.list_chapter_links().unwrap();
    let order = links
        .iter()
        .filter(|link| link.metadata.source_id == "source-a")
        .map(|link| link.metadata.chapter_id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        order,
        vec!["c2", "c25", "c3", "extra-a", "extra-b", "v2c1"],
        "decimal extras sort in place and unnumbered chapters fall back to title order"
    );
    let decimal = links
        .iter()
        .find(|link| link.metadata.chapter_id == "c25")
        .unwrap();
    assert_eq!(
        decimal.metadata.chapter_number,
        Some(2.5),
        "decimal chapter numbers are kept, not truncated"
    );
    assert_eq!(links.len(), 7, "cross-source links are not merged");
    assert!(
        links
            .iter()
            .any(|link| link.metadata.source_id == "source-b")
    );
}
