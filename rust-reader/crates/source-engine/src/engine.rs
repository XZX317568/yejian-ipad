//! Per-source worker thread and the plain-Rust async API consumed by the UI.
//!
//! `wasmi` instances are single-threaded and `!Send`, so each [`SourceEngine`]
//! owns one OS thread for the whole lifetime of the source. Public methods
//! return `Pin<Box<dyn Future<Output = …> + Send>>`; they post a job to that
//! thread and await the reply. No Flutter, Dart or tokio runtime is involved.

use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::future::Future;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Sender};

use parking_lot::Mutex;
use serde_json::Value;

use crate::catalog::{InstalledSource, SourceCatalog, SourceManifest};
use crate::error::SourceError;
use crate::http::{DEFAULT_USER_AGENT, NativeHttp, NetRequestData, NetResponseData};
use crate::wasm::host_defaults::{self, DefaultValue};
use crate::wasm::interpreter::AidokuInterpreter;
use crate::wasm::models::{
    Chapter, ContentRating, DeepLinkResult, Filter, FilterValue, ImageRequest, Listing, Manga,
    MangaPageResult, Page, PublishingStatus, SourceFeatures, UpdateStrategy, Viewer,
};

pub type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send + 'static>>;

/// A platform-provided replacement for the built-in `reqwest` transport.
pub type RequestHandler = Arc<dyn Fn(NetRequestData) -> NetResponseData + Send + Sync + 'static>;

/// Optional hooks for the host application.
#[derive(Clone, Default)]
pub struct EngineOptions {
    /// User agent used by the built-in native HTTP client.
    pub user_agent: Option<String>,
    /// Replace the built-in native client (for a system proxy, DoH, etc.).
    /// When set, `user_agent` is ignored.
    pub request_handler: Option<RequestHandler>,
    /// Receives `std.print` / `env.print` output from the source module.
    pub print_handler: Option<Arc<dyn Fn(String) + Send + Sync>>,
    /// Receives incremental results from `std.send_partial_result`.
    pub partial_result_handler: Option<Arc<dyn Fn(Vec<u8>) + Send + Sync>>,
}

impl fmt::Debug for EngineOptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EngineOptions")
            .field("user_agent", &self.user_agent)
            .field("custom_request_handler", &self.request_handler.is_some())
            .field("print_handler", &self.print_handler.is_some())
            .field(
                "partial_result_handler",
                &self.partial_result_handler.is_some(),
            )
            .finish()
    }
}

type Job = Box<dyn FnOnce(&mut WorkerState) + Send>;

const MAX_CACHED_MANGAS: usize = 256;
const MAX_CACHED_CHAPTERS: usize = 8192;
const WORKER_STACK_BYTES: usize = 8 * 1024 * 1024;

/// Default WASM fuel budget for one source call. Roughly comparable to a few
/// seconds of bytecode execution; it stops runaway loops without rejecting
/// normal catalogue parsing. Set `0` through [`SourceEngine::set_fuel_per_call`]
/// to disable the budget.
pub const DEFAULT_FUEL_PER_CALL: u64 = 1_000_000_000;

/// One loaded Aidoku source module, running on its own thread.
pub struct SourceEngine {
    tx: Mutex<Option<Sender<Job>>>,
    closed: Arc<AtomicBool>,
    poisoned: Arc<AtomicBool>,
    fuel_per_call: Arc<AtomicU64>,
    manifest: Arc<SourceManifest>,
    features: SourceFeatures,
}

impl fmt::Debug for SourceEngine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SourceEngine")
            .field("source_id", &self.manifest.id)
            .field("features", &self.features)
            .field("closed", &self.closed.load(Ordering::Relaxed))
            .field("fuel_per_call", &self.fuel_per_call.load(Ordering::Relaxed))
            .finish()
    }
}

impl SourceEngine {
    /// Loads an installed source from the catalogue.
    pub fn load(catalog: &SourceCatalog, source_id: &str) -> Result<Self, SourceError> {
        let installed = catalog
            .get(source_id)
            .map_err(|e| SourceError::Load(e.to_string()))?;
        Self::load_installed(&installed)
    }

    /// Loads a source directory that was already resolved by the catalogue.
    pub fn load_installed(source: &InstalledSource) -> Result<Self, SourceError> {
        Self::load_installed_with(source, EngineOptions::default())
    }

    pub fn load_installed_with(
        source: &InstalledSource,
        options: EngineOptions,
    ) -> Result<Self, SourceError> {
        let wasm = source
            .wasm_bytes()
            .map_err(|e| SourceError::Load(e.to_string()))?;
        let settings = source.settings_json();
        // A malformed or unreadable `filters.json` does NOT fail the source —
        // AidokuRunner fails init, but the reader keeps the source usable and
        // surfaces the error only when `filters()` is asked for the schema.
        // Read the advertised path directly so an unreadable (but present)
        // file is reported, not mistaken for "no filters".
        let static_filters = match &source.filters_path {
            Some(path) => Some(match std::fs::read_to_string(path) {
                Ok(text) => crate::filters::parse_filters_json(&text),
                Err(e) => Err(SourceError::Deserialize(format!(
                    "filters.json could not be read ({}): {e}",
                    path.display()
                ))),
            }),
            None => None,
        };
        Self::spawn(
            source.manifest.clone(),
            wasm,
            options,
            settings,
            static_filters,
        )
    }

    /// Loads raw module bytes with explicit metadata. Useful for previews and
    /// tests before a package is installed.
    pub fn load_wasm(
        manifest: SourceManifest,
        wasm: Vec<u8>,
        options: EngineOptions,
    ) -> Result<Self, SourceError> {
        Self::spawn(manifest, wasm, options, None, None)
    }

    fn spawn(
        manifest: SourceManifest,
        wasm: Vec<u8>,
        options: EngineOptions,
        settings_json: Option<String>,
        static_filters: Option<Result<Vec<crate::wasm::models::Filter>, SourceError>>,
    ) -> Result<Self, SourceError> {
        if wasm.len() < 4 || !wasm.starts_with(b"\0asm") {
            return Err(SourceError::Load(
                "module is empty or is not WebAssembly".into(),
            ));
        }

        let request_handler: RequestHandler = match options.request_handler.clone() {
            Some(handler) => handler,
            None => {
                let http = NativeHttp::with_user_agent(
                    options.user_agent.as_deref().unwrap_or(DEFAULT_USER_AGENT),
                )?;
                Arc::new(move |request| http.send(request))
            }
        };

        let source_id = manifest.id.clone();
        let worker_manifest = manifest.clone();
        let print_handler = options.print_handler.clone();
        let partial_handler = options.partial_result_handler.clone();
        let (tx, rx) = mpsc::channel::<Job>();
        let (init_tx, init_rx) = mpsc::sync_channel::<Result<SourceFeatures, SourceError>>(1);
        let poisoned = Arc::new(AtomicBool::new(false));
        let worker_poisoned = poisoned.clone();
        let fuel_per_call = Arc::new(AtomicU64::new(DEFAULT_FUEL_PER_CALL));
        let worker_fuel = fuel_per_call.clone();

        std::thread::Builder::new()
            .name(format!("yejian-source-{source_id}"))
            .stack_size(WORKER_STACK_BYTES)
            .spawn(move || {
                let init = catch_unwind(AssertUnwindSafe(|| {
                    seed_defaults(&worker_manifest, settings_json.as_deref());
                    let interpreter = AidokuInterpreter::create(
                        &wasm,
                        worker_manifest.id.clone(),
                        request_handler,
                        print_handler,
                        partial_handler,
                        worker_fuel,
                    )?;
                    let features: SourceFeatures = interpreter.features.into();
                    Ok::<_, SourceError>((interpreter, features))
                }));
                let (interpreter, features) = match init {
                    Ok(Ok(value)) => value,
                    Ok(Err(error)) => {
                        let _ = init_tx.send(Err(error));
                        return;
                    }
                    Err(payload) => {
                        let _ = init_tx.send(Err(SourceError::EnginePanicked(panic_text(payload))));
                        return;
                    }
                };
                let _ = init_tx.send(Ok(features));

                let mut state = WorkerState::new(
                    interpreter,
                    worker_manifest,
                    features,
                    worker_poisoned.clone(),
                    static_filters,
                );
                while let Ok(job) = rx.recv() {
                    job(&mut state);
                    if worker_poisoned.load(Ordering::SeqCst) {
                        break;
                    }
                }
                state.interpreter.dispose();
            })
            .map_err(|e| SourceError::Load(format!("starting source thread: {e}")))?;

        let features = init_rx.recv().map_err(|_| SourceError::EngineStopped)??;
        Ok(Self {
            tx: Mutex::new(Some(tx)),
            closed: Arc::new(AtomicBool::new(false)),
            poisoned,
            fuel_per_call,
            manifest: Arc::new(manifest),
            features,
        })
    }

    pub fn manifest(&self) -> &SourceManifest {
        &self.manifest
    }

    pub fn features(&self) -> SourceFeatures {
        self.features
    }

    /// Changes the per-call WASM fuel budget. The new value applies from the
    /// next call; a call already running keeps its old budget. `0` disables
    /// the limit (a runaway source can then occupy its own thread forever).
    pub fn set_fuel_per_call(&self, fuel: u64) {
        self.fuel_per_call.store(fuel, Ordering::Relaxed);
    }

    /// Current per-call fuel budget; `0` means unlimited.
    pub fn fuel_per_call(&self) -> u64 {
        self.fuel_per_call.load(Ordering::Relaxed)
    }

    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }

    /// Stops the worker thread and releases the WASM instance. Any in-flight
    /// call finishes first; later calls return [`SourceError::EngineStopped`].
    pub fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
        let _ = self.tx.lock().take();
    }

    fn submit<T, F>(&self, job: F) -> BoxFuture<Result<T, SourceError>>
    where
        T: Send + 'static,
        F: FnOnce(&mut WorkerState) -> Result<T, SourceError> + Send + 'static,
    {
        if self.closed.load(Ordering::SeqCst) || self.poisoned.load(Ordering::SeqCst) {
            return Box::pin(std::future::ready(Err(SourceError::EngineStopped)));
        }
        let (reply_tx, reply_rx) = futures_channel::oneshot::channel();
        let poisoned = self.poisoned.clone();
        let packed: Job =
            Box::new(
                move |state| match catch_unwind(AssertUnwindSafe(|| job(state))) {
                    Ok(value) => {
                        let _ = reply_tx.send(value);
                    }
                    Err(payload) => {
                        poisoned.store(true, Ordering::SeqCst);
                        let _ =
                            reply_tx.send(Err(SourceError::EnginePanicked(panic_text(payload))));
                    }
                },
            );
        let sender = self.tx.lock();
        let Some(sender) = sender.as_ref() else {
            return Box::pin(std::future::ready(Err(SourceError::EngineStopped)));
        };
        if sender.send(packed).is_err() {
            return Box::pin(std::future::ready(Err(SourceError::EngineStopped)));
        }
        Box::pin(async move {
            reply_rx
                .await
                .unwrap_or_else(|_| Err(SourceError::EngineStopped))
        })
    }

    /// Replaces the empty-query search with the source's real search entry
    /// point. `page` is one-based, matching Aidoku.
    pub fn search(&self, query: &str) -> BoxFuture<Result<MangaPageResult, SourceError>> {
        self.search_page(query, 1, Vec::new())
    }

    pub fn search_page(
        &self,
        query: &str,
        page: i32,
        filters: Vec<FilterValue>,
    ) -> BoxFuture<Result<MangaPageResult, SourceError>> {
        let query = query.to_string();
        self.submit(move |state| {
            // AidokuRunner clears the caller's filters when the source
            // declares `hidesFiltersWhileSearching` and a non-empty query is
            // being run, so a text search is not narrowed by stale filters.
            let filters = if state.manifest.hides_filters_while_searching && !query.is_empty() {
                Vec::new()
            } else {
                filters
            };
            let mut result =
                state
                    .interpreter
                    .get_search_manga_list(Some(&query), page.max(1), &filters)?;
            result.set_source_key(&state.source_id);
            state.remember_page(&result);
            Ok(result)
        })
    }

    /// Browses one static/dynamic listing by id.
    pub fn browse(
        &self,
        listing_id: &str,
        page: i32,
    ) -> BoxFuture<Result<MangaPageResult, SourceError>> {
        let listing_id = listing_id.to_string();
        self.submit(move |state| {
            let listing = state.find_listing(&listing_id)?;
            let mut result = state.interpreter.get_manga_list(&listing, page.max(1))?;
            result.set_source_key(&state.source_id);
            state.remember_page(&result);
            Ok(result)
        })
    }

    /// Returns merged details for one manga key.
    pub fn manga_details(&self, manga_id: &str) -> BoxFuture<Result<Manga, SourceError>> {
        let manga_id = manga_id.to_string();
        self.submit(move |state| state.manga_details(&manga_id))
    }

    /// Returns the chapter list for one manga key.
    pub fn chapters(&self, manga_id: &str) -> BoxFuture<Result<Vec<Chapter>, SourceError>> {
        let manga_id = manga_id.to_string();
        self.submit(move |state| state.chapters(&manga_id))
    }

    /// Chapter list for a manga restored from storage, so sources that read
    /// its `url`/`title` work without a fresh search or details request.
    pub fn chapters_with(&self, manga: Manga) -> BoxFuture<Result<Vec<Chapter>, SourceError>> {
        self.submit(move |state| {
            let key = manga.key.clone();
            state.remember_manga(manga);
            state.chapters(&key)
        })
    }

    /// Returns page entries for a chapter previously returned by
    /// [`Self::chapters`]. Returns [`SourceError::UnknownChapter`] when the
    /// chapter has not been loaded yet.
    pub fn pages(&self, chapter_id: &str) -> BoxFuture<Result<Vec<Page>, SourceError>> {
        let chapter_id = chapter_id.to_string();
        self.submit(move |state| state.pages(&chapter_id))
    }

    /// Explicit manga+chapter variant for callers that already hold both keys
    /// (for example a downloaded chapter on a new app start).
    pub fn pages_for(
        &self,
        manga_id: &str,
        chapter_id: &str,
    ) -> BoxFuture<Result<Vec<Page>, SourceError>> {
        let manga_id = manga_id.to_string();
        let chapter_id = chapter_id.to_string();
        self.submit(move |state| state.pages_for(&manga_id, &chapter_id))
    }

    /// Faithful variant for downloads resumed from a local database: pass the
    /// persisted [`Manga`] and [`Chapter`] records so sources that read
    /// `url`/`title` (not just `key`) still work.
    pub fn pages_with(
        &self,
        manga: Manga,
        chapter: Chapter,
    ) -> BoxFuture<Result<Vec<Page>, SourceError>> {
        self.submit(move |state| state.interpreter.get_page_list(&manga, &chapter))
    }

    pub fn listings(&self) -> BoxFuture<Result<Vec<Listing>, SourceError>> {
        self.submit(|state| state.listings())
    }

    /// The source's full filter list: static `filters.json` entries followed
    /// by the dynamic `get_filters` result when the module exports it, with no
    /// deduplication (matching AidokuRunner's `getSearchFilters`). A source
    /// whose `filters.json` is missing, malformed or unreadable still loads
    /// and searches; the parse/read error is returned by this call only.
    pub fn filters(&self) -> BoxFuture<Result<Vec<Filter>, SourceError>> {
        self.submit(|state| state.filters())
    }

    pub fn base_url(&self) -> BoxFuture<Result<Option<String>, SourceError>> {
        self.submit(|state| {
            if state.features.provides_base_url {
                state.interpreter.get_base_url()
            } else {
                Ok(None)
            }
        })
    }

    /// Resolves an `aidoku://` deep link through the source's
    /// `handle_deep_link` export. Returns `None` for links the source does not
    /// claim, and [`SourceError::Unimplemented`] when it has no such export.
    pub fn deep_link(&self, url: &str) -> BoxFuture<Result<Option<DeepLinkResult>, SourceError>> {
        let url = url.to_string();
        self.submit(move |state| state.interpreter.handle_deep_link(&url))
    }

    /// Basic (username/password) login handled by the source.
    pub fn basic_login(
        &self,
        key: &str,
        username: &str,
        password: &str,
    ) -> BoxFuture<Result<bool, SourceError>> {
        let key = key.to_string();
        let username = username.to_string();
        let password = password.to_string();
        self.submit(move |state| {
            state
                .interpreter
                .handle_basic_login(&key, &username, &password)
        })
    }

    /// Hands cookies obtained by a platform web view to the source.
    pub fn web_login(
        &self,
        key: &str,
        cookies: Vec<(String, String)>,
    ) -> BoxFuture<Result<bool, SourceError>> {
        let key = key.to_string();
        self.submit(move |state| state.interpreter.handle_web_login(&key, &cookies))
    }

    /// Migrates a stored key through the source's `handle_key_migration`.
    /// `kind` follows the Aidoku ABI: 0 manga, 1 chapter.
    pub fn migrate_key(
        &self,
        kind: i32,
        manga_key: &str,
        chapter_key: Option<&str>,
    ) -> BoxFuture<Result<String, SourceError>> {
        let manga_key = manga_key.to_string();
        let chapter_key = chapter_key.map(str::to_string);
        self.submit(move |state| {
            state
                .interpreter
                .handle_migration(kind, &manga_key, chapter_key.as_deref())
        })
    }

    /// Resolves the request for one page image (headers/body may be required
    /// by sources that sign image URLs).
    pub fn image_request(
        &self,
        url: &str,
        context: Option<Vec<(String, String)>>,
    ) -> BoxFuture<Result<Option<ImageRequest>, SourceError>> {
        let url = url.to_string();
        self.submit(move |state| {
            let context: Option<HashMap<String, String>> =
                context.map(|pairs| pairs.into_iter().collect());
            state.interpreter.get_image_request(&url, context.as_ref())
        })
    }

    /// Downloads bytes through the source's own HTTP session, preserving its
    /// cookies and headers. Intended for page images and archives: the
    /// returned [`NetResponseData`] keeps the real status code and transport
    /// error, so callers can distinguish 404 from a timeout.
    pub fn http_request(
        &self,
        request: NetRequestData,
    ) -> BoxFuture<Result<NetResponseData, SourceError>> {
        self.submit(move |state| Ok(state.interpreter.request(request)))
    }
}

impl Drop for SourceEngine {
    fn drop(&mut self) {
        if let Some(sender) = self.tx.get_mut().take() {
            drop(sender);
        }
    }
}

struct WorkerState {
    interpreter: AidokuInterpreter,
    source_id: String,
    manifest: SourceManifest,
    features: SourceFeatures,
    /// `None` = no `filters.json`; `Some(Err)` = file present but failed to
    /// read/parse (reported when `filters()` is asked for the schema).
    static_filters: Option<Result<Vec<Filter>, SourceError>>,
    mangas: HashMap<String, Manga>,
    manga_order: VecDeque<String>,
    chapters: HashMap<String, Chapter>,
    chapter_manga: HashMap<String, String>,
    chapter_order: VecDeque<String>,
    poisoned: Arc<AtomicBool>,
}

impl WorkerState {
    fn new(
        interpreter: AidokuInterpreter,
        manifest: SourceManifest,
        features: SourceFeatures,
        poisoned: Arc<AtomicBool>,
        static_filters: Option<Result<Vec<Filter>, SourceError>>,
    ) -> Self {
        Self {
            source_id: manifest.id.clone(),
            manifest,
            interpreter,
            features,
            static_filters,
            mangas: HashMap::new(),
            manga_order: VecDeque::new(),
            chapters: HashMap::new(),
            chapter_manga: HashMap::new(),
            chapter_order: VecDeque::new(),
            poisoned,
        }
    }

    fn remember_page(&mut self, page: &MangaPageResult) {
        for manga in &page.entries {
            self.remember_manga(manga.clone());
        }
    }

    fn remember_manga(&mut self, manga: Manga) {
        if manga.key.is_empty() {
            return;
        }
        let key = manga.key.clone();
        if self.mangas.insert(key.clone(), manga).is_none() {
            self.manga_order.push_back(key);
            while self.manga_order.len() > MAX_CACHED_MANGAS {
                if let Some(oldest) = self.manga_order.pop_front() {
                    self.mangas.remove(&oldest);
                }
            }
        }
    }

    fn cached_manga(&self, manga_id: &str) -> Manga {
        self.mangas.get(manga_id).cloned().unwrap_or_else(|| Manga {
            source_key: self.source_id.clone(),
            key: manga_id.to_string(),
            title: manga_id.to_string(),
            ..Default::default()
        })
    }

    fn remember_chapters(&mut self, manga_id: &str, chapters: &[Chapter]) {
        for chapter in chapters {
            if chapter.key.is_empty() {
                continue;
            }
            self.chapter_manga
                .insert(chapter.key.clone(), manga_id.to_string());
            if self
                .chapters
                .insert(chapter.key.clone(), chapter.clone())
                .is_none()
            {
                self.chapter_order.push_back(chapter.key.clone());
                while self.chapter_order.len() > MAX_CACHED_CHAPTERS {
                    if let Some(oldest) = self.chapter_order.pop_front() {
                        self.chapters.remove(&oldest);
                        self.chapter_manga.remove(&oldest);
                    }
                }
            }
        }
    }

    fn find_listing(&mut self, listing_id: &str) -> Result<Listing, SourceError> {
        // A known static listing does not depend on fetching dynamic lists.
        // Keep it usable when that optional upstream request is unavailable.
        if let Some(listing)=self.manifest.listings.iter().find(|listing|listing.id==listing_id) {
            return Ok(listing.clone());
        }
        for listing in self.listings()? {
            if listing.id == listing_id {
                return Ok(listing);
            }
        }
        Err(SourceError::Message(format!(
            "source has no listing \"{listing_id}\""
        )))
    }

    fn listings(&mut self) -> Result<Vec<Listing>, SourceError> {
        // Upstream `Source.getListings` concatenates without deduplication:
        // `staticListings + dynamicListings`, static first.
        let mut listings = self.manifest.listings.clone();
        if self.features.dynamic_listings {
            listings.extend(self.interpreter.get_listings()?);
        }
        Ok(listings)
    }

    fn filters(&mut self) -> Result<Vec<Filter>, SourceError> {
        // Upstream `Source.getSearchFilters` concatenates without
        // deduplication: `staticFilters + dynamicFilters`, static first.
        // A filters.json that failed to parse surfaces here (and only here);
        // the source can still load and search.
        let mut filters = match &self.static_filters {
            Some(Ok(static_filters)) => static_filters.clone(),
            Some(Err(error)) => return Err(error.clone()),
            None => Vec::new(),
        };
        if self.features.dynamic_filters {
            filters.extend(self.interpreter.get_filters()?);
        }
        Ok(filters)
    }

    fn manga_details(&mut self, manga_id: &str) -> Result<Manga, SourceError> {
        let base = self.cached_manga(manga_id);
        let update = self.interpreter.get_manga_update(&base, true, false)?;
        let mut merged = merge_manga(base, update);
        merged.source_key = self.source_id.clone();
        self.remember_manga(merged.clone());
        Ok(merged)
    }

    fn chapters(&mut self, manga_id: &str) -> Result<Vec<Chapter>, SourceError> {
        let base = self.cached_manga(manga_id);
        let update = self.interpreter.get_manga_update(&base, false, true)?;
        let chapters = update.chapters.clone().unwrap_or_default();
        let mut merged = merge_manga(base, update);
        merged.source_key = self.source_id.clone();
        merged.chapters = Some(chapters.clone());
        self.remember_manga(merged);
        self.remember_chapters(manga_id, &chapters);
        Ok(chapters)
    }

    fn pages(&mut self, chapter_id: &str) -> Result<Vec<Page>, SourceError> {
        let chapter = self
            .chapters
            .get(chapter_id)
            .cloned()
            .ok_or_else(|| SourceError::UnknownChapter(chapter_id.to_string()))?;
        let manga_id = self
            .chapter_manga
            .get(chapter_id)
            .cloned()
            .unwrap_or_default();
        let manga = self.cached_manga(&manga_id);
        self.interpreter.get_page_list(&manga, &chapter)
    }

    fn pages_for(&mut self, manga_id: &str, chapter_id: &str) -> Result<Vec<Page>, SourceError> {
        let manga = self.cached_manga(manga_id);
        let chapter = self
            .chapters
            .get(chapter_id)
            .cloned()
            .unwrap_or_else(|| Chapter {
                key: chapter_id.to_string(),
                ..Default::default()
            });
        self.interpreter.get_page_list(&manga, &chapter)
    }

    #[allow(dead_code)]
    fn is_poisoned(&self) -> bool {
        self.poisoned.load(Ordering::SeqCst)
    }
}

/// Aidoku's `get_manga_update` only fills the fields the caller requested, so
/// an update is layered over the cached record instead of replacing it.
fn merge_manga(base: Manga, update: Manga) -> Manga {
    Manga {
        source_key: base.source_key,
        key: if update.key.is_empty() {
            base.key
        } else {
            update.key
        },
        title: if update.title.is_empty() {
            base.title
        } else {
            update.title
        },
        cover: update.cover.or(base.cover),
        artists: update.artists.filter(|v| !v.is_empty()).or(base.artists),
        authors: update.authors.filter(|v| !v.is_empty()).or(base.authors),
        description: update.description.or(base.description),
        url: update.url.or(base.url),
        tags: update.tags.filter(|v| !v.is_empty()).or(base.tags),
        status: match update.status {
            PublishingStatus::Unknown => base.status,
            value => value,
        },
        content_rating: match update.content_rating {
            ContentRating::Unknown => base.content_rating,
            value => value,
        },
        viewer: match update.viewer {
            Viewer::Unknown => base.viewer,
            value => value,
        },
        update_strategy: match update.update_strategy {
            UpdateStrategy::Always => base.update_strategy,
            value => value,
        },
        next_update_time: update.next_update_time.or(base.next_update_time),
        chapters: update.chapters.or(base.chapters),
    }
}

fn seed_defaults(manifest: &SourceManifest, settings_json: Option<&str>) {
    if manifest.languages.len() > 1 {
        host_defaults::seed_value(
            &manifest.id,
            "languages",
            DefaultValue::StrList(manifest.languages.clone()),
        );
    }
    if manifest.allows_base_url_select && manifest.urls.len() > 1 {
        host_defaults::seed_value(
            &manifest.id,
            "url",
            DefaultValue::Str(manifest.urls[0].clone()),
        );
    }
    let Some(settings) = settings_json else {
        return;
    };
    let Ok(value) = serde_json::from_str::<Value>(settings) else {
        return;
    };
    seed_settings_value(&manifest.id, &value);
}

fn seed_settings_value(source_id: &str, value: &Value) {
    match value {
        Value::Array(items) => {
            for item in items {
                seed_settings_value(source_id, item);
            }
        }
        Value::Object(map) => {
            if let Some(items) = map.get("items").and_then(Value::as_array) {
                for item in items {
                    seed_settings_value(source_id, item);
                }
            }
            let Some(key) = map
                .get("key")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|key| !key.is_empty())
            else {
                return;
            };
            let ty = map.get("type").and_then(Value::as_str).unwrap_or("group");
            let default = map.get("default");
            let seeded = match ty {
                "select" | "picker" | "text" => default
                    .and_then(Value::as_str)
                    .map(|v| DefaultValue::Str(v.to_string())),
                "multi-select" | "editable-list" => {
                    default.and_then(Value::as_array).map(|items| {
                        DefaultValue::StrList(
                            items
                                .iter()
                                .filter_map(|v| v.as_str().map(str::to_string))
                                .collect(),
                        )
                    })
                }
                "switch" | "toggle" => default.and_then(Value::as_bool).map(DefaultValue::Bool),
                "stepper" | "segment" => default.and_then(Value::as_f64).map(|number| {
                    if number.fract() == 0.0 {
                        DefaultValue::Int(number as i32)
                    } else {
                        DefaultValue::Float(number as f32)
                    }
                }),
                _ => None,
            };
            if let Some(seeded) = seeded {
                host_defaults::seed_value(source_id, key, seeded);
            }
        }
        _ => {}
    }
}

fn panic_text(payload: Box<dyn std::any::Any + Send>) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        (*message).to_string()
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else {
        "unknown panic payload".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::default_filter_values;
    use crate::wasm::interpreter::AidokuInterpreter;
    use crate::wasm::models::MangaPageResult;
    use crate::wasm::models::{FilterTypeConfig, ListingKind, PublishingStatus};
    use crate::wasm::postcard::PostcardWriter;
    use std::sync::Arc;
    use std::sync::atomic::AtomicU64;

    fn wat_escape(bytes: &[u8]) -> String {
        let mut out = String::new();
        for byte in bytes {
            out.push_str(&format!("\\{byte:02x}"));
        }
        out
    }

    /// A module that returns a fixed postcard `MangaPageResult` from memory.
    fn catalogue_module() -> Vec<u8> {
        let manga = Manga {
            key: "manga-1".into(),
            title: "Test Manga".into(),
            cover: Some("https://example.test/cover.jpg".into()),
            status: PublishingStatus::Ongoing,
            ..Default::default()
        };
        let mut payload = PostcardWriter::new();
        payload.write_list(&[manga], |w, manga| manga.to_postcard(w));
        payload.write_bool(false);
        let payload = payload.into_bytes();
        let mut buffer = Vec::with_capacity(payload.len() + 8);
        buffer.extend_from_slice(&((payload.len() + 8) as u32).to_le_bytes());
        buffer.extend_from_slice(&[0u8; 4]);
        buffer.extend_from_slice(&payload);
        let source = format!(
            r#"(module
                (memory (export "memory") 1)
                (data (i32.const 1024) "{}")
                (func (export "get_search_manga_list")
                    (param i32 i32 i32) (result i32)
                    i32.const 1024))"#,
            wat_escape(&buffer)
        );
        wat::parse_str(source).unwrap()
    }

    fn js_module() -> Vec<u8> {
        let source = r#"(module
            (import "js" "context_eval" (func $eval (param i32 i32 i32) (result i32)))
            (memory (export "memory") 1)
            (func (export "get_search_manga_list")
                (param i32 i32 i32) (result i32)
                i32.const 0
                i32.const 0
                i32.const 0
                call $eval
                drop
                i32.const -1))"#;
        wat::parse_str(source).unwrap()
    }

    /// Never returns: `get_search_manga_list` spins forever.
    fn runaway_module() -> Vec<u8> {
        let source = r#"(module
            (memory (export "memory") 1)
            (func (export "get_search_manga_list")
                (param i32 i32 i32) (result i32)
                (loop $spin (br $spin))
                i32.const 0))"#;
        wat::parse_str(source).unwrap()
    }

    fn manifest() -> SourceManifest {
        SourceManifest {
            id: "test.source".into(),
            name: "Test Source".into(),
            version: 1,
            alt_names: Vec::new(),
            languages: vec!["en".into()],
            urls: vec!["https://example.test".into()],
            content_rating: 0,
            listings: Vec::new(),
            allows_base_url_select: false,
            hides_filters_while_searching: false,
        }
    }

    fn no_network() -> crate::wasm::interpreter::RequestHandler {
        Arc::new(|_request| NetResponseData {
            status_code: 200,
            ..Default::default()
        })
    }

    #[test]
    fn original_interpreter_round_trips_a_page_of_manga() {
        let mut interpreter = AidokuInterpreter::create(
            &catalogue_module(),
            "test.source".into(),
            no_network(),
            None,
            None,
            Arc::new(AtomicU64::new(0)),
        )
        .unwrap();
        let page = interpreter
            .get_search_manga_list(Some("test"), 1, &[])
            .unwrap();
        assert_eq!(page.entries.len(), 1);
        assert_eq!(page.entries[0].key, "manga-1");
        assert_eq!(page.entries[0].title, "Test Manga");
        assert!(!page.has_next_page);
    }

    #[test]
    fn public_engine_search_runs_on_its_worker_thread() {
        let engine =
            SourceEngine::load_wasm(manifest(), catalogue_module(), EngineOptions::default())
                .unwrap();
        let page: MangaPageResult = pollster::block_on(engine.search("test")).unwrap();
        assert_eq!(page.entries.len(), 1);
        assert_eq!(page.entries[0].source_key, "test.source");
        assert_eq!(engine.manifest().id, "test.source");
    }

    #[test]
    fn broken_module_reports_a_load_error() {
        let error =
            SourceEngine::load_wasm(manifest(), b"not wasm".to_vec(), EngineOptions::default())
                .unwrap_err();
        assert!(matches!(error, SourceError::Load(_)));
    }

    #[test]
    fn fuel_limit_is_configurable() {
        let engine =
            SourceEngine::load_wasm(manifest(), catalogue_module(), EngineOptions::default())
                .unwrap();
        assert_eq!(engine.fuel_per_call(), DEFAULT_FUEL_PER_CALL);
        engine.set_fuel_per_call(1_234);
        assert_eq!(engine.fuel_per_call(), 1_234);
        engine.set_fuel_per_call(0);
        assert_eq!(engine.fuel_per_call(), 0);
    }

    #[test]
    fn js_only_source_reports_web_verification_instead_of_empty_results() {
        let mut interpreter = AidokuInterpreter::create(
            &js_module(),
            "test.source".into(),
            no_network(),
            None,
            None,
            Arc::new(AtomicU64::new(0)),
        )
        .unwrap();
        let error = interpreter
            .get_search_manga_list(Some("test"), 1, &[])
            .unwrap_err();
        assert!(error.is_web_verification(), "{error}");
        assert_eq!(interpreter.features.provides_listings, false);
        assert_eq!(
            interpreter.features.provides_home, false,
            "unimplemented features stay visible to the UI"
        );
        let _ = ListingKind::Default;
    }

    #[test]
    fn runaway_source_hits_the_fuel_budget() {
        let mut interpreter = AidokuInterpreter::create(
            &runaway_module(),
            "test.source".into(),
            no_network(),
            None,
            None,
            Arc::new(AtomicU64::new(50_000)),
        )
        .unwrap();
        let error = interpreter
            .get_search_manga_list(Some("test"), 1, &[])
            .unwrap_err();
        assert!(
            matches!(error, SourceError::FuelExhausted { .. }),
            "{error}"
        );
    }

    // -----------------------------------------------------------------
    // Static filters / listings / hidesFiltersWhileSearching
    // -----------------------------------------------------------------

    /// A result block (`len+8` prefix, 4-byte pad, postcard payload) embedded
    /// in WASM memory, in the shape `handle_result` expects.
    fn result_block(payload: Vec<u8>) -> Vec<u8> {
        let mut buffer = Vec::with_capacity(payload.len() + 8);
        buffer.extend_from_slice(&((payload.len() + 8) as u32).to_le_bytes());
        buffer.extend_from_slice(&[0u8; 4]);
        buffer.extend_from_slice(&payload);
        buffer
    }

    fn page_result(key: &str) -> Vec<u8> {
        let manga = Manga {
            key: key.into(),
            title: key.into(),
            ..Default::default()
        };
        let mut payload = PostcardWriter::new();
        payload.write_list(&[manga], |w, manga| manga.to_postcard(w));
        payload.write_bool(false);
        result_block(payload.into_bytes())
    }

    /// Postcard-encoded `[Filter]` / `[Listing]` results for the dynamic
    /// exports.
    fn filters_result(filters: &[Filter]) -> Vec<u8> {
        let mut payload = PostcardWriter::new();
        payload.write_list(filters, |w, f| f.to_postcard(w));
        result_block(payload.into_bytes())
    }

    fn listings_result(listings: &[Listing]) -> Vec<u8> {
        let mut payload = PostcardWriter::new();
        payload.write_list(listings, |w, l| l.to_postcard(w));
        result_block(payload.into_bytes())
    }

    /// A source that:
    /// - returns `no-filters` / `with-filters` manga keys depending on whether
    ///   the `filters` descriptor it received encodes more than an empty list;
    /// - exports a dynamic `get_filters` and `get_listings`.
    fn filter_aware_module() -> Vec<u8> {
        let dynamic_filters = filters_result(&[Filter {
            id: "dyn-check".into(),
            title: Some("Dynamic".into()),
            hide_from_header: None,
            config: FilterTypeConfig::Check {
                name: None,
                can_exclude: false,
                default_value: Some(true),
            },
        }]);
        let dynamic_listings = listings_result(&[Listing {
            id: "popular".into(), // same id as the static listing: must append
            name: "Dynamic Popular".into(),
            kind: ListingKind::Default,
        }]);
        let no_filters = page_result("no-filters");
        let with_filters = page_result("with-filters");

        // memory layout
        let base = 1024usize;
        let off_filters = base;
        let off_listings = off_filters + dynamic_filters.len();
        let off_no = off_listings + dynamic_listings.len();
        let off_with = off_no + no_filters.len();

        let source = format!(
            r#"(module
                (import "std" "buffer_len" (func $len (param i32) (result i32)))
                (memory (export "memory") 1)
                (data (i32.const {off_filters}) "{}")
                (data (i32.const {off_listings}) "{}")
                (data (i32.const {off_no}) "{}")
                (data (i32.const {off_with}) "{}")
                ;; get_search_manga_list(query_desc, page, filters_desc)
                (func (export "get_search_manga_list")
                    (param $q i32) (param $page i32) (param $f i32) (result i32)
                    ;; empty filter list encodes as a single varint byte; real
                    ;; filters always exceed 4 bytes
                    (if (result i32)
                        (i32.gt_s (call $len (local.get $f)) (i32.const 4))
                        (then (i32.const {off_with}))
                        (else (i32.const {off_no}))))
                (func (export "get_filters") (result i32)
                    i32.const {off_filters})
                (func (export "get_listings") (result i32)
                    i32.const {off_listings}))"#,
            wat_escape(&dynamic_filters),
            wat_escape(&dynamic_listings),
            wat_escape(&no_filters),
            wat_escape(&with_filters),
        );
        wat::parse_str(source).unwrap()
    }

    fn manifest_with(
        listings: Vec<Listing>,
        hides_filters_while_searching: bool,
    ) -> SourceManifest {
        SourceManifest {
            id: "test.source".into(),
            name: "Test Source".into(),
            version: 1,
            alt_names: Vec::new(),
            languages: vec!["en".into()],
            urls: vec!["https://example.test".into()],
            content_rating: 0,
            listings,
            allows_base_url_select: false,
            hides_filters_while_searching,
        }
    }

    /// Builds an installed source directory on disk and loads it through the
    /// catalogue, so the `filters.json` path is exercised end to end.
    fn install_source(
        root: &std::path::Path,
        manifest: &SourceManifest,
        wasm: &[u8],
        filters_json: Option<&str>,
    ) -> InstalledSource {
        let dir = root.join(&manifest.id);
        std::fs::create_dir_all(&dir).unwrap();
        let source_json = format!(
            r#"{{"info":{{"id":"{}","name":"{}","version":1,"languages":["en"],
                "urls":["https://example.test"]}},
             "listings":[{}],
             "config":{{"hidesFiltersWhileSearching":{}}}}}"#,
            manifest.id,
            manifest.name,
            manifest
                .listings
                .iter()
                .map(|l| format!(
                    r#"{{"id":"{}","name":"{}","kind":{}}}"#,
                    l.id, l.name, l.kind as u8
                ))
                .collect::<Vec<_>>()
                .join(","),
            manifest.hides_filters_while_searching
        );
        std::fs::write(dir.join("source.json"), source_json).unwrap();
        std::fs::write(dir.join("main.wasm"), wasm).unwrap();
        if let Some(json) = filters_json {
            std::fs::write(dir.join("filters.json"), json).unwrap();
        }
        let catalog = SourceCatalog::new(root);
        catalog.get(&manifest.id).unwrap()
    }

    #[test]
    fn static_filters_load_and_merge_with_dynamic() {
        let temp = tempfile::tempdir().unwrap();
        let installed = install_source(
            temp.path(),
            &manifest(),
            &filter_aware_module(),
            Some(
                r#"[
                    {"id":"status","type":"select","title":"Status",
                     "options":["any","ongoing"],"ids":["","1"],"default":"1"},
                    {"id":"note","type":"note","text":"hi"}
                ]"#,
            ),
        );
        let engine = SourceEngine::load_installed(&installed).unwrap();
        let filters = pollster::block_on(engine.filters()).unwrap();
        // static first, then dynamic, no dedup
        assert_eq!(filters.len(), 3);
        assert_eq!(filters[0].id, "status");
        assert_eq!(filters[1].id, "note");
        assert_eq!(filters[2].id, "dyn-check");
        // the parsed defaults feed `default_filter_values`
        let values = default_filter_values(&filters);
        assert_eq!(
            values,
            vec![
                FilterValue::Select {
                    id: "status".into(),
                    value: "1".into()
                },
                FilterValue::Check {
                    id: "dyn-check".into(),
                    value: 1
                },
            ]
        );
    }

    #[test]
    fn corrupt_filters_json_reports_on_filters_but_still_loads() {
        let temp = tempfile::tempdir().unwrap();
        let installed = install_source(
            temp.path(),
            &manifest(),
            &filter_aware_module(),
            Some("[{\"type\":\"bogus\"}]"),
        );
        // The source still loads and searches; only filters() reports it.
        let engine = SourceEngine::load_installed(&installed).unwrap();
        let error = pollster::block_on(engine.filters()).unwrap_err();
        assert!(matches!(error, SourceError::Deserialize(_)), "{error}");
        let page = pollster::block_on(engine.search("x")).unwrap();
        assert_eq!(page.entries[0].key, "no-filters");

        // A valid filters.json still parses and merges.
        let temp2 = tempfile::tempdir().unwrap();
        let ok = install_source(
            temp2.path(),
            &manifest(),
            &filter_aware_module(),
            Some(r#"[{"id":"a","type":"text","placeholder":"x"}]"#),
        );
        let engine2 = SourceEngine::load_installed(&ok).unwrap();
        let filters = pollster::block_on(engine2.filters()).unwrap();
        assert_eq!(filters[0].id, "a");
    }

    #[test]
    fn advertised_but_missing_filters_file_reports_on_filters() {
        // filters_path points at a file that isn't there — reported, not
        // mistaken for "no filters", while the source still loads/searches.
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join("test.source");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("source.json"),
            r#"{"info":{"id":"test.source","name":"Test","version":1,
                "languages":["en"],"urls":["https://example.test"]}}"#,
        )
        .unwrap();
        std::fs::write(dir.join("main.wasm"), &filter_aware_module()).unwrap();
        let manifest = manifest();
        let installed = InstalledSource {
            manifest,
            directory: dir.clone(),
            wasm_path: dir.join("main.wasm"),
            settings_path: None,
            filters_path: Some(dir.join("filters.json")), // does not exist
            icon_path: None,
        };
        let engine = SourceEngine::load_installed(&installed).unwrap();
        let error = pollster::block_on(engine.filters()).unwrap_err();
        assert!(matches!(error, SourceError::Deserialize(_)), "{error}");
        assert!(pollster::block_on(engine.search("x")).is_ok());
    }

    #[test]
    fn search_forwards_filters_and_hides_them_while_searching() {
        // hidesFiltersWhileSearching = false: filters reach the module.
        let temp = tempfile::tempdir().unwrap();
        let installed = install_source(
            temp.path(),
            &manifest_with(Vec::new(), false),
            &filter_aware_module(),
            None,
        );
        let engine = SourceEngine::load_installed(&installed).unwrap();
        let page = pollster::block_on(engine.search_page(
            "one piece",
            1,
            vec![FilterValue::Text {
                id: "author".into(),
                value: "oda".into(),
            }],
        ))
        .unwrap();
        assert_eq!(page.entries[0].key, "with-filters");
        let page = pollster::block_on(engine.search_page("one piece", 1, Vec::new())).unwrap();
        assert_eq!(page.entries[0].key, "no-filters");

        // hidesFiltersWhileSearching = true + non-empty query: cleared.
        let temp2 = tempfile::tempdir().unwrap();
        let installed2 = install_source(
            temp2.path(),
            &manifest_with(Vec::new(), true),
            &filter_aware_module(),
            None,
        );
        let engine2 = SourceEngine::load_installed(&installed2).unwrap();
        let page = pollster::block_on(engine2.search_page(
            "one piece",
            1,
            vec![FilterValue::Text {
                id: "author".into(),
                value: "oda".into(),
            }],
        ))
        .unwrap();
        assert_eq!(page.entries[0].key, "no-filters");
        // empty query keeps the filters even when the flag is set.
        let page = pollster::block_on(engine2.search_page(
            "",
            1,
            vec![FilterValue::Text {
                id: "author".into(),
                value: "oda".into(),
            }],
        ))
        .unwrap();
        assert_eq!(page.entries[0].key, "with-filters");
    }

    #[test]
    fn listings_append_static_then_dynamic_without_dedup() {
        let temp = tempfile::tempdir().unwrap();
        let installed = install_source(
            temp.path(),
            &manifest_with(
                vec![Listing {
                    id: "popular".into(),
                    name: "Static Popular".into(),
                    kind: ListingKind::Default,
                }],
                false,
            ),
            &filter_aware_module(),
            None,
        );
        let engine = SourceEngine::load_installed(&installed).unwrap();
        let listings = pollster::block_on(engine.listings()).unwrap();
        // upstream `staticListings + dynamicListings` keeps both, same id ok
        assert_eq!(listings.len(), 2);
        assert_eq!(listings[0].name, "Static Popular");
        assert_eq!(listings[1].name, "Dynamic Popular");
        // browse resolves the first match (the static one)
        let listing = listings[0].clone();
        assert_eq!(listing.id, "popular");
    }

    #[test]
    fn static_listing_still_browses_when_dynamic_listing_export_fails() {
        let payload=wat_escape(&page_result("static-still-works"));
        let wasm=wat::parse_str(format!(r#"(module
            (memory (export "memory") 1)
            (data (i32.const 1024) "{payload}")
            (func (export "get_listings") (result i32) i32.const -1)
            (func (export "get_manga_list") (param i32 i32) (result i32) i32.const 1024)
        )"#)).unwrap();
        let engine=SourceEngine::load_wasm(manifest_with(vec![Listing{
            id:"static".into(),name:"Static".into(),kind:ListingKind::Default,
        }],false),wasm,EngineOptions::default()).unwrap();
        assert!(pollster::block_on(engine.listings()).is_err());
        let page=pollster::block_on(engine.browse("static",1)).unwrap();
        assert_eq!(page.entries[0].key,"static-still-works");
    }
}
