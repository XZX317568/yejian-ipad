
use crate::follows::self;




use anyhow::Context as _;
use serde::Deserialize;
use serde::Serialize;

use std::collections::HashSet;


use std::io::Write;
use std::path::Path;
use std::path::PathBuf;

use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicU8;

use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::sync::mpsc;

use yejian_core::ChapterLink;
use yejian_core::ChapterMetadata;
use yejian_sources::*;
use super::*;
use super::tasks::DownloadTask;
use super::tasks::EnqueueOutcome;
use super::tasks::PartCleanup;
use super::tasks::TaskStore;
use super::ui_detail::chapter_list;
use super::ui_detail::chapter_name;
use super::ui_detail::chapter_number_value;
use super::tasks::enqueue_task;
use super::tasks::next_queued;

pub struct DownloadedChapter {
    pub id: String,
    pub path: PathBuf,
    pub metadata: ChapterMetadata,
    /// Page reached while reading online, carried into the library book.
    pub read_page: Option<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(super) enum DownloadStatus {
    Queued,
    Running,
    Paused,
    Failed,
    Done,
}

impl DownloadStatus {
    pub(super)fn label(self) -> &'static str {
        match self {
            Self::Queued => "排队中",
            Self::Running => "下载中",
            Self::Paused => "已暂停",
            Self::Failed => "失败",
            Self::Done => "已完成",
        }
    }
}

/// manga/chapter snapshots carried by each task.
#[derive(Clone)]
pub(super) struct ChapterQueueState {
    pub(super) status: DownloadStatus,
    pub(super) has_archive: bool,
    pub(super) done: usize,
    pub(super) total: usize,
    pub(super) read_page: Option<usize>,
    pub(super) read_at: Option<i64>,
}

pub(super) enum DlCmd {
    Run(DownloadTask),
}

pub(super) enum DlEvent {
    Progress {
        id: String,
        done: usize,
        total: usize,
        signature: Option<String>,
    },
    Finished {
        id: String,
        path: PathBuf,
    },
    Failed {
        id: String,
        error: String,
    },
    Aborted {
        id: String,
        how: u8,
    },
    LockUnavailable {
        id: String,
        error: String,
    },
}

pub(super) const STOP_NONE: u8 = 0;

pub(super) const STOP_PAUSE: u8 = 1;

pub(super) const STOP_CANCEL: u8 = 2;

pub(crate) fn identity_id(source: &str, manga: &str, chapter: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for part in [source, manga, chapter] {
        for byte in part.as_bytes().iter().chain(std::iter::once(&0xffu8)) {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    format!("{hash:016x}")
}

pub(super) fn work_root(root: &Path) -> PathBuf {
    root.join("download-work")
}

pub const STREAM_PREFIX: &str = "stream:";

pub(crate) fn stream_page(root: &Path, id: &str, page: usize) -> anyhow::Result<Option<Vec<u8>>> {
    let dir = task_dir(root, id)?;
    match stored_page(&dir, page + 1) {
        Some(path) => Ok(Some(std::fs::read(path)?)),
        None => Ok(None),
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct StreamChapter {
    pub id: String,
    pub metadata: ChapterMetadata,
    /// Entered backwards from the following chapter: start on its last page.
    pub from_end: bool,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct StreamState {
    pub total: usize,
    pub done: usize,
    pub stopped: Option<String>,
    /// Page-list signature; staged pages from another list are discarded.
    pub signature: Option<String>,
    /// Where the user left off in this chapter, kept across app restarts.
    pub page: Option<usize>,
}

pub(super) fn valid_task_id(id: &str) -> bool {
    id.len() == 16
        && id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

pub(super) fn task_dir(root: &Path, id: &str) -> anyhow::Result<PathBuf> {
    anyhow::ensure!(valid_task_id(id), "非法的下载任务 id：{id:?}");
    let work = work_root(root);
    let dir = work.join(format!("task-{id}"));
    anyhow::ensure!(
        dir.parent() == Some(work.as_path()),
        "下载任务路径越界：{}",
        dir.display()
    );
    match std::fs::symlink_metadata(&dir) {
        Ok(_) => {
            std::fs::create_dir_all(&work)?;
            let work_real = std::fs::canonicalize(&work)?;
            let dir_real = std::fs::canonicalize(&dir)?;
            anyhow::ensure!(
                dir_real.starts_with(&work_real),
                "下载任务路径指向 download-work 之外：{}",
                dir.display()
            );
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(anyhow::anyhow!("检查下载任务路径失败：{error}")),
    }
    Ok(dir)
}

pub(super) fn remove_task_cache(root: &Path, id: &str) -> anyhow::Result<()> {
    let dir = task_dir(root, id)?;
    match std::fs::symlink_metadata(&dir) {
        Ok(_) => std::fs::remove_dir_all(&dir)
            .map_err(|error| anyhow::anyhow!("清理缓存 {} 失败：{error}", dir.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(anyhow::anyhow!("检查缓存 {} 失败：{error}", dir.display())),
    }
}

pub(super) fn snapshot_root(root: &Path) -> PathBuf {
    work_root(root).join("snapshots")
}

pub(super) fn manga_snapshot_key(source: &str, manga: &str) -> String {
    identity_id(source, manga, "manga-snapshot")
}

pub(crate) fn manga_snapshot_path(root: &Path, key: &str) -> anyhow::Result<PathBuf> {
    anyhow::ensure!(valid_task_id(key), "非法的漫画快照 id：{key:?}");
    Ok(snapshot_root(root).join(format!("{key}.manga")))
}

pub(crate) fn save_manga_snapshot(root: &Path, manga: &Manga) -> anyhow::Result<String> {
    let key = manga_snapshot_key(&manga.source_key, &manga.key);
    let path = manga_snapshot_path(root, &key)?;
    let bytes = manga.to_postcard_bytes();
    if std::fs::read(&path).ok().as_deref() == Some(bytes.as_slice()) {
        return Ok(key);
    }
    let directory = snapshot_root(root);
    std::fs::create_dir_all(&directory)
        .map_err(|error| anyhow::anyhow!("创建漫画快照目录失败：{error}"))?;
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or(0);
    let part = directory.join(format!("{key}-{}-{nonce}.part", std::process::id()));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&part)
        .map_err(|error| anyhow::anyhow!("写入漫画快照失败：{error}"))?;
    let cleanup = PartCleanup::new(&part);
    file.write_all(&bytes)
        .and_then(|_| file.sync_all())
        .map_err(|error| anyhow::anyhow!("写入漫画快照失败：{error}"))?;
    drop(file);
    std::fs::rename(&part, &path)
        .map_err(|error| anyhow::anyhow!("保存漫画快照 {} 失败：{error}", path.display()))?;
    cleanup.keep();
    Ok(key)
}

pub(super) fn acquire_download_lock(root: &Path) -> anyhow::Result<std::fs::File> {
    let work = work_root(root);
    std::fs::create_dir_all(&work)?;
    let path = work.join(".download.lock");
    let file = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(&path)?;
    file.try_lock()
        .map_err(|error| anyhow::anyhow!("上一次下载仍在退出，请稍后重试（{error}）"))?;
    Ok(file)
}

pub(super) fn stored_page(dir: &Path, number: usize) -> Option<PathBuf> {
    ["jpg", "png", "webp"]
        .into_iter()
        .map(|extension| dir.join(format!("page-{number:04}.{extension}")))
        .find(|path| path.is_file())
}

pub(super) fn existing_pages(dir: &Path) -> HashSet<usize> {
    let mut found = HashSet::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return found;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with("page-") || name.ends_with(".part") {
            continue;
        }
        let number = &name["page-".len()..];
        let Some((digits, extension)) = number.split_once('.') else {
            continue;
        };
        if matches!(extension, "jpg" | "png" | "webp")
            && let Ok(value) = digits.parse::<usize>()
            && value > 0
            && entry.file_type().is_ok_and(|kind| kind.is_file())
        {
            found.insert(value);
        }
    }
    found
}

pub(super) fn page_signature(pages: &[Page]) -> String {
    use std::fmt::Write as _;
    let mut text = String::with_capacity(pages.len() * 48);
    for page in pages {
        match &page.content {
            PageContent::Url { url, context } => {
                let _ = write!(text, "u|{url}|");
                if let Some(context) = context {
                    for (name, value) in context {
                        let _ = write!(text, "{name}={value};");
                    }
                }
                text.push('\n');
            }
            PageContent::ZipFile { url, file_path } => {
                let _ = writeln!(text, "z|{url}|{file_path}");
            }
            PageContent::Text(body) => {
                let _ = writeln!(text, "t|{}", body.len());
            }
            PageContent::Image {
                image_ref,
                image_data,
            } => {
                let _ = writeln!(
                    text,
                    "i|{image_ref}|{}",
                    image_data.as_ref().map_or(0, |bytes| bytes.len())
                );
            }
        }
    }
    text
}

pub(super) fn reset_page_cache(dir: &Path) -> anyhow::Result<()> {
    if !dir.exists() {
        return Ok(());
    }
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with("page-") {
            std::fs::remove_file(entry.path())?;
        }
    }
    Ok(())
}

#[derive(Debug)]
pub(super) struct FetchSummary {
    pub(super) done: usize,
    pub(super) aborted: bool,
}

pub(super) fn cached_inline_page_matches(dir: &Path, number: usize, page: &Page) -> anyhow::Result<bool> {
    let PageContent::Image { image_data, .. } = &page.content else {
        return Ok(true);
    };
    let Some(expected) = image_data else {
        return Ok(false);
    };
    let Some(path) = stored_page(dir, number) else {
        return Ok(false);
    };
    Ok(std::fs::read(path)? == *expected)
}

pub(super) const PAGE_CONCURRENCY: usize = 4;

pub(super) fn fetch_missing_pages(
    dir: &Path,
    pages: &[Page],
    fetch: impl Fn(&Page) -> anyhow::Result<Vec<u8>> + Sync,
    mut progress: impl FnMut(usize, usize),
    stop: &AtomicU8,
) -> anyhow::Result<FetchSummary> {
    std::fs::create_dir_all(dir)?;
    let mut stored = existing_pages(dir);
    let total = pages.len();
    let mut done = stored.iter().filter(|number| **number <= total).count();
    let mut missing = Vec::new();
    for (index, page) in pages.iter().enumerate() {
        let number = index + 1;
        if stored.contains(&number) {
            if cached_inline_page_matches(dir, number, page)? {
                continue;
            }
            if let Some(path) = stored_page(dir, number) {
                std::fs::remove_file(path)?;
            }
            stored.remove(&number);
            done = done.saturating_sub(1);
        }
        missing.push(index);
    }
    let next = AtomicUsize::new(0);
    let failed = AtomicBool::new(false);
    let (tx, rx) = mpsc::channel::<anyhow::Result<()>>();
    let mut first_error = None;
    let missing = &missing;
    let next = &next;
    let failed = &failed;
    let fetch = &fetch;
    std::thread::scope(|scope| {
        for _ in 0..PAGE_CONCURRENCY.min(missing.len().max(1)) {
            let tx = tx.clone();
            scope.spawn(move || {
                loop {
                    if failed.load(Ordering::SeqCst) || stop.load(Ordering::SeqCst) != STOP_NONE {
                        return;
                    }
                    let Some(&index) = missing.get(next.fetch_add(1, Ordering::SeqCst)) else {
                        return;
                    };
                    let number = index + 1;
                    let result = (|| -> anyhow::Result<()> {
                        let bytes = fetch(&pages[index])?;
                        let reader = crate::worker::image_reader(&bytes)?;
                        let extension = match reader.format() {
                            Some(image::ImageFormat::Jpeg) => "jpg",
                            Some(image::ImageFormat::Png) => "png",
                            Some(image::ImageFormat::WebP) => "webp",
                            _ => anyhow::bail!("第 {number} 页图像格式暂不支持"),
                        };
                        // A server can return a truncated image with a valid

                        // signature. Decode with the reader's limits before

                        // marking the page complete.

                        drop(
                            reader
                                .decode()
                                .with_context(|| format!("第 {number} 页图片不完整或无法解码"))?,
                        );
                        let part = dir.join(format!("page-{number:04}.part"));
                        let target = dir.join(format!("page-{number:04}.{extension}"));
                        let mut file = std::fs::OpenOptions::new()
                            .write(true)
                            .create(true)
                            .truncate(true)
                            .open(&part)?;
                        file.write_all(&bytes)?;
                        file.sync_all()?;
                        drop(file);
                        std::fs::rename(&part, &target)?;
                        Ok(())
                    })();
                    if result.is_err() {
                        failed.store(true, Ordering::SeqCst);
                    }
                    let failed_page = result.is_err();
                    if tx.send(result).is_err() || failed_page {
                        return;
                    }
                }
            });
        }
        drop(tx);
        for result in rx {
            match result {
                Ok(()) => {
                    done += 1;
                    progress(done, total);
                }
                Err(error) => {
                    if first_error.is_none() {
                        first_error = Some(error);
                    }
                }
            }
        }
    });
    if let Some(error) = first_error {
        return Err(error);
    }
    Ok(FetchSummary {
        done,
        aborted: stop.load(Ordering::SeqCst) != STOP_NONE,
    })
}

pub(super) fn safe_name(title: &str) -> String {
    let mut safe = String::new();
    for ch in title.chars().take(80) {
        let ch = if ch.is_control() || "<>:\"/\\|?*".contains(ch) {
            '_'
        } else {
            ch
        };
        if safe.len() + ch.len_utf8() > 180 {
            break;
        }
        safe.push(ch);
    }
    if safe.trim().is_empty() {
        "chapter".into()
    } else {
        safe
    }
}

pub(super) fn chapter_archive(root: &Path, task: &DownloadTask) -> PathBuf {
    let title = safe_name(&format!("{} - {}", task.manga_title, task.chapter_title));
    root.join("downloads")
        .join(format!("{title}-{}.cbz", task.id))
}

pub(super) fn build_cbz(
    root: &Path,
    task: &DownloadTask,
    dir: &Path,
    total: usize,
) -> anyhow::Result<PathBuf> {
    let directory = root.join("downloads");
    std::fs::create_dir_all(&directory)?;
    let path = chapter_archive(root, task);
    if path.is_file() {
        return Ok(path);
    }
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_nanos();
    let part = path.with_extension(format!("{nonce}.part"));
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&part)?;
    let cleanup = PartCleanup::new(&part);
    let mut zip = zip::ZipWriter::new(file);
    let options =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    for number in 1..=total {
        let page =
            stored_page(dir, number).ok_or_else(|| anyhow::anyhow!("第 {number} 页缓存缺失"))?;
        let bytes = std::fs::read(&page)?;
        let extension = page
            .extension()
            .and_then(|ext| ext.to_str())
            .unwrap_or("jpg");
        zip.start_file(format!("{number:05}.{extension}"), options)?;
        zip.write_all(&bytes)?;
    }
    zip.finish()?.sync_all()?;
    cleanup.keep();
    std::fs::rename(&part, &path)?;
    Ok(path)
}
impl Sources {
    pub fn take_downloaded(&mut self) -> Option<DownloadedChapter> {
        while let Some(id) = self.ready.pop_front() {
            let Some(task) = self.tasks.iter().find(|task| task.id == id) else {
                continue;
            };
            if task.status != DownloadStatus::Done || task.linked {
                continue;
            }
            let Some(path) = task.cbz.clone() else {
                continue;
            };
            if !path.exists() {
                continue;
            }
            self.import_attempts.insert(task.id.clone());
            return Some(DownloadedChapter {
                id: task.id.clone(),
                path,
                metadata: task.metadata(),
                read_page: task.read_page,
            });
        }
        None
    }
    pub fn take_open_chapter(&mut self) -> Option<ChapterMetadata> {
        self.open_chapter.take()
    }
    pub fn set_chapter_progress(
        &mut self,
        map: std::collections::HashMap<String, (usize, usize, Option<i64>)>,
    ) {
        self.chapter_progress = map;
    }
    pub(super)fn resume_chapter(
        &self,
        source: &str,
        manga: &Manga,
        ordered: &[Chapter],
        states: &std::collections::HashMap<String, ChapterQueueState>,
    ) -> (Option<Chapter>, bool) {
        let mut best: Option<(usize, i64)> = None;
        for (index, chapter) in ordered.iter().enumerate() {
            let id = identity_id(source, &manga.key, &chapter.key);
            let read_at = [
                self.chapter_progress.get(&id).and_then(|(_, _, at)| *at),
                states.get(&chapter.key).and_then(|state| state.read_at),
            ]
            .into_iter()
            .flatten()
            .max();
            if let Some(at) = read_at {
                if best.is_none_or(|(_, stamp)| at > stamp) {
                    best = Some((index, at));
                }
            }
        }
        match best {
            Some((index, _)) => {
                let chapter = &ordered[index];
                let id = identity_id(source, &manga.key, &chapter.key);
                let finished = self
                    .chapter_progress
                    .get(&id)
                    .is_some_and(|&(page, count, _)| count > 0 && page + 1 >= count)
                    || states.get(&chapter.key).is_some_and(|state| {
                        state.total > 0
                            && state.read_page.is_some_and(|page| page + 1 >= state.total)
                    });
                if finished {
                    let next = ordered[index + 1..].iter().find(|item| !item.locked);
                    (next.or(Some(chapter)).cloned(), true)
                } else {
                    (Some(chapter.clone()), true)
                }
            }
            None => {
                // A migrated work with no reading record on this source yet
                // resumes by chapter number: the chapter equal to the carried
                // number (it was not finished) or the first one past it.
                if let Some(follow) = self
                    .follows
                    .iter()
                    .find(|follow| follow.source_id == source && follow.manga_id == manga.key)
                    && let Some(number) = follow.resume_number
                {
                    let target = ordered
                        .iter()
                        .find(|chapter| {
                            let value = chapter_number_value(chapter);
                            value.is_some_and(|value| {
                                if follow.resume_done {
                                    value > number
                                } else {
                                    value >= number
                                }
                            })
                        })
                        .or_else(|| {
                            // No chapter reaches the carried number: resume at
                            // the newest available chapter, not the first.
                            ordered.iter().rev().find(|chapter| !chapter.locked)
                        })
                        .or_else(|| ordered.last());
                    return (target.cloned(), true);
                }
                (
                    ordered
                        .iter()
                        .find(|chapter| !chapter.locked)
                        .or_else(|| ordered.first())
                        .cloned(),
                    false,
                )
            }
        }
    }
    pub fn take_stream(&mut self) -> Option<StreamChapter> {
        self.stream_request.take()
    }
    pub fn stream_state(&self, id: &str) -> Option<StreamState> {
        let task = self.tasks.iter().find(|task| task.id == id)?;
        let stopped = match task.status {
            DownloadStatus::Failed => Some(task.error.clone().unwrap_or_else(|| "下载失败".into())),
            DownloadStatus::Paused if self.preempted.as_deref() != Some(id) => {
                Some(task.error.clone().unwrap_or_else(|| "下载已暂停".into()))
            }
            _ => None,
        };
        Some(StreamState {
            total: task.pages_total,
            done: task.pages_done,
            stopped,
            signature: task.pages_signature.clone(),
            page: task.read_page,
        })
    }
    pub fn set_stream_page(&mut self, id: &str, page: usize) {
        let Some(task) = self.tasks.iter_mut().find(|task| task.id == id) else {
            return;
        };
        if task.read_page == Some(page) {
            return;
        }
        task.read_page = Some(page);
        task.read_at = Some(follows::now() as i64);
        if let Err(error) = self.persist() {
            self.status = format!("阅读位置保存失败：{error:#}");
        }
    }
    pub(super)fn prioritize(&mut self, id: &str) {
        if self.running.as_deref() == Some(id) {
            return;
        }
        if !self.tasks.iter().any(|task| {
            task.id == id
                && matches!(
                    task.status,
                    DownloadStatus::Queued | DownloadStatus::Paused | DownloadStatus::Failed
                )
        }) {
            return;
        }
        if let Some(running) = self.running.clone() {
            self.preempted = Some(running);
            self.dl_stop.store(STOP_PAUSE, Ordering::SeqCst);
        }
        self.set_task_queued(id);
    }
    pub fn resume_stream(&mut self, id: &str) {
        self.prioritize(id);
    }
    /// A bookmarked online chapter reopens as a stream when its download
    /// task exists; returns false when nothing for the chapter was queued.
    pub(crate) fn open_bookmarked(&mut self, source: &str, manga: &str, chapter: &str) -> bool {
        let Some(task) = self.tasks.iter().find(|task| {
            let meta = task.metadata();
            meta.source_id == source && meta.manga_id == manga && meta.chapter_id == chapter
        }) else {
            return false;
        };
        let id = task.id.clone();
        let metadata = task.metadata();
        self.prioritize(&id);
        self.stream_request = Some(StreamChapter {
            id,
            metadata,
            from_end: false,
        });
        true
    }
    pub(super)fn read_online(&mut self, manga: &Manga, chapter: &Chapter, from_end: bool) {
        let source = self.source_for(manga);
        let id = identity_id(&source, &manga.key, &chapter.key);
        if let Some(metadata) = self.library_chapters.get(&id) {
            self.open_chapter = Some(metadata.clone());
            return;
        }
        if chapter.locked {
            self.status = "这一章暂不可阅读".into();
            return;
        }
        let queued = self
            .tasks
            .iter()
            .any(|task| task.id == id && task.status != DownloadStatus::Failed);
        if !queued {
            self.enqueue_chapter(manga, chapter);
        }
        let Some(task) = self.tasks.iter().find(|task| task.id == id) else {
            return;
        };
        if task.status == DownloadStatus::Done {
            if task.linked || self.import_attempts.contains(&id) {
                return;
            }
            self.add_downloaded_to_library(&id);
            self.status = "这一章已下载，正在加入书架".into();
            return;
        }
        let metadata = task.metadata();
        self.prioritize(&id);
        self.stream_request = Some(StreamChapter {
            id,
            metadata,
            from_end,
        });
        if let Some(next) = self.adjacent_online(&source, &manga.key, &chapter.key, 1)
            && self.chapter_needs_download(&source, &manga.key, &next)
        {
            let status = std::mem::take(&mut self.status);
            self.enqueue_chapter(manga, &next);
            self.status = status;
        }
    }
    pub(super)fn adjacent_online(
        &self,
        source: &str,
        manga: &str,
        chapter: &str,
        delta: i32,
    ) -> Option<Chapter> {
        let selected = self.selected_manga.as_ref()?;
        if selected.key != manga || self.source_for(selected) != source {
            return None;
        }
        let chapters = chapter_list(&self.chapters, "", false);
        let index = chapters.iter().position(|item| item.key == chapter)? as i64 + delta as i64;
        usize::try_from(index)
            .ok()
            .and_then(|index| chapters.get(index).cloned())
    }
    pub fn adjacent_online_key(&self, metadata: &ChapterMetadata, delta: i32) -> Option<String> {
        self.adjacent_online(
            &metadata.source_id,
            &metadata.manga_id,
            &metadata.chapter_id,
            delta,
        )
        .map(|chapter| chapter.key)
    }
    /// Display name of the neighbouring chapter in reading order.
    pub fn adjacent_online_name(&self, metadata: &ChapterMetadata, delta: i32) -> Option<String> {
        self.adjacent_online(
            &metadata.source_id,
            &metadata.manga_id,
            &metadata.chapter_id,
            delta,
        )
        .map(|chapter| chapter_name(&chapter))
    }
    pub fn has_adjacent_online(&self, metadata: &ChapterMetadata, delta: i32) -> bool {
        self.adjacent_online(
            &metadata.source_id,
            &metadata.manga_id,
            &metadata.chapter_id,
            delta,
        )
        .is_some_and(|chapter| !chapter.locked)
    }
    pub fn prepare_next(
        &mut self,
        metadata: &ChapterMetadata,
    ) -> Option<(String, ChapterMetadata)> {
        let chapter = self.adjacent_online(
            &metadata.source_id,
            &metadata.manga_id,
            &metadata.chapter_id,
            1,
        )?;
        if chapter.locked {
            return None;
        }
        let id = identity_id(&metadata.source_id, &metadata.manga_id, &chapter.key);
        if self.library_chapters.contains_key(&id) {
            return None;
        }
        if self.chapter_needs_download(&metadata.source_id, &metadata.manga_id, &chapter) {
            let manga = self.selected_manga.clone().filter(|manga| {
                self.source_for(manga) == metadata.source_id && manga.key == metadata.manga_id
            })?;
            let status = std::mem::take(&mut self.status);
            self.enqueue_chapter(&manga, &chapter);
            self.status = status;
        }
        let task = self.tasks.iter().find(|task| task.id == id)?;
        let metadata = task.metadata();
        match task.status {
            // Its staged pages are already archived: push the finished file

            // at the import queue so the segment becomes a library book.
            DownloadStatus::Done => {
                if !task.linked {
                    self.add_downloaded_to_library(&id);
                }
                None
            }
            // A chapter paused earlier keeps downloading now that the reader

            // approaches it; the preempted stream itself is left alone.
            DownloadStatus::Paused | DownloadStatus::Failed
                if self.preempted.as_deref() != Some(id.as_str()) =>
            {
                self.set_task_queued(&id);
                Some((id, metadata))
            }
            _ => Some((id, metadata)),
        }
    }
    pub fn stream_metadata(&self, id: &str) -> Option<ChapterMetadata> {
        self.tasks
            .iter()
            .find(|task| task.id == id)
            .map(|task| task.metadata())
    }
    pub fn viewer_hint(&self, source_id: &str, manga_id: &str) -> Option<Viewer> {
        let viewer = self
            .selected_manga
            .as_ref()
            .filter(|manga| self.source_for(manga) == source_id && manga.key == manga_id)
            .map(|manga| manga.viewer)
            .or_else(|| {
                let path =
                    manga_snapshot_path(&self.root, &manga_snapshot_key(source_id, manga_id))
                        .ok()?;
                let bytes = std::fs::read(path).ok()?;
                Manga::from_postcard_bytes(&bytes)
                    .ok()
                    .map(|manga| manga.viewer)
            });
        viewer.filter(|viewer| *viewer != Viewer::Unknown)
    }
    pub fn open_adjacent_online(&mut self, metadata: &ChapterMetadata, delta: i32) -> bool {
        let Some(chapter) = self.adjacent_online(
            &metadata.source_id,
            &metadata.manga_id,
            &metadata.chapter_id,
            delta,
        ) else {
            return false;
        };
        if chapter.locked {
            return false;
        }
        let Some(manga) = self.selected_manga.clone() else {
            return false;
        };
        self.read_online(&manga, &chapter, delta < 0);
        true
    }
    pub fn set_library_chapters(&mut self, links: &[ChapterLink]) {
        self.library_chapters = links
            .iter()
            .map(|link| {
                let metadata = &link.metadata;
                (
                    identity_id(
                        &metadata.source_id,
                        &metadata.manga_id,
                        &metadata.chapter_id,
                    ),
                    metadata.clone(),
                )
            })
            .collect();
    }
    pub(super)fn read_downloaded(&mut self, task: &DownloadTask) {
        if let Some(metadata) = self.library_chapters.get(&task.id) {
            self.open_chapter = Some(metadata.clone());
        }
    }
    pub fn ack_downloaded(&mut self, id: &str) {
        self.import_attempts.remove(id);
        let mut changed = false;
        if let Some(task) = self.tasks.iter_mut().find(|task| task.id == id) {
            task.linked = true;
            task.error = None;
            changed = true;
        }
        if changed {
            if let Err(error) = self.persist() {
                self.status =
                    format!("已加入书架，但队列状态保存失败：{error:#}；重启后可能重复导入此文件");
            }
        }
    }
    pub fn requeue_import(&mut self, id: &str) {
        self.import_attempts.remove(id);
        if self.tasks.iter().any(|task| {
            task.id == id
                && task.status == DownloadStatus::Done
                && !task.linked
                && task.cbz.as_ref().is_some_and(|path| path.exists())
        }) && !self.ready.iter().any(|queued| queued == id)
        {
            self.ready.push_front(id.to_string());
        }
    }
    pub(super)fn add_downloaded_to_library(&mut self, id: &str) {
        let Some(task) = self.tasks.iter_mut().find(|task| task.id == id) else {
            return;
        };
        if task.status != DownloadStatus::Done {
            return;
        }
        if !task.cbz.as_ref().is_some_and(|path| path.is_file()) {
            task.status = DownloadStatus::Failed;
            task.error = Some("下载文件已丢失，可重试下载".into());
            task.cbz = None;
            self.status = "下载文件已丢失，请重试下载此章".into();
            if let Err(error) = self.persist() {
                self.status
                    .push_str(&format!("；队列状态保存失败：{error:#}"));
            }
            return;
        }
        task.linked = false;
        self.requeue_import(id);
    }
    pub fn import_failed(&mut self, id: &str, error: &str) {
        self.import_attempts.remove(id);
        if let Some(task) = self.tasks.iter_mut().find(|task| task.id == id) {
            task.error = Some(format!("加入书架失败：{error}"));
        }
        self.status = format!("加入书架失败：{error}；文件已保留，可点“重试导入”");
        if let Err(persist_error) = self.persist() {
            self.status
                .push_str(&format!("；队列状态保存失败：{persist_error:#}"));
        }
    }
    pub(super)fn persist(&self) -> anyhow::Result<()> {
        let store = TaskStore {
            tasks: self.tasks.clone(),
        };
        let bytes = serde_json::to_vec(&store)
            .map_err(|error| anyhow::anyhow!("序列化下载队列失败：{error}"))?;
        std::fs::create_dir_all(&self.root)
            .map_err(|error| anyhow::anyhow!("创建数据目录失败：{error}"))?;
        let path = self.root.join("download-tasks.json");
        let tmp = self.root.join("download-tasks.json.tmp");
        let result: std::io::Result<()> = (|| {
            std::fs::write(&tmp, &bytes)?;
            std::fs::rename(&tmp, &path)?;
            Ok(())
        })();
        if let Err(error) = result {
            let _ = std::fs::remove_file(&tmp);
            return Err(anyhow::anyhow!("写入 {} 失败：{error}", path.display()));
        }
        Ok(())
    }
    /// Whether the page's original bytes already exist locally — required
    /// before sharing is enabled.
    pub(crate) fn page_ready(&self, id: &str, page: usize) -> bool {
        let Some(task_id) = id.strip_prefix(STREAM_PREFIX) else {
            return true;
        };
        let Some(task) = self.tasks.iter().find(|task| task.id == task_id) else {
            return false;
        };
        if task.pages_done > page {
            return true;
        }
        task_dir(&self.root, task_id)
            .ok()
            .and_then(|dir| stored_page(&dir, page + 1))
            .is_some()
    }
    pub(super)fn chapter_state(&self, source: &str, manga: &str, chapter: &str) -> Option<String> {
        let id = identity_id(source, manga, chapter);
        if self.library_chapters.contains_key(&id) {
            return Some("已在书架".into());
        }
        self.tasks
            .iter()
            .find(|task| task.id == id)
            .map(|task| task.status.label().to_string())
    }
    pub(super)fn chapter_failed(&self, source: &str, manga: &str, chapter: &str) -> bool {
        let id = identity_id(source, manga, chapter);
        if self.library_chapters.contains_key(&id) {
            return false;
        }
        self.tasks
            .iter()
            .any(|task| task.id == id && task.status == DownloadStatus::Failed)
    }
    pub(super)fn chapter_needs_download(&self, source: &str, manga: &str, chapter: &Chapter) -> bool {
        !chapter.locked
            && (self.chapter_state(source, manga, &chapter.key).is_none()
                || self.chapter_failed(source, manga, &chapter.key))
    }
    pub(super)fn enqueue_chapter(&mut self, manga: &Manga, chapter: &Chapter) {
        let source = self.source_for(manga);
        if source.is_empty() {
            self.status = "请先选择并搜索一个书源".into();
            return;
        }
        let mut snapshot_manga = manga.clone();
        snapshot_manga.source_key = source.clone();
        let snapshot_key = match save_manga_snapshot(&self.root, &snapshot_manga) {
            Ok(key) => key,
            Err(error) => {
                self.status = format!("无法保存漫画快照，未开始下载：{error:#}");
                return;
            }
        };
        let mut task = DownloadTask::from_chapter(&source, manga, chapter);
        task.manga_snapshot = Some(snapshot_key);
        let id = task.id.clone();
        let title = task.chapter_title.clone();
        let was_failed = self
            .tasks
            .iter()
            .any(|task| task.id == id && task.status == DownloadStatus::Failed);
        let previous = self.tasks.clone();
        match enqueue_task(&mut self.tasks, task) {
            EnqueueOutcome::Added => {
                if let Err(error) = self.persist() {
                    self.tasks = previous;
                    self.status = format!("无法保存下载队列，未开始下载：{error:#}");
                    return;
                }
                self.status = format!("已加入下载队列：{title}");
                self.pump();
            }
            EnqueueOutcome::Duplicate => {
                if let Err(error) = self.persist() {
                    self.tasks = previous;
                    self.status = format!("无法保存下载队列，未开始下载：{error:#}");
                    return;
                }
                self.status = if was_failed {
                    format!("已用完整快照重新加入：{title}")
                } else {
                    format!("{title} 已在下载队列中")
                };
                self.pump();
            }
            EnqueueOutcome::AlreadyDownloaded => self.status = format!("{title} 已下载完成"),
        }
    }
    pub(super)fn enqueue_missing(&mut self, manga: &Manga) {
        let source = self.source_for(manga);
        if source.is_empty() {
            return;
        }
        let mut snapshot_manga = manga.clone();
        snapshot_manga.source_key = source.clone();
        let snapshot_key = match save_manga_snapshot(&self.root, &snapshot_manga) {
            Ok(key) => key,
            Err(error) => {
                self.status = format!("无法保存漫画快照，未开始下载：{error:#}");
                return;
            }
        };
        let previous = self.tasks.clone();
        let mut added = 0usize;
        for chapter in chapter_list(&self.chapters, "", false) {
            if !self.chapter_needs_download(&source, &manga.key, &chapter) {
                continue;
            }
            let mut task = DownloadTask::from_chapter(&source, manga, &chapter);
            task.manga_snapshot = Some(snapshot_key.clone());
            if !matches!(
                enqueue_task(&mut self.tasks, task),
                EnqueueOutcome::AlreadyDownloaded
            ) {
                added += 1;
            }
        }
        if let Err(error) = self.persist() {
            self.tasks = previous;
            self.status = format!("无法保存下载队列，未开始下载：{error:#}");
            return;
        }
        self.status = if added == 0 {
            "没有需要加入的章节".into()
        } else {
            format!("已加入 {added} 个章节")
        };
        self.pump();
    }
    pub(super)fn pump(&mut self) {
        if self.running.is_some() {
            return;
        }
        let Some(index) = next_queued(&self.tasks) else {
            return;
        };
        let previous = self.tasks[index].status;
        self.tasks[index].status = DownloadStatus::Running;
        self.tasks[index].error = None;
        let id = self.tasks[index].id.clone();
        let task = self.tasks[index].clone();
        if let Err(error) = self.persist() {
            self.tasks[index].status = previous;
            self.status = format!("无法保存下载队列，暂不开始下载：{error:#}");
            return;
        }
        self.dl_stop.store(STOP_NONE, Ordering::SeqCst);
        self.running = Some(id.clone());
        if self.dl_tx.send(DlCmd::Run(task)).is_err() {
            self.running = None;
            if let Some(task) = self.tasks.iter_mut().find(|task| task.id == id) {
                task.status = DownloadStatus::Failed;
                task.error = Some("下载线程已停止，请重新打开应用".into());
            }
            if let Err(error) = self.persist() {
                self.status = format!("下载线程已停止，且队列状态保存失败：{error:#}");
            } else {
                self.status = "下载线程已停止，请重新打开应用".into();
            }
        }
    }
    pub(super)fn set_task_queued(&mut self, id: &str) {
        let Some(position) = self.tasks.iter().position(|task| task.id == id) else {
            return;
        };
        let mut task = self.tasks.remove(position);
        let previous = task.clone();
        task.status = DownloadStatus::Queued;
        task.error = None;
        // Explicit continue/retry runs this task next, even if older

        // queued tasks are still waiting.

        self.tasks.insert(0, task);
        if let Err(error) = self.persist() {
            let _ = self.tasks.remove(0);
            self.tasks.insert(position.min(self.tasks.len()), previous);
            self.status = format!("无法保存下载队列，未开始下载：{error:#}");
            return;
        }
        self.pump();
    }
    pub(super)fn request_pause(&mut self) {
        self.dl_stop.store(STOP_PAUSE, Ordering::SeqCst);
        self.status = "已请求暂停，当前图片请求结束后生效".into();
    }
    pub(super)fn retry_task(&mut self, task: &DownloadTask) {
        if let Some(manga) = self.selected_manga.clone()
            && manga.key == task.manga_id
            && self.source_for(&manga) == task.source_id
        {
            if let Some(chapter) = self
                .chapters
                .iter()
                .find(|chapter| chapter.key == task.chapter_id)
                .cloned()
            {
                self.enqueue_chapter(&manga, &chapter);
                return;
            }
        }
        self.set_task_queued(&task.id);
    }
    pub(super)fn request_cancel(&mut self, id: &str) {
        if self.running.as_deref() == Some(id) {
            self.dl_stop.store(STOP_CANCEL, Ordering::SeqCst);
            self.status = "已请求取消，当前图片请求结束后生效".into();
        } else {
            if self.remove_task_now(id, "已从下载队列移除") {
                self.pump();
            }
        }
    }
    pub(super)fn remove_task_now(&mut self, id: &str, success: &str) -> bool {
        self.import_attempts.remove(id);
        let Some(position) = self.tasks.iter().position(|task| task.id == id) else {
            return true;
        };
        let removed = self.tasks.remove(position);
        if let Err(error) = self.persist() {
            self.tasks.insert(position.min(self.tasks.len()), removed);
            self.status = format!("无法保存下载队列，任务未移除：{error:#}");
            return false;
        }
        let cleanup = remove_task_cache(&self.root, id);
        self.status = match cleanup {
            Ok(()) => success.to_string(),
            Err(error) => format!("{success}，但缓存未清理：{error:#}"),
        };
        true
    }
    pub(super)fn handle_download_event(&mut self, event: DlEvent) {
        if let DlEvent::Finished { id, .. } = &event
            && self.preempted.as_deref() == Some(id.as_str())
        {
            self.preempted = None;
        }
        match event {
            DlEvent::Progress {
                id,
                done,
                total,
                signature,
            } => {
                if let Some(task) = self.tasks.iter_mut().find(|task| task.id == id) {
                    task.pages_done = done;
                    task.pages_total = total;
                    if signature.is_some() {
                        task.pages_signature = signature;
                    }
                }
                if let Err(error) = self.persist() {
                    self.status = format!("下载队列保存失败：{error:#}；下载继续进行");
                }
            }
            DlEvent::Finished { id, path } => {
                if let Some(task) = self.tasks.iter_mut().find(|task| task.id == id) {
                    task.status = DownloadStatus::Done;
                    task.error = None;
                    task.cbz = Some(path.clone());
                    task.pages_done = task.pages_total;
                    task.linked = false;
                }
                if !self.ready.iter().any(|queued| queued == &id) {
                    self.ready.push_back(id.clone());
                }
                self.running = None;
                self.status = "章节下载完成，正在加入书架".into();
                if let Err(error) = self.persist() {
                    self.status = format!("章节已下载，但队列状态保存失败：{error:#}；文件已保留");
                    return;
                }
                self.pump();
            }
            DlEvent::Failed { id, error } => {
                // A timeout can race with a pause/cancel tap. Honor the

                // explicit user action once the in-flight request returns.

                let stop = self.dl_stop.load(Ordering::SeqCst);
                if self.running.as_deref() == Some(id.as_str()) && stop != STOP_NONE {
                    self.handle_download_event(DlEvent::Aborted { id, how: stop });
                    return;
                }
                if self.preempted.as_deref() == Some(id.as_str()) {
                    self.preempted = None;
                }
                if let Some(task) = self.tasks.iter_mut().find(|task| task.id == id) {
                    task.status = DownloadStatus::Failed;
                    task.error = Some(error.clone());
                }
                self.running = None;
                self.status = format!("章节下载失败：{error}");
                if let Err(error) = self.persist() {
                    self.status = format!("章节下载失败，且队列状态保存失败：{error:#}");
                    return;
                }
                // A failed chapter must not block the rest of the queue.

                self.pump();
            }
            DlEvent::Aborted { id, how } => {
                self.running = None;
                if how == STOP_CANCEL {
                    if self.remove_task_now(&id, "已取消下载") {
                        self.pump();
                    }
                } else {
                    let preempted = self.preempted.as_deref() == Some(id.as_str());
                    if preempted {
                        self.preempted = None;
                    }
                    if let Some(task) = self.tasks.iter_mut().find(|task| task.id == id) {
                        task.status = if preempted {
                            DownloadStatus::Queued
                        } else {
                            DownloadStatus::Paused
                        };
                        task.error = None;
                    }
                    self.status = if preempted {
                        "正在优先下载正在阅读的章节".into()
                    } else {
                        "下载已暂停，可稍后继续".into()
                    };
                    if let Err(error) = self.persist() {
                        self.status = format!("下载已暂停，但队列状态保存失败：{error:#}");
                        return;
                    }
                    self.pump();
                }
            }
            DlEvent::LockUnavailable { id, error } => {
                self.running = None;
                if let Some(task) = self.tasks.iter_mut().find(|task| task.id == id) {
                    task.status = DownloadStatus::Paused;
                    task.error = Some(error.clone());
                }
                self.status = format!("{error}；未改动任何缓存");
                if let Err(persist_error) = self.persist() {
                    self.status = format!("{error}；且队列状态保存失败：{persist_error:#}");
                }
                // Another instance still owns the lock. Do not immediately

                // retry the next queued task or hammer the same lock.
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sources::test_support::*;
    use crate::sources::tasks::{load_tasks, normalize_loaded};

    #[test]
    fn online_reading_runs_first_requeues_the_preempted_download_and_queues_the_next_chapter() {
        let root = tempfile::tempdir().unwrap();
        let mut sources = test_sources(root.path());
        let book = manga("source.a", "m");
        let chapter = |key: &str, number: f32| Chapter {
            key: key.into(),
            title: Some(key.into()),
            chapter_number: Some(number),
            ..Default::default()
        };
        sources.selected_source = "source.a".into();
        sources.selected_manga = Some(book.clone());
        sources.chapters = vec![chapter("c2", 2.), chapter("c1", 1.), chapter("c3", 3.)];
        let mut other = task("source.a", "other", "x");
        other.status = DownloadStatus::Running;
        sources.running = Some(other.id.clone());
        sources.tasks.push(other.clone());

        sources.read_online(&book, &chapter("c1", 1.), false);
        let stream = sources.take_stream().expect("reader opens immediately");
        assert_eq!(stream.id, identity_id("source.a", "m", "c1"));
        assert_eq!(
            sources.tasks[0].id, stream.id,
            "read chapter downloads first"
        );
        assert_eq!(sources.preempted.as_deref(), Some(other.id.as_str()));
        assert_eq!(sources.dl_stop.load(Ordering::SeqCst), STOP_PAUSE);
        assert!(
            sources.tasks.iter().any(|task| task.chapter_id == "c2"),
            "next chapter in reading order is prefetched"
        );
        assert!(!sources.tasks.iter().any(|task| task.chapter_id == "c3"));
        assert_eq!(sources.stream_state(&stream.id).unwrap().stopped, None);

        sources.handle_download_event(DlEvent::Aborted {
            id: other.id.clone(),
            how: STOP_PAUSE,
        });
        let requeued = sources
            .tasks
            .iter()
            .find(|task| task.id == other.id)
            .unwrap();
        assert_eq!(
            requeued.status,
            DownloadStatus::Queued,
            "no manual continue needed"
        );
        assert!(sources.preempted.is_none());
        assert_eq!(sources.running.as_deref(), Some(stream.id.as_str()));

        assert!(sources.has_adjacent_online(&stream.metadata, 1));
        assert!(!sources.has_adjacent_online(&stream.metadata, -1));
        assert_eq!(
            sources.adjacent_online_key(&stream.metadata, 1).as_deref(),
            Some("c2")
        );

        assert!(sources.open_adjacent_online(&sources.tasks[0].metadata(), 0));
        let next = sources
            .tasks
            .iter()
            .find(|task| task.chapter_id == "c2")
            .unwrap()
            .metadata();
        assert!(sources.open_adjacent_online(&next, -1));
        assert!(
            sources.take_stream().unwrap().from_end,
            "backwards starts on the last page"
        );
    }
    #[test]
    fn a_preempted_download_whose_request_fails_is_still_requeued() {
        let root = tempfile::tempdir().unwrap();
        let mut sources = test_sources(root.path());
        let mut other = task("source.a", "other", "x");
        other.status = DownloadStatus::Running;
        sources.running = Some(other.id.clone());
        sources.tasks.push(other.clone());
        let mut read = task("source.a", "m", "c");
        read.status = DownloadStatus::Queued;
        sources.tasks.push(read.clone());
        sources.prioritize(&read.id);
        sources.handle_download_event(DlEvent::Failed {
            id: other.id.clone(),
            error: "timeout".into(),
        });
        let other = sources
            .tasks
            .iter()
            .find(|task| task.id == other.id)
            .unwrap();
        assert_eq!(other.status, DownloadStatus::Queued);
        assert_eq!(sources.running.as_deref(), Some(read.id.as_str()));
    }
    #[test]
    fn online_pages_are_served_from_the_download_staging_directory() {
        let root = tempfile::tempdir().unwrap();
        let id = identity_id("source.a", "m", "c");
        assert!(stream_page(root.path(), &id, 0).unwrap().is_none());
        let dir = task_dir(root.path(), &id).unwrap();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("page-0001.png"), png()).unwrap();
        std::fs::write(dir.join("page-0002.part"), b"half").unwrap();
        assert_eq!(stream_page(root.path(), &id, 0).unwrap(), Some(png()));
        assert!(
            stream_page(root.path(), &id, 1).unwrap().is_none(),
            "partial page is not ready"
        );
        assert!(stream_page(root.path(), "../escape", 0).is_err());
    }
    #[test]
    fn a_paused_or_failed_online_chapter_reports_why_it_stopped() {
        let root = tempfile::tempdir().unwrap();
        let mut sources = test_sources(root.path());
        let mut item = task("source.a", "m", "c");
        item.status = DownloadStatus::Failed;
        item.error = Some("图片服务器返回 HTTP 403".into());
        item.pages_total = 12;
        item.pages_done = 4;
        sources.tasks.push(item.clone());
        let state = sources.stream_state(&item.id).unwrap();
        assert_eq!((state.total, state.done), (12, 4));
        assert_eq!(state.stopped.as_deref(), Some("图片服务器返回 HTTP 403"));
        sources.resume_stream(&item.id);
        assert_eq!(sources.running.as_deref(), Some(item.id.as_str()));
        assert_eq!(sources.stream_state(&item.id).unwrap().stopped, None);
        sources.set_stream_page(&item.id, 7);
        assert_eq!(sources.stream_state(&item.id).unwrap().page, Some(7));
        let saved = load_tasks(root.path()).unwrap().tasks;
        assert_eq!(
            saved
                .iter()
                .find(|task| task.id == item.id)
                .unwrap()
                .read_page,
            Some(7)
        );
    }
    #[test]
    fn restored_chapters_are_readable_without_a_download_queue_and_hidden_links_are_removed() {
        let root = tempfile::tempdir().unwrap();
        let mut sources = test_sources(root.path());
        let item = task("source.a", "m", "c");
        let link = ChapterLink {
            book_id: "book".into(),
            metadata: item.metadata(),
        };
        sources.set_library_chapters(&[link]);
        assert!(
            sources.tasks.is_empty(),
            "restore does not need download-tasks.json"
        );
        assert_eq!(
            sources.chapter_state("source.a", "m", "c").as_deref(),
            Some("已在书架")
        );
        assert!(!sources.chapter_needs_download("source.a", "m", &item.chapter()));
        sources.read_downloaded(&item);
        assert_eq!(sources.take_open_chapter(), Some(item.metadata()));
        assert!(
            sources.take_open_chapter().is_none(),
            "consume one request once"
        );
        assert!(sources.chapter_state("source.b", "m", "c").is_none());
        sources.set_library_chapters(&[]);
        sources.read_downloaded(&item);
        assert!(sources.take_open_chapter().is_none());
    }
    #[test]
    fn retry_keeps_the_task_identity_when_another_manga_reuses_its_chapter_key() {
        let root = tempfile::tempdir().unwrap();
        for (other_source, other_manga) in [("source.a", "other"), ("source.b", "m")] {
            let mut sources = test_sources(root.path());
            let mut original = task("source.a", "m", "1");
            original.status = DownloadStatus::Failed;
            sources.tasks.push(original.clone());
            sources.selected_source = other_source.into();
            sources.selected_manga = Some(manga(other_source, other_manga));
            sources.chapters = vec![Chapter {
                key: "1".into(),
                ..Default::default()
            }];
            sources.retry_task(&original);
            assert_eq!(
                sources.tasks.len(),
                1,
                "must not enqueue the current foreign manga"
            );
            assert_eq!(sources.running.as_deref(), Some(original.id.as_str()));
            assert_eq!(sources.tasks[0].manga_id, "m");
            assert_eq!(sources.tasks[0].source_id, "source.a");
        }
    }
    #[test]
    fn pause_or_cancel_is_honored_when_the_current_request_fails() {
        let root = tempfile::tempdir().unwrap();
        for stop in [STOP_PAUSE, STOP_CANCEL] {
            let mut sources = test_sources(root.path());
            let mut current = task("source.a", "m", "1");
            current.status = DownloadStatus::Running;
            let id = current.id.clone();
            sources.tasks.push(current);
            sources.running = Some(id.clone());
            sources.dl_stop.store(stop, Ordering::SeqCst);
            sources.handle_download_event(DlEvent::Failed {
                id,
                error: "request timed out".into(),
            });
            assert!(sources.running.is_none());
            if stop == STOP_PAUSE {
                assert_eq!(sources.tasks[0].status, DownloadStatus::Paused);
                assert!(sources.tasks[0].error.is_none());
            } else {
                assert!(sources.tasks.is_empty());
            }
        }
    }
    #[test]
    fn bulk_download_runs_in_reading_order_and_requeues_failed_chapters() {
        let root = tempfile::tempdir().unwrap();
        let mut sources = test_sources(root.path());
        sources.selected_source = "source.a".into();
        sources.chapters = [10., 2., 2.5, 1.]
            .into_iter()
            .map(|number| Chapter {
                key: number.to_string(),
                chapter_number: Some(number),
                ..Default::default()
            })
            .collect();
        let mut failed =
            DownloadTask::from_chapter("source.a", &manga("source.a", "m"), &sources.chapters[1]);
        failed.status = DownloadStatus::Failed;
        sources.tasks.push(failed);
        sources.enqueue_missing(&manga("source.a", "m"));
        assert_eq!(sources.tasks.len(), 4);
        assert!(sources.tasks.iter().all(|task| matches!(
            task.status,
            DownloadStatus::Running | DownloadStatus::Queued
        )));
        assert!(
            sources.status.contains("4"),
            "retry count should match action"
        );
        // A failed task keeps its existing queue position; newly added tasks

        // are queued from the first chapter rather than the source's newest.

        assert_eq!(
            sources
                .tasks
                .iter()
                .skip(1)
                .map(|task| task.chapter_id.as_str())
                .collect::<Vec<_>>(),
            ["1", "2.5", "10"],
        );
    }
    #[test]
    fn resume_fetches_only_missing_pages() {
        let dir = tempfile::tempdir().unwrap();
        let work = dir.path().join("task");
        let pages = vec![url_page("p1"), url_page("p2")];
        let stop = AtomicU8::new(STOP_NONE);
        let error = fetch_missing_pages(
            &work,
            &pages,
            |page| match &page.content {
                PageContent::Url { url, .. } if url == "p1" => Ok(png()),
                _ => anyhow::bail!("第 2 页上游返回 500"),
            },
            |_, _| {},
            &stop,
        )
        .unwrap_err();
        assert!(error.to_string().contains("500"));
        assert!(stored_page(&work, 1).is_some());
        assert!(stored_page(&work, 2).is_none());
        let fetched = std::sync::Mutex::new(Vec::new());
        let summary = fetch_missing_pages(
            &work,
            &pages,
            |page| {
                if let PageContent::Url { url, .. } = &page.content {
                    fetched.lock().unwrap().push(url.clone());
                }
                Ok(png())
            },
            |_, _| {},
            &stop,
        )
        .unwrap();
        assert_eq!(*fetched.lock().unwrap(), vec!["p2"]);
        assert_eq!(summary.done, 2);
        assert!(stored_page(&work, 2).is_some());
    }
    #[test]
    fn a_truncated_image_cannot_finish_and_retry_reuses_pages_before_the_ui_saves() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let task = task("fixture", "m", "chapter");
        let pages = vec![url_page("1"), url_page("2")];
        let stop = AtomicU8::new(STOP_NONE);
        let failure = execute_task(
            root,
            &task,
            pages.clone(),
            &stop,
            |page| {
                if matches!(&page.content, PageContent::Url { url, .. } if url == "2") {
                    return Ok(b"\x89PNG\r\n\x1a\n".to_vec());
                }
                Ok(png())
            },
            |_, _, _| {},
        )
        .unwrap_err();
        assert!(format!("{failure:#}").contains("第 2 页"));
        let dir = task_dir(root, &task.id).unwrap();
        assert!(stored_page(&dir, 1).is_some());
        assert!(stored_page(&dir, 2).is_none());
        assert!(!chapter_archive(root, &task).exists());
        // Simulate the app closing before either progress or completion is

        // drained by its UI. Both stages recover using the worker's files.

        let recovered = execute_task(
            root,
            &task,
            pages,
            &stop,
            |page| {
                assert!(matches!(&page.content, PageContent::Url { url, .. } if url == "2"));
                Ok(png())
            },
            |_, _, _| {},
        )
        .unwrap();
        let Outcome::Done { path } = recovered else {
            panic!("retry did not complete")
        };
        let mut on_disk = task;
        on_disk.status = DownloadStatus::Running;
        let mut tasks = [on_disk];
        normalize_loaded(root, &mut tasks);
        assert_eq!(tasks[0].status, DownloadStatus::Done);
        assert_eq!(tasks[0].cbz.as_ref(), Some(&path));
        assert_eq!(pending_imports(&tasks), vec![tasks[0].id.clone()]);
    }
    #[test]
    fn completed_downloads_follow_a_moved_container_without_reimporting_linked_chapters() {
        let temp = tempfile::tempdir().unwrap();
        let old_root = temp.path().join("old");
        let new_root = temp.path().join("new");
        let mut tasks = Vec::new();
        let stop = AtomicU8::new(STOP_NONE);
        for linked in [false, true] {
            let mut item = task("fixture", "m", if linked { "linked" } else { "pending" });
            let Outcome::Done { path } = execute_task(
                &old_root,
                &item,
                vec![url_page("1")],
                &stop,
                |_| Ok(png()),
                |_, _, _| {},
            )
            .unwrap() else {
                panic!("fixture did not complete")
            };
            item.status = DownloadStatus::Done;
            item.cbz = Some(path);
            item.linked = linked;
            tasks.push(item);
        }
        std::fs::write(
            old_root.join("download-tasks.json"),
            serde_json::to_vec(&TaskStore {
                tasks: tasks.clone(),
            })
            .unwrap(),
        )
        .unwrap();
        std::fs::rename(&old_root, &new_root).unwrap();
        // A leftover old container must never take precedence over the

        // complete archive inside the library we actually opened.

        let stale = tasks[0].cbz.as_ref().unwrap();
        std::fs::create_dir_all(stale.parent().unwrap()).unwrap();
        std::fs::write(stale, b"stale").unwrap();
        let (loaded, warning) = load_queue(&new_root);
        assert!(warning.is_none());
        for item in &loaded {
            assert_eq!(item.status, DownloadStatus::Done);
            assert_eq!(item.cbz.as_ref(), Some(&chapter_archive(&new_root, item)));
            let archive = std::fs::File::open(item.cbz.as_ref().unwrap()).unwrap();
            assert_eq!(zip::ZipArchive::new(archive).unwrap().len(), 1);
        }
        assert!(!loaded[0].linked);
        assert!(loaded[1].linked);
        assert_eq!(pending_imports(&loaded), vec![loaded[0].id.clone()]);
    }
    #[test]
    fn inline_pages_with_equal_length_but_different_bytes_are_rewritten() {
        let dir = tempfile::tempdir().unwrap();
        let work = dir.path().join("task");
        std::fs::create_dir_all(&work).unwrap();
        let old = colored_png([255, 0, 0, 255]);
        let new = colored_png([0, 0, 255, 255]);
        assert_eq!(
            old.len(),
            new.len(),
            "fixture must have different bytes but equal length"
        );
        assert_ne!(old, new);
        std::fs::write(work.join("page-0001.png"), &old).unwrap();
        let page = Page {
            content: PageContent::Image {
                image_ref: 7,
                image_data: Some(new.clone()),
            },
            thumbnail: None,
            has_description: false,
            description: None,
        };
        let stop = AtomicU8::new(STOP_NONE);
        let fetched = AtomicUsize::new(0);
        let summary = fetch_missing_pages(
            &work,
            &[page],
            |_| {
                fetched.fetch_add(1, Ordering::SeqCst);
                Ok(new.clone())
            },
            |_, _| {},
            &stop,
        )
        .unwrap();
        assert_eq!(
            fetched.load(Ordering::SeqCst),
            1,
            "equal-length inline bytes must not be trusted from the signature"
        );
        assert_eq!(summary.done, 1);
        assert_eq!(std::fs::read(work.join("page-0001.png")).unwrap(), new);
    }
    #[test]
    fn paused_download_stops_before_fetching() {
        let dir = tempfile::tempdir().unwrap();
        let pages = vec![url_page("p1"), url_page("p2")];
        let stop = AtomicU8::new(STOP_PAUSE);
        let summary = fetch_missing_pages(
            &dir.path().join("task"),
            &pages,
            |_| panic!("must not fetch while paused"),
            |_, _| {},
            &stop,
        )
        .unwrap();
        assert!(summary.aborted);
        assert_eq!(summary.done, 0);
        assert!(existing_pages(&dir.path().join("task")).is_empty());
    }
    #[test]
    fn missing_pages_download_in_parallel_in_page_order() {
        let dir = tempfile::tempdir().unwrap();
        let work = dir.path().join("task");
        let pages: Vec<_> = (0..8).map(|index| url_page(&format!("p{index}"))).collect();
        let stop = AtomicU8::new(STOP_NONE);
        let in_flight = AtomicUsize::new(0);
        let peak = AtomicUsize::new(0);
        let order = std::sync::Mutex::new(Vec::new());
        let summary = fetch_missing_pages(
            &work,
            &pages,
            |page| {
                if let PageContent::Url { url, .. } = &page.content {
                    order.lock().unwrap().push(url.clone());
                }
                let now = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(now, Ordering::SeqCst);
                std::thread::sleep(std::time::Duration::from_millis(40));
                in_flight.fetch_sub(1, Ordering::SeqCst);
                Ok(png())
            },
            |_, _| {},
            &stop,
        )
        .unwrap();
        assert_eq!(summary.done, 8);
        let peak = peak.load(Ordering::SeqCst);
        assert!(
            (2..=PAGE_CONCURRENCY).contains(&peak),
            "pages did not overlap as expected: peak {peak}"
        );
        let started = order.lock().unwrap().clone();
        let first_batch = &started[..PAGE_CONCURRENCY.min(started.len())];
        assert!(
            first_batch
                .iter()
                .all(|url| url.starts_with('p')
                    && url[1..].parse::<usize>().unwrap() < PAGE_CONCURRENCY),
            "first pages were not handed out in ascending order: {started:?}"
        );
        for number in 1..=8 {
            assert!(stored_page(&work, number).is_some());
        }
    }
    #[test]
    fn a_failed_page_keeps_completed_siblings_and_resumes_the_rest() {
        let dir = tempfile::tempdir().unwrap();
        let work = dir.path().join("task");
        let pages: Vec<_> = (0..6).map(|index| url_page(&format!("p{index}"))).collect();
        let stop = AtomicU8::new(STOP_NONE);
        let error = fetch_missing_pages(
            &work,
            &pages,
            |page| {
                if matches!(&page.content, PageContent::Url { url, .. } if url == "p2") {
                    anyhow::bail!("第 3 页上游返回 500")
                } else {
                    std::thread::sleep(std::time::Duration::from_millis(30));
                    Ok(png())
                }
            },
            |_, _| {},
            &stop,
        )
        .unwrap_err();
        assert!(error.to_string().contains("500"));
        let stored = existing_pages(&work);
        assert!(
            stored.len() >= 2,
            "in-flight siblings of the failed page stay reusable: {stored:?}"
        );
        assert!(!stored.contains(&3));
        assert!(
            !stored.contains(&5) && !stored.contains(&6),
            "pages after the failure must not have started: {stored:?}"
        );

        let fetched = AtomicUsize::new(0);
        let summary = fetch_missing_pages(
            &work,
            &pages,
            |_| {
                fetched.fetch_add(1, Ordering::SeqCst);
                Ok(png())
            },
            |_, _| {},
            &stop,
        )
        .unwrap();
        assert_eq!(summary.done, 6);
        assert_eq!(fetched.load(Ordering::SeqCst), 6 - stored.len());
    }
    #[test]
    fn viewer_hint_reads_memory_then_the_saved_snapshot() {
        let root = tempfile::tempdir().unwrap();
        let mut sources = test_sources(root.path());
        assert!(sources.viewer_hint("source.a", "m").is_none());
        let mut stored = manga("source.a", "m");
        stored.viewer = Viewer::RightToLeft;
        save_manga_snapshot(root.path(), &stored).unwrap();
        assert_eq!(
            sources.viewer_hint("source.a", "m"),
            Some(Viewer::RightToLeft)
        );
        let mut selected = manga("source.a", "m");
        selected.viewer = Viewer::LeftToRight;
        sources.selected_manga = Some(selected);
        assert_eq!(
            sources.viewer_hint("source.a", "m"),
            Some(Viewer::LeftToRight),
            "loaded details win over the snapshot"
        );
        let mut unknown = manga("source.a", "other");
        unknown.viewer = Viewer::Unknown;
        sources.selected_manga = Some(unknown);
        assert!(sources.viewer_hint("source.a", "other").is_none());
        assert_eq!(
            sources.viewer_hint("source.a", "m"),
            Some(Viewer::RightToLeft),
            "a different open detail still falls back to the snapshot"
        );
    }
    #[test]
    fn page_list_change_clears_only_this_task_pages() {
        let dir = tempfile::tempdir().unwrap();
        let work = dir.path().join("task");
        std::fs::create_dir_all(&work).unwrap();
        std::fs::write(work.join("page-0001.png"), png()).unwrap();
        std::fs::write(work.join("notes.txt"), b"keep").unwrap();
        reset_page_cache(&work).unwrap();
        assert!(existing_pages(&work).is_empty());
        assert_eq!(std::fs::read(work.join("notes.txt")).unwrap(), b"keep");
    }
    #[test]
    fn enqueue_dedupes_task_and_protects_existing_cbz() {
        let dir = tempfile::tempdir().unwrap();
        let mut tasks = Vec::new();
        assert!(matches!(
            enqueue_task(&mut tasks, task("source.a", "m", "c")),
            EnqueueOutcome::Added
        ));
        assert!(matches!(
            enqueue_task(&mut tasks, task("source.a", "m", "c")),
            EnqueueOutcome::Duplicate
        ));
        assert_eq!(tasks.len(), 1);
        let cbz = dir.path().join("chapter.cbz");
        std::fs::write(&cbz, b"done").unwrap();
        tasks[0].status = DownloadStatus::Done;
        tasks[0].cbz = Some(cbz);
        assert!(matches!(
            enqueue_task(&mut tasks, task("source.a", "m", "c")),
            EnqueueOutcome::AlreadyDownloaded
        ));
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].status, DownloadStatus::Done);
    }
    #[test]
    fn reopened_download_fetches_only_missing_pages() {
        let root = tempfile::tempdir().unwrap();
        let mut item = task("source.a", "m", "c");
        let pages = vec![url_page("p1"), url_page("p2")];
        let stop = AtomicU8::new(STOP_NONE);
        let mut signature = None;
        let first = execute_task(
            root.path(),
            &item,
            pages.clone(),
            &stop,
            |page| match &page.content {
                PageContent::Url { url, .. } if url == "p2" => anyhow::bail!("第 2 页上游返回 500"),
                _ => Ok(png()),
            },
            |_, _, value| {
                if value.is_some() {
                    signature = value;
                }
            },
        );
        assert!(first.is_err());
        let work = task_dir(root.path(), &item.id).unwrap();
        assert!(stored_page(&work, 1).is_some());
        assert!(stored_page(&work, 2).is_none());
        // Persist the page fingerprint the UI received before the failure and

        // reload, as after an app restart.

        item.pages_signature = signature;
        item.status = DownloadStatus::Failed;
        let store = TaskStore { tasks: vec![item] };
        std::fs::write(
            root.path().join("download-tasks.json"),
            serde_json::to_vec(&store).unwrap(),
        )
        .unwrap();
        let loaded = load_tasks(root.path()).unwrap();
        assert_eq!(loaded.tasks.len(), 1);
        let reopened = loaded.tasks[0].clone();
        assert!(reopened.pages_signature.is_some());

        let fetched = std::sync::Mutex::new(Vec::new());
        let outcome = execute_task(
            root.path(),
            &reopened,
            pages,
            &stop,
            |page| {
                if let PageContent::Url { url, .. } = &page.content {
                    fetched.lock().unwrap().push(url.clone());
                }
                Ok(png())
            },
            |_, _, _| {},
        )
        .unwrap();
        assert_eq!(
            *fetched.lock().unwrap(),
            vec!["p2"],
            "the complete first page must not be fetched again"
        );
        assert!(matches!(outcome, Outcome::Done { .. }));
    }
    #[test]
    fn failed_download_does_not_block_the_next_task() {
        let root = tempfile::tempdir().unwrap();
        let mut sources = test_sources(root.path());
        let first = task("source.a", "m", "c1");
        let second = task("source.a", "m", "c2");
        sources.tasks = vec![first.clone(), second.clone()];
        sources.tasks[0].status = DownloadStatus::Running;
        sources.running = Some(first.id.clone());
        let stop = AtomicU8::new(STOP_NONE);
        let error = execute_task(
            root.path(),
            &first,
            vec![url_page("a1"), url_page("a2")],
            &stop,
            |page| match &page.content {
                PageContent::Url { url, .. } if url == "a2" => anyhow::bail!("a2 上游返回 500"),
                _ => Ok(png()),
            },
            |_, _, _| {},
        )
        .unwrap_err();
        sources.handle_download_event(DlEvent::Failed {
            id: first.id.clone(),
            error: error.to_string(),
        });
        assert_eq!(sources.tasks[0].status, DownloadStatus::Failed);
        assert_eq!(sources.running.as_deref(), Some(second.id.as_str()));

        let outcome = execute_task(
            root.path(),
            &second,
            vec![url_page("b1")],
            &stop,
            |_| Ok(png()),
            |_, _, _| {},
        )
        .unwrap();
        let Outcome::Done { path } = outcome else {
            panic!("second task did not finish")
        };
        sources.handle_download_event(DlEvent::Finished {
            id: second.id.clone(),
            path: path.clone(),
        });
        assert_eq!(sources.tasks[1].status, DownloadStatus::Done);
        assert!(sources.ready.contains(&second.id));
        assert!(path.exists());
    }
    #[test]
    fn finished_downloads_survive_import_failure_and_drain_in_order() {
        let root = tempfile::tempdir().unwrap();
        let mut sources = test_sources(root.path());
        let mut items = Vec::new();
        for chapter in ["c1", "c2", "c3"] {
            let mut item = task("source.a", "m", chapter);
            let path = root.path().join(format!("{chapter}.cbz"));
            std::fs::write(&path, b"cbz").unwrap();
            item.status = DownloadStatus::Done;
            item.cbz = Some(path);
            items.push(item);
        }
        sources.tasks = items.clone();
        for item in &items {
            sources.ready.push_back(item.id.clone());
        }

        let first = sources.take_downloaded().unwrap();
        let second = sources.take_downloaded().unwrap();
        let third = sources.take_downloaded().unwrap();
        assert_eq!(first.id, items[0].id);
        assert_eq!(second.id, items[1].id);
        assert_eq!(third.id, items[2].id);
        sources.ack_downloaded(&first.id);
        // The second import failed: put only that file back and retry it

        // without touching the third file or re-downloading anything.

        sources.requeue_import(&second.id);
        let retried = sources.take_downloaded().unwrap();
        assert_eq!(retried.id, second.id);
        assert!(retried.path.exists());
        sources.ack_downloaded(&third.id);
        sources.ack_downloaded(&retried.id);
        assert!(sources.take_downloaded().is_none());
        assert!(sources.tasks.iter().all(|item| item.linked));
        assert!(
            sources
                .tasks
                .iter()
                .all(|item| item.status == DownloadStatus::Done)
        );
    }
    #[test]
    fn import_failure_keeps_the_file_and_allows_manual_retry_only() {
        let root = tempfile::tempdir().unwrap();
        let mut sources = test_sources(root.path());
        let mut item = task("source.a", "m", "c");
        let path = root.path().join("chapter.cbz");
        std::fs::write(&path, b"cbz").unwrap();
        item.status = DownloadStatus::Done;
        item.cbz = Some(path.clone());
        sources.tasks = vec![item.clone()];
        sources.ready.push_back(item.id.clone());

        let offered = sources.take_downloaded().unwrap();
        assert!(
            sources.take_downloaded().is_none(),
            "an in-flight import must not be offered twice"
        );
        sources.import_failed(&offered.id, "cbz 解压失败");
        assert!(sources.status.contains("加入书架失败"));
        assert_eq!(sources.tasks[0].status, DownloadStatus::Done);
        assert!(!sources.tasks[0].linked);
        assert!(path.exists());
        assert!(
            sources.take_downloaded().is_none(),
            "failure must wait for the explicit retry entry"
        );

        sources.requeue_import(&offered.id);
        let retried = sources.take_downloaded().unwrap();
        assert_eq!(retried.id, item.id);
        sources.ack_downloaded(&retried.id);
        assert!(sources.tasks[0].linked);
    }
    #[test]
    fn wasm_source_pages_flow_through_download_and_import_handoff() {
        let root = tempfile::tempdir().unwrap();
        let (base, server) = serve_png(2);
        let urls = vec![format!("{base}/page-1.png"), format!("{base}/page-2.png")];
        let engine = SourceEngine::load_wasm(
            source_manifest(),
            pages_wasm(&urls),
            EngineOptions::default(),
        )
        .unwrap();
        let item = task("test.source", "m1", "c1");
        let pages = pollster::block_on(engine.pages_with(item.manga(), item.chapter())).unwrap();
        assert_eq!(pages.len(), 2);
        assert!(matches!(&pages[0].content,PageContent::Url{url,..} if url==&urls[0]));

        let stop = AtomicU8::new(STOP_NONE);
        let fetched = AtomicUsize::new(0);
        let outcome = execute_task(
            root.path(),
            &item,
            pages,
            &stop,
            |page| {
                fetched.fetch_add(1, Ordering::SeqCst);
                page_bytes(&engine, page)
            },
            |_, _, _| {},
        )
        .unwrap();
        let Outcome::Done { path } = outcome else {
            panic!("download did not finish")
        };
        assert_eq!(fetched.load(Ordering::SeqCst), 2);

        let mut sources = test_sources(root.path());
        sources.tasks = vec![item.clone()];
        sources.running = Some(item.id.clone());
        sources.handle_download_event(DlEvent::Finished {
            id: item.id.clone(),
            path: path.clone(),
        });
        let downloaded = sources
            .take_downloaded()
            .expect("finished chapter offered to the importer");
        assert_eq!(downloaded.id, item.id);
        assert_eq!(downloaded.metadata.source_id, "test.source");
        assert_eq!(downloaded.metadata.manga_id, "m1");
        assert_eq!(downloaded.metadata.chapter_id, "c1");
        assert!(downloaded.path.exists());
        let library = yejian_core::Library::open(&root.path().join("library")).unwrap();
        let book = library.import(&downloaded.path).unwrap();
        library
            .link_chapter(&book.id, &downloaded.metadata)
            .unwrap();
        assert_eq!(book.page_count, 2);
        for page in 0..book.page_count {
            let bytes = library.page_bytes(&book.id, page).unwrap();
            assert_eq!(crate::worker::decode(&bytes, false).unwrap().size, [4, 4]);
        }
        sources.ack_downloaded(&downloaded.id);
        assert!(sources.take_downloaded().is_none());
        server.join().unwrap();
    }
    #[test]
    fn restart_pauses_running_downloads_and_detects_missing_cbz() {
        let dir = tempfile::tempdir().unwrap();
        let existing = dir.path().join("ok.cbz");
        std::fs::write(&existing, b"done").unwrap();
        let mut running = task("source.a", "m", "c1");
        running.status = DownloadStatus::Running;
        let mut done_ok = task("source.a", "m", "c2");
        done_ok.status = DownloadStatus::Done;
        done_ok.cbz = Some(existing);
        let mut done_missing = task("source.a", "m", "c3");
        done_missing.status = DownloadStatus::Done;
        done_missing.cbz = Some(dir.path().join("gone.cbz"));
        let mut tasks = vec![running, done_ok, done_missing];
        normalize_loaded(dir.path(), &mut tasks);
        assert_eq!(tasks[0].status, DownloadStatus::Paused);
        assert!(tasks[0].error.is_some());
        assert_eq!(tasks[1].status, DownloadStatus::Done);
        assert_eq!(tasks[2].status, DownloadStatus::Failed);
        assert!(tasks[2].cbz.is_none());
    }
    #[test]
    fn corrupt_queue_entry_does_not_discard_the_rest() {
        let dir = tempfile::tempdir().unwrap();
        let valid = task("source.a", "m", "c");
        let mut bad_id = task("source.a", "m", "other");
        bad_id.id = "../escape".into();
        let value = serde_json::json!({"tasks":[serde_json::to_value(&valid).unwrap(),{"id":5},serde_json::to_value(&bad_id).unwrap()]});
        std::fs::write(
            dir.path().join("download-tasks.json"),
            serde_json::to_vec(&value).unwrap(),
        )
        .unwrap();
        let loaded = load_tasks(dir.path()).unwrap();
        assert_eq!(loaded.tasks.len(), 1);
        assert_eq!(loaded.tasks[0].id, valid.id);
        assert!(loaded.warning.unwrap().contains("2 条损坏记录"));
    }
    #[test]
    fn corrupt_queue_file_is_reported_and_preserved() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("download-tasks.json");
        let original = b"{ this is not json";
        std::fs::write(&path, original).unwrap();
        let error = load_tasks(dir.path()).unwrap_err();
        assert!(error.to_string().contains("损坏"));
        assert_eq!(std::fs::read(&path).unwrap(), original);

        let (tasks, warning) = load_queue(dir.path());
        assert!(tasks.is_empty());
        assert!(warning.unwrap().contains("读取失败"));
        assert_eq!(std::fs::read(&path).unwrap(), original);
    }
    #[test]
    fn persist_failure_is_reported_and_never_starts_an_unrecorded_task() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("not-a-directory");
        std::fs::write(&root, b"keep").unwrap();
        let sources = test_sources(&root);
        let error = sources.persist().unwrap_err();
        assert!(!error.to_string().is_empty());
        assert_eq!(std::fs::read(&root).unwrap(), b"keep");
        // Allow the new metadata snapshot to be written, then fail the queue

        // file's atomic replacement rather than the earlier snapshot stage.

        let queue_root = dir.path().join("queue-write-failure");
        let blocked = queue_root.join("download-tasks.json");
        std::fs::create_dir_all(&blocked).unwrap();
        std::fs::write(blocked.join("keep.txt"), b"keep").unwrap();
        let mut sources = test_sources(&queue_root);
        sources.selected_source = "source.a".into();
        let book = manga("source.a", "m");
        let chapter = Chapter {
            key: "c".into(),
            title: Some("c".into()),
            ..Default::default()
        };
        sources.enqueue_chapter(&book, &chapter);
        assert!(
            sources.tasks.is_empty(),
            "an unpersisted task must be rolled back"
        );
        assert!(
            sources.running.is_none(),
            "an unpersisted task must not start"
        );
        assert!(sources.status.contains("无法保存下载队列"));
        assert_eq!(std::fs::read(blocked.join("keep.txt")).unwrap(), b"keep");
        // Retrying an existing failed task and bulk-adding chapters must also

        // roll back status/snapshot changes if the atomic queue write fails.

        let mut failed = DownloadTask::from_chapter("source.a", &book, &chapter);
        failed.status = DownloadStatus::Failed;
        failed.error = Some("original failure".into());
        failed.manga_snapshot = None;
        sources.tasks.push(failed.clone());
        sources.enqueue_chapter(&book, &chapter);
        assert_eq!(sources.tasks[0].status, DownloadStatus::Failed);
        assert_eq!(sources.tasks[0].manga_snapshot, None);
        assert_eq!(sources.tasks[0].error, failed.error);
        sources.chapters = vec![
            chapter,
            Chapter {
                key: "new".into(),
                ..Default::default()
            },
        ];
        sources.enqueue_missing(&book);
        assert_eq!(sources.tasks.len(), 1);
        assert_eq!(sources.tasks[0].status, DownloadStatus::Failed);
        assert_eq!(sources.tasks[0].manga_snapshot, None);
        assert!(sources.running.is_none());
    }
    #[test]
    fn invalid_or_linked_task_paths_never_delete_neighbour_files() {
        let root = tempfile::tempdir().unwrap();
        let work = work_root(root.path());
        std::fs::create_dir_all(&work).unwrap();
        let outside = root.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("keep.txt"), b"keep").unwrap();

        let error = remove_task_cache(root.path(), "x/../../outside").unwrap_err();
        assert!(error.to_string().contains("非法"));
        assert_eq!(std::fs::read(outside.join("keep.txt")).unwrap(), b"keep");

        let id = "0123456789abcdef";
        let link = work.join(format!("task-{id}"));
        #[cfg(windows)]
        let linked = std::os::windows::fs::symlink_dir(&outside, &link).is_ok();
        #[cfg(unix)]
        let linked = std::os::unix::fs::symlink(&outside, &link).is_ok();
        #[cfg(not(any(windows, unix)))]
        let linked = false;
        if linked {
            let error = remove_task_cache(root.path(), id).unwrap_err();
            assert!(error.to_string().contains("download-work 之外"));
            assert_eq!(std::fs::read(outside.join("keep.txt")).unwrap(), b"keep");
            let _ = std::fs::remove_dir(&link);
        }
    }
    #[test]
    fn pending_imports_skip_linked_and_missing_files() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("chapter.cbz");
        std::fs::write(&file, b"cbz").unwrap();
        let mut waiting = task("source.a", "m", "c1");
        waiting.status = DownloadStatus::Done;
        waiting.cbz = Some(file.clone());
        let mut linked = task("source.a", "m", "c2");
        linked.status = DownloadStatus::Done;
        linked.cbz = Some(file.clone());
        linked.linked = true;
        let mut missing = task("source.a", "m", "c3");
        missing.status = DownloadStatus::Done;
        missing.cbz = Some(dir.path().join("gone.cbz"));
        let ids = pending_imports(&[waiting.clone(), linked, missing]);
        assert_eq!(ids, vec![waiting.id]);
    }
    #[test]
    fn import_ack_keeps_finished_file_without_downloading_again() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("chapter.cbz");
        std::fs::write(&file, b"cbz").unwrap();
        let mut done = task("source.a", "m", "c");
        done.status = DownloadStatus::Done;
        done.cbz = Some(file);
        let mut sources = test_sources(dir.path());
        sources.ready.push_back(done.id.clone());
        sources.tasks = vec![done.clone()];
        let item = sources.take_downloaded().unwrap();
        assert_eq!(item.id, done.id);
        assert!(item.path.exists());
        // A failed import can put it back; the file is not downloaded again.

        sources.requeue_import(&done.id);
        assert!(sources.take_downloaded().is_some());
        sources.ack_downloaded(&done.id);
        assert!(sources.tasks[0].linked);
        assert!(sources.take_downloaded().is_none());
    }
    #[test]
    fn cancel_removes_only_its_own_work_dir() {
        let root = tempfile::tempdir().unwrap();
        let id = identity_id("source.a", "m", "c");
        let own = task_dir(root.path(), &id).unwrap();
        let other = task_dir(root.path(), "0000000000000000").unwrap();
        std::fs::create_dir_all(&own).unwrap();
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(own.join("page-0001.png"), png()).unwrap();
        std::fs::write(other.join("page-0001.png"), png()).unwrap();
        let downloads = root.path().join("downloads");
        std::fs::create_dir_all(&downloads).unwrap();
        std::fs::write(downloads.join("keep.cbz"), b"keep").unwrap();
        let mut sources = test_sources(root.path());
        sources.tasks = vec![task("source.a", "m", "c")];
        assert!(sources.remove_task_now(&id, "已从下载队列移除"));
        assert!(sources.tasks.is_empty());
        assert!(!own.exists());
        assert!(other.exists());
        assert_eq!(std::fs::read(downloads.join("keep.cbz")).unwrap(), b"keep");
    }
    #[test]
    fn dropping_sources_requests_pause_without_blocking() {
        let root = tempfile::tempdir().unwrap();
        let sources = test_sources(root.path());
        let stop = sources.dl_stop.clone();
        assert_eq!(stop.load(Ordering::SeqCst), STOP_NONE);
        drop(sources);
        assert_eq!(stop.load(Ordering::SeqCst), STOP_PAUSE);
    }
    #[test]
    fn download_lock_is_exclusive_between_instances() {
        let root = tempfile::tempdir().unwrap();
        let first = acquire_download_lock(root.path()).unwrap();
        let error = acquire_download_lock(root.path()).unwrap_err();
        assert!(error.to_string().contains("上一次下载仍在退出"));
        drop(first);
        let _second = acquire_download_lock(root.path()).unwrap();
    }
    #[test]
    fn incomplete_cbz_build_removes_its_part_and_keeps_existing_files() {
        let root = tempfile::tempdir().unwrap();
        let downloads = root.path().join("downloads");
        std::fs::create_dir_all(&downloads).unwrap();
        let existing = downloads.join("previous.cbz");
        std::fs::write(&existing, b"previous download").unwrap();
        let work = root.path().join("download-work").join("task");
        std::fs::create_dir_all(&work).unwrap();
        std::fs::write(work.join("page-0001.png"), png()).unwrap();
        let task = task("source.a", "m", "c");
        let error = build_cbz(root.path(), &task, &work, 2).unwrap_err();
        assert!(error.to_string().contains("缓存缺失"));
        assert_eq!(std::fs::read(&existing).unwrap(), b"previous download");
        let parts: Vec<_> = std::fs::read_dir(&downloads)
            .unwrap()
            .flatten()
            .filter(|entry| {
                entry
                    .path()
                    .extension()
                    .is_some_and(|ext| ext.to_string_lossy() == "part")
            })
            .collect();
        assert!(
            parts.is_empty(),
            "failed CBZ build must not leave a .part file"
        );
    }
    #[test]
    fn full_manga_and_chapter_snapshots_survive_restart_and_page_requests() {
        let root = tempfile::tempdir().unwrap();
        let mut book = manga("test.source", "m42");
        book.title = "Full Work".into();
        book.cover = Some("cover.png".into());
        book.artists = Some(vec!["Artist A".into(), "Artist B".into()]);
        book.authors = Some(vec!["Author A".into()]);
        book.description = Some("简介".into());
        book.url = Some("https://example.test/m42".into());
        book.tags = Some(vec!["action".into(), "drama".into()]);
        book.status = PublishingStatus::Ongoing;
        book.content_rating = ContentRating::Safe;
        book.viewer = Viewer::Vertical;
        book.update_strategy = UpdateStrategy::Never;
        book.next_update_time = Some(1_700_000_000);
        book.chapters = Some(vec![Chapter {
            key: "c0".into(),
            title: Some("Chapter 0".into()),
            ..Default::default()
        }]);
        let chapter = Chapter {
            key: "c42".into(),
            title: Some("Chapter 42".into()),
            chapter_number: Some(42.5),
            volume_number: Some(4.),
            date_uploaded: Some(1_690_000_000),
            scanlators: Some(vec!["Group X".into()]),
            url: Some("https://example.test/c42".into()),
            language: Some("zh".into()),
            thumbnail: Some("thumb.png".into()),
            locked: true,
        };
        // Real enqueue path: writes the shared manga snapshot and the queue.

        let mut sources = test_sources(root.path());
        sources.enqueue_chapter(&book, &chapter);
        let id = identity_id("test.source", "m42", "c42");
        assert!(
            sources.tasks.iter().any(|task| task.id == id),
            "task queued"
        );
        sources.enqueue_chapter(
            &book,
            &Chapter {
                key: "c43".into(),
                title: Some("Chapter 43".into()),
                ..Default::default()
            },
        );
        // One snapshot per work, not one per chapter task.

        let snapshots: Vec<_> = std::fs::read_dir(snapshot_root(root.path()))
            .unwrap()
            .flatten()
            .filter(|entry| {
                entry
                    .path()
                    .extension()
                    .is_some_and(|ext| ext.to_string_lossy() == "manga")
            })
            .collect();
        assert_eq!(snapshots.len(), 1, "same manga must be stored once");
        let json = std::fs::read_to_string(root.path().join("download-tasks.json")).unwrap();
        assert!(
            !json.contains("\"c0\""),
            "the manga chapter list must not be duplicated into task JSON"
        );
        assert!(
            json.contains("\"manga_snapshot\""),
            "task references the shared snapshot"
        );
        // Restart: reload the queue from disk and restore both objects.

        let (tasks, warning) = load_queue(root.path());
        assert!(warning.is_none());
        let restored = tasks
            .into_iter()
            .find(|task| task.id == id)
            .expect("task survives restart");
        assert_eq!(
            restored.manga_title, "Full Work",
            "legacy fields stay readable"
        );
        let manga = restored.manga_for(root.path()).unwrap();
        let restored_chapter = restored.chapter_for().unwrap();
        assert_eq!(manga.authors, book.authors);
        assert_eq!(manga.artists, book.artists);
        assert_eq!(manga.tags, book.tags);
        assert_eq!(manga.description, book.description);
        assert_eq!(manga.viewer, Viewer::Vertical);
        assert_eq!(manga.status, PublishingStatus::Ongoing);
        assert_eq!(manga.chapters.as_ref().map(Vec::len), Some(1));
        assert_eq!(restored_chapter.scanlators, chapter.scanlators);
        assert_eq!(restored_chapter.date_uploaded, chapter.date_uploaded);
        assert_eq!(restored_chapter.thumbnail, chapter.thumbnail);
        assert!(restored_chapter.locked);
        // These are exactly the bytes the interpreter sends to get_page_list.

        assert_eq!(
            manga.to_postcard_bytes(),
            book.to_postcard_bytes(),
            "source receives the identical manga bytes"
        );
        assert_eq!(
            restored_chapter.to_postcard_bytes(),
            chapter.to_postcard_bytes(),
            "source receives the identical chapter bytes"
        );
        // A real WASM engine still accepts the restored objects for page listing.

        let urls = vec!["https://example.test/page-1.png".to_string()];
        let engine = SourceEngine::load_wasm(
            source_manifest(),
            pages_wasm(&urls),
            EngineOptions::default(),
        )
        .unwrap();
        let pages = pollster::block_on(engine.pages_with(
            restored.manga_for(root.path()).unwrap(),
            restored.chapter_for().unwrap(),
        ))
        .unwrap();
        assert_eq!(pages.len(), 1);
    }
    #[test]
    fn broken_recorded_snapshot_fails_while_legacy_tasks_keep_the_fallback() {
        let root = tempfile::tempdir().unwrap();
        let mut book = manga("test.source", "m7");
        book.authors = Some(vec!["Author A".into()]);
        book.tags = Some(vec!["drama".into()]);
        book.viewer = Viewer::RightToLeft;
        let chapter = Chapter {
            key: "c7".into(),
            title: Some("Chapter 7".into()),
            scanlators: Some(vec!["Group".into()]),
            ..Default::default()
        };
        let task = DownloadTask::from_chapter("test.source", &book, &chapter);
        assert!(
            task.chapter_snapshot.is_some(),
            "new tasks record snapshots"
        );
        // Old queues have no snapshot at all and keep the reduced DTO fallback.

        let mut legacy = task.clone();
        legacy.manga_snapshot = None;
        legacy.chapter_snapshot = None;
        assert_eq!(legacy.manga_for(root.path()).unwrap().title, "m7");
        assert_eq!(legacy.chapter_for().unwrap().key, "c7");
        // A task that recorded a snapshot never degrades to that fallback.

        let key = manga_snapshot_key("test.source", "m7");
        let mut recorded = task.clone();
        recorded.manga_snapshot = Some(key.clone());
        let error = recorded.manga_for(root.path()).unwrap_err().to_string();
        assert!(
            error.contains("漫画快照不可用") && error.contains("请回到作品详情"),
            "missing snapshot is explicit: {error}"
        );

        recorded.manga_snapshot = Some("../escape".into());
        assert!(
            recorded
                .manga_for(root.path())
                .unwrap_err()
                .to_string()
                .contains("非法")
        );

        std::fs::create_dir_all(snapshot_root(root.path())).unwrap();
        let path = manga_snapshot_path(root.path(), &key).unwrap();
        std::fs::write(&path, b"not a postcard").unwrap();
        recorded.manga_snapshot = Some(key.clone());
        let error = recorded.manga_for(root.path()).unwrap_err().to_string();
        assert!(
            error.contains("已损坏"),
            "corrupt snapshot is explicit: {error}"
        );

        let mut other = book.clone();
        other.key = "another".into();
        std::fs::write(&path, other.to_postcard_bytes()).unwrap();
        let error = recorded.manga_for(root.path()).unwrap_err().to_string();
        assert!(
            error.contains("不匹配"),
            "snapshot of another manga is rejected: {error}"
        );

        let mut bad_chapter = task.clone();
        bad_chapter.manga_snapshot = None;
        bad_chapter.chapter_snapshot = Some(vec![0, 1, 2]);
        assert!(
            bad_chapter
                .chapter_for()
                .unwrap_err()
                .to_string()
                .contains("章节快照已损坏")
        );
        // Re-adding the chapter from the manga detail page rebuilds the

        // snapshot from the new complete object and requeues the task.

        let mut updated = book.clone();
        updated.authors = Some(vec!["New Author".into()]);
        let mut sources = test_sources(root.path());
        let mut failed = DownloadTask::from_chapter("test.source", &book, &chapter);
        failed.status = DownloadStatus::Failed;
        failed.manga_snapshot = Some("0000000000000000".into());
        failed.chapter_snapshot = None;
        sources.tasks = vec![failed];
        sources.enqueue_chapter(&updated, &chapter);
        let task_id = identity_id("test.source", "m7", "c7");
        let retried = sources
            .tasks
            .iter()
            .find(|item| item.id == task_id)
            .unwrap();
        assert_ne!(
            retried.status,
            DownloadStatus::Failed,
            "failed chapter is requeued"
        );
        let rebuilt = retried
            .manga_for(root.path())
            .expect("re-join writes a readable snapshot");
        assert_eq!(rebuilt.authors, Some(vec!["New Author".into()]));
    }
    #[test]
    fn set_stream_page_records_the_read_timestamp() {
        let root = tempfile::tempdir().unwrap();
        let mut sources = test_sources(root.path());
        let mut item = task("source.a", "m", "c1");
        item.status = DownloadStatus::Running;
        item.pages_total = 30;
        sources.tasks.push(item);
        let id = identity_id("source.a", "m", "c1");
        sources.set_stream_page(&id, 7);
        let task = sources.tasks.iter().find(|task| task.id == id).unwrap();
        assert_eq!(task.read_page, Some(7));
        assert!(task.read_at.is_some());
    }
    #[test]
    fn bookmarked_online_chapter_reopens_its_stream_task() {
        let root = tempfile::tempdir().unwrap();
        let mut sources = test_sources(root.path());
        let mut download = task("source.a", "m1", "c1");
        download.id = "aaaaaaaaaaaaaaaa".into();
        sources.tasks.push(download);
        assert!(!sources.open_bookmarked("source.a", "m1", "missing"));
        assert!(sources.open_bookmarked("source.a", "m1", "c1"));
        let stream = sources.take_stream().unwrap();
        assert_eq!(stream.id, "aaaaaaaaaaaaaaaa");
        assert_eq!(stream.metadata.chapter_id, "c1");
    }
}
