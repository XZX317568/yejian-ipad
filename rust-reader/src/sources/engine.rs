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
use super::downloads::*;
use super::covers::*;
use super::updates::*;
use super::global_search::*;
use super::migrate::*;
use super::storage::*;
use super::web::*;
use super::repos::*;
use super::ui_browse::*;
use super::ui_detail::*;
use super::ui_library::*;
use super::ui_downloads::*;
#[cfg(any(test, debug_assertions))]
use super::test_support::*;

#[derive(Clone)]
pub(super) enum Task {
    List,
    Fetch(String),
    Install(RepositorySource),
    /// Remove one source directory; follows/downloaded chapters are kept.
    Uninstall(String),
    /// Refresh every saved repository index for update badges.
    FetchAllRepos(Vec<String>),
    Metadata(String),
    Search {
        source: String,
        query: String,
        page: i32,
        filters: Vec<FilterValue>,
    },
    Browse {
        source: String,
        listing: String,
        page: i32,
    },
    Chapters(String, Manga),
    Login {
        source: String,
        key: String,
        /// `Ok((user, password))` for basic login, `Err(cookies)` for web.
        credentials: Result<(String, String), Vec<(String, String)>>,
    },
    /// A `settings.json` button notification forwarded to the source.
    Notify {
        source: String,
        notification: String,
    },
}

pub(super) enum Reply {
    Installed {
        list: Vec<InstalledSource>,
        updated: Option<String>,
    },
    Repository(RepositoryIndex),
    /// One result per requested repo URL; failures keep the old cache.
    RepoAll(Vec<(String, Result<RepositoryIndex, String>)>),
    Login {
        key: String,
        user: String,
        ok: bool,
        web: bool,
    },
    Metadata {
        filters: Vec<Filter>,
        listings: Vec<Listing>,
        filter_error: Option<String>,
        listing_error: Option<String>,
    },
    Results {
        source: String,
        query: String,
        page: i32,
        entries: Vec<Manga>,
        has_next: bool,
    },
    Chapters(String, Manga, Vec<Chapter>),
    Notified,
}

#[derive(Default, Clone)]
pub(super) struct BrowseSession {
    pub(super) query: String,
    pub(super) filters: Vec<FilterValue>,
    pub(super) listing: Option<String>,
}

#[derive(Debug)]
pub(super) enum Outcome {
    Done { path: PathBuf },
    Aborted { how: u8 },
}

pub(super) fn request(
    handler: &RequestHandler,
    url: String,
    headers: Vec<(String, String)>,
    body: Option<Vec<u8>>,
) -> anyhow::Result<Vec<u8>> {
    let response = handler(NetRequestData {
        method: if body.is_some() { "POST" } else { "GET" }.into(),
        url,
        headers,
        body,
        timeout_ms: 60000,
    });
    if let Some(error) = response.error {
        anyhow::bail!("{error}");
    }
    anyhow::ensure!(
        (200..300).contains(&response.status_code),
        "图片服务器返回 HTTP {}",
        response.status_code
    );
    anyhow::ensure!(
        response.body.len() <= 64 * 1024 * 1024,
        "单页图片超过 64 MB"
    );
    Ok(response.body)
}

pub(super) fn page_bytes(engine: &SourceEngine, page: &Page) -> anyhow::Result<Vec<u8>> {
    let handler = engine.request_handler();
    match &page.content {
        PageContent::Url { url, context } => {
            let modified = pollster::block_on(engine.image_request(url, context.clone()))?;
            let bytes = if let Some(req) = modified {
                request(&handler, req.url, req.headers, req.body)
            } else {
                request(&handler, url.clone(), vec![], None)
            }?;
            // Obfuscated-image sources deobfuscate through

            // `process_page_image`; it runs on the engine's single worker

            // thread, which serializes the WASM calls even though the HTTP

            // fetches above run concurrently.
            if !engine.features().processes_pages {
                return Ok(bytes);
            }
            match pollster::block_on(engine.process_page_image(bytes, context.clone()))? {
                Some(processed) => Ok(processed),
                None => anyhow::bail!("书源的图片处理未返回有效图像"),
            }
        }
        PageContent::Image {
            image_data: Some(bytes),
            ..
        } => Ok(bytes.clone()),
        PageContent::ZipFile { url, file_path } => {
            let bytes = request(&handler, url.clone(), vec![], None)?;
            let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes))?;
            let mut entry = zip.by_name(file_path)?;
            anyhow::ensure!(entry.size() <= 64 * 1024 * 1024, "单页图片超过 64 MB");
            let mut data = Vec::new();
            entry.read_to_end(&mut data)?;
            Ok(data)
        }
        PageContent::Text(_) => anyhow::bail!("此章节包含文字页，当前漫画阅读模式无法显示"),
        _ => anyhow::bail!("书源未返回有效图像数据"),
    }
}

pub(super) fn execute_task(
    root: &Path,
    task: &DownloadTask,
    pages: Vec<Page>,
    stop: &AtomicU8,
    fetch: impl Fn(&Page) -> anyhow::Result<Vec<u8>> + Sync,
    mut report: impl FnMut(usize, usize, Option<String>),
) -> anyhow::Result<Outcome> {
    anyhow::ensure!(!pages.is_empty(), "这一章没有可读页面");
    let dir = task_dir(root, &task.id)?;
    let signature = page_signature(&pages);
    let signature_file = dir.join("page-list.v2");
    let recorded = match std::fs::read_to_string(&signature_file) {
        Ok(previous) => Some(previous),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };
    if recorded.as_deref() != Some(signature.as_str()) {
        if recorded.is_none() && task.pages_signature.as_deref() == Some(signature.as_str()) {
            // Old queues kept only the signature and accepted image headers.

            // Keep every legacy page the current reader can actually display.

            for number in existing_pages(&dir) {
                if let Some(path) = stored_page(&dir, number) {
                    let bytes = std::fs::read(&path)?;
                    if crate::worker::image_reader(&bytes)
                        .and_then(|r| Ok(r.decode()?))
                        .is_err()
                    {
                        std::fs::remove_file(path)?;
                    }
                }
            }
        } else {
            reset_page_cache(&dir)?;
        }
        std::fs::create_dir_all(&dir)?;
        let part = dir.join("page-list.part");
        let mut file = std::fs::File::create(&part)?;
        file.write_all(signature.as_bytes())?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&part, &signature_file)?;
    }
    if task.pages_signature.as_deref() != Some(signature.as_str()) {
        report(0, pages.len(), Some(signature));
    }
    let summary = fetch_missing_pages(
        &dir,
        &pages,
        fetch,
        |done, total| report(done, total, None),
        stop,
    )?;
    let how = stop.load(Ordering::SeqCst);
    if summary.aborted || how != STOP_NONE {
        return Ok(Outcome::Aborted {
            how: if how == STOP_NONE { STOP_PAUSE } else { how },
        });
    }
    anyhow::ensure!(summary.done == pages.len(), "章节存在未完成的图片");
    let path = build_cbz(root, task, &dir, pages.len())?;
    // The CBZ is already complete; a cleanup problem must not turn a real

    // download into a failed task or delete anything outside this cache.

    let _ = remove_task_cache(root, &task.id);
    Ok(Outcome::Done { path })
}

impl Sources {
    pub(super)fn submit(&mut self, task: Task) {
        self.request_id = self.request_id.wrapping_add(1);
        self.current_request
            .store(self.request_id, Ordering::Relaxed);
        self.failed = None;
        self.pending = Some(task.clone());
        if self.tx.send((self.request_id, task)).is_err() {
            self.busy = false;
            self.failed = self.pending.take();
            self.status = "书源线程已停止，请重新打开应用".into();
        } else {
            self.busy = true;
            self.status = "正在加载…".into();
        }
    }
}

