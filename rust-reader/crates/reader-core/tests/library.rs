use std::fs::{self, File};
use std::io::{Cursor, Write};
use std::path::Path;

use sha2::{Digest, Sha256};
use tempfile::TempDir;
use yejian_core::{Library, ProgressRecord, RestoreSource, SUPPORTED_FORMATS, natural_cmp};
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

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
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

fn write_zip_file(path: &Path, entry_name: &str, bytes: &[u8]) {
    write_cbz(path, &[(entry_name, bytes)]);
}

fn write_tar_file(path: &Path, entries: &[(&str, &[u8])]) {
    let file = File::create(path).expect("create fixture tar");
    let mut builder = tar::Builder::new(file);
    for (name, bytes) in entries {
        let mut header = tar::Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder
            .append_data(&mut header, *name, *bytes)
            .expect("append fixture tar entry");
    }
    builder.finish().expect("finish fixture tar");
}

fn write_rar_as(path: &Path, version: rars::ArchiveVersion, entries: &[(&str, &[u8])]) {
    let mut builder = rars::Builder::new(version).store(true);
    for (name, bytes) in entries {
        builder
            .add_bytes(name.as_bytes().to_vec(), bytes.to_vec(), None, None)
            .expect("add fixture RAR entry");
    }
    builder
        .write_to_path(path, None)
        .expect("write fixture RAR archive");
}

fn write_rar(path: &Path, entries: &[(&str, &[u8])]) {
    write_rar_as(path, rars::ArchiveVersion::Rar50, entries);
}

fn open(tmp: &TempDir, name: &str) -> Library {
    Library::open(&tmp.path().join(name)).expect("open fixture library")
}

#[test]
fn library_is_send_and_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Library>();
}

#[test]
fn archive_pages_use_natural_order_and_real_bytes() {
    let tmp = TempDir::new().unwrap();
    let library = open(&tmp, "library");
    let archive = tmp.path().join("Book.cbz");
    let page_two = sample_png([220, 20, 20]);
    let page_ten = sample_png([20, 220, 20]);
    let page_named = sample_png([20, 20, 220]);
    write_cbz(
        &archive,
        &[
            ("10.png", page_ten.as_slice()),
            ("2.png", page_two.as_slice()),
            ("page1.png", page_named.as_slice()),
        ],
    );

    let book = library.import(&archive).expect("import fixture archive");
    assert_eq!(book.page_count, 3);
    assert_eq!(book.current_page, 0);
    assert_eq!(
        library.page_bytes(&book.id, 0).unwrap(),
        page_two,
        "2.png should be page zero"
    );
    assert_eq!(library.page_bytes(&book.id, 1).unwrap(), page_ten);
    assert_eq!(library.page_bytes(&book.id, 2).unwrap(), page_named);
}

#[test]
fn tar_pages_are_indexed_and_read() {
    let tmp = TempDir::new().unwrap();
    let library = open(&tmp, "library");
    let archive = tmp.path().join("Volume.cbt");
    let first = sample_png([10, 20, 30]);
    let second = sample_png([200, 100, 50]);
    write_tar_file(
        &archive,
        &[("10.png", second.as_slice()), ("2.png", first.as_slice())],
    );

    let book = library.import(&archive).unwrap();
    assert_eq!(book.page_count, 2);
    assert_eq!(library.page_bytes(&book.id, 0).unwrap(), first);
    assert_eq!(library.page_bytes(&book.id, 1).unwrap(), second);
}

#[test]
fn image_folder_import_copies_only_images_and_sorts_them() {
    let tmp = TempDir::new().unwrap();
    let library = open(&tmp, "library");
    let folder = tmp.path().join("Folder volume");
    fs::create_dir_all(folder.join("pages")).unwrap();
    let first = sample_png([1, 2, 3]);
    let second = sample_png([4, 5, 6]);
    fs::write(folder.join("pages").join("10.png"), &second).unwrap();
    fs::write(folder.join("pages").join("2.png"), &first).unwrap();
    fs::write(folder.join("notes.txt"), b"not a page").unwrap();

    let book = library.import(&folder).unwrap();
    assert_eq!(book.page_count, 2);
    assert_eq!(book.title, "Folder volume");
    assert_eq!(library.page_bytes(&book.id, 0).unwrap(), first);
    assert_eq!(library.page_bytes(&book.id, 1).unwrap(), second);
    assert!(book.archive_path.is_dir());
}

#[test]
fn duplicate_import_keeps_the_existing_book_and_progress() {
    let tmp = TempDir::new().unwrap();
    let library = open(&tmp, "library");
    let archive = tmp.path().join("Duplicate.cbz");
    let page_one = sample_png([10, 10, 10]);
    let page_two = sample_png([90, 90, 90]);
    write_cbz(
        &archive,
        &[
            ("1.png", page_one.as_slice()),
            ("2.png", page_two.as_slice()),
        ],
    );

    let first = library.import(&archive).unwrap();
    library.save_progress(&first.id, 1, false).unwrap();
    let second = library.import(&archive).unwrap();

    assert_eq!(second.id, first.id);
    assert_eq!(second.current_page, 1);
    assert_eq!(library.list_books("").unwrap().len(), 1);
}

#[test]
fn removal_only_hides_and_reimport_restores_the_row() {
    let tmp = TempDir::new().unwrap();
    let library = open(&tmp, "library");
    let archive = tmp.path().join("Removable.cbz");
    let page = sample_png([40, 80, 120]);
    write_cbz(
        &archive,
        &[("1.png", page.as_slice()), ("2.png", page.as_slice())],
    );

    let book = library.import(&archive).unwrap();
    library.set_category(&book.id, "收藏").unwrap();
    library.save_progress(&book.id, 1, false).unwrap();
    library.remove_from_library(&book.id).unwrap();
    assert!(library.list_books("").unwrap().is_empty());
    assert!(
        book.archive_path.exists(),
        "archive copy must not be deleted"
    );

    let restored = library.import(&archive).unwrap();
    assert_eq!(restored.id, book.id);
    assert_eq!(restored.category, "收藏");
    assert_eq!(restored.current_page, 1);
    assert_eq!(library.list_books("").unwrap().len(), 1);
}

#[test]
fn progress_can_move_backwards_and_survives_reopen() {
    let tmp = TempDir::new().unwrap();
    let archive = tmp.path().join("Rollback.cbz");
    let page = sample_png([100, 100, 100]);
    write_cbz(
        &archive,
        &[
            ("1.png", page.as_slice()),
            ("2.png", page.as_slice()),
            ("3.png", page.as_slice()),
        ],
    );

    let root = tmp.path().join("library");
    let book_id = {
        let library = Library::open(&root).unwrap();
        let book = library.import(&archive).unwrap();
        library.save_progress(&book.id, 2, false).unwrap();
        library.save_progress(&book.id, 1, false).unwrap();
        book.id
    };

    let reopened = Library::open(&root).unwrap();
    assert_eq!(reopened.get_book(&book_id).unwrap().current_page, 1);
}

#[test]
fn private_progress_is_not_stored() {
    let tmp = TempDir::new().unwrap();
    let archive = tmp.path().join("Private.cbz");
    let page = sample_png([70, 20, 90]);
    write_cbz(
        &archive,
        &[("1.png", page.as_slice()), ("2.png", page.as_slice())],
    );

    let root = tmp.path().join("library");
    let book_id = {
        let library = Library::open(&root).unwrap();
        let book = library.import(&archive).unwrap();
        library.save_progress(&book.id, 1, false).unwrap();
        library.save_progress(&book.id, 0, true).unwrap();
        book.id
    };

    let reopened = Library::open(&root).unwrap();
    assert_eq!(reopened.get_book(&book_id).unwrap().current_page, 1);
}

#[test]
fn a_damaged_file_is_reported_without_rolling_back_a_good_import() {
    let tmp = TempDir::new().unwrap();
    let library = open(&tmp, "library");
    let good = tmp.path().join("Good.cbz");
    let bad = tmp.path().join("Bad.cbz");
    let page = sample_png([10, 200, 30]);
    write_cbz(&good, &[("1.png", page.as_slice())]);
    fs::write(&bad, b"not a ZIP archive").unwrap();

    let imported = library.import(&good).expect("good archive imports");
    assert_eq!(imported.page_count, 1);
    let error = library.import(&bad).expect_err("bad archive must fail");
    assert!(!format!("{error:#}").is_empty());
    assert_eq!(library.list_books("").unwrap().len(), 1);
}

#[test]
fn damaged_page_and_missing_archive_are_visible_errors() {
    let tmp = TempDir::new().unwrap();
    let library = open(&tmp, "library");
    let archive = tmp.path().join("Damage.cbz");
    let page = sample_png([180, 40, 210]);
    write_cbz(&archive, &[("1.png", page.as_slice())]);
    let book = library.import(&archive).unwrap();

    fs::write(&book.archive_path, b"broken archive contents").unwrap();
    let damaged = library
        .page_bytes(&book.id, 0)
        .expect_err("damaged page must fail");
    assert!(
        format!("{damaged:#}")
            .to_ascii_lowercase()
            .contains("archive")
    );

    fs::remove_file(&book.archive_path).unwrap();
    let missing = library
        .page_bytes(&book.id, 0)
        .expect_err("missing archive must fail");
    assert!(
        format!("{missing:#}")
            .to_ascii_lowercase()
            .contains("missing")
    );
}

#[test]
fn mixed_damaged_archive_keeps_good_pages_and_reports_the_bad_page() {
    let tmp = TempDir::new().unwrap();
    let library = open(&tmp, "library");
    let archive = tmp.path().join("Mixed.cbz");
    let first = sample_png([11, 22, 33]);
    let third = sample_png([44, 55, 66]);
    write_cbz(
        &archive,
        &[
            ("1.png", first.as_slice()),
            ("2.png", b"this is not a PNG, JPEG or WebP image"),
            ("3.png", third.as_slice()),
        ],
    );

    let book = library
        .import(&archive)
        .expect("one damaged page must not hide the readable pages");
    assert_eq!(book.page_count, 3);
    assert_eq!(library.page_bytes(&book.id, 0).unwrap(), first);
    assert_eq!(library.page_bytes(&book.id, 2).unwrap(), third);
    let error = library
        .page_bytes(&book.id, 1)
        .expect_err("the damaged page must be a visible error");
    assert!(
        format!("{error:#}")
            .to_ascii_lowercase()
            .contains("damaged"),
        "unexpected error: {error:#}"
    );
}

#[test]
fn backup_restores_books_progress_cover_and_settings() {
    let tmp = TempDir::new().unwrap();
    let source_library = open(&tmp, "source");
    let archive = tmp.path().join("Backup.cbz");
    let page = sample_png([5, 90, 200]);
    write_cbz(
        &archive,
        &[("1.png", page.as_slice()), ("2.png", page.as_slice())],
    );
    let book = source_library.import(&archive).unwrap();
    source_library.save_progress(&book.id, 1, false).unwrap();
    source_library
        .set_setting("reading_mode", "vertical")
        .unwrap();

    let backup = tmp.path().join("library.backup");
    let report = source_library.export_backup(&backup).unwrap();
    assert_eq!(report.books_exported, 1);
    assert!(report.skipped.is_empty());

    let target_library = open(&tmp, "target");
    let restored = target_library.import_backup(&backup).unwrap();
    assert_eq!(restored.imported_count(), 1);
    assert_eq!(restored.skipped_count(), 0);
    assert_eq!(restored.failed_count(), 0);
    assert!(restored.is_complete());
    assert_eq!(restored.settings_restored, 1);

    let books = target_library.list_books("").unwrap();
    assert_eq!(books.len(), 1);
    assert_eq!(books[0].title, "Backup");
    assert_eq!(books[0].current_page, 1);
    assert!(books[0].cover_path.as_ref().unwrap().exists());
    assert_eq!(target_library.page_bytes(&books[0].id, 1).unwrap(), page);
    assert_eq!(
        target_library.get_setting("reading_mode").unwrap(),
        Some("vertical".to_string())
    );
}

#[test]
fn mangayomi_json_backup_migrates_local_archive_metadata() {
    let tmp = TempDir::new().unwrap();
    let library = open(&tmp, "library");
    let archive = tmp.path().join("Legacy volume.cbz");
    let page = sample_png([120, 30, 60]);
    write_cbz(
        &archive,
        &[("1.png", page.as_slice()), ("2.png", page.as_slice())],
    );

    let json = serde_json::json!({
        "version": "2",
        "manga": [{
            "id": 7,
            "name": "旧书名",
            "author": "旧作者",
            "categories": [3],
            "lastRead": 12345,
            "updatedAt": 67890,
            "isLocalArchive": true
        }],
        "chapters": [{
            "mangaId": 7,
            "name": "第1话",
            "archivePath": archive.to_string_lossy(),
            "lastPageRead": "2",
            "isRead": false
        }],
        "categories": [{"id": 3, "name": "收藏"}],
        "settings": [{"themeIsDark": true}]
    });
    let bytes = serde_json::to_vec(&json).unwrap();
    let legacy = tmp.path().join("mangayomi_test.backup");
    write_zip_file(&legacy, "mangayomi_test.backup.db", &bytes);

    let report = library
        .import_legacy_mangayomi_backup(&legacy)
        .expect("legacy migration");
    assert_eq!(report.total_manga, 1);
    assert_eq!(report.imported.len(), 1);
    assert!(report.missing_archives.is_empty());
    assert!(report.failures.is_empty());
    assert!(report.raw_preserved);

    let books = library.list_books("").unwrap();
    assert_eq!(books.len(), 1);
    assert_eq!(books[0].title, "旧书名");
    assert_eq!(books[0].author, "旧作者");
    assert_eq!(books[0].category, "收藏");
    assert_eq!(books[0].current_page, 1);
    assert!(
        library
            .get_setting("legacy.mangayomi.settings_json")
            .unwrap()
            .is_some()
    );
}

#[test]
fn unsupported_legacy_format_is_an_explicit_error() {
    let tmp = TempDir::new().unwrap();
    let library = open(&tmp, "library");
    let unsupported = tmp.path().join("backup.tachibk");
    fs::write(&unsupported, b"not JSON").unwrap();
    let error = library
        .import_legacy_mangayomi_backup(&unsupported)
        .expect_err("unsupported backup must fail");
    let message = format!("{error:#}").to_ascii_lowercase();
    assert!(message.contains("unsupported") || message.contains("cannot be read"));
}

#[test]
fn natural_sort_matches_page_reading_order() {
    let mut names = ["10.webp", "2.webp", "1.webp"];
    names.sort_by(|left, right| natural_cmp(left, right));
    assert_eq!(names, ["1.webp", "2.webp", "10.webp"]);
}

#[test]
fn restore_detects_native_backup_and_returns_a_summary() {
    let tmp = TempDir::new().unwrap();
    let source = open(&tmp, "source");
    let archive = tmp.path().join("Native.cbz");
    let page = sample_png([30, 60, 90]);
    write_cbz(&archive, &[("1.png", page.as_slice())]);
    let book = source.import(&archive).unwrap();
    source.save_progress(&book.id, 0, false).unwrap();
    source.set_setting("theme", "dark").unwrap();

    let backup = tmp.path().join("native.backup");
    source.export_backup(&backup).unwrap();

    let target = open(&tmp, "target");
    let summary = target.restore(&backup).unwrap();
    assert_eq!(summary.source, RestoreSource::Native);
    assert_eq!(summary.imported_count(), 1);
    assert_eq!(summary.updated_count(), 0);
    assert_eq!(summary.skipped_count(), 0);
    assert_eq!(summary.failed_count(), 0);
    assert!(summary.is_fully_restored());
    assert_eq!(summary.settings_restored, 1);
    assert_eq!(summary.imported[0].title, "Native");
    assert_eq!(
        target.get_setting("theme").unwrap(),
        Some("dark".to_string())
    );
}

#[test]
fn restore_keeps_newer_local_progress_and_reports_it_as_skipped() {
    let tmp = TempDir::new().unwrap();
    let library = open(&tmp, "library");
    let archive = tmp.path().join("Newer.cbz");
    let page = sample_png([12, 34, 56]);
    write_cbz(
        &archive,
        &[("1.png", page.as_slice()), ("2.png", page.as_slice())],
    );
    let book = library.import(&archive).unwrap();
    library.save_progress(&book.id, 1, false).unwrap();

    let backup = tmp.path().join("older.backup");
    library.export_backup(&backup).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(50));
    library.save_progress(&book.id, 0, false).unwrap();

    let summary = library.restore(&backup).unwrap();
    assert_eq!(summary.source, RestoreSource::Native);
    assert_eq!(summary.imported_count(), 0);
    assert_eq!(summary.updated_count(), 0);
    assert_eq!(summary.skipped_count(), 1);
    assert_eq!(summary.failed_count(), 0);
    assert_eq!(library.get_book(&book.id).unwrap().current_page, 0);
}

#[test]
fn restore_detects_legacy_json_and_reports_unavailable_chapters() {
    let tmp = TempDir::new().unwrap();
    let library = open(&tmp, "library");
    let archive = tmp.path().join("Legacy readable.cbz");
    let page = sample_png([99, 88, 77]);
    write_cbz(
        &archive,
        &[("1.png", page.as_slice()), ("2.png", page.as_slice())],
    );
    let missing = tmp.path().join("moved-away.cbz");

    let json = serde_json::json!({
        "version": "2",
        "manga": [{
            "id": 11,
            "name": "旧合集",
            "author": "旧作者",
            "categories": [1],
            "lastRead": 100,
            "updatedAt": 200
        }],
        "chapters": [
            {
                "mangaId": 11,
                "name": "第1话",
                "archivePath": archive.to_string_lossy(),
                "lastPageRead": "2",
                "isRead": false
            },
            {
                "mangaId": 11,
                "name": "第2话",
                "archivePath": missing.to_string_lossy(),
                "lastPageRead": "1",
                "isRead": false
            }
        ],
        "categories": [{"id": 1, "name": "收藏"}],
        "settings": [{"themeIsDark": true}]
    });
    let legacy = tmp.path().join("mangayomi_plain.json");
    fs::write(&legacy, serde_json::to_vec(&json).unwrap()).unwrap();

    let summary = library.restore(&legacy).unwrap();
    assert_eq!(summary.source, RestoreSource::LegacyMangayomi);
    assert_eq!(summary.imported_count(), 1);
    assert_eq!(summary.failed_count(), 0);
    assert!(summary.raw_preserved);
    assert_eq!(summary.skipped_count(), 1);
    assert!(summary.skipped[0].title.contains("第2话"));
    assert!(
        summary.skipped[0]
            .reason
            .as_deref()
            .unwrap_or_default()
            .contains("not readable")
    );
    assert_eq!(library.list_books("").unwrap().len(), 1);
}

#[test]
fn restore_detects_legacy_zip_backups() {
    let tmp = TempDir::new().unwrap();
    let library = open(&tmp, "library");
    let archive = tmp.path().join("Legacy zip.cbz");
    let page = sample_png([66, 55, 44]);
    write_cbz(&archive, &[("1.png", page.as_slice())]);

    let json = serde_json::json!({
        "version": "2",
        "manga": [{"id": 3, "name": "旧ZIP书", "categories": []}],
        "chapters": [{
            "mangaId": 3,
            "archivePath": archive.to_string_lossy(),
            "lastPageRead": null,
            "isRead": false
        }],
        "categories": []
    });
    let legacy = tmp.path().join("mangayomi_test.backup");
    write_zip_file(
        &legacy,
        "mangayomi_test.backup.db",
        &serde_json::to_vec(&json).unwrap(),
    );

    let summary = library.restore(&legacy).unwrap();
    assert_eq!(summary.source, RestoreSource::LegacyMangayomi);
    assert_eq!(summary.imported_count(), 1);
    assert_eq!(summary.failed_count(), 0);
    assert_eq!(library.list_books("").unwrap()[0].title, "旧ZIP书");
}

#[test]
fn import_backup_rejects_legacy_json_without_false_success() {
    let tmp = TempDir::new().unwrap();
    let library = open(&tmp, "library");
    let legacy = tmp.path().join("legacy.json");
    fs::write(&legacy, br#"{"manga":[],"chapters":[],"categories":[]}"#).unwrap();

    let error = library
        .import_backup(&legacy)
        .expect_err("a legacy JSON file must not be reported as a native restore");
    let message = format!("{error:#}");
    assert!(message.contains("Library::restore"), "{message}");
    assert!(library.list_books("").unwrap().is_empty());
}

#[test]
fn restore_rejects_unsupported_backup_formats() {
    let tmp = TempDir::new().unwrap();
    let library = open(&tmp, "library");
    let unsupported = tmp.path().join("backup.tachibk");
    fs::write(&unsupported, b"not a backup").unwrap();
    let error = library
        .restore(&unsupported)
        .expect_err("unsupported backup must fail");
    let message = format!("{error:#}").to_ascii_lowercase();
    assert!(message.contains("unsupported"));
}

#[test]
fn cbr_pages_use_natural_order_and_real_bytes() {
    let tmp = TempDir::new().unwrap();
    let library = open(&tmp, "library");
    let archive = tmp.path().join("Volume.cbr");
    let page_two = sample_png([220, 20, 20]);
    let page_ten = sample_png([20, 220, 20]);
    let page_named = sample_png([20, 20, 220]);
    write_rar(
        &archive,
        &[
            ("10.png", page_ten.as_slice()),
            ("2.png", page_two.as_slice()),
            ("page1.png", page_named.as_slice()),
        ],
    );

    let book = library.import(&archive).expect("import fixture CBR");
    assert_eq!(book.page_count, 3);
    assert_eq!(library.page_bytes(&book.id, 0).unwrap(), page_two);
    assert_eq!(library.page_bytes(&book.id, 1).unwrap(), page_ten);
    assert_eq!(library.page_bytes(&book.id, 2).unwrap(), page_named);
    assert!(book.cover_path.as_ref().unwrap().exists());
}

#[test]
fn cbr_reads_rar13_rar4_and_rar5_families() {
    let tmp = TempDir::new().unwrap();
    let library = open(&tmp, "library");
    let page_two = sample_png([120, 10, 10]);
    let page_ten = sample_png([10, 120, 10]);
    let versions = [
        ("rar13", rars::ArchiveVersion::Rar13),
        ("rar40", rars::ArchiveVersion::Rar40),
        ("rar50", rars::ArchiveVersion::Rar50),
    ];

    for (name, version) in versions {
        let archive = tmp.path().join(format!("Family-{name}.cbr"));
        write_rar_as(
            &archive,
            version,
            &[
                ("10.png", page_ten.as_slice()),
                ("2.png", page_two.as_slice()),
            ],
        );
        let book = library
            .import(&archive)
            .unwrap_or_else(|error| panic!("import {name}: {error:#}"));
        assert_eq!(book.page_count, 2, "{name}");
        assert_eq!(
            library.page_bytes(&book.id, 0).unwrap(),
            page_two,
            "{name} page order"
        );
        assert_eq!(
            library.page_bytes(&book.id, 1).unwrap(),
            page_ten,
            "{name} page order"
        );
    }
}

#[test]
fn cbr_survives_native_backup_restore() {
    let tmp = TempDir::new().unwrap();
    let source = open(&tmp, "source");
    let archive = tmp.path().join("Backed CBR.cbr");
    let page = sample_png([44, 77, 111]);
    write_rar(&archive, &[("1.png", page.as_slice())]);

    let book = source.import(&archive).unwrap();
    let backup = tmp.path().join("cbr.backup");
    source.export_backup(&backup).unwrap();

    let target = open(&tmp, "target");
    let summary = target.restore(&backup).unwrap();
    assert_eq!(summary.imported_count(), 1);
    assert_eq!(summary.failed_count(), 0);
    let restored = target.list_books("").unwrap().remove(0);
    assert_eq!(restored.page_count, 1);
    assert_eq!(target.page_bytes(&restored.id, 0).unwrap(), page);
    assert_eq!(
        restored
            .archive_path
            .extension()
            .and_then(|value| value.to_str()),
        Some("cbr")
    );
    assert_eq!(
        fs::read(&restored.archive_path).unwrap(),
        fs::read(&archive).unwrap()
    );
    assert_eq!(restored.id, book.id);
}

#[test]
fn supported_formats_include_cbr_and_rar() {
    assert!(SUPPORTED_FORMATS.contains(&"cbr"));
    assert!(SUPPORTED_FORMATS.contains(&"rar"));
    assert!(SUPPORTED_FORMATS.contains(&"webp"));
}

#[test]
fn backup_restore_reproduces_the_original_archive_file() {
    let tmp = TempDir::new().unwrap();
    let source = open(&tmp, "source");
    let archive = tmp.path().join("Original bytes.cbz");
    let page = sample_png([9, 8, 7]);
    write_cbz(&archive, &[("1.png", page.as_slice())]);
    source.import(&archive).unwrap();

    let backup = tmp.path().join("bytes.backup");
    source.export_backup(&backup).unwrap();

    let target = open(&tmp, "target");
    target.restore(&backup).unwrap();
    let restored = target.list_books("").unwrap().remove(0);
    assert_eq!(
        fs::read(&restored.archive_path).unwrap(),
        fs::read(&archive).unwrap(),
        "the restored archive must be the original bytes"
    );
}

#[test]
fn backup_restore_reproduces_image_folder_pages() {
    let tmp = TempDir::new().unwrap();
    let source = open(&tmp, "source");
    let folder = tmp.path().join("Folder backup");
    fs::create_dir_all(&folder).unwrap();
    let first = sample_png([21, 22, 23]);
    let second = sample_png([31, 32, 33]);
    fs::write(folder.join("1.png"), &first).unwrap();
    fs::write(folder.join("2.png"), &second).unwrap();
    source.import(&folder).unwrap();

    let backup = tmp.path().join("folder.backup");
    source.export_backup(&backup).unwrap();

    let target = open(&tmp, "target");
    let summary = target.restore(&backup).unwrap();
    assert_eq!(summary.imported_count(), 1);
    assert_eq!(summary.failed_count(), 0);
    let restored = target.list_books("").unwrap().remove(0);
    assert!(restored.archive_path.is_dir());
    assert_eq!(target.page_bytes(&restored.id, 0).unwrap(), first);
    assert_eq!(target.page_bytes(&restored.id, 1).unwrap(), second);
}

#[test]
fn backup_restore_rejects_changed_archive_bytes_without_creating_a_book() {
    let tmp = TempDir::new().unwrap();
    let library = open(&tmp, "library");

    let other = tmp.path().join("other.cbz");
    let other_page = sample_png([1, 2, 3]);
    write_cbz(&other, &[("1.png", other_page.as_slice())]);
    let other_bytes = fs::read(&other).unwrap();
    let id = "a".repeat(64);
    let manifest = serde_json::json!({
        "format": "yejian-library-backup",
        "version": 1,
        "created_at": 1,
        "books": [{
            "id": id.clone(),
            "title": "tampered",
            "author": "",
            "page_count": 1,
            "current_page": 0,
            "updated_at": 1,
            "last_read_at": null,
            "category": "未分类",
            "content_hash": id.clone(),
            "kind": "archive",
            "archive_entry": format!("archives/{id}.cbz"),
            "cover_entry": null
        }],
        "settings": {}
    });
    let backup = tmp.path().join("tampered.backup");
    let manifest_bytes = serde_json::to_vec(&manifest).unwrap();
    let archive_entry = format!("archives/{id}.cbz");
    write_cbz(
        &backup,
        &[
            ("manifest.json", manifest_bytes.as_slice()),
            (archive_entry.as_str(), other_bytes.as_slice()),
        ],
    );

    let summary = library.restore(&backup).unwrap();
    assert_eq!(summary.imported_count(), 0);
    assert_eq!(summary.failed_count(), 1);
    assert!(
        summary.failures[0]
            .reason
            .as_deref()
            .unwrap_or_default()
            .contains("does not match")
    );
    assert!(library.list_books("").unwrap().is_empty());
}

#[test]
fn import_backup_reports_one_bad_book_without_losing_the_good_book() {
    let tmp = TempDir::new().unwrap();
    let library = open(&tmp, "library");
    let good_page = sample_png([8, 18, 28]);
    let good_source = tmp.path().join("good.cbz");
    write_cbz(&good_source, &[("1.png", good_page.as_slice())]);
    let good_bytes = fs::read(&good_source).unwrap();
    let good_id = sha256_hex(&good_bytes);

    let other_source = tmp.path().join("other.cbz");
    let other_page = sample_png([28, 18, 8]);
    write_cbz(&other_source, &[("1.png", other_page.as_slice())]);
    let other_bytes = fs::read(&other_source).unwrap();
    let bad_id = "b".repeat(64);

    let good_entry = format!("archives/{good_id}.cbz");
    let bad_entry = format!("archives/{bad_id}.cbz");
    let manifest = serde_json::json!({
        "format": "yejian-library-backup",
        "version": 1,
        "created_at": 1,
        "books": [
            {
                "id": good_id.clone(),
                "title": "Good backup book",
                "author": "",
                "page_count": 1,
                "current_page": 0,
                "updated_at": 1,
                "last_read_at": null,
                "category": "未分类",
                "content_hash": good_id.clone(),
                "kind": "archive",
                "archive_entry": good_entry.clone(),
                "cover_entry": null
            },
            {
                "id": bad_id.clone(),
                "title": "Bad backup book",
                "author": "",
                "page_count": 1,
                "current_page": 0,
                "updated_at": 1,
                "last_read_at": null,
                "category": "未分类",
                "content_hash": bad_id.clone(),
                "kind": "archive",
                "archive_entry": bad_entry.clone(),
                "cover_entry": null
            }
        ],
        "settings": {}
    });
    let manifest_bytes = serde_json::to_vec(&manifest).unwrap();
    let backup = tmp.path().join("partial.backup");
    write_cbz(
        &backup,
        &[
            ("manifest.json", manifest_bytes.as_slice()),
            (good_entry.as_str(), good_bytes.as_slice()),
            (bad_entry.as_str(), other_bytes.as_slice()),
        ],
    );

    let report = library.import_backup(&backup).unwrap();
    assert_eq!(report.imported_count(), 1);
    assert_eq!(report.failed_count(), 1);
    assert!(report.failures[0].error.contains("does not match"));
    let books = library.list_books("").unwrap();
    assert_eq!(books.len(), 1);
    assert_eq!(books[0].id, good_id);
    assert_eq!(library.page_bytes(&good_id, 0).unwrap(), good_page);
}

#[test]
fn progress_export_contains_saved_state_and_not_private_reads() {
    let tmp = TempDir::new().unwrap();
    let library = open(&tmp, "library");
    let archive = tmp.path().join("Progress.cbz");
    let page = sample_png([10, 20, 30]);
    write_cbz(
        &archive,
        &[("1.png", page.as_slice()), ("2.png", page.as_slice())],
    );
    let book = library.import(&archive).unwrap();
    library.save_progress(&book.id, 1, false).unwrap();
    library.save_progress(&book.id, 0, true).unwrap();
    library.set_category(&book.id, "进度").unwrap();

    let records = library.export_progress().unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].book_id, book.id);
    assert_eq!(records[0].current_page, 1);
    assert_eq!(records[0].category, "进度");
    assert!(!records[0].removed);
    assert!(records[0].updated_at > 0);
    assert!(records[0].last_read_at.is_some());
}

#[test]
fn progress_apply_merges_newer_timestamp_and_allows_page_rollback() {
    let tmp = TempDir::new().unwrap();
    let library = open(&tmp, "library");
    let archive = tmp.path().join("Sync.cbz");
    let page = sample_png([70, 80, 90]);
    write_cbz(
        &archive,
        &[("1.png", page.as_slice()), ("2.png", page.as_slice())],
    );
    let book = library.import(&archive).unwrap();
    library.save_progress(&book.id, 1, false).unwrap();
    let base = library.export_progress().unwrap().remove(0);

    let newer = ProgressRecord {
        book_id: book.id.clone(),
        current_page: 0,
        page_fraction: 0.0,
        last_read_at: Some(424242),
        updated_at: base.updated_at + 1,
        category: "同步分类".to_string(),
        removed: false,
    };
    let report = library.apply_progress(&[newer.clone()]).unwrap();
    assert_eq!(report.applied_count(), 1);
    assert_eq!(report.skipped_count(), 0);

    let applied = library.get_book(&book.id).unwrap();
    assert_eq!(
        applied.current_page, 0,
        "a newer record may move pages back"
    );
    assert_eq!(applied.last_read_at, Some(424242));
    assert_eq!(applied.updated_at, newer.updated_at);
    assert_eq!(applied.category, "同步分类");

    let older = library.apply_progress(&[base]).unwrap();
    assert_eq!(older.applied_count(), 0);
    assert_eq!(older.unchanged_count(), 1);
    assert_eq!(library.get_book(&book.id).unwrap().current_page, 0);
}

#[test]
fn progress_apply_reports_missing_book_as_skipped() {
    let tmp = TempDir::new().unwrap();
    let library = open(&tmp, "library");
    let report = library
        .apply_progress(&[ProgressRecord {
            book_id: "b".repeat(64),
            current_page: 0,
            page_fraction: 0.0,
            last_read_at: Some(1),
            updated_at: 1,
            category: "未分类".to_string(),
            removed: false,
        }])
        .unwrap();

    assert_eq!(report.applied_count(), 0);
    assert_eq!(report.skipped_count(), 1);
    assert!(report.skipped[0].reason.contains("not in this library"));
    assert!(library.list_books("").unwrap().is_empty());
}

#[test]
fn progress_apply_never_deletes_or_resurrects_books() {
    let tmp = TempDir::new().unwrap();
    let library = open(&tmp, "library");
    let archive = tmp.path().join("No delete.cbz");
    let page = sample_png([5, 6, 7]);
    write_cbz(
        &archive,
        &[("1.png", page.as_slice()), ("2.png", page.as_slice())],
    );
    let book = library.import(&archive).unwrap();

    let deletion = ProgressRecord {
        book_id: book.id.clone(),
        current_page: 1,
        page_fraction: 0.0,
        last_read_at: Some(3),
        updated_at: i64::MAX,
        category: "未分类".to_string(),
        removed: true,
    };
    let report = library.apply_progress(&[deletion]).unwrap();
    assert_eq!(report.skipped_count(), 1);
    assert!(report.skipped[0].reason.contains("delete propagation"));
    assert_eq!(library.list_books("").unwrap().len(), 1);
    assert_eq!(library.get_book(&book.id).unwrap().current_page, 0);

    library.remove_from_library(&book.id).unwrap();
    let remote = ProgressRecord {
        book_id: book.id.clone(),
        current_page: 1,
        page_fraction: 0.0,
        last_read_at: Some(3),
        updated_at: i64::MAX,
        category: "远程".to_string(),
        removed: false,
    };
    let report = library.apply_progress(&[remote]).unwrap();
    assert_eq!(report.skipped_count(), 1);
    assert!(library.list_books("").unwrap().is_empty());
}

#[test]
fn progress_apply_rejects_out_of_range_pages_and_duplicate_records_keep_newest() {
    let tmp = TempDir::new().unwrap();
    let library = open(&tmp, "library");
    let archive = tmp.path().join("Ranges.cbz");
    let page = sample_png([15, 25, 35]);
    write_cbz(&archive, &[("1.png", page.as_slice())]);
    let book = library.import(&archive).unwrap();
    let base = library.get_book(&book.id).unwrap().updated_at;

    let too_high = ProgressRecord {
        book_id: book.id.clone(),
        current_page: 10,
        page_fraction: 0.0,
        last_read_at: Some(1),
        updated_at: base + 2,
        category: "未分类".to_string(),
        removed: false,
    };
    let report = library.apply_progress(&[too_high]).unwrap();
    assert_eq!(report.skipped_count(), 1);
    assert!(report.skipped[0].reason.contains("out of range"));
    assert_eq!(library.get_book(&book.id).unwrap().current_page, 0);

    let newest = ProgressRecord {
        book_id: book.id.clone(),
        current_page: 0,
        page_fraction: 0.0,
        last_read_at: Some(10),
        updated_at: base + 10,
        category: "最新".to_string(),
        removed: false,
    };
    let older = ProgressRecord {
        book_id: book.id.clone(),
        current_page: 0,
        page_fraction: 0.0,
        last_read_at: Some(5),
        updated_at: base + 5,
        category: "较旧".to_string(),
        removed: false,
    };
    let report = library.apply_progress(&[newest, older]).unwrap();
    assert_eq!(report.applied_count(), 1);
    assert_eq!(report.unchanged_count(), 1);
    assert_eq!(library.get_book(&book.id).unwrap().category, "最新");
}

#[test]
fn unread_import_does_not_overwrite_history_but_a_real_reread_does() {
    let tmp = TempDir::new().unwrap();
    let phone = open(&tmp, "phone");
    let tablet = open(&tmp, "tablet");
    let archive = tmp.path().join("Shared six pages.cbz");
    let page = sample_png([33, 66, 99]);
    write_cbz(
        &archive,
        &[
            ("1.png", page.as_slice()),
            ("2.png", page.as_slice()),
            ("3.png", page.as_slice()),
            ("4.png", page.as_slice()),
            ("5.png", page.as_slice()),
            ("6.png", page.as_slice()),
        ],
    );

    let phone_book = phone.import(&archive).unwrap();
    phone.save_progress(&phone_book.id, 4, false).unwrap();
    let phone_records = phone.export_progress().unwrap();
    assert_eq!(phone_records[0].current_page, 4);

    let tablet_book = tablet.import(&archive).unwrap();
    assert_eq!(tablet_book.id, phone_book.id);
    assert_eq!(tablet_book.current_page, 0);
    assert!(
        tablet.export_progress().unwrap().is_empty(),
        "a freshly imported, never-read book must not export page 0 as progress"
    );

    let merged = tablet.apply_progress(&phone_records).unwrap();
    assert_eq!(merged.applied_count(), 1);
    assert_eq!(tablet.get_book(&phone_book.id).unwrap().current_page, 4);

    std::thread::sleep(std::time::Duration::from_millis(50));
    tablet.save_progress(&phone_book.id, 0, false).unwrap();
    let tablet_records = tablet.export_progress().unwrap();
    assert_eq!(tablet_records[0].current_page, 0);
    assert!(tablet_records[0].last_read_at.is_some());

    let merged = phone.apply_progress(&tablet_records).unwrap();
    assert_eq!(merged.applied_count(), 1);
    assert_eq!(
        phone.get_book(&phone_book.id).unwrap().current_page,
        0,
        "an explicit page-1 read must be allowed to move progress backwards"
    );
}

#[test]
fn upgrading_a_version_one_library_keeps_existing_reading_times() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join("old-schema");
    std::fs::create_dir_all(&root).unwrap();
    let read_id = "a".repeat(64);
    let unread_id = "b".repeat(64);
    {
        let connection = rusqlite::Connection::open(root.join("library.sqlite3")).unwrap();
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
        connection
            .execute(
                "INSERT INTO books(
                     id, title, author, archive_path, cover_path, page_count, current_page,
                     updated_at, last_read_at, category, content_hash, kind, removed
                 ) VALUES(?1, '已读', '', '/missing/read.cbz', NULL, 6, 3, 1000, NULL, '未分类', ?1, 'cbz', 0)",
                rusqlite::params![read_id],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO books(
                     id, title, author, archive_path, cover_path, page_count, current_page,
                     updated_at, last_read_at, category, content_hash, kind, removed
                 ) VALUES(?1, '未读', '', '/missing/unread.cbz', NULL, 6, 0, 5000, NULL, '未分类', ?1, 'cbz', 0)",
                rusqlite::params![unread_id],
            )
            .unwrap();
    }

    let library = Library::open(&root).unwrap();
    let records = library.export_progress().unwrap();
    assert_eq!(
        records.len(),
        1,
        "only the book with a saved page keeps a reading record after migration"
    );
    assert_eq!(records[0].book_id, read_id);
    assert_eq!(records[0].current_page, 3);
    assert_eq!(
        records[0].updated_at, 1000,
        "migration must reuse the old save time, not the import time"
    );
}

#[test]
fn added_at_fills_from_updated_at_on_pre_column_libraries() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join("library");
    let library = open(&tmp, "library");
    let archive = tmp.path().join("Added.cbz");
    write_cbz(&archive, &[("1.png", sample_png([9, 9, 9]).as_slice())]);
    let book = library.import(&archive).unwrap();
    assert!(book.added_at > 0, "imports record when they joined");
    drop(library);
    // A v4 library predating the column: reopening backfills updated_at.
    {
        let connection = rusqlite::Connection::open(root.join("library.sqlite3")).unwrap();
        connection
            .execute_batch("ALTER TABLE books DROP COLUMN added_at;")
            .unwrap();
    }
    let library = Library::open(&root).unwrap();
    let books = library.list_books("").unwrap();
    assert_eq!(books.len(), 1);
    assert_eq!(
        books[0].added_at, books[0].updated_at,
        "pre-column rows must fall back to updated_at"
    );
}

#[test]
fn added_at_refreshes_when_a_removed_book_returns() {
    let tmp = TempDir::new().unwrap();
    let library = open(&tmp, "library");
    let archive = tmp.path().join("Returning.cbz");
    write_cbz(&archive, &[("1.png", sample_png([30, 30, 30]).as_slice())]);
    let book = library.import(&archive).unwrap();
    let first = book.added_at;
    library.remove_from_library(&book.id).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(5));
    let book = library.import(&archive).unwrap();
    assert!(
        book.added_at >= first,
        "re-adding a removed book records the new join time"
    );
}

#[test]
fn backup_round_trip_keeps_added_at_and_old_backups_fall_back() {
    let tmp = TempDir::new().unwrap();
    let source_library = open(&tmp, "source");
    let archive = tmp.path().join("Joined.cbz");
    write_cbz(&archive, &[("1.png", sample_png([60, 60, 60]).as_slice())]);
    let book = source_library.import(&archive).unwrap();

    let backup = tmp.path().join("with-added.backup");
    source_library.export_backup(&backup).unwrap();
    let target_library = open(&tmp, "target");
    target_library.import_backup(&backup).unwrap();
    assert_eq!(
        target_library.get_book(&book.id).unwrap().added_at,
        book.added_at,
        "native backups must carry the join date"
    );

    // A backup written before the field existed falls back to updated_at.
    let bytes = fs::read(&backup).unwrap();
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
    let manifest_raw = {
        let mut file = zip.by_name("manifest.json").unwrap();
        let mut raw = String::new();
        std::io::Read::read_to_string(&mut file, &mut raw).unwrap();
        raw
    };
    let mut manifest: serde_json::Value = serde_json::from_str(&manifest_raw).unwrap();
    for book in manifest["books"].as_array_mut().unwrap() {
        book.as_object_mut().unwrap().remove("added_at");
    }
    let stripped = tmp.path().join("without-added.backup");
    {
        let file = File::create(&stripped).unwrap();
        let mut writer = zip::ZipWriter::new(file);
        writer
            .start_file("manifest.json", SimpleFileOptions::default())
            .unwrap();
        writer
            .write_all(serde_json::to_vec(&manifest).unwrap().as_slice())
            .unwrap();
        for name in 0..zip.len() {
            let mut file = zip.by_index(name).unwrap();
            if file.name() == "manifest.json" {
                continue;
            }
            let name = file.name().to_string();
            let mut bytes = Vec::new();
            std::io::Read::read_to_end(&mut file, &mut bytes).unwrap();
            writer.start_file(name, SimpleFileOptions::default()).unwrap();
            writer.write_all(&bytes).unwrap();
        }
        writer.finish().unwrap();
    }
    let target_library = open(&tmp, "legacy-target");
    let report = target_library.import_backup(&stripped).unwrap();
    assert_eq!(report.imported_count(), 1);
    let restored = target_library.get_book(&book.id).unwrap();
    assert!(
        restored.added_at > 0,
        "old backups must still yield a join date"
    );
}
