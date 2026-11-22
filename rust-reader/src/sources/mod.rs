
use crate::follows::self;
use crate::follows::Follow;
use crate::source_filters::FilterEditor;
use crate::source_repos::Lang;
use crate::source_repos::SourceRepos;
use egui::*;



use std::collections::HashSet;
use std::collections::VecDeque;



use std::path::PathBuf;
use std::sync::Arc;

use std::sync::atomic::AtomicU8;
use std::sync::atomic::AtomicU64;

use std::sync::atomic::Ordering;
use std::sync::mpsc;

use yejian_core::ChapterMetadata;
use yejian_sources::*;

mod engine;
mod downloads;
mod covers;
mod updates;
mod global_search;
mod migrate;
mod storage;
mod tasks;
mod web;
mod repos;
mod ui_browse;
mod ui_detail;
mod ui_library;
mod ui_downloads;
#[cfg(any(test, debug_assertions))]
mod test_support;
use engine::BrowseSession;
use covers::CoverDone;
use covers::CoverHttpJob;
use covers::CoverReq;
use tasks::DownloadTask;
use global_search::GLOBAL_SEARCH_TIMEOUT;
use global_search::GlobalCmd;
use global_search::GlobalEvent;
use global_search::GlobalGroup;
use engine::Outcome;
use engine::Reply;
use ui_browse::ResultsState;
use web::SettingsStore;
use storage::StorageCmd;
use storage::StorageEvent;
use engine::Task;
use web::WebSession;
use web::WebSiteCookies;
use ui_detail::chapter_list;
use engine::execute_task;
use covers::fetch_follow_cover;
use global_search::global_search_run;
use tasks::load_queue;
use web::load_source_settings;
use web::load_web_cookies;
use engine::page_bytes;
use tasks::pending_imports;
use tasks::recover_completed_archive;
use engine::request;
use covers::shrink_cover;
use covers::source_cover_path;
use storage::storage_worker_run;
use covers::store_source_cover;

// Re-exports: names other modules reach as `crate::sources::X`, at the same
// visibility they had before the split.
pub use downloads::{STREAM_PREFIX, StreamChapter};
pub(crate) use downloads::{identity_id, manga_snapshot_path, save_manga_snapshot, stream_page};
pub(crate) use global_search::GlobalState;
pub(crate) use migrate::{MigratePost, MigrateState};
pub use storage::SourceStorage;
pub(crate) use updates::{DEFAULT_FOLLOW_INTERVAL_MIN, FOLLOW_INTERVAL_CHOICES};

// Names used by this parent module itself (kept `pub(super)` in the children).
use downloads::{DlCmd, DlEvent, STOP_NONE, STOP_PAUSE, acquire_download_lock};
use global_search::{GlobalEngine, RealGlobalEngine};
use migrate::MigrateOutcome;
use updates::UpdateEvent;

pub struct Sources {
    root: PathBuf,
    tx: mpsc::Sender<(u64, Task)>,
    rx: mpsc::Receiver<(u64, Result<Reply, String>)>,
    installed: Vec<InstalledSource>,
    repository: Option<RepositoryIndex>,
    results: ResultsState,
    chapters: Vec<Chapter>,
    chapter_query: String,
    chapters_newest_first: bool,
    open_chapter: Option<ChapterMetadata>,
    library_chapters: std::collections::HashMap<String, ChapterMetadata>,
    selected_source: String,
    selected_manga: Option<Manga>,
    url: String,
    query: String,
    /// "current" or "global": whether the search field covers one or all sources.
    search_scope: String,
    global_gen: u64,
    global_query: String,
    global_groups: Vec<GlobalGroup>,
    /// New scope choice for `ReaderApp` to persist under `search-scope`.
    scope_dirty: Option<String>,
    /// Submitted keywords, newest first, capped at 20. Persisted by
    /// `ReaderApp` under `search-history`.
    search_history: Vec<String>,
    /// History changed and `ReaderApp` should persist it.
    history_dirty: bool,
    /// A tap on 清空 asks for confirmation before dropping the history.
    history_confirm: bool,
    /// Private reading suppresses history recording (mirrors the setting).
    private: bool,
    global_tx: mpsc::Sender<GlobalCmd>,
    global_rx: mpsc::Receiver<GlobalEvent>,
    storage_tx: mpsc::Sender<StorageCmd>,
    storage_rx: mpsc::Receiver<StorageEvent>,
    /// Latest sizes from `yejian-source-storage`; all scans run off-thread.
    storage: Option<SourceStorage>,
    request_id: u64,
    current_request: Arc<AtomicU64>,
    pending: Option<Task>,
    failed: Option<Task>,
    filters: Vec<Filter>,
    listings: Vec<Listing>,
    applied_filters: Vec<FilterValue>,
    listing: Option<String>,
    filter_editor: Option<FilterEditor>,
    metadata_loaded: bool,
    filter_error: Option<String>,
    listing_error: Option<String>,
    sessions: std::collections::HashMap<String, BrowseSession>,
    repository_search: String,
    #[cfg(debug_assertions)]
    preview_mode: Option<String>,
    busy: bool,
    status: String,
    /// When the current status string was first observed by the UI. Success
    /// notices fade out four seconds later; errors stay until the next
    /// action replaces them.
    status_seen: Option<(String, std::time::Instant)>,
    tasks: Vec<DownloadTask>,
    running: Option<String>,
    ready: VecDeque<String>,
    /// Import attempts that are really in flight, kept only in memory.

    /// `ack_downloaded` or `import_failed` clears the entry; while it is

    /// present the UI never offers a duplicate retry for the same file.
    import_attempts: HashSet<String>,
    dl_tx: mpsc::Sender<DlCmd>,
    dl_rx: mpsc::Receiver<DlEvent>,
    dl_stop: Arc<AtomicU8>,
    stream_request: Option<StreamChapter>,
    /// A download paused only so that a chapter being read can run first.

    /// It goes back to the queue instead of waiting for a manual "继续".
    preempted: Option<String>,
    follows: Vec<Follow>,
    /// A followed manga to open once its source has finished loading.
    pending_open: Option<Manga>,
    /// Detail was opened from the shelf: 返回 pops back to 书架 instead of
    /// the source browser.
    shelf_origin: bool,
    shelf_return: bool,
    /// A shelf 继续阅读 asked for the resume action once chapters arrive.
    pending_resume: bool,
    checking: usize,
    /// Whether the in-flight batch came from the scheduler — its completion

    /// summary goes to the notice bar rather than the sources status line.
    checking_background: bool,
    auto_checked: bool,
    /// egui context the scheduler uses for one-shot wake repaints.
    ctx: Context,
    /// Minutes between automatic follow checks; `0` disables them. Persisted

    /// as `follow-interval` through the library settings store.
    follow_interval_min: u64,
    /// egui-time deadline of the next scheduled check. Kept as wall-clock

    /// `Instant` so a suspended frame clock cannot skip a whole interval.
    follow_next_due: Option<std::time::Instant>,
    /// Fresh chapters collected by finished checks, waiting for the shell
    /// to turn into system notifications (deduped in notify.rs).
    notify_pending: Vec<crate::notify::FreshUpdate>,
    /// New-chapter summary surfaced to the app shell's notice bar once a

    /// background (scheduled) check finishes.
    pub(crate) follow_notice: Option<String>,
    up_tx: mpsc::Sender<Vec<(String, Manga)>>,
    up_rx: mpsc::Receiver<UpdateEvent>,
    /// Decoded follow covers by snapshot key; `None` means not on disk yet.
    covers: std::collections::HashMap<String, Option<TextureHandle>>,
    /// Each followed manga's cover URL resolved from its snapshot, once per

    /// session — the grid's `source-covers` cache keys are URL-based, so the

    /// follow rows reuse the same pipeline instead of `follow-covers/`.
    follow_cover_urls: std::collections::HashMap<String, Option<String>>,
    /// Result-grid covers. `cover_queue` only holds cells near the viewport;

    /// `cover_inflight` dedupes sent-but-unfinished requests and

    /// `cover_outstanding` bounds how many are with the workers at once.
    cover_req_tx: mpsc::Sender<CoverReq>,
    cover_done_rx: mpsc::Receiver<CoverDone>,
    cover_queue: VecDeque<CoverReq>,
    cover_inflight: HashSet<String>,
    cover_outstanding: usize,
    /// URLs that failed once this session; placeholders render instead of

    /// hammering a dead image host every frame.
    cover_failed: HashSet<String>,
    cover_ready: std::collections::HashMap<String, Option<ColorImage>>,
    cover_missing_disk: HashSet<String>,
    cover_textures: std::collections::HashMap<String, TextureHandle>,
    cover_lru: VecDeque<String>,
    /// Page already auto-loaded when its last grid row scrolled into view, so

    /// the marker cannot fire the same request again on the next frame.
    auto_next_page: Option<i32>,
    /// Reading progress per `identity_id` chapter, fed by the host each frame

    /// so the detail page can offer 继续阅读 and row read states.
    chapter_progress: std::collections::HashMap<String, (usize, usize, Option<i64>)>,
    description_expanded: bool,
    /// Clickable rects registered this frame, merged into the app's hitboxes

    /// so acceptance and automation can find grid cells and detail actions.
    pub(crate) hitboxes: std::collections::BTreeMap<String, Rect>,
    settings_open: bool,
    settings_changed: bool,
    /// User choices per source, persisted in `source-settings.json`.
    source_settings: SettingsStore,
    /// Sources whose loaded engines must restart to read changed settings.
    stale_engines: Arc<std::sync::Mutex<HashSet<String>>>,
    /// Origin a Cloudflare challenge blocked, until the user verifies.
    challenge: Option<String>,
    /// `auto-download` setting: "off" (default), "all", "latest1", "latest3".
    auto_download: String,
    /// 换源 panel state; while `Some` it owns the global-generation events.
    migrate: Option<MigrateState>,
    /// Background migration completions (one-shot threads report here).
    migrate_tx: mpsc::Sender<anyhow::Result<MigrateOutcome>>,
    migrate_rx: mpsc::Receiver<anyhow::Result<MigrateOutcome>>,
    /// Migration side-effects the host still owes the user once (tracker
    /// re-key + optional old-download purge + opening the new detail).
    migrate_post: Option<MigratePost>,
    /// The in-flight verification/login page — an in-app WebView on mobile

    /// or the system-browser + paste dialog on desktop.
    web_session: Option<WebSession>,
    login_user: String,
    login_pass: String,
    /// Cookies from verification/login pages by URL, in `web-cookies.json`.
    web_cookies: std::collections::BTreeMap<String, WebSiteCookies>,
    /// Saved repo URLs + cached indexes backing update badges (source_repos.rs).
    repos: SourceRepos,
    /// Language chip filter on the installable-sources list.
    repo_lang: Lang,
    /// A repo-index refresh (manual or the once-a-day auto check) is in flight.
    repo_fetching: bool,
    /// The daily auto check fires once per process, guarded by `checked_day`.
    repo_auto_done: bool,
    /// 测试 results per source id: "可用 · 820 ms" or a Chinese reason.
    repo_test_results: std::collections::HashMap<String, String>,
    /// Tests currently running (≤4), keyed by source id for the 15 s timeout.
    repo_testing: std::collections::HashMap<String, std::time::Instant>,
    /// Tests waiting for a slot.
    repo_test_queue: VecDeque<String>,
    test_tx: mpsc::Sender<(String, Result<u64, String>)>,
    test_rx: mpsc::Receiver<(String, Result<u64, String>)>,
    /// Pending 卸载 confirmation card.
    uninstall: Option<InstalledSource>,
    /// Setting writes (source-repos, source-repos-checked-at) the host owes
    /// the settings store.
    setting_writes: VecDeque<(String, String)>,
}

/// One online work's latest read chapter — a 阅读足迹 row.
pub(crate) struct OnlineHistory {
    pub source_id: String,
    pub manga_id: String,
    pub manga_title: String,
    pub chapter_title: String,
    pub chapter_number: Option<f64>,
    pub read_page: usize,
    pub pages_total: usize,
    pub read_at: i64,
}

impl Drop for Sources {
    fn drop(&mut self) {
        // Do not block window/Activity teardown. The worker notices this after

        // the current HTTP request finishes, then exits and releases its lock.

        self.dl_stop.store(STOP_PAUSE, Ordering::SeqCst);
    }
}

/// Online chapter read state for cloud sync — one record per task that has a
/// `read_at` (i.e. the user actually opened it).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OnlineReadState {
    pub source_id: String,
    pub manga_id: String,
    pub chapter_id: String,
    pub read_page: usize,
    pub total: usize,
    pub read_at: i64,
}

impl Sources {
    pub fn new(root: PathBuf, ctx: Context) -> Self {
        let (tx, rx_task) = mpsc::channel();
        let (tx_reply, rx) = mpsc::channel();
        let meta_root = root.clone();
        let meta_ctx = ctx.clone();
        let current_request = Arc::new(AtomicU64::new(0));
        let worker_request = current_request.clone();
        let source_settings = load_source_settings(&root);
        for (source, values) in &source_settings {
            for (key, value) in values {
                set_setting(source, key, value.clone());
            }
        }
        let stale_engines = Arc::new(std::sync::Mutex::new(HashSet::<String>::new()));
        let web_cookies = load_web_cookies(&root);
        for (url, site) in &web_cookies {
            add_web_cookies(url, &site.cookies);
            if let Some(agent) = &site.user_agent {
                set_site_user_agent(url, agent);
            }
        }
        let worker_stale = stale_engines.clone();
        crate::diagnostics::spawn_supervised(
                "yejian-sources",
                true,
                || crate::diagnostics::report_recovery("书源引擎"),
                move || {
                let catalog = SourceCatalog::new(meta_root.join("sources"));
                let mut engines = std::collections::HashMap::<String, SourceEngine>::new();
                while let Ok((request_id, task)) = rx_task.recv() {
                    for id in worker_stale.lock().unwrap_or_else(std::sync::PoisonError::into_inner).drain() {
                        engines.remove(&id);
                    }
                    // Skip queued reads already replaced by a newer user choice.

                    // An installation is an explicit mutation and must still finish.

                    if request_id != worker_request.load(Ordering::Relaxed)
                        && !matches!(
                            &task,
                            Task::Install(_) | Task::Uninstall(_) | Task::FetchAllRepos(_)
                        )
                    {
                        continue;
                    }
                    let result = (|| -> anyhow::Result<Reply> {
                        Ok(match task {
                            Task::List => Reply::Installed {
                                list: catalog.list()?,
                                updated: None,
                            },
                            Task::Fetch(url) => {
                                let installer =
                                    SourceInstaller::new(SourceCatalog::new(catalog.root()))?;
                                Reply::Repository(installer.fetch_index(&url)?)
                            }
                            Task::Install(entry) => {
                                let installer =
                                    SourceInstaller::new(SourceCatalog::new(catalog.root()))?;
                                installer.install_entry(&entry)?;
                                engines.remove(&entry.id);
                                Reply::Installed {
                                    list: catalog.list()?,
                                    updated: Some(entry.id),
                                }
                            }
                            Task::Uninstall(id) => {
                                catalog.remove(&id)?;
                                engines.remove(&id);
                                Reply::Installed {
                                    list: catalog.list()?,
                                    updated: None,
                                }
                            }
                            Task::FetchAllRepos(urls) => {
                                let installer =
                                    SourceInstaller::new(SourceCatalog::new(catalog.root()))?;
                                let results = urls
                                    .into_iter()
                                    .map(|url| {
                                        let result = installer
                                            .fetch_index(&url)
                                            .map_err(|error| format!("{error}"));
                                        (url, result)
                                    })
                                    .collect();
                                Reply::RepoAll(results)
                            }
                            Task::Metadata(source) => {
                                if !engines.contains_key(&source) {
                                    engines.insert(
                                        source.clone(),
                                        SourceEngine::load(&catalog, &source)?,
                                    );
                                }
                                let engine = &engines[&source];
                                let (filters, filter_error) =
                                    match pollster::block_on(engine.filters()) {
                                        Ok(filters) => (filters, None),
                                        Err(error) => {
                                            // A remote dynamic-filter failure must not

                                            // conceal static options already installed.

                                            let static_filters = catalog
                                                .get(&source)
                                                .ok()
                                                .and_then(|source| source.filters_path)
                                                .and_then(|path| std::fs::read_to_string(path).ok())
                                                .and_then(|text| parse_filters_json(&text).ok())
                                                .unwrap_or_default();
                                            (static_filters, Some(error.to_string()))
                                        }
                                    };
                                let (listings, listing_error) =
                                    match pollster::block_on(engine.listings()) {
                                        Ok(listings) => (listings, None),
                                        Err(error) => (
                                            engine.manifest().listings.clone(),
                                            Some(error.to_string()),
                                        ),
                                    };
                                Reply::Metadata {
                                    filters,
                                    listings,
                                    filter_error,
                                    listing_error,
                                }
                            }
                            Task::Search {
                                source,
                                query,
                                page,
                                filters,
                            } => {
                                if !engines.contains_key(&source) {
                                    engines.insert(
                                        source.clone(),
                                        SourceEngine::load(&catalog, &source)?,
                                    );
                                }
                                let result = pollster::block_on(
                                    engines[&source].search_page(&query, page, filters),
                                )?;
                                Reply::Results {
                                    source,
                                    query,
                                    page,
                                    entries: result.entries,
                                    has_next: result.has_next_page,
                                }
                            }
                            Task::Browse {
                                source,
                                listing,
                                page,
                            } => {
                                if !engines.contains_key(&source) {
                                    engines.insert(
                                        source.clone(),
                                        SourceEngine::load(&catalog, &source)?,
                                    );
                                }
                                let result =
                                    pollster::block_on(engines[&source].browse(&listing, page))?;
                                Reply::Results {
                                    source,
                                    query: String::new(),
                                    page,
                                    entries: result.entries,
                                    has_next: result.has_next_page,
                                }
                            }
                            Task::Chapters(source, manga) => {
                                if !engines.contains_key(&source) {
                                    engines.insert(
                                        source.clone(),
                                        SourceEngine::load(&catalog, &source)?,
                                    );
                                }
                                let engine = &engines[&source];
                                let detail = pollster::block_on(engine.manga_details(&manga.key))?;
                                let chapters = pollster::block_on(engine.chapters(&manga.key))?;
                                Reply::Chapters(source, detail, chapters)
                            }
                            Task::Login {
                                source,
                                key,
                                credentials,
                            } => {
                                if !engines.contains_key(&source) {
                                    engines.insert(
                                        source.clone(),
                                        SourceEngine::load(&catalog, &source)?,
                                    );
                                }
                                let engine = &engines[&source];
                                let web = credentials.is_err();
                                let (user, ok) = match credentials {
                                    Ok((user, password)) => {
                                        let ok = pollster::block_on(
                                            engine.basic_login(&key, &user, &password),
                                        )?;
                                        (user, ok)
                                    }
                                    Err(cookies) => {
                                        let ok =
                                            pollster::block_on(engine.web_login(&key, cookies))?;
                                        ("网页登录".to_string(), ok)
                                    }
                                };
                                Reply::Login { key, user, ok, web }
                            }
                            Task::Notify {
                                source,
                                notification,
                            } => {
                                if !engines.contains_key(&source) {
                                    engines.insert(
                                        source.clone(),
                                        SourceEngine::load(&catalog, &source)?,
                                    );
                                }
                                pollster::block_on(
                                    engines[&source].handle_notification(notification),
                                )?;
                                Reply::Notified
                            }
                        })
                    })();
                    let _ =
                        tx_reply.send((request_id, result.map_err(|error| format!("{error:#}"))));
                    meta_ctx.request_repaint();
                }
            },
            );
        // Update checks get their own thread so a slow source never blocks

        // browsing or downloads.

        let (up_tx, up_cmd_rx) = mpsc::channel::<Vec<(String, Manga)>>();
        let (up_event_tx, up_rx) = mpsc::channel();
        let up_root = root.clone();
        let up_ctx = ctx.clone();
        crate::diagnostics::spawn_supervised(
                "yejian-updates",
                true,
                || crate::diagnostics::report_recovery("追更检查"),
                move || {
                let catalog = SourceCatalog::new(up_root.join("sources"));
                while let Ok(batch) = up_cmd_rx.recv() {
                    // Reload each round so an updated source is picked up.

                    let mut engines = std::collections::HashMap::<String, SourceEngine>::new();
                    for (source, manga) in batch.iter().cloned() {
                        let chapters = (|| -> anyhow::Result<Vec<Chapter>> {
                            if !engines.contains_key(&source) {
                                engines
                                    .insert(source.clone(), SourceEngine::load(&catalog, &source)?);
                            }
                            Ok(pollster::block_on(
                                engines[&source].chapters_with(manga.clone()),
                            )?)
                        })()
                        .map_err(|error| format!("{error:#}"));
                        let event = UpdateEvent {
                            source,
                            manga: manga.key,
                            chapters: Some(chapters),
                            cover: None,
                        };
                        if up_event_tx.send(event).is_err() {
                            return;
                        }
                        up_ctx.request_repaint();
                    }
                    // Covers come last so a slow image host never delays

                    // new-chapter results; a missing cover is not an error.

                    for (source, manga) in batch {
                        let Some(engine) = engines.get(&source) else {
                            continue;
                        };
                        if let Ok((key, image)) = fetch_follow_cover(&up_root, engine, &manga) {
                            let event = UpdateEvent {
                                source,
                                manga: manga.key,
                                chapters: None,
                                cover: Some((key, image)),
                            };
                            if up_event_tx.send(event).is_err() {
                                return;
                            }
                            up_ctx.request_repaint();
                        }
                    }
                }
            },
            );
        // Result covers resolve through each source's interpreter on a single

        // thread, then fetch bytes on a small HTTP pool so a slow image host

        // never blocks browsing. Both stages share the source's cookie jar.

        let (cover_req_tx, cover_req_rx) = mpsc::channel::<CoverReq>();
        let (cover_done_tx, cover_done_rx) = mpsc::channel::<CoverDone>();
        let cover_jobs: Arc<(std::sync::Mutex<VecDeque<CoverHttpJob>>, std::sync::Condvar)> =
            Arc::new((
                std::sync::Mutex::new(VecDeque::new()),
                std::sync::Condvar::new(),
            ));
        {
            let cover_root = root.clone();
            let cover_ctx = ctx.clone();
            let queue = cover_jobs.clone();
            let done_tx = cover_done_tx.clone();
            let stale = stale_engines.clone();
            crate::diagnostics::spawn_supervised(
                "yejian-covers",
                true,
                || crate::diagnostics::report_recovery("封面"),
                move || {
                    let catalog = SourceCatalog::new(cover_root.join("sources"));
                    let mut engines = std::collections::HashMap::<String, SourceEngine>::new();
                    while let Ok(req) = cover_req_rx.recv() {
                        for id in stale.lock().unwrap_or_else(std::sync::PoisonError::into_inner).drain() {
                            engines.remove(&id);
                        }
                        let job = (|| -> anyhow::Result<CoverHttpJob> {
                            if !engines.contains_key(&req.source) {
                                engines.insert(
                                    req.source.clone(),
                                    SourceEngine::load(&catalog, &req.source)?,
                                );
                            }
                            let engine = &engines[&req.source];
                            let (url, headers, body) =
                                match pollster::block_on(engine.image_request(&req.url, None))? {
                                    Some(image) => (image.url, image.headers, image.body),
                                    None => (req.url.clone(), Vec::new(), None),
                                };
                            Ok(CoverHttpJob {
                                key: req.key.clone(),
                                root: cover_root.clone(),
                                handler: engine.request_handler(),
                                url,
                                headers,
                                body,
                            })
                        })();
                        match job {
                            Ok(job) => {
                                let (lock, cvar) = &*queue;
                                lock.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push_back(job);
                                cvar.notify_one();
                            }
                            Err(_) => {
                                if done_tx
                                    .send(CoverDone {
                                        key: req.key,
                                        image: None,
                                    })
                                    .is_err()
                                {
                                    return;
                                }
                                cover_ctx.request_repaint();
                            }
                        }
                    }
                },
            );
        }
        for _ in 0..4 {
            let queue = cover_jobs.clone();
            let done_tx = cover_done_tx.clone();
            let ctx = ctx.clone();
            crate::diagnostics::spawn_supervised(
                "yejian-cover-http",
                true,
                || crate::diagnostics::report_recovery("封面"),
                move || {
                    let (lock, cvar) = &*queue;
                    loop {
                        let job = {
                            let mut guard = lock.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                            loop {
                                if let Some(job) = guard.pop_front() {
                                    break job;
                                }
                                guard = cvar.wait(guard).unwrap();
                            }
                        };
                        let image = request(&job.handler, job.url, job.headers, job.body)
                            .and_then(|bytes| {
                                let image = shrink_cover(image::load_from_memory(&bytes)?);
                                let path = source_cover_path(&job.root, &job.key)?;
                                store_source_cover(&path, &image)
                            })
                            .ok();
                        if done_tx
                            .send(CoverDone {
                                key: job.key,
                                image,
                            })
                            .is_err()
                        {
                            return;
                        }
                        ctx.request_repaint();
                    }
                },
            );
        }
        // Downloads live on their own thread and run one task at a time, so a

        // slow image server never blocks search, chapter loading or the UI.

        let (dl_tx, dl_cmd_rx) = mpsc::channel::<DlCmd>();
        let (dl_ev_tx, dl_rx) = mpsc::channel::<DlEvent>();
        let dl_stop = Arc::new(AtomicU8::new(STOP_NONE));
        {
            let root = root.clone();
            let ctx = ctx.clone();
            let stop = dl_stop.clone();
            crate::diagnostics::spawn_supervised(
                "yejian-downloads",
                true,
                || crate::diagnostics::report_recovery("下载"),
                move || {
                    let catalog = SourceCatalog::new(root.join("sources"));
                    let mut engines = std::collections::HashMap::<String, SourceEngine>::new();
                    let send = |event| {
                        let _ = dl_ev_tx.send(event);
                        ctx.request_repaint();
                    };
                    while let Ok(DlCmd::Run(task)) = dl_cmd_rx.recv() {
                        let id = task.id.clone();
                        let lock = match acquire_download_lock(&root) {
                            Ok(lock) => lock,
                            Err(error) => {
                                let _ = send(DlEvent::LockUnavailable {
                                    id,
                                    error: format!("{error:#}"),
                                });
                                continue;
                            }
                        };
                        let outcome = (|| -> anyhow::Result<Outcome> {
                            if stop.load(Ordering::SeqCst) != STOP_NONE {
                                return Ok(Outcome::Aborted {
                                    how: stop.load(Ordering::SeqCst),
                                });
                            }
                            if let Some(path) = recover_completed_archive(&root, &task) {
                                return Ok(Outcome::Done { path });
                            }
                            if !engines.contains_key(&task.source_id) {
                                engines.insert(
                                    task.source_id.clone(),
                                    SourceEngine::load(&catalog, &task.source_id)?,
                                );
                            }
                            let engine = &engines[&task.source_id];
                            let pages = pollster::block_on(
                                engine.pages_with(task.manga_for(&root)?, task.chapter_for()?),
                            )?;
                            execute_task(
                                &root,
                                &task,
                                pages,
                                &stop,
                                |page| page_bytes(engine, page),
                                |done, total, signature| {
                                    send(DlEvent::Progress {
                                        id: id.clone(),
                                        done,
                                        total,
                                        signature,
                                    });
                                },
                            )
                        })();
                        drop(lock);
                        match outcome {
                            Ok(Outcome::Done { path }) => {
                                let _ = send(DlEvent::Finished { id, path });
                            }
                            Ok(Outcome::Aborted { how }) => {
                                let _ = send(DlEvent::Aborted { id, how });
                            }
                            Err(error) => {
                                let _ = send(DlEvent::Failed {
                                    id,
                                    error: format!("{error:#}"),
                                });
                            }
                        }
                    }
                },
            );
        }
        // Cross-source searches keep their own engine map so a source update
        // invalidates them like every other worker.
        let (global_tx, global_rx) = mpsc::channel::<GlobalEvent>();
        let (global_cmd_tx, global_cmd_rx) = mpsc::channel::<GlobalCmd>();
        {
            let (catalog_root, stale, ctx) =
                (root.clone(), stale_engines.clone(), ctx.clone());
            crate::diagnostics::spawn_supervised(
                "yejian-global-search",
                true,
                || crate::diagnostics::report_recovery("跨书源搜索"),
                move || {
                    let catalog = SourceCatalog::new(catalog_root.join("sources"));
                    global_search_run(
                        &global_cmd_rx,
                        &global_tx,
                        move |source| {
                            SourceEngine::load(&catalog, source)
                                .map(|engine| {
                                    Arc::new(RealGlobalEngine(Arc::new(engine)))
                                        as Arc<dyn GlobalEngine>
                                })
                                .map_err(|error| format!("{error:#}"))
                        },
                        GLOBAL_SEARCH_TIMEOUT,
                        &stale,
                        &ctx,
                    );
                },
            );
        }
        // Storage scans and cleanup run on their own worker so large
        // download-work trees never stall a frame.
        let (storage_tx, storage_cmd_rx) = mpsc::channel::<StorageCmd>();
        let (storage_event_tx, storage_rx) = mpsc::channel::<StorageEvent>();
        {
            let (root, ctx) = (root.clone(), ctx.clone());
            crate::diagnostics::spawn_supervised(
                "yejian-source-storage",
                true,
                || crate::diagnostics::report_recovery("存储统计"),
                move || {
                    storage_worker_run(&storage_cmd_rx, &storage_event_tx, &root, &ctx)
                },
            );
        }

        let (migrate_tx, migrate_rx) = mpsc::channel();
        let (test_tx, test_rx) = mpsc::channel();
        let (tasks, load_warning) = load_queue(&root);
        let ready: VecDeque<String> = pending_imports(&tasks).into();
        let sources = Self {
            root,
            tx,
            rx,
            installed: Vec::new(),
            repository: None,
            results: ResultsState::default(),
            chapters: Vec::new(),
            chapter_query: String::new(),
            chapters_newest_first: true,
            open_chapter: None,
            library_chapters: Default::default(),
            selected_source: String::new(),
            selected_manga: None,
            url: String::new(),
            query: String::new(),
            search_scope: "current".into(),
            global_gen: 0,
            global_query: String::new(),
            global_groups: Vec::new(),
            scope_dirty: None,
            search_history: Vec::new(),
            history_dirty: false,
            history_confirm: false,
            private: false,
            global_tx: global_cmd_tx,
            global_rx,
            storage_tx,
            storage_rx,
            storage: None,
            request_id: 0,
            current_request,
            pending: None,
            failed: None,
            filters: Vec::new(),
            listings: Vec::new(),
            applied_filters: Vec::new(),
            listing: None,
            filter_editor: None,
            metadata_loaded: false,
            filter_error: None,
            listing_error: None,
            sessions: Default::default(),
            repository_search: String::new(),
            #[cfg(debug_assertions)]
            preview_mode: None,
            busy: false,
            status: load_warning.clone().unwrap_or_default(),
            status_seen: None,
            tasks,
            running: None,
            ready,
            import_attempts: HashSet::new(),
            dl_tx,
            dl_rx,
            dl_stop,
            stream_request: None,
            preempted: None,
            follows: Vec::new(),
            pending_open: None,
            shelf_origin: false,
            shelf_return: false,
            pending_resume: false,
            checking: 0,
            checking_background: false,
            auto_checked: false,
            ctx,
            follow_interval_min: DEFAULT_FOLLOW_INTERVAL_MIN,
            follow_next_due: None,
            follow_notice: None,
            notify_pending: Vec::new(),
            up_tx,
            up_rx,
            covers: Default::default(),
            follow_cover_urls: Default::default(),
            cover_req_tx,
            cover_done_rx,
            cover_queue: VecDeque::new(),
            cover_inflight: HashSet::new(),
            cover_outstanding: 0,
            cover_failed: HashSet::new(),
            cover_ready: Default::default(),
            cover_missing_disk: HashSet::new(),
            cover_textures: Default::default(),
            cover_lru: VecDeque::new(),
            auto_next_page: None,
            chapter_progress: Default::default(),
            description_expanded: false,
            hitboxes: Default::default(),
            settings_open: false,
            settings_changed: false,
            source_settings,
            stale_engines,
            challenge: None,
            auto_download: "off".into(),
            migrate: None,
            migrate_tx,
            migrate_rx,
            migrate_post: None,
            web_session: None,
            login_user: String::new(),
            login_pass: String::new(),
            web_cookies,
            repos: SourceRepos::default(),
            repo_lang: Lang::default(),
            repo_fetching: false,
            repo_auto_done: false,
            repo_test_results: Default::default(),
            repo_testing: Default::default(),
            repo_test_queue: VecDeque::new(),
            test_tx,
            test_rx,
            uninstall: None,
            setting_writes: VecDeque::new(),
        };
        let mut sources = sources;
        sources.repos.load_cache(&sources.root);
        match follows::load(&sources.root) {
            Ok(list) => sources.follows = list,
            Err(error) => sources.status = format!("{error:#}"),
        }
        // Never overwrite a file that startup could not fully parse.

        if load_warning.is_none() {
            let mut sources = sources;
            if let Err(error) = sources.persist() {
                sources.status = format!("初始化下载队列失败：{error:#}；原有存档未被覆盖");
            }
            sources.submit(Task::List);
            return sources;
        }
        let mut sources = sources;
        sources.submit(Task::List);
        sources
    }
    /// Opens a followed work's detail page, remembering whether it came from
    /// the shelf so 返回 can hand back to 书架. `resume` makes the chapter
    /// list auto-fire the existing 继续阅读 once it lands.
    pub(crate) fn open_work(&mut self, source: &str, manga: &str, resume: bool) -> bool {
        let Some(index) = self.follow_index(source, manga) else {
            return false;
        };
        if !self.open_follow(index) {
            return false;
        }
        self.shelf_origin = true;
        self.pending_resume = resume;
        true
    }
    /// `open_work` marked this visit as shelf-originated: 返回 pops to 书架.
    pub(crate) fn take_shelf_return(&mut self) -> bool {
        std::mem::take(&mut self.shelf_return)
    }
    /// A resume queued by the shelf is still waiting for the chapter list.
    pub(crate) fn resume_pending(&self) -> bool {
        self.pending_resume
    }
    /// The followed works, in follow order (newest first).
    pub(crate) fn follows(&self) -> &[Follow] {
        &self.follows
    }
    /// Setting writes the host persists (`source-repos`, `-checked-at`).
    pub(crate) fn take_setting_writes(&mut self) -> Vec<(String, String)> {
        self.setting_writes.drain(..).collect()
    }
    pub(super)fn write_setting(&mut self, key: &str, value: String) {
        self.setting_writes.push_back((key.to_string(), value));
    }
    pub fn poll(&mut self) {
        self.drain_repo_tests();
        while let Ok(done) = self.cover_done_rx.try_recv() {
            self.cover_outstanding = self.cover_outstanding.saturating_sub(1);
            self.cover_inflight.remove(&done.key);
            if done.image.is_none() {
                self.cover_failed.insert(done.key.clone());
            }
            self.cover_ready.insert(done.key, done.image);
        }
        if let Some(origin) = take_web_challenge() {
            self.challenge = Some(origin);
        }
        match yejian_platform::poll_web() {
            Ok(Some(result)) => self.finish_web(result),
            Ok(None) => {}
            Err(error) => self.status = format!("网页结果读取失败：{error:?}"),
        }
        while let Ok((request_id, result)) = self.rx.try_recv() {
            // Every operation (including failures and detail loads) belongs to

            // one request. Same text with different filters/listing is a new

            // identity; old replies cannot clear its spinner or replace data.

            if request_id != self.request_id {
                // A completed installation still updates the catalogue even

                // when the user has navigated to another source in the meantime.

                if let Ok(Reply::Installed {
                    list,
                    updated: Some(_),
                }) = result
                {
                    self.installed = list;
                } else if let Ok(Reply::RepoAll(results)) = result {
                    self.apply_repo_all(results);
                }
                continue;
            }
            self.busy = false;
            let completed = self.pending.take();
            let reply = match result {
                Ok(reply) => reply,
                Err(error) => {
                    self.status = match &completed {
                        Some(Task::Search { page, .. } | Task::Browse { page, .. })
                            if *page > 1 =>
                        {
                            format!("第 {page} 页加载失败：{error}")
                        }
                        Some(Task::Metadata(_)) => format!("书源选项加载失败：{error}"),
                        Some(Task::Login { .. }) => format!("登录失败：{error}"),
                        Some(Task::Notify { .. }) => format!("操作失败：{error}"),
                        _ => format!("加载失败：{error}"),
                    };
                    self.failed = completed;
                    self.pending_resume = false;
                    if let Some(manga) = self.pending_open.take() {
                        self.open_manga(manga);
                    }
                    continue;
                }
            };
            match reply {
                Reply::Installed { list, updated } => {
                    self.installed = list;
                    self.status = "书源列表已更新".into();
                    if let Some(id) = updated {
                        if self.selected_source == id {
                            self.metadata_loaded = false;
                            self.applied_filters.clear();
                            self.submit(Task::Metadata(id));
                        }
                    }
                }
                Reply::Repository(index) => {
                    self.status = format!("找到 {} 个书源", index.sources.len());
                    // A fetched repo joins the saved list and its index
                    // feeds the update badges right away.
                    if self.repos.record_repo(&index.index_url) {
                        self.write_setting("source-repos", self.repos.urls_setting());
                    }
                    self.repos
                        .merge_index(&index.index_url, &index, follows::now());
                    let _ = self.repos.save_cache(&self.root);
                    self.repository = Some(index);
                }
                Reply::RepoAll(results) => {
                    self.apply_repo_all(results);
                }
                Reply::Metadata {
                    filters,
                    listings,
                    filter_error,
                    listing_error,
                } => {
                    self.filters = filters;
                    self.listings = listings;
                    self.filter_error = filter_error;
                    self.listing_error = listing_error;
                    self.metadata_loaded = true;
                    let pending = self.pending_open.take();
                    // Source updates can remove options; a session must never

                    // submit an identifier absent from the current schema.

                    self.applied_filters = crate::source_filters::overrides(
                        &self.filters,
                        &crate::source_filters::effective_values(
                            &self.filters,
                            &self.applied_filters,
                        ),
                    );
                    #[cfg(debug_assertions)]
                    if self.preview_mode.as_deref() == Some("filtered") {
                        self.applied_filters = vec![FilterValue::Select {
                            id: "genre".into(),
                            value: "adv".into(),
                        }];
                        self.start_search();
                        continue;
                    }
                    // A followed manga opened from another source replaces the

                    // default listing; both would share one request slot.

                    if let Some(manga) = pending {
                        self.open_manga(manga);
                        continue;
                    }
                    let listing = self
                        .listing
                        .clone()
                        .filter(|id| self.listings.iter().any(|listing| listing.id == *id))
                        .or_else(|| {
                            if self.sessions.contains_key(&self.selected_source) {
                                None
                            } else {
                                self.listings.first().map(|listing| listing.id.clone())
                            }
                        });
                    if let Some(id) = listing {
                        self.start_listing(&id);
                    } else {
                        self.start_search();
                    }
                }
                Reply::Results {
                    source,
                    query,
                    page,
                    entries,
                    has_next,
                } => {
                    if self
                        .results
                        .accept(&source, &query, page, entries, has_next)
                    {
                        self.selected_source = source;
                        self.status = if has_next {
                            format!("已加载第 {page} 页，可继续加载下一页")
                        } else if page > 1 {
                            format!("已加载到第 {page} 页，没有更多结果")
                        } else if self.results.entries.is_empty() {
                            "没有找到漫画，可调整关键词或筛选".into()
                        } else {
                            format!("共 {} 部漫画", self.results.entries.len())
                        };
                        #[cfg(debug_assertions)]
                        if let Some(mode) = self.preview_mode.take() {
                            if mode == "filters" {
                                self.filter_editor =
                                    Some(FilterEditor::new(&self.filters, &self.applied_filters));
                            }
                        }
                    }
                }
                Reply::Login { key, user, ok, web } => {
                    if ok {
                        self.login_pass.clear();
                        let marker = format!("{key}.username");
                        if web {
                            self.change_setting(&marker, SettingValue::Str(user));
                        } else {
                            // A basic-login session lives only in this process,

                            // so the form returns after a restart.

                            set_setting(&self.selected_source, &marker, SettingValue::Str(user));
                        }
                        self.status = "登录成功".into();
                    } else {
                        self.status = "登录失败，请检查账号或稍后再试".into();
                    }
                }
                Reply::Chapters(source, manga, list) => {
                    if source == self.selected_source {
                        if let Some(index) = self.follow_index(&source, &manga.key) {
                            let keys: Vec<String> =
                                list.iter().map(|chapter| chapter.key.clone()).collect();
                            self.follows[index].reading_order =
                                chapter_list(&list, "", false)
                                    .iter()
                                    .map(|chapter| chapter.key.clone())
                                    .collect();
                            self.follows[index].mark_seen(&keys);
                            self.follows[index].title = manga.title.clone();
                            self.save_follows();
                        }
                        self.selected_manga = Some(manga);
                        self.chapters = list;
                        self.status.clear();
                        if self.pending_resume {
                            self.pending_resume = false;
                            self.auto_resume();
                        }
                    }
                }
                Reply::Notified => {
                    self.status = "已完成".into();
                }
            }
        }
        while let Ok(event) = self.dl_rx.try_recv() {
            self.handle_download_event(event);
        }
        while let Ok(event) = self.up_rx.try_recv() {
            self.handle_update(event);
        }
        while let Ok(event) = self.global_rx.try_recv() {
            match event {
                GlobalEvent::Search {
                    generation,
                    source,
                    result,
                } => {
                    if generation != self.global_gen {
                        continue;
                    }
                    // The 换源 panel owns this generation's events while open.
                    if let Some(migrate) = self.migrate.as_mut()
                        && migrate.generation == generation
                    {
                        if let Some(group) = migrate
                            .groups
                            .iter_mut()
                            .find(|group| group.source == source)
                        {
                            group.state = match result {
                                Ok(mut entries) => {
                                    for manga in &mut entries {
                                        manga.source_key = source.clone();
                                    }
                                    GlobalState::Done(entries)
                                }
                                Err(reason) => GlobalState::Failed(reason),
                            };
                        }
                        continue;
                    }
                    if let Some(group) = self
                        .global_groups
                        .iter_mut()
                        .find(|group| group.source == source)
                    {
                        group.state = match result {
                            Ok(mut entries) => {
                                for manga in &mut entries {
                                    manga.source_key = source.clone();
                                }
                                GlobalState::Done(entries)
                            }
                            Err(reason) => GlobalState::Failed(reason),
                        };
                    }
                }
                GlobalEvent::Chapters {
                    generation,
                    source,
                    manga,
                    result,
                } => {
                    let Some(migrate) = self.migrate.as_mut() else {
                        continue;
                    };
                    if generation != migrate.generation {
                        continue;
                    }
                    migrate.expanding = None;
                    migrate.expanded.insert((source, manga.key.clone()), result);
                }
            }
        }
        while let Ok(done) = self.migrate_rx.try_recv() {
            self.apply_migrate(done);
        }
        while let Ok(event) = self.storage_rx.try_recv() {
            match event {
                StorageEvent::Report(report) => self.storage = Some(report),
                // Disk cache is gone; drop decoded textures and the
                // failure set so covers fetch again on demand.
                StorageEvent::CoversCleared => {
                    self.cover_textures.clear();
                    self.cover_failed.clear();
                    self.cover_ready.clear();
                    self.cover_missing_disk.clear();
                }
                StorageEvent::Failed(error) => self.status = error,
            }
        }
        if !self.auto_checked && !self.installed.is_empty() {
            self.auto_checked = true;
            self.check_updates(false);
        }
        self.maybe_auto_repo_check();
        self.poll_follow_schedule();
    }
    /// Every task's latest online read position — the cloud snapshot's
    /// `online` group.
    pub(crate) fn online_read_state(&self) -> Vec<OnlineReadState> {
        self.tasks
            .iter()
            .filter_map(|task| {
                Some(OnlineReadState {
                    source_id: task.source_id.clone(),
                    manga_id: task.manga_id.clone(),
                    chapter_id: task.chapter_id.clone(),
                    read_page: task.read_page?,
                    total: task.pages_total,
                    read_at: task.read_at?,
                })
            })
            .collect()
    }
    /// Applies remote online read state onto matching tasks; returns how
    /// many tasks were moved. `skip` excludes the work currently open in
    /// the reader so sync never yanks a live position.
    pub(crate) fn apply_online_state(
        &mut self,
        states: &[OnlineReadState],
        skip: Option<(&str, &str)>,
    ) -> usize {
        let mut changed = 0;
        for state in states {
            if skip.is_some_and(|(s, m)| s == state.source_id && m == state.manga_id) {
                continue;
            }
            let Some(task) = self.tasks.iter_mut().find(|task| {
                task.source_id == state.source_id
                    && task.manga_id == state.manga_id
                    && task.chapter_id == state.chapter_id
            }) else {
                continue;
            };
            if state.read_at > task.read_at.unwrap_or(0) {
                task.read_page = Some(state.read_page);
                task.read_at = Some(state.read_at);
                changed += 1;
            }
        }
        if changed > 0 {
            let _ = self.persist();
        }
        changed
    }
    /// Replaces the follows list wholesale after a cloud merge and persists.
    pub(crate) fn apply_synced_follows(&mut self, follows: Vec<Follow>) {
        self.follows = follows;
        self.save_follows();
    }
}

