use crate::{
    design,
    follows::{self, Follow},
    source_filters::FilterEditor,
    source_repos::{Lang, SourceRepos},
};
use anyhow::Context as _;
use egui::*;
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU8, AtomicU64, AtomicUsize, Ordering},
        mpsc,
    },
    thread,
};
use yejian_core::{ChapterLink, ChapterMetadata};
use yejian_sources::*;
use super::*;
use super::covers::*;
use super::downloads::*;
use super::engine::*;
use super::global_search::*;
use super::migrate::*;
use super::repos::*;
use super::storage::*;
use super::ui_browse::*;
use super::ui_detail::*;
use super::ui_downloads::*;
use super::ui_library::*;
use super::updates::*;
use super::web::*;


#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct DownloadTask {
    pub(super) id: String,
    pub(super) source_id: String,
    pub(super) manga_id: String,
    pub(super) manga_title: String,
    #[serde(default)]
    pub(super) manga_url: Option<String>,
    #[serde(default)]
    pub(super) manga_cover: Option<String>,
    pub(super) chapter_id: String,
    pub(super) chapter_title: String,
    #[serde(default)]
    pub(super) chapter_number: Option<f64>,
    #[serde(default)]
    pub(super) volume_number: Option<f64>,
    #[serde(default)]
    pub(super) chapter_url: Option<String>,
    #[serde(default)]
    pub(super) chapter_language: Option<String>,
    /// Id of the stored snapshot of the complete `Manga` object passed to

    /// `get_page_list`. The bytes live once per manga under

    /// `download-work/snapshots`, so many chapter tasks of the same work never

    /// duplicate its chapter list in this JSON.

    #[serde(default)]
    pub(super) manga_snapshot: Option<String>,
    /// Postcard bytes of the exact `Chapter` object the source is asked for

    /// pages, keeping scanlators/date/thumbnail/locked etc. across restarts.

    #[serde(default)]
    pub(super) chapter_snapshot: Option<Vec<u8>>,
    /// Last page read online before the chapter joined the library.

    #[serde(default)]
    pub(super) read_page: Option<usize>,
    /// When `read_page` last moved; lets 继续阅读 pick the most recently read

    /// chapter across library books and online streams.

    #[serde(default)]
    pub(super) read_at: Option<i64>,
    pub(super) status: DownloadStatus,
    #[serde(default)]
    pub(super) error: Option<String>,
    #[serde(default)]
    pub(super) pages_done: usize,
    #[serde(default)]
    pub(super) pages_total: usize,
    /// Page-list fingerprint the cached page files belong to. A different list

    /// clears this chapter's own cache before refetching.

    #[serde(default)]
    pub(super) pages_signature: Option<String>,
    #[serde(default)]
    pub(super) cbz: Option<PathBuf>,
    /// True once the library import + chapter link succeeded. Unlinked

    /// finished files are offered again on the next start instead of being

    /// downloaded again.

    #[serde(default)]
    pub(super) linked: bool,
    /// Queued by 自动下载新章节 — the 我的追更 row reports these as
    /// "自动下载中 N 章" while they wait or run.
    #[serde(default)]
    pub(super) auto: bool,
}

impl DownloadTask {
    pub(super)fn from_chapter(source: &str, manga: &Manga, chapter: &Chapter) -> Self {
        let chapter_title = chapter.title.clone().unwrap_or_else(|| {
            chapter
                .chapter_number
                .map(|n| format!("第 {n} 话"))
                .unwrap_or_else(|| chapter.key.clone())
        });
        Self {
            id: identity_id(source, &manga.key, &chapter.key),
            source_id: source.to_string(),
            manga_id: manga.key.clone(),
            manga_title: manga.title.clone(),
            manga_url: manga.url.clone(),
            manga_cover: manga.cover.clone(),
            chapter_id: chapter.key.clone(),
            chapter_title,
            chapter_number: chapter.chapter_number.map(|number| number as f64),
            volume_number: chapter.volume_number.map(|number| number as f64),
            chapter_url: chapter.url.clone(),
            chapter_language: chapter.language.clone(),
            manga_snapshot: None,
            chapter_snapshot: Some(chapter.to_postcard_bytes()),
            read_page: None,
            read_at: None,
            status: DownloadStatus::Queued,
            error: None,
            pages_done: 0,
            pages_total: 0,
            pages_signature: None,
            cbz: None,
            linked: false,
            auto: false,
        }
    }
    pub(super)fn manga(&self) -> Manga {
        Manga {
            source_key: self.source_id.clone(),
            key: self.manga_id.clone(),
            title: self.manga_title.clone(),
            cover: self.manga_cover.clone(),
            url: self.manga_url.clone(),
            ..Default::default()
        }
    }
    pub(super)fn chapter(&self) -> Chapter {
        Chapter {
            key: self.chapter_id.clone(),
            title: Some(self.chapter_title.clone()),
            chapter_number: self.chapter_number.map(|n| n as f32),
            volume_number: self.volume_number.map(|n| n as f32),
            url: self.chapter_url.clone(),
            language: self.chapter_language.clone(),
            ..Default::default()
        }
    }
    /// The original `Manga` for this task. Only queues written before

    /// snapshots existed fall back to the reduced legacy fields; a task that

    /// *has* a snapshot must restore it exactly, otherwise it fails visibly

    /// instead of sending a half-empty object to the source.

    pub(super)fn manga_for(&self, root: &Path) -> anyhow::Result<Manga> {
        let Some(key) = self.manga_snapshot.as_deref() else {
            return Ok(self.manga());
        };
        let path = manga_snapshot_path(root, key)?;
        let bytes = std::fs::read(&path).map_err(|error| {
            anyhow::anyhow!(
                "漫画快照不可用（{key}）：{error}；请回到作品详情重新加入该章节以重建快照"
            )
        })?;
        let mut manga = Manga::from_postcard_bytes(&bytes).map_err(|error| {
            anyhow::anyhow!(
                "漫画快照已损坏（{key}）：{error}；请回到作品详情重新加入该章节以重建快照"
            )
        })?;
        anyhow::ensure!(
            manga.key == self.manga_id,
            "漫画快照与任务不匹配（{key}）；请回到作品详情重新加入该章节以重建快照"
        );
        manga.source_key = self.source_id.clone();
        Ok(manga)
    }
    /// The original `Chapter` for this task. Legacy queues without a snapshot

    /// keep working; a broken snapshot fails instead of silently degrading.

    pub(super)fn chapter_for(&self) -> anyhow::Result<Chapter> {
        let Some(bytes) = self.chapter_snapshot.as_deref() else {
            return Ok(self.chapter());
        };
        let chapter = Chapter::from_postcard_bytes(bytes).map_err(|error| {
            anyhow::anyhow!("章节快照已损坏：{error}；请回到作品详情重新加入该章节以重建快照")
        })?;
        anyhow::ensure!(
            chapter.key == self.chapter_id,
            "章节快照与任务不匹配；请回到作品详情重新加入该章节以重建快照"
        );
        Ok(chapter)
    }
    pub(super)fn metadata(&self) -> ChapterMetadata {
        ChapterMetadata {
            source_id: self.source_id.clone(),
            manga_id: self.manga_id.clone(),
            manga_title: self.manga_title.clone(),
            chapter_id: self.chapter_id.clone(),
            chapter_title: self.chapter_title.clone(),
            chapter_number: self.chapter_number,
            volume_number: self.volume_number,
        }
    }
}

#[derive(Default, Serialize, Deserialize)]
pub(super) struct TaskStore {
    #[serde(default)]
    pub(super) tasks: Vec<DownloadTask>,
}

#[derive(Debug)]
pub(super) struct LoadedTasks {
    pub(super) tasks: Vec<DownloadTask>,
    /// Present when part of the file was unreadable. The original file is

    /// never overwritten just because startup could not parse all of it.
    pub(super) warning: Option<String>,
}

pub(super) fn load_tasks(root: &Path) -> anyhow::Result<LoadedTasks> {
    let path = root.join("download-tasks.json");
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(LoadedTasks {
                tasks: Vec::new(),
                warning: None,
            });
        }
        Err(error) => return Err(anyhow::anyhow!("读取 {} 失败：{error}", path.display())),
    };
    let value = serde_json::from_slice::<serde_json::Value>(&bytes)
        .map_err(|error| anyhow::anyhow!("下载队列 JSON 损坏：{error}"))?;
    let Some(items) = value.get("tasks").and_then(|tasks| tasks.as_array()) else {
        return Err(anyhow::anyhow!("下载队列 JSON 缺少 tasks 数组"));
    };
    // Parse task by task: one damaged entry must not discard the rest of the

    // queue, but tell the user that something was skipped.

    let mut tasks = Vec::new();
    let mut skipped = 0usize;
    for item in items {
        match serde_json::from_value::<DownloadTask>(item.clone()) {
            Ok(task) if valid_task_id(&task.id) => tasks.push(task),
            Err(_) => skipped += 1,
            Ok(_) => skipped += 1,
        }
    }
    let warning =
        (skipped > 0).then(|| format!("下载队列有 {skipped} 条损坏记录已跳过，原文件已保留"));
    Ok(LoadedTasks { tasks, warning })
}

pub(super) fn load_queue(root: &Path) -> (Vec<DownloadTask>, Option<String>) {
    match load_tasks(root) {
        Ok(loaded) => {
            let mut tasks = loaded.tasks;
            normalize_loaded(root, &mut tasks);
            (tasks, loaded.warning)
        }
        Err(error) => (
            Vec::new(),
            Some(format!(
                "下载队列读取失败：{error:#}；原文件已保留，未开始任何下载"
            )),
        ),
    }
}

pub(super) fn recover_completed_archive(root: &Path, task: &DownloadTask) -> Option<PathBuf> {
    let archive = chapter_archive(root, task);
    archive.is_file().then_some(archive)
}

pub(super) fn normalize_loaded(root: &Path, tasks: &mut [DownloadTask]) {
    for task in tasks {
        if let Some(path) = recover_completed_archive(root, task) {
            // iOS can relocate the data container during an update. A done

            // task still records the old absolute CBZ path; prefer this

            // library's own complete archive and keep its import state.

            if task.status != DownloadStatus::Done {
                task.status = DownloadStatus::Done;
                task.error = None;
                task.linked = false;
                task.pages_done = task.pages_total;
            }
            task.cbz = Some(path);
        } else if task.status == DownloadStatus::Running {
            task.status = DownloadStatus::Paused;
            task.error = Some("上次下载中断，点“继续”恢复".into());
        } else if task.status == DownloadStatus::Done
            && !task.cbz.as_ref().is_some_and(|path| path.exists())
        {
            task.status = DownloadStatus::Failed;
            task.error = Some("下载文件已丢失，可重试".into());
            task.cbz = None;
        }
    }
}

pub(super) fn pending_imports(tasks: &[DownloadTask]) -> Vec<String> {
    tasks
        .iter()
        .filter(|task| {
            task.status == DownloadStatus::Done
                && !task.linked
                && task.cbz.as_ref().is_some_and(|path| path.exists())
        })
        .map(|task| task.id.clone())
        .collect()
}

pub(super) enum EnqueueOutcome {
    Added,
    Duplicate,
    AlreadyDownloaded,
}

pub(super) fn enqueue_task(tasks: &mut Vec<DownloadTask>, task: DownloadTask) -> EnqueueOutcome {
    if let Some(existing) = tasks.iter_mut().find(|existing| existing.id == task.id) {
        if existing.status == DownloadStatus::Done {
            if existing.cbz.as_ref().is_some_and(|path| path.exists()) {
                return EnqueueOutcome::AlreadyDownloaded;
            }
            existing.status = DownloadStatus::Queued;
            existing.error = None;
            existing.cbz = None;
        }
        // Re-adding from the manga detail page carries freshly saved complete

        // objects. Replace the references so a task whose snapshot file was

        // lost or corrupted can be retried without losing its cached pages.

        existing.manga_snapshot = task.manga_snapshot;
        existing.chapter_snapshot = task.chapter_snapshot;
        if existing.status == DownloadStatus::Failed {
            existing.status = DownloadStatus::Queued;
            existing.error = None;
        }
        return EnqueueOutcome::Duplicate;
    }
    tasks.push(task);
    EnqueueOutcome::Added
}

pub(super) fn next_queued(tasks: &[DownloadTask]) -> Option<usize> {
    tasks
        .iter()
        .position(|task| task.status == DownloadStatus::Queued)
}

pub(super) struct PartCleanup {
    pub(super) path: PathBuf,
    pub(super) armed: bool,
}

impl PartCleanup {
    pub(super)fn new(path: &Path) -> Self {
        Self {
            path: path.to_path_buf(),
            armed: true,
        }
    }
    pub(super)fn keep(mut self) {
        self.armed = false;
    }
}

impl Drop for PartCleanup {
    fn drop(&mut self) {
        if self.armed {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

