use egui::{ColorImage, Context};
use std::{
    collections::VecDeque,
    path::PathBuf,
    sync::{
        Arc, RwLock,
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    thread,
};
use yejian_core::{Book, ChapterLink, ChapterMetadata, Library};

pub enum Command {
    Refresh,
    Import(Vec<PathBuf>),
    ImportChapter {
        task_id: String,
        path: PathBuf,
        metadata: ChapterMetadata,
        read_page: Option<usize>,
    },
    Image {
        id: String,
        page: usize,
        cover: bool,
        generation: u64,
    },
    Progress {
        id: String,
        page: usize,
        fraction: f32,
        private: bool,
    },
    Setting(String, String),
    Category(Vec<String>, String),
    Remove(Vec<String>),
    Backup(PathBuf),
    Restore(PathBuf),
    Flush(mpsc::Sender<Result<(), String>>),
    #[cfg(test)]
    BlockLibrary {
        entered: mpsc::Sender<()>,
        release: mpsc::Receiver<()>,
    },
}
pub enum Event {
    Ready(Vec<Book>, Vec<(String, String)>, Vec<ChapterLink>),
    Library(Vec<Book>, Vec<ChapterLink>),
    Image {
        id: String,
        page: usize,
        cover: bool,
        generation: u64,
        result: Result<DecodedPage, String>,
    },
    Notice(String),
    Error(String),
    StartupError(String),
    BackupReady(PathBuf),
    ChapterImported(String),
    ChapterImportFailed {
        id: String,
        error: String,
    },
    ProgressSaved {
        id: String,
        page: usize,
        fraction: f32,
    },
    ProgressSaveFailed {
        id: String,
        page: usize,
        fraction: f32,
        private: bool,
        error: String,
    },
}
enum PositionCommand {
    Save {
        id: String,
        page: usize,
        fraction: f32,
        private: bool,
    },
    Flush(mpsc::Sender<Result<(), String>>),
}
/// Image result for a page of an online chapter that has not downloaded yet.
pub const PAGE_NOT_READY: &str = "page-not-ready";

pub struct DecodedPage {
    pub tiles: Vec<ColorImage>,
    pub size: [usize; 2],
    /// Normalized `[left, top, right, bottom]` of the drawn content inside
    /// the page once uniform white/black margins are ignored.
    pub content: [f32; 4],
}
struct ImageRequest {
    id: String,
    page: usize,
    cover: bool,
    generation: u64,
}
pub struct Worker {
    pub tx: mpsc::Sender<Command>,
    pub rx: mpsc::Receiver<Event>,
    pub generation: Arc<AtomicU64>,
    position_tx: mpsc::Sender<PositionCommand>,
    image_tx: mpsc::Sender<ImageRequest>,
}
impl Worker {
    pub fn start(root: PathBuf, ctx: Context) -> Self {
        let (tx, command_rx) = mpsc::channel();
        let (event_tx, rx) = mpsc::channel();
        let (position_tx, position_rx) = mpsc::channel();
        let position_queue = position_tx.clone();
        let (image_tx, image_rx) = mpsc::channel();
        let image_queue = image_tx.clone();
        let generation = Arc::new(AtomicU64::new(0));
        let latest = generation.clone();
        let archive_access = Arc::new(RwLock::new(()));
        thread::Builder::new()
            .name("yejian-library".into())
            .spawn(move || {
                let notify = |event| {
                    let _ = event_tx.send(event);
                    ctx.request_repaint();
                };
                let library = match Library::open(&root) {
                    Ok(library) => library,
                    Err(e) => {
                        notify(Event::StartupError(format!("无法打开书库：{e:#}")));
                        return;
                    }
                };
                // Reading writes use their own connection and FIFO lane.
                // Archive copies and speculative decoding cannot delay them.
                let position_library=match Library::open(&root) {
                    Ok(library)=>library,
                    Err(e)=>{notify(Event::StartupError(format!("无法打开阅读位置存储：{e:#}")));return;}
                };
                let image_library = match Library::open(&root) {
                    Ok(library) => library,
                    Err(error) => {
                        notify(Event::StartupError(format!("无法打开漫画图片存储：{error:#}")));
                        return;
                    }
                };
                let image_events = event_tx.clone();
                let image_ctx = ctx.clone();
                let image_archives = archive_access.clone();
                let image_root = root.clone();
                thread::Builder::new()
                    .name("yejian-images".into())
                    .spawn(move || {
                        read_images(
                            image_root,
                            image_library,
                            image_rx,
                            image_events,
                            image_ctx,
                            latest,
                            image_archives,
                        );
                    })
                    .expect("Unable to start image worker");
                let position_events=event_tx.clone();let position_ctx=ctx.clone();
                thread::Builder::new().name("yejian-progress".into()).spawn(move||{
                    let mut last_error:Option<String>=None;
                    while let Ok(command)=position_rx.recv() {
                        match command {
                            PositionCommand::Save{id,page,fraction,private}=>{
                                match position_library.save_position(&id,page,fraction,private) {
                                    Ok(())=>{last_error=None;let _=position_events.send(Event::ProgressSaved{id,page,fraction:fraction.clamp(0.,1.)});}
                                    Err(error)=>{let error=format!("阅读位置保存失败：{error:#}");last_error=Some(error.clone());let _=position_events.send(Event::ProgressSaveFailed{id,page,fraction,private,error});}
                                }
                                position_ctx.request_repaint();
                            }
                            PositionCommand::Flush(reply)=>{let _=reply.send(last_error.clone().map_or(Ok(()),Err));}
                        }
                    }
                }).expect("Unable to start reading-position worker");
                // A refresh or backup requested after a position change must
                // see the committed row, even with two SQLite connections.
                let wait_for_positions = || -> anyhow::Result<()> {
                    let (reply, committed) = mpsc::channel();
                    position_queue.send(PositionCommand::Flush(reply))
                        .map_err(|_| anyhow::anyhow!("阅读位置线程已停止"))?;
                    // Individual save failures already reach the UI with a retry
                    // action. Its last readable snapshot can still be backed up.
                    let _last_save = committed.recv()
                        .map_err(|_| anyhow::anyhow!("无法确认阅读位置写入顺序"))?;
                    Ok(())
                };
                let settings = match read_settings(&library) {
                    Ok(settings) => settings,
                    Err(e) => {
                        notify(Event::StartupError(format!("无法读取阅读设置：{e:#}")));
                        return;
                    }
                };
                match library
                    .list_books("")
                    .and_then(|books| Ok((books, library.list_chapter_links()?)))
                {
                    Ok((books, links)) => notify(Event::Ready(books, settings, links)),
                    Err(e) => {
                        notify(Event::StartupError(format!("无法读取书库：{e:#}")));
                        return;
                    }
                }
                while let Ok(command) = command_rx.recv() {
                    let result: anyhow::Result<()> = (|| {
                        match command {
                            Command::Refresh => {
                                wait_for_positions()?;
                                notify(Event::Library(library.list_books("")?, library.list_chapter_links()?));
                            }
                            Command::ImportChapter {
                                task_id,
                                path,
                                metadata,
                                read_page,
                            } => {
                                let result=(||->anyhow::Result<()> {
                                    let book=library.import(&path)?;
                                    library.link_chapter(&book.id,&metadata)?;
                                    if let Some(page)=read_page.filter(|_| book.current_page==0) {
                                        library.save_progress(&book.id,page.min(book.page_count.saturating_sub(1)),false)?;
                                    }
                                    Ok(())
                                })();
                                if let Err(error)=result {
                                    notify(Event::ChapterImportFailed{id:task_id,error:format!("{error:#}")});
                                    return Ok(());
                                }
                                notify(Event::ChapterImported(task_id));
                                notify(Event::Library(
                                    library.list_books("")?,
                                    library.list_chapter_links()?,
                                ));
                                notify(Event::Notice(format!(
                                    "已加入书架：{} · {}",
                                    metadata.manga_title, metadata.chapter_title
                                )));
                            }
                            Command::Import(paths) => {
                                let mut imported = 0;
                                let mut errors = Vec::new();
                                for path in paths {
                                    match library.import(&path) {
                                        Ok(_) => imported += 1,
                                        Err(e) => errors.push(format!(
                                            "{}：{e:#}",
                                            path.file_name().unwrap_or_default().to_string_lossy()
                                        )),
                                    }
                                }
                                notify(Event::Library(
                                    library.list_books("")?,
                                    library.list_chapter_links()?,
                                ));
                                if errors.is_empty() {
                                    notify(Event::Notice(format!(
                                        "已导入 {imported} 本，书库已保存"
                                    )));
                                } else {
                                    notify(Event::Error(format!(
                                        "已导入 {imported} 本；以下文件未导入：\n{}",
                                        errors.join("\n")
                                    )));
                                }
                            }
                            Command::Image {
                                id,
                                page,
                                cover,
                                generation,
                            } => {
                                image_queue.send(ImageRequest { id, page, cover, generation })
                                    .map_err(|_| anyhow::anyhow!("图片读取线程已停止"))?;
                            }
                            Command::Progress {
                                id,
                                page,
                                fraction,
                                private,
                            } => {
                                position_queue.send(PositionCommand::Save{id,page,fraction,private})
                                    .map_err(|_|anyhow::anyhow!("阅读位置线程已停止"))?;
                            }
                            Command::Flush(reply)=>{
                                let _=position_queue.send(PositionCommand::Flush(reply));
                            }
                            Command::Setting(key, value) => {
                                library.set_setting(&key, &value)?;
                            }
                            Command::Category(ids, category) => {
                                for id in ids {
                                    library.set_category(&id, &category)?;
                                }
                                notify(Event::Library(
                                    library.list_books("")?,
                                    library.list_chapter_links()?,
                                ));
                            }
                            Command::Remove(ids) => {
                                for id in ids {
                                    library.remove_from_library(&id)?;
                                }
                                notify(Event::Library(
                                    library.list_books("")?,
                                    library.list_chapter_links()?,
                                ));
                            }
                            Command::Backup(path) => {
                                wait_for_positions()?;
                                let report = library.export_backup(&path)?;
                                if report.is_complete() {
                                    notify(Event::Notice(format!(
                                        "{} 本书已备份到 {}",
                                        report.books_exported,
                                        path.display()
                                    )));
                                } else {
                                    notify(Event::Error(format!(
                                        "已备份 {} 本；以下内容未备份：\n{}",
                                        report.books_exported,
                                        report
                                            .skipped
                                            .iter()
                                            .map(|s| format!("{}：{}", s.title, s.reason))
                                            .collect::<Vec<_>>()
                                            .join("\n")
                                    )));
                                }
                                notify(Event::BackupReady(path));
                            }
                            Command::Restore(path) => {
                                wait_for_positions()?;
                                // Restore may repair an existing archive or replace its
                                // cover. Readers take the matching lock only while
                                // fetching bytes, not during decoding.
                                let _archive_guard = archive_access.write()
                                    .map_err(|_| anyhow::anyhow!("漫画档案锁已失效"))?;
                                let report = library.restore(&path)?;
                                notify(Event::Ready(
                                    library.list_books("")?,
                                    read_settings(&library)?,
                                    library.list_chapter_links()?,
                                ));
                                let message = format!(
                                    "恢复完成：新增 {} 本，更新 {} 本，跳过 {} 本，失败 {} 本；章节关联未恢复 {} 条",
                                    report.imported_count(),
                                    report.updated_count(),
                                    report.skipped_count(),
                                    report.failed_count(),
                                    report.link_failures.len()
                                );
                                if report.is_fully_restored() {
                                    notify(Event::Notice(message));
                                } else {
                                    notify(Event::Error(format!(
                                        "{message}\n{}",
                                        report
                                            .skipped
                                            .iter()
                                            .chain(report.failures.iter())
                                            .chain(report.link_failures.iter())
                                            .map(|r| format!(
                                                "{}：{}",
                                                r.title,
                                                r.reason.as_deref().unwrap_or("未恢复")
                                            ))
                                            .collect::<Vec<_>>()
                                            .join("\n")
                                    )));
                                }
                            }
                            #[cfg(test)]
                            Command::BlockLibrary { entered, release } => {
                                let _ = entered.send(());
                                let _ = release.recv();
                            }
                        }
                        Ok(())
                    })();
                    if let Err(e) = result {
                        notify(Event::Error(format!("{e:#}")));
                    }
                }
            })
            .expect("Unable to start library worker");
        Self {
            tx,
            rx,
            generation,
            position_tx,
            image_tx,
        }
    }
    pub fn send(&self, command: Command) -> Result<(), String> {
        if let Command::Progress {
            id,
            page,
            fraction,
            private,
        } = command
        {
            return self
                .position_tx
                .send(PositionCommand::Save {
                    id,
                    page,
                    fraction,
                    private,
                })
                .map_err(|_| "阅读位置线程已停止，请重新打开应用".into());
        }
        if let Command::Image {
            id,
            page,
            cover,
            generation,
        } = command
        {
            return self
                .image_tx
                .send(ImageRequest {
                    id,
                    page,
                    cover,
                    generation,
                })
                .map_err(|_| "图片读取线程已停止，请重新打开应用".into());
        }
        self.tx
            .send(command)
            .map_err(|_| "书库线程已停止，请退出后重新打开应用".into())
    }
    pub fn flush(&self, timeout: std::time::Duration) -> Result<(), String> {
        let (tx, rx) = mpsc::channel();
        self.send(Command::Flush(tx))?;
        rx.recv_timeout(timeout)
            .map_err(|_| "等待书库和阅读位置保存超时".to_string())?
    }
}

fn next_image(pending: &mut VecDeque<ImageRequest>, generation: u64) -> Option<ImageRequest> {
    pending.retain(|request| request.cover || request.generation == generation);
    // The UI submits the visible page first and neighbours afterwards.
    // Preserve that order, but never put reading behind shelf thumbnails.
    let index = pending
        .iter()
        .position(|request| !request.cover)
        .unwrap_or(0);
    pending.remove(index)
}

fn read_images(
    root: PathBuf,
    library: Library,
    requests: mpsc::Receiver<ImageRequest>,
    events: mpsc::Sender<Event>,
    ctx: Context,
    latest: Arc<AtomicU64>,
    archive_access: Arc<RwLock<()>>,
) {
    let mut pending = VecDeque::new();
    loop {
        if pending.is_empty() {
            match requests.recv() {
                Ok(request) => pending.push_back(request),
                Err(_) => return,
            }
        }
        // Requests contain ids only. Stale image pixels are never decoded
        // just to be discarded later by the UI.
        pending.extend(requests.try_iter().take(1024));
        let Some(request) = next_image(&mut pending, latest.load(Ordering::Relaxed)) else {
            continue;
        };
        let current = || request.cover || request.generation == latest.load(Ordering::Relaxed);
        let bytes = (|| -> anyhow::Result<Vec<u8>> {
            let _guard = archive_access
                .read()
                .map_err(|_| anyhow::anyhow!("漫画档案锁已失效"))?;
            if !current() {
                anyhow::bail!("阅读请求已失效");
            }
            if let Some(task) = request.id.strip_prefix(crate::sources::STREAM_PREFIX) {
                return crate::sources::stream_page(&root, task, request.page)?
                    .ok_or_else(|| anyhow::anyhow!(PAGE_NOT_READY));
            }
            if request.cover {
                let book = library.get_book(&request.id)?;
                match book.cover_path {
                    Some(path) => Ok(std::fs::read(path)?),
                    None => library.page_bytes(&request.id, 0),
                }
            } else {
                library.page_bytes(&request.id, request.page)
            }
        })();
        if !current() {
            continue;
        }
        let result = match bytes {
            Err(error) if error.to_string() == PAGE_NOT_READY => Err(PAGE_NOT_READY.to_string()),
            bytes => bytes
                .and_then(|bytes| decode(&bytes, request.cover))
                .map_err(|error| format!("{error:#}")),
        };
        if !current() {
            continue;
        }
        if events
            .send(Event::Image {
                id: request.id,
                page: request.page,
                cover: request.cover,
                generation: request.generation,
                result,
            })
            .is_err()
        {
            return;
        }
        ctx.request_repaint();
    }
}

fn read_settings(library: &Library) -> anyhow::Result<Vec<(String, String)>> {
    let mut settings = Vec::new();
    for key in [
        "theme",
        "reduce-motion",
        "reading-direction",
        "private",
        "reading-mode",
        "font-scale",
        "reader-crop",
        "reader-fit",
        "reader-brightness",
        "reader-spread-offset",
        "follow-interval",
    ] {
        if let Some(value) = library.get_setting(key)? {
            settings.push((key.to_string(), value));
        }
    }
    Ok(settings)
}

pub(super) fn image_reader(
    bytes: &[u8],
) -> anyhow::Result<image::ImageReader<std::io::Cursor<&[u8]>>> {
    let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format()?;
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(128 * 1024 * 1024);
    limits.max_image_width = Some(32000);
    limits.max_image_height = Some(64000);
    reader.limits(limits);
    Ok(reader)
}

pub fn decode(bytes: &[u8], cover: bool) -> anyhow::Result<DecodedPage> {
    let mut image = image_reader(bytes)?.decode()?;
    if cover {
        image = image.thumbnail(512, 768);
    } else if image.width() > 3072 {
        let height = (image.height() as f64 * 3072. / image.width() as f64)
            .round()
            .max(1.) as u32;
        image = image.resize_exact(3072, height, image::imageops::FilterType::Triangle);
    }
    let image = image.into_rgba8();
    let size = [image.width() as usize, image.height() as usize];
    let content = if cover {
        [0., 0., 1., 1.]
    } else {
        content_bounds(&image)
    };
    let mut tiles = Vec::new();
    for top in (0..image.height()).step_by(2048) {
        let height = (image.height() - top).min(2048);
        let stride = image.width() as usize * 4;
        let start = top as usize * stride;
        let end = start + height as usize * stride;
        tiles.push(ColorImage::from_rgba_unmultiplied(
            [image.width() as usize, height as usize],
            &image.as_raw()[start..end],
        ));
    }
    Ok(DecodedPage {
        tiles,
        size,
        content,
    })
}

/// Bounds of the drawn area when the page is wrapped in a near-uniform white
/// or black border. Rows and columns are sampled on a coarse grid; a side
/// keeps shrinking while every sampled pixel sits within 12 luminance of the
/// border color, but never removes more than a quarter of the dimension.
pub(super) fn content_bounds(image: &image::RgbaImage) -> [f32; 4] {
    let full = [0., 0., 1., 1.];
    let (w, h) = (image.width() as usize, image.height() as usize);
    if w < 8 || h < 8 {
        return full;
    }
    let step = (w.max(h) / 96).max(1);
    let luminance = |x: usize, y: usize| -> i32 {
        let p = image.get_pixel(x as u32, y as u32).0;
        (p[0] as i32 * 299 + p[1] as i32 * 587 + p[2] as i32 * 114 + 500) / 1000
    };
    let mut corners = [
        luminance(0, 0),
        luminance(w - 1, 0),
        luminance(0, h - 1),
        luminance(w - 1, h - 1),
    ];
    corners.sort_unstable();
    let border = (corners[1] + corners[2]) / 2;
    if !(border <= 24 || border >= 231) {
        return full;
    }
    let border_row = |y: usize| {
        (0..w)
            .step_by(step)
            .all(|x| (luminance(x, y) - border).abs() <= 12)
    };
    let border_column = |x: usize| {
        (0..h)
            .step_by(step)
            .all(|y| (luminance(x, y) - border).abs() <= 12)
    };
    let mut top = 0;
    while top < h / 4 && border_row(top) {
        top += 1;
    }
    let mut bottom = h;
    while bottom > h - h / 4 && bottom > top && border_row(bottom - 1) {
        bottom -= 1;
    }
    let mut left = 0;
    while left < w / 4 && border_column(left) {
        left += 1;
    }
    let mut right = w;
    while right > w - w / 4 && right > left && border_column(right - 1) {
        right -= 1;
    }
    if top == 0 && bottom == h && left == 0 && right == w {
        return full;
    }
    [
        left as f32 / w as f32,
        top as f32 / h as f32,
        right as f32 / w as f32,
        bottom as f32 / h as f32,
    ]
}

#[cfg(test)]
mod tests {
    fn small_archive(path: &std::path::Path) {
        use std::io::Write;
        let mut png = std::io::Cursor::new(Vec::new());
        image::RgbImage::from_pixel(4, 4, image::Rgb([55, 75, 95]))
            .write_to(&mut png, image::ImageFormat::Png)
            .unwrap();
        let mut writer = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
        writer
            .start_file("1.png", zip::write::SimpleFileOptions::default())
            .unwrap();
        writer.write_all(png.get_ref()).unwrap();
        writer.finish().unwrap();
    }

    #[test]
    fn image_queue_discards_old_pages_and_prioritizes_reading_over_covers() {
        let request = |id: &str, page, cover, generation| super::ImageRequest {
            id: id.into(),
            page,
            cover,
            generation,
        };
        let mut pending = std::collections::VecDeque::from([
            request("old", 0, false, 7),
            request("cover-a", 0, true, 0),
            request("old", 1, false, 7),
            request("current", 3, false, 8),
            request("current", 4, false, 8),
            request("cover-b", 0, true, 0),
        ]);
        let first = super::next_image(&mut pending, 8).unwrap();
        assert_eq!((first.id.as_str(), first.page), ("current", 3));
        let second = super::next_image(&mut pending, 8).unwrap();
        assert_eq!((second.id.as_str(), second.page), ("current", 4));
        assert_eq!(super::next_image(&mut pending, 8).unwrap().id, "cover-a");
        assert_eq!(super::next_image(&mut pending, 8).unwrap().id, "cover-b");
        assert!(super::next_image(&mut pending, 8).is_none());
    }

    #[test]
    fn visible_page_and_position_save_do_not_wait_for_bulk_library_work() {
        let tmp = tempfile::tempdir().unwrap();
        let archive = tmp.path().join("comic.cbz");
        small_archive(&archive);
        let root = tmp.path().join("library");
        let library = yejian_core::Library::open(&root).unwrap();
        let book = library.import(&archive).unwrap();
        let worker = super::Worker::start(root, egui::Context::default());
        let timeout = std::time::Duration::from_secs(5);
        assert!(matches!(
            worker.rx.recv_timeout(timeout).unwrap(),
            super::Event::Ready(..)
        ));

        // Hold the exact lane used by bulk import/backup, without making the
        // test depend on filesystem speed or a huge artificial archive.
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        worker
            .send(super::Command::BlockLibrary {
                entered: entered_tx,
                release: release_rx,
            })
            .unwrap();
        entered_rx.recv_timeout(timeout).unwrap();
        worker
            .generation
            .store(9, std::sync::atomic::Ordering::Relaxed);
        worker
            .send(super::Command::Image {
                id: "old-invalid-book".into(),
                page: 0,
                cover: false,
                generation: 8,
            })
            .unwrap();
        worker
            .send(super::Command::Image {
                id: book.id.clone(),
                page: 0,
                cover: false,
                generation: 9,
            })
            .unwrap();
        worker
            .send(super::Command::Progress {
                id: book.id.clone(),
                page: 0,
                fraction: 0.4,
                private: false,
            })
            .unwrap();
        let mut image_ready = false;
        let mut position_saved = false;
        while !image_ready || !position_saved {
            match worker.rx.recv_timeout(timeout).unwrap() {
                super::Event::Image {
                    id,
                    generation,
                    result,
                    ..
                } => {
                    assert_eq!(
                        id, book.id,
                        "obsolete requests must not emit decoded results or errors"
                    );
                    assert_eq!(generation, 9);
                    assert_eq!(result.unwrap().size, [4, 4]);
                    image_ready = true;
                }
                super::Event::ProgressSaved { id, fraction, .. } => {
                    assert_eq!(id, book.id);
                    assert!((fraction - 0.4).abs() < 0.0001);
                    position_saved = true;
                }
                _ => panic!("unexpected event while the library work lane is blocked"),
            }
        }
        assert!((library.get_book(&book.id).unwrap().page_fraction - 0.4).abs() < 0.0001);
        release_tx.send(()).unwrap();
        worker.flush(timeout).unwrap();
    }

    #[test]
    fn image_worker_reads_new_imports_and_restore_repairs_from_its_existing_connection() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("library");
        let worker = super::Worker::start(root.clone(), egui::Context::default());
        let timeout = std::time::Duration::from_secs(5);
        assert!(matches!(
            worker.rx.recv_timeout(timeout).unwrap(),
            super::Event::Ready(..)
        ));
        let archive = tmp.path().join("comic.cbz");
        small_archive(&archive);
        worker.send(super::Command::Import(vec![archive])).unwrap();
        let book = loop {
            if let super::Event::Library(books, _) = worker.rx.recv_timeout(timeout).unwrap() {
                break books.into_iter().next().unwrap();
            }
        };
        worker
            .generation
            .store(1, std::sync::atomic::Ordering::Relaxed);
        worker
            .send(super::Command::Image {
                id: book.id.clone(),
                page: 0,
                cover: false,
                generation: 1,
            })
            .unwrap();
        loop {
            match worker.rx.recv_timeout(timeout).unwrap() {
                super::Event::Image {
                    result,
                    id,
                    generation,
                    ..
                } => {
                    assert_eq!(id, book.id);
                    assert_eq!(generation, 1);
                    assert_eq!(result.unwrap().size, [4, 4]);
                    break;
                }
                super::Event::Error(error) => panic!("{error}"),
                _ => {}
            }
        }
        let library = yejian_core::Library::open(&root).unwrap();
        let backup = tmp.path().join("backup.zip");
        library.export_backup(&backup).unwrap();
        std::fs::remove_file(&book.archive_path).unwrap();
        assert!(!book.archive_path.exists());
        worker.send(super::Command::Restore(backup)).unwrap();
        loop {
            match worker.rx.recv_timeout(timeout).unwrap() {
                super::Event::Ready(books, _, _) => {
                    assert_eq!(books.len(), 1);
                    assert_eq!(books[0].id, book.id);
                    assert_eq!(books[0].current_page, book.current_page);
                    break;
                }
                super::Event::Error(error) => panic!("{error}"),
                _ => {}
            }
        }
        // Restoration repairs missing files before comparing reading state.
        // The unchanged reading state is intentionally counted as skipped;
        // consume and verify that summary instead of mistaking it for a
        // failed archive repair when the subsequent image result arrives.
        match worker.rx.recv_timeout(timeout).unwrap() {
            super::Event::Error(summary) => {
                assert!(summary.contains("新增 0 本，更新 0 本，跳过 1 本，失败 0 本"));
                assert!(summary.contains("章节关联未恢复 0 条"));
                assert!(summary.contains("already present with the same reading state"));
            }
            _ => panic!("expected the unchanged-reading-state restoration summary"),
        }
        assert!(
            book.archive_path.is_file(),
            "restore must repair the archive"
        );
        worker
            .generation
            .store(2, std::sync::atomic::Ordering::Relaxed);
        worker
            .send(super::Command::Image {
                id: book.id.clone(),
                page: 0,
                cover: false,
                generation: 2,
            })
            .unwrap();
        loop {
            match worker.rx.recv_timeout(timeout).unwrap() {
                super::Event::Image {
                    result,
                    id,
                    generation,
                    ..
                } => {
                    assert_eq!(id, book.id);
                    assert_eq!(generation, 2);
                    let page = result.unwrap();
                    assert_eq!(page.size, [4, 4]);
                    assert_eq!(page.tiles[0].pixels[0], egui::Color32::from_rgb(55, 75, 95));
                    break;
                }
                super::Event::Error(error) => panic!("{error}"),
                _ => {}
            }
        }
    }

    #[test]
    fn position_flush_reports_failures_and_waits_for_a_corrected_save() {
        use std::io::Write;
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("library");
        let mut png = std::io::Cursor::new(Vec::new());
        image::RgbImage::from_pixel(4, 4, image::Rgb([55, 75, 95]))
            .write_to(&mut png, image::ImageFormat::Png)
            .unwrap();
        let archive = tmp.path().join("comic.cbz");
        let mut writer = zip::ZipWriter::new(std::fs::File::create(&archive).unwrap());
        writer
            .start_file("1.png", zip::write::SimpleFileOptions::default())
            .unwrap();
        writer.write_all(png.get_ref()).unwrap();
        writer.finish().unwrap();
        let library = yejian_core::Library::open(&root).unwrap();
        let book = library.import(&archive).unwrap();
        let worker = super::Worker::start(root, egui::Context::default());
        let timeout = std::time::Duration::from_secs(5);
        assert!(matches!(
            worker.rx.recv_timeout(timeout).unwrap(),
            super::Event::Ready(_, _, _)
        ));
        worker
            .send(super::Command::Progress {
                id: book.id.clone(),
                page: 99,
                fraction: 0.5,
                private: false,
            })
            .unwrap();
        assert!(worker.flush(timeout).unwrap_err().contains("保存失败"));
        assert!(matches!(
            worker.rx.recv_timeout(timeout).unwrap(),
            super::Event::ProgressSaveFailed { page: 99, .. }
        ));
        // The original public command sender is also preserved: the normal
        // flush forwards its barrier behind any reading writes it dispatches.
        worker
            .tx
            .send(super::Command::Progress {
                id: book.id.clone(),
                page: 0,
                fraction: 0.35,
                private: false,
            })
            .unwrap();
        worker.flush(timeout).unwrap();
        assert!((library.get_book(&book.id).unwrap().page_fraction - 0.35).abs() < 0.0001);

        // Back requests a refresh immediately after a save. Backup likewise
        // has to include every earlier write without waiting in the UI.
        worker
            .send(super::Command::Progress {
                id: book.id.clone(),
                page: 0,
                fraction: 0.65,
                private: false,
            })
            .unwrap();
        worker.send(super::Command::Refresh).unwrap();
        let backup = tmp.path().join("reading-position.zip");
        worker
            .tx
            .send(super::Command::Progress {
                id: book.id.clone(),
                page: 0,
                fraction: 0.85,
                private: false,
            })
            .unwrap();
        worker.send(super::Command::Backup(backup.clone())).unwrap();
        let mut saw_refresh = false;
        loop {
            match worker.rx.recv_timeout(timeout).unwrap() {
                super::Event::Library(books, _) => {
                    assert!((books[0].page_fraction - 0.65).abs() < 0.0001);
                    saw_refresh = true;
                }
                super::Event::BackupReady(_) => break,
                super::Event::Error(error) => panic!("{error}"),
                _ => {}
            }
        }
        assert!(saw_refresh);
        let copy = yejian_core::Library::open(&tmp.path().join("restored")).unwrap();
        copy.import_backup(&backup).unwrap();
        assert!((copy.get_book(&book.id).unwrap().page_fraction - 0.85).abs() < 0.0001);
    }
    #[test]
    fn downloaded_chapter_is_acknowledged_only_after_import_and_link() {
        use std::io::Write;
        let tmp = tempfile::tempdir().unwrap();
        let archive = tmp.path().join("chapter.cbz");
        let mut png = std::io::Cursor::new(Vec::new());
        image::RgbImage::from_pixel(4, 4, image::Rgb([35, 65, 95]))
            .write_to(&mut png, image::ImageFormat::Png)
            .unwrap();
        let mut writer = zip::ZipWriter::new(std::fs::File::create(&archive).unwrap());
        writer
            .start_file("1.png", zip::write::SimpleFileOptions::default())
            .unwrap();
        writer.write_all(png.get_ref()).unwrap();
        writer.finish().unwrap();
        let worker = super::Worker::start(tmp.path().join("library"), egui::Context::default());
        let timeout = std::time::Duration::from_secs(5);
        assert!(matches!(
            worker.rx.recv_timeout(timeout).unwrap(),
            super::Event::Ready(_, _, _)
        ));
        let metadata = yejian_core::ChapterMetadata {
            source_id: "fixture".into(),
            manga_id: "story".into(),
            manga_title: "作品".into(),
            chapter_id: "chapter-1".into(),
            chapter_title: "第1话".into(),
            chapter_number: Some(1.),
            volume_number: None,
        };
        worker
            .send(super::Command::ImportChapter {
                task_id: "failed".into(),
                path: tmp.path().join("missing.cbz"),
                metadata: metadata.clone(),
                read_page: None,
            })
            .unwrap();
        assert!(
            matches!(worker.rx.recv_timeout(timeout).unwrap(),super::Event::ChapterImportFailed{id,..} if id=="failed")
        );
        worker
            .send(super::Command::ImportChapter {
                task_id: "finished".into(),
                path: archive.clone(),
                metadata,
                read_page: None,
            })
            .unwrap();
        assert!(
            matches!(worker.rx.recv_timeout(timeout).unwrap(),super::Event::ChapterImported(id) if id=="finished")
        );
        match worker.rx.recv_timeout(timeout).unwrap() {
            super::Event::Library(books, links) => {
                assert_eq!(books.len(), 1);
                assert_eq!(links.len(), 1);
                assert_eq!(books[0].id, links[0].book_id);
            }
            _ => panic!("successful import must update books and chapter links together"),
        }
        assert!(
            archive.exists(),
            "import must retain the completed download for recovery"
        );
    }
    #[test]
    fn restored_settings_are_delivered_to_the_running_ui() {
        let tmp = tempfile::tempdir().unwrap();
        let original = yejian_core::Library::open(&tmp.path().join("original")).unwrap();
        original.set_setting("theme", "dark").unwrap();
        original.set_setting("reading-direction", "rtl").unwrap();
        let backup = tmp.path().join("settings.zip");
        original.export_backup(&backup).unwrap();
        let worker = super::Worker::start(tmp.path().join("destination"), egui::Context::default());
        let deadline = std::time::Duration::from_secs(5);
        assert!(matches!(
            worker.rx.recv_timeout(deadline).unwrap(),
            super::Event::Ready(_, _, _)
        ));
        worker.send(super::Command::Restore(backup)).unwrap();
        match worker.rx.recv_timeout(deadline).unwrap() {
            super::Event::Ready(_, settings, _) => {
                assert!(settings.contains(&("theme".into(), "dark".into())));
                assert!(settings.contains(&("reading-direction".into(), "rtl".into())));
            }
            _ => panic!("restore must refresh settings together with the library"),
        }
    }
    #[test]
    fn long_comic_keeps_its_width_and_tiles_below_gpu_limits() {
        let image = image::RgbImage::from_fn(320, 9000, |x, y| {
            image::Rgb([(x % 251) as u8, (y % 251) as u8, 43])
        });
        let mut encoded = std::io::Cursor::new(Vec::new());
        image
            .write_to(&mut encoded, image::ImageFormat::Png)
            .unwrap();
        let decoded = super::decode(encoded.get_ref(), false).unwrap();
        assert_eq!(decoded.size, [320, 9000]);
        assert_eq!(decoded.tiles.len(), 5);
        assert!(decoded.tiles.iter().all(|t| t.size[1] <= 2048));
        assert_eq!(decoded.tiles.iter().map(|t| t.size[1]).sum::<usize>(), 9000);
        let mut top = 0;
        for tile in &decoded.tiles {
            for row in [0, tile.size[1] - 1] {
                for x in [0, 319] {
                    assert_eq!(
                        tile.pixels[row * 320 + x],
                        egui::Color32::from_rgb((x % 251) as u8, ((top + row) % 251) as u8, 43),
                    );
                }
            }
            top += tile.size[1];
        }
    }

    fn bordered_image(border: u8, border_px: u32) -> image::RgbaImage {
        image::RgbaImage::from_fn(200, 160, |x, y| {
            let inside = x >= border_px
                && x < 200 - border_px
                && y >= border_px
                && y < 160 - border_px;
            if inside {
                image::Rgba([40, 60, 80, 255])
            } else {
                image::Rgba([border, border, border, 255])
            }
        })
    }

    #[test]
    fn content_bounds_find_uniform_borders_and_respect_the_cap() {
        for border in [0u8, 12, 250, 255] {
            let bounds = super::content_bounds(&bordered_image(border, 24));
            assert!(
                (bounds[0] - 24. / 200.).abs() < 0.02
                    && (bounds[1] - 24. / 160.).abs() < 0.02
                    && (bounds[2] - 176. / 200.).abs() < 0.02
                    && (bounds[3] - 136. / 160.).abs() < 0.02,
                "border {border} cropped wrong: {bounds:?}"
            );
        }
        // Nothing to crop stays untouched.
        let flat = image::RgbaImage::from_pixel(120, 90, image::Rgba([128, 40, 60, 255]));
        assert_eq!(super::content_bounds(&flat), [0., 0., 1., 1.]);
        // Coloured or noisy borders are never removed.
        let noise = image::RgbaImage::from_fn(140, 100, |x, y| {
            image::Rgba([((x * 31 + y * 17) % 256) as u8, 90, 40, 255])
        });
        assert_eq!(super::content_bounds(&noise), [0., 0., 1., 1.]);
        // A page that is all margin is capped at a quarter per side.
        let blank = image::RgbaImage::from_pixel(200, 160, image::Rgba([255, 255, 255, 255]));
        let bounds = super::content_bounds(&blank);
        assert_eq!(bounds, [0.25, 0.25, 0.75, 0.75]);
    }
}
