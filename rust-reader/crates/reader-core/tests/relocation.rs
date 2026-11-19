//! The whole library directory must keep working after it is moved to a new
//! root, which is what happens when iOS or a desktop OS assigns the app a new
//! data container. Managed paths are stored as `archives/<id>[.ext]` and
//! `covers/<id>.jpg` relative to the library root, so `Library::open(new_root)`
//! keeps pages, covers, backups and duplicate imports working without
//! re-importing anything.

use std::fs::{self, File};
use std::io::{Cursor, Write};
use std::path::{Path, PathBuf};

use tempfile::TempDir;
use yejian_core::{ChapterMetadata, Library};
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

fn metadata() -> ChapterMetadata {
    ChapterMetadata {
        source_id: "source-a".to_string(),
        manga_id: "manga-1".to_string(),
        manga_title: "迁址漫画".to_string(),
        chapter_id: "chapter-1".to_string(),
        chapter_title: "第1话".to_string(),
        chapter_number: Some(1.0),
        volume_number: Some(1.0),
    }
}

/// Rewrites one book's stored paths the way a pre-relocation build recorded
/// them: absolute paths under `old_root`. The caller moves the directory
/// afterwards, so the values are genuinely stale by the time the library is
/// reopened.
fn force_absolute_rows(root: &Path, book_ids: &[&str]) {
    let connection = rusqlite::Connection::open(root.join("library.sqlite3")).unwrap();
    for id in book_ids {
        connection
            .execute(
                "UPDATE books SET archive_path = ?1, cover_path = ?2 WHERE id = ?3",
                rusqlite::params![
                    root.join("archives")
                        .join(format!("{id}.cbz"))
                        .to_string_lossy()
                        .into_owned(),
                    root.join("covers")
                        .join(format!("{id}.jpg"))
                        .to_string_lossy()
                        .into_owned(),
                    id
                ],
            )
            .unwrap();
    }
}

fn column_text(root: &Path, book_id: &str, column: &str) -> String {
    let connection = rusqlite::Connection::open(root.join("library.sqlite3")).unwrap();
    connection
        .query_row(
            &format!("SELECT {column} FROM books WHERE id = ?1"),
            rusqlite::params![book_id],
            |row| row.get(0),
        )
        .unwrap()
}

fn column_int(root: &Path, book_id: &str, column: &str) -> Option<i64> {
    let connection = rusqlite::Connection::open(root.join("library.sqlite3")).unwrap();
    connection
        .query_row(
            &format!("SELECT {column} FROM books WHERE id = ?1"),
            rusqlite::params![book_id],
            |row| row.get(0),
        )
        .unwrap()
}

/// Books imported while the library lived under `old_root` must stay fully
/// usable after the whole directory is moved to `new_root`, including when the
/// original location no longer exists on disk.
#[test]
fn moved_library_reads_pages_covers_backups_and_links() {
    let tmp = TempDir::new().unwrap();
    let old_root = tmp.path().join("old").join("library");
    let new_root = tmp.path().join("new").join("library");

    let archive_page = sample_png([10, 90, 40]);
    let folder_first = sample_png([200, 30, 30]);
    let folder_second = sample_png([30, 200, 30]);

    let (archive_id, folder_id, archive_source, folder_source) = {
        let library = Library::open(&old_root).unwrap();

        let archive_source = tmp.path().join("Archive volume.cbz");
        write_cbz(
            &archive_source,
            &[
                ("1.png", archive_page.as_slice()),
                ("2.png", archive_page.as_slice()),
            ],
        );
        let archive_book = library.import(&archive_source).unwrap();
        library
            .save_position(&archive_book.id, 1, 0.5, false)
            .unwrap();
        library.set_category(&archive_book.id, "收藏").unwrap();
        library.link_chapter(&archive_book.id, &metadata()).unwrap();

        let folder_source = tmp.path().join("Folder volume");
        fs::create_dir_all(folder_source.join("pages")).unwrap();
        fs::write(folder_source.join("pages").join("1.png"), &folder_first).unwrap();
        fs::write(folder_source.join("pages").join("2.png"), &folder_second).unwrap();
        let folder_book = library.import(&folder_source).unwrap();
        library.save_progress(&folder_book.id, 1, false).unwrap();

        (
            archive_book.id,
            folder_book.id,
            archive_source,
            folder_source,
        )
    };

    // The archive book carries the absolute path an older build would have
    // written; the folder book already uses the canonical managed form.
    force_absolute_rows(&old_root, &[&archive_id]);

    fs::create_dir_all(new_root.parent().unwrap()).unwrap();
    fs::rename(&old_root, &new_root).unwrap();
    assert!(!old_root.exists(), "the old library root is gone");

    let library = Library::open(&new_root).unwrap();

    let books = library.list_books("").unwrap();
    assert_eq!(books.len(), 2);
    assert!(
        books.iter().all(|book| book
            .cover_path
            .as_ref()
            .is_some_and(|cover| cover.is_file())),
        "covers must resolve under the new root"
    );

    let archive_book = library.get_book(&archive_id).unwrap();
    assert_eq!(archive_book.current_page, 1);
    assert_eq!(archive_book.page_fraction, 0.5);
    assert_eq!(archive_book.category, "收藏");
    assert_eq!(
        library.page_bytes(&archive_id, 0).unwrap(),
        archive_page,
        "archive pages must read from the relocated library"
    );

    let folder_book = library.get_book(&folder_id).unwrap();
    assert_eq!(folder_book.current_page, 1);
    assert_eq!(library.page_bytes(&folder_id, 0).unwrap(), folder_first);
    assert_eq!(library.page_bytes(&folder_id, 1).unwrap(), folder_second);

    let links = library.list_chapter_links().unwrap();
    assert_eq!(links.len(), 1);
    assert_eq!(links[0].book_id, archive_id);
    assert_eq!(links[0].metadata.chapter_id, "chapter-1");

    // A backup exported after the move contains real archive bytes.
    let backup = tmp.path().join("moved.backup");
    let report = library.export_backup(&backup).unwrap();
    assert_eq!(report.books_exported, 2);
    assert!(
        report.skipped.is_empty(),
        "no book may be skipped: {:?}",
        report.skipped
    );
    let restored_target = Library::open(&tmp.path().join("restored")).unwrap();
    let summary = restored_target.import_backup(&backup).unwrap();
    assert_eq!(summary.imported_count(), 2);
    assert_eq!(summary.failed_count(), 0);
    assert!(summary.is_complete());
    let restored = restored_target.get_book(&archive_id).unwrap();
    assert_eq!(restored.current_page, 1);
    assert_eq!(restored.page_fraction, 0.5);
    assert_eq!(restored.category, "收藏");
    assert_eq!(
        restored_target.page_bytes(&archive_id, 0).unwrap(),
        archive_page
    );

    // Re-importing the same content at the new location returns the same book
    // and never repairs against the vanished old root.
    let again = library.import(&archive_source).unwrap();
    assert_eq!(again.id, archive_id);
    assert_eq!(again.current_page, 1);
    assert_eq!(library.list_books("").unwrap().len(), 2);

    let again_folder = library.import(&folder_source).unwrap();
    assert_eq!(again_folder.id, folder_id);
    assert_eq!(library.list_books("").unwrap().len(), 2);
}

/// If the old container directory is still present, the moved library must
/// read its own managed copies anyway: the stored path belongs to *this*
/// library, not to the original container.
#[test]
fn relocation_uses_the_current_root_even_when_the_old_path_exists() {
    let tmp = TempDir::new().unwrap();
    let old_root = tmp.path().join("old").join("library");
    let new_root = tmp.path().join("new").join("library");

    let good_page = sample_png([1, 2, 3]);
    let book_id = {
        let library = Library::open(&old_root).unwrap();
        let source = tmp.path().join("Swapped.cbz");
        write_cbz(&source, &[("1.png", good_page.as_slice())]);
        library.import(&source).unwrap().id
    };
    force_absolute_rows(&old_root, &[&book_id]);

    fs::create_dir_all(new_root.parent().unwrap()).unwrap();
    fs::rename(&old_root, &new_root).unwrap();

    // Simulate a stale copy of the old container left behind with damaged
    // archive bytes. Reading through the moved library must not touch it.
    let stale_archive = old_root.join("archives").join(format!("{book_id}.cbz"));
    fs::create_dir_all(stale_archive.parent().unwrap()).unwrap();
    fs::write(&stale_archive, b"stale damaged bytes").unwrap();

    let library = Library::open(&new_root).unwrap();
    assert_eq!(
        library.page_bytes(&book_id, 0).unwrap(),
        good_page,
        "the relocated library must read its own archive, not the stale old path"
    );
}

/// A database written by an older build stored absolute paths. Opening that
/// database at a new root rewrites the managed rows once, keeps every
/// timestamp and the hidden state untouched, and keeps working when the
/// archive file itself is genuinely gone.
#[test]
fn legacy_absolute_rows_relocate_once_and_missing_archives_still_fail() {
    let tmp = TempDir::new().unwrap();
    let old_root = tmp.path().join("old").join("library");
    let new_root = tmp.path().join("new").join("library");

    let page = sample_png([77, 88, 99]);
    let (book_id, hidden_id, missing_id, updated_at, last_read_at, progress_at) = {
        let library = Library::open(&old_root).unwrap();
        let source = tmp.path().join("Real.cbz");
        write_cbz(&source, &[("1.png", page.as_slice())]);
        let book = library.import(&source).unwrap();
        library.save_progress(&book.id, 0, false).unwrap();
        let book = library.get_book(&book.id).unwrap();

        // A hidden book keeps its hidden state through relocation; nothing
        // about the path rewrite may resurrect it.
        let hidden_source = tmp.path().join("Hidden.cbz");
        write_cbz(
            &hidden_source,
            &[("1.png", sample_png([9, 9, 9]).as_slice())],
        );
        let hidden = library.import(&hidden_source).unwrap();
        library.remove_from_library(&hidden.id).unwrap();

        // A row whose archive was deleted long ago must stay a visible
        // failure after relocation instead of being guessed from filenames.
        let missing_source = tmp.path().join("Gone.cbz");
        write_cbz(
            &missing_source,
            &[("1.png", sample_png([1, 1, 1]).as_slice())],
        );
        let missing = library.import(&missing_source).unwrap();
        fs::remove_file(&missing.archive_path).unwrap();

        let progress_at: i64 = rusqlite::Connection::open(old_root.join("library.sqlite3"))
            .unwrap()
            .query_row(
                "SELECT progress_at FROM books WHERE id = ?1",
                rusqlite::params![book.id],
                |row| row.get(0),
            )
            .unwrap();

        (
            book.id,
            hidden.id,
            missing.id,
            book.updated_at,
            book.last_read_at.unwrap(),
            progress_at,
        )
    };
    force_absolute_rows(&old_root, &[&book_id, &hidden_id, &missing_id]);

    fs::create_dir_all(new_root.parent().unwrap()).unwrap();
    fs::rename(&old_root, &new_root).unwrap();

    let library = Library::open(&new_root).unwrap();
    let book = library.get_book(&book_id).unwrap();
    assert_eq!(
        library.page_bytes(&book_id, 0).unwrap(),
        page,
        "the relocated archive must read under the new root"
    );
    assert_eq!(
        book.updated_at, updated_at,
        "relocation never edits timestamps"
    );
    assert_eq!(book.last_read_at, Some(last_read_at));
    assert_eq!(
        book.archive_path,
        new_root.join("archives").join(format!("{book_id}.cbz")),
        "the stored path is the managed file under the new root"
    );
    assert_eq!(
        column_int(&new_root, &book_id, "progress_at"),
        Some(progress_at),
        "the reading-state timestamp is untouched by relocation"
    );

    // The hidden book stays hidden, keeps its link data, and is not listed.
    assert!(
        library
            .list_books("")
            .unwrap()
            .iter()
            .all(|book| book.id != hidden_id),
        "a removed book must not reappear after relocation"
    );

    let error = library
        .page_bytes(&missing_id, 0)
        .expect_err("a deleted archive must remain a visible failure");
    assert!(
        format!("{error:#}")
            .to_ascii_lowercase()
            .contains("missing"),
        "unexpected error: {error:#}"
    );

    let report = library
        .export_backup(&tmp.path().join("partial.backup"))
        .unwrap();
    assert_eq!(report.books_exported, 1);
    assert_eq!(report.skipped.len(), 1);
    assert_eq!(report.skipped[0].book_id, missing_id);

    // Reopening the library does not rewrite the rows a second time.
    let second = Library::open(&new_root).unwrap();
    assert_eq!(
        column_text(&new_root, &book_id, "archive_path"),
        format!("archives/{book_id}.cbz")
    );
    assert_eq!(
        column_text(&new_root, &book_id, "cover_path"),
        format!("covers/{book_id}.jpg")
    );
    assert_eq!(
        second.get_book(&book_id).unwrap().updated_at,
        updated_at,
        "a second open stays a no-op"
    );
}

/// Rows may also arrive already stored as root-relative paths (written by a
/// build that already stores the canonical form). They resolve against the
/// current root too.
#[test]
fn relative_paths_in_existing_rows_resolve_against_the_current_root() {
    let tmp = TempDir::new().unwrap();
    let old_root = tmp.path().join("old").join("library");
    let new_root = tmp.path().join("new").join("library");

    let page = sample_png([5, 6, 7]);
    let book_id = {
        let library = Library::open(&old_root).unwrap();
        let source = tmp.path().join("Relative.cbz");
        write_cbz(&source, &[("1.png", page.as_slice())]);
        library.import(&source).unwrap().id
    };

    fs::create_dir_all(new_root.parent().unwrap()).unwrap();
    fs::rename(&old_root, &new_root).unwrap();

    // Rewrite the stored row to the canonical relative form, as if the
    // database had been created by a build that already stores it.
    {
        let connection = rusqlite::Connection::open(new_root.join("library.sqlite3")).unwrap();
        connection
            .execute(
                "UPDATE books SET archive_path = ?1, cover_path = ?2 WHERE id = ?3",
                rusqlite::params![
                    format!("archives/{book_id}.cbz"),
                    format!("covers/{book_id}.jpg"),
                    book_id
                ],
            )
            .unwrap();
    }

    let library = Library::open(&new_root).unwrap();
    assert_eq!(library.page_bytes(&book_id, 0).unwrap(), page);
    assert!(
        library
            .get_book(&book_id)
            .unwrap()
            .cover_path
            .as_ref()
            .is_some_and(|cover: &PathBuf| cover.is_file())
    );
}

/// Old builds could also record working-directory-relative values such as
/// `data/archives/<id>.cbz` or `.\data\archives\<id>.cbz`. Opening at a new
/// root must still recognise the managed pair, not join `data/` under the new
/// root and fail.
#[test]
fn cwd_relative_managed_rows_relocate_to_the_current_root() {
    let tmp = TempDir::new().unwrap();
    let old_root = tmp.path().join("old").join("library");
    let new_root = tmp.path().join("new").join("library");

    let page = sample_png([8, 9, 10]);
    let (posix_id, windows_id) = {
        let library = Library::open(&old_root).unwrap();
        let first = tmp.path().join("First.cbz");
        write_cbz(&first, &[("1.png", page.as_slice())]);
        let posix = library.import(&first).unwrap();
        let second = tmp.path().join("Second.cbz");
        write_cbz(&second, &[("1.png", sample_png([3, 3, 3]).as_slice())]);
        let windows = library.import(&second).unwrap();
        (posix.id, windows.id)
    };

    // Both spellings older builds could produce; each names the same managed
    // pair `archives/<id>.cbz` / `covers/<id>.jpg` under some prefix.
    {
        let connection = rusqlite::Connection::open(old_root.join("library.sqlite3")).unwrap();
        connection
            .execute(
                "UPDATE books SET archive_path = ?1, cover_path = ?2 WHERE id = ?3",
                rusqlite::params![
                    format!("data/archives/{posix_id}.cbz"),
                    format!("data/covers/{posix_id}.jpg"),
                    posix_id
                ],
            )
            .unwrap();
        connection
            .execute(
                "UPDATE books SET archive_path = ?1, cover_path = ?2 WHERE id = ?3",
                rusqlite::params![
                    format!(".\\data\\archives\\{windows_id}.cbz"),
                    format!(".\\data\\covers\\{windows_id}.jpg"),
                    windows_id
                ],
            )
            .unwrap();
    }

    fs::create_dir_all(new_root.parent().unwrap()).unwrap();
    fs::rename(&old_root, &new_root).unwrap();

    let library = Library::open(&new_root).unwrap();
    assert_eq!(library.page_bytes(&posix_id, 0).unwrap(), page);
    assert_eq!(
        column_text(&new_root, &posix_id, "archive_path"),
        format!("archives/{posix_id}.cbz"),
        "a `data/archives/...` row collapses to its managed pair"
    );
    assert_eq!(
        column_text(&new_root, &windows_id, "archive_path"),
        format!("archives/{windows_id}.cbz"),
        "a Windows `.\\data\\archives\\...` row collapses the same way"
    );
    assert_eq!(
        column_text(&new_root, &windows_id, "cover_path"),
        format!("covers/{windows_id}.jpg")
    );

    // A row that only shares the file name but not the managed directory is
    // never claimed.
    {
        let connection = rusqlite::Connection::open(new_root.join("library.sqlite3")).unwrap();
        connection
            .execute(
                "UPDATE books SET archive_path = ?1 WHERE id = ?2",
                rusqlite::params![format!("data/stuff/{windows_id}.cbz"), windows_id],
            )
            .unwrap();
    }
    let library = Library::open(&new_root).unwrap();
    let error = library
        .page_bytes(&windows_id, 0)
        .expect_err("a non-managed relative row keeps its literal contract");
    assert!(
        format!("{error:#}")
            .to_ascii_lowercase()
            .contains("missing")
            || format!("{error:#}")
                .to_ascii_lowercase()
                .contains("archive"),
        "unexpected error: {error:#}"
    );
}

/// A row whose stored path merely shares a directory or file name with a
/// managed copy must not be claimed or even written: an external file keeps
/// its literal path and a wrong-hash name stays untouched.
#[test]
fn foreign_and_mismatched_rows_are_left_untouched() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join("library");

    let book_id = {
        let library = Library::open(&root).unwrap();
        let source = tmp.path().join("External.cbz");
        write_cbz(&source, &[("1.png", sample_png([4, 5, 6]).as_slice())]);
        library.import(&source).unwrap().id
    };

    // Point the row at shapes that look almost managed but are not: a wrong
    // id, an unsupported suffix, a cover that is not `<id>.jpg`, and a plain
    // external file. None may be rewritten on open.
    let wrong_id = "f".repeat(64);
    let cases = [
        format!("archives/{wrong_id}.cbz"),
        format!("archives/{book_id}.foo"),
        format!("covers/{book_id}.png"),
        format!("other/{book_id}.cbz"),
        "D:\\elsewhere\\archive.cbz".to_string(),
    ];
    {
        let connection = rusqlite::Connection::open(root.join("library.sqlite3")).unwrap();
        for case in &cases {
            connection
                .execute(
                    "UPDATE books SET archive_path = ?1 WHERE id = ?2",
                    rusqlite::params![case, book_id],
                )
                .unwrap();
            drop(Library::open(&root).expect("reopen while the row is non-canonical"));
            assert_eq!(
                column_text(&root, &book_id, "archive_path"),
                *case,
                "non-managed value must be preserved verbatim: {case}"
            );
        }
    }
}
