
use crate::motion::self;
use crate::motion::ShelfMotion;
use crate::shelf::self;
use crate::shelf::Plane;


use crate::worker::Command;
use crate::worker::Event;
use crate::worker::PAGE_NOT_READY;
use crate::worker::Worker;

use egui::*;
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use yejian_core::Book;
use yejian_core::ChapterLink;

mod shelf_page;
mod reader_page;
mod settings_page;
mod history_page;
mod categories;
mod dialogs;
mod test_support;
use shelf_page::Flight;
use reader_page::PageTexture;
use shelf_page::Reorder;
use shelf_page::ShelfSort;
use dialogs::StorageConfirm;
use reader_page::StreamBook;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Library,
    Sources,
    History,
    Settings,
}
#[derive(Clone)]
enum Screen {
    Library,
    Detail(String),
    Reader(String),
}
pub struct ReaderApp {
    root: PathBuf,
    worker: Worker,
    books: Vec<Book>,
    chapters: Vec<ChapterLink>,
    chapter_indices: HashMap<String, usize>,
    chapter_dialog: Option<String>,
    ready: bool,
    startup_error: Option<String>,
    query: String,
    search: bool,
    category: String,
    /// User-created categories in stored order (the `categories` setting).
    user_categories: Vec<String>,
    /// The 分类管理 panel (opaque card) is open.
    category_manage: bool,
    /// Multi-select 分类 picker is open.
    category_pick: bool,
    /// 新建分类 input inside the manage/pick panels.
    category_new: String,
    /// Category being renamed inside the manage panel.
    category_renaming: Option<String>,
    /// Draft name for the row being renamed.
    category_rename_edit: String,
    bookmarks: crate::bookmarks::Bookmarks,
    /// Chapter dialog shows the 书签 tab instead of 章节.
    dialog_bookmarks: bool,
    /// Page to land on when a bookmarked online chapter's stream opens.
    pending_bookmark_page: Option<usize>,
    /// Last page bytes staged under `share/` (acceptance asserts on it).
    last_shared: Option<PathBuf>,
    selected: HashSet<String>,
    tab: Tab,
    screen: Screen,
    reader_return: Screen,
    reader_options: bool,
    pending_reader_tap: Option<(f64, Pos2)>,
    motion: ShelfMotion,
    #[cfg(debug_assertions)]
    snapshot_request: Option<String>,
    #[cfg(debug_assertions)]
    snapshot_pending: Option<(String, f64)>,
    first_frame: Option<f64>,
    shelf_viewport: Rect,
    shelf_shift: f32,
    flight: Option<Flight>,
    covers: HashMap<String, TextureHandle>,
    cover_usage: HashMap<String, u64>,
    cover_clock: u64,
    visible_covers: HashSet<String>,
    cover_pending: HashSet<String>,
    cover_failed: HashSet<String>,
    pages: HashMap<(String, usize), PageTexture>,
    page_pending: HashSet<(String, usize)>,
    prefetch_blocked: HashSet<(String, usize)>,
    page_errors: HashMap<(String, usize), String>,
    page: usize,
    page_generation: u64,
    vertical_reader: crate::vertical::VerticalReader,
    /// Strip segment before/after the current chapter as `(book or
    /// `stream:` id, page count)`. `prev` exists only after the anchor
    /// crossed a seam; `next` only once its page count is known.
    vertical_prev: Option<(String, usize)>,
    vertical_next: Option<(String, usize)>,
    next_stream: Option<StreamBook>,
    saved_position: Option<(String, usize, f32)>,
    persisted_position: Option<(String, usize, f32)>,
    failed_position: Option<(String, usize, f32, bool)>,
    position_save_error: Option<String>,
    last_position_save: f64,
    zoom: f32,
    pan: Vec2,
    controls: bool,
    controls_anim: Option<motion::TimedTween>,
    page_slide: motion::PageSlide,
    /// Strip gap between the incoming and outgoing page during a turn:
    /// the eased outgoing offset is `current_offset - slide_turn_gap`.
    slide_turn_gap: f32,
    zoom_anim: motion::ZoomAnim,
    nav_pill: Option<motion::TimedTween>,
    last_nav_tab: Tab,
    tab_enter: Option<f64>,
    /// `(book id, start time)` driving the stage info's 0.12s opacity settle.
    focus_book_anim: Option<(String, f64)>,
    progress_tween: Option<motion::TimedTween>,
    last_thumb_selected: Option<usize>,
    /// Open timestamp driving the reader options panel's 0.16s fade/lift.
    reader_options_at: Option<f64>,
    /// Showing the 本章完 end card past the last page.
    chapter_end: bool,
    /// "lr" | "next" | "off" — tap-zone behaviour (`tap-zones` setting).
    tap_zones: String,
    /// 阅读时屏幕常亮 setting; `awake` mirrors the platform flag.
    keep_awake_setting: bool,
    perf_overlay: bool,
    last_frame_at: Option<std::time::Instant>,
    page_target_px: f32,
    hires_requested: HashSet<(String, usize)>,
    awake: bool,
    /// Calls made to `set_keep_awake`, asserted by tests.
    #[cfg(test)]
    awake_log: Vec<bool>,
    /// 显示时间和电量 setting + cached battery (refreshed ≤1/min).
    reader_status: bool,
    battery: Option<f32>,
    battery_at: Option<std::time::Instant>,
    reader_drag: f32,
    reader_drag_y: f32,
    reader_fit_limit: f32,
    private: bool,
    rtl: bool,
    vertical: bool,
    spread: bool,
    crop: bool,
    fit_width: bool,
    brightness: f32,
    spread_offset: bool,
    reader_globals: crate::reader_prefs::ReaderPrefs,
    reader_prefs: HashMap<String, crate::reader_prefs::ReaderPrefs>,
    dark: bool,
    reduced: bool,
    theme: String,
    color_scheme: String,
    library_grid: bool,
    shelf_sort: ShelfSort,
    sort_menu: bool,
    shelf_reorder: Option<Reorder>,
    shelf_reserve: f32,
    shelf_stage: Rect,
    shelf_books: Rect,
    font_scale: f32,
    widget_font_scale: f32,
    error: Option<String>,
    notice: Option<(String, f64)>,
    pending_notice: Option<String>,
    /// How the previous run ended (panic vs silent kill), announced as a
    /// toast once the shelf is ready; crash offers a 导出日志 action.
    exit_notice: Option<crate::diagnostics::ExitNotice>,
    exit_announced: bool,
    confirm_remove: bool,
    /// 移出书架 confirm: also delete archive/cover files (default on).
    remove_files: bool,
    /// Library storage report for the 存储 settings group.
    storage: Option<yejian_core::StorageReport>,
    /// Re-requests storage numbers each time the settings tab opens.
    storage_opened: bool,
    /// Pending destructive storage action awaiting confirmation.
    storage_confirm: Option<StorageConfirm>,
    /// Chapter-picker manage mode: multi-select for deletion.
    chapter_manage: bool,
    chapter_selected: std::collections::HashSet<String>,
    import_text: String,
    backup_path: String,
    sources: crate::sources::Sources,
    stream: Option<StreamBook>,
    sync: crate::sync::ProgressSync,
    trackers: crate::trackers::Trackers,
    tracker_anilist_token: String,
    tracker_kitsu_user: String,
    tracker_kitsu_pass: String,
    tracker_bangumi_token: String,
    track_panel: Option<String>,
    track_query: String,
    /// Unsaved status/progress edits in the tracking panel, by binding key.
    /// Keyed by binding key and remote media id so a rebind starts fresh.
    /// Each field is `Some` only while the user has edited it.
    track_draft: Option<(String, String, Option<crate::trackers::TrackStatus>, Option<i64>)>,
    /// 应用锁 state; the lock itself is in-memory, never persisted.
    applock: crate::applock::AppLock,
    /// 阅读统计 per local day per device (`reading-stats.json`).
    stats: crate::stats::Stats,
    /// 新章节通知: enabled by the 追更 settings toggle after the system
    /// grants permission; `notify_sent` dedupes chapter keys forever.
    notify_enabled: bool,
    notify_permission_pending: bool,
    notify_sent: crate::notify::SentStore,
    /// 云同步（WebDAV）worker + the settings text buffers.
    cloud: crate::cloud::CloudSync,
    cloud_url: String,
    cloud_user: String,
    cloud_password: String,
    cloud_dir: String,
    /// Source ids other devices installed, offered for 一键安装.
    cloud_missing: Vec<String>,
    /// Last appearance change (theme/palette/font-scale/direction) — the
    /// `appearance` group's shared `updated_at`.
    appearance_ts: i64,
    /// Whether the shell currently reports the app as foregrounded
    /// (desktop stays true; mobile reports via ForegroundEvent).
    app_foreground: bool,
    /// `history-hidden` setting: footprint row key -> hidden_at epoch secs.
    history_hidden: std::collections::BTreeMap<String, i64>,
    /// 阅读足迹 search field.
    history_query: String,
    /// Pending 清空足迹 confirmation.
    history_clear: bool,
    pub hitboxes: BTreeMap<String, Rect>,
    pub rendered_book_nodes: usize,
    pub frame_is_animating: bool,
    pub rendered_page_nodes: usize,
}
impl ReaderApp {
    pub fn new(root: PathBuf, ctx: Context) -> Self {
        crate::diagnostics::attach_root(&root);
        let exit_notice = crate::diagnostics::take_exit_notice();
        crate::perf::init();
        crate::perf::mark(crate::perf::Stage::PlatformInit);
        let backup_path = root.join("backup.yejian.zip").to_string_lossy().to_string();
        let reader_prefs = crate::reader_prefs::load(&root).unwrap_or_default();
        // Page files staged for the share sheet are one-shot temp files.
        let _ = std::fs::remove_dir_all(root.join("share"));
        let bookmarks = crate::bookmarks::Bookmarks::load(&root);
        // iOS BGAppRefreshTask runs this handler on a system thread; it only
        // touches disk state and the source engine (notify.rs keeps it
        // egui-free) and posts whatever it returns through the OS.
        let bg_root = root.clone();
        yejian_platform::set_background_refresh_handler(Box::new(move || {
            let mut sent = crate::notify::SentStore::load(&bg_root);
            crate::notify::run_background_check(
                &bg_root,
                &mut sent,
                std::time::Instant::now() + crate::notify::BG_DEADLINE,
            )
        }));
        Self {
            worker: Worker::start(root.clone(), ctx.clone()),
            sync: crate::sync::ProgressSync::new(root.clone(), ctx.clone()),
            trackers: crate::trackers::Trackers::new(root.clone(), ctx.clone()),
            tracker_anilist_token: String::new(),
            tracker_kitsu_user: String::new(),
            tracker_kitsu_pass: String::new(),
            tracker_bangumi_token: String::new(),
            track_panel: None,
            track_query: String::new(),
            track_draft: None,
            applock: crate::applock::AppLock::default(),
            stats: crate::stats::Stats::load(&root),
            notify_enabled: false,
            notify_permission_pending: false,
            notify_sent: crate::notify::SentStore::load(&root),
            cloud: crate::cloud::CloudSync::new(root.clone()),
            cloud_url: String::new(),
            cloud_user: String::new(),
            cloud_password: crate::cloud::load_password(&root),
            cloud_dir: "yejian".into(),
            cloud_missing: Vec::new(),
            appearance_ts: 0,
            app_foreground: true,
            history_hidden: Default::default(),
            history_query: String::new(),
            history_clear: false,
            sources: crate::sources::Sources::new(root.clone(), ctx),
            root,
            stream: None,
            books: vec![],
            chapters: Vec::new(),
            chapter_indices: HashMap::new(),
            chapter_dialog: None,
            ready: false,
            startup_error: None,
            query: String::new(),
            search: false,
            category: String::new(),
            user_categories: Vec::new(),
            category_manage: false,
            category_pick: false,
            category_new: String::new(),
            category_renaming: None,
            category_rename_edit: String::new(),
            bookmarks,
            dialog_bookmarks: false,
            pending_bookmark_page: None,
            last_shared: None,
            selected: HashSet::new(),
            tab: Tab::Library,
            screen: Screen::Library,
            reader_return: Screen::Library,
            reader_options: false,
            pending_reader_tap: None,
            motion: ShelfMotion::default(),
            #[cfg(debug_assertions)]
            snapshot_request: None,
            #[cfg(debug_assertions)]
            snapshot_pending: None,
            first_frame: None,
            shelf_viewport: Rect::NOTHING,
            shelf_shift: 0.,
            flight: None,
            covers: HashMap::new(),
            cover_usage: HashMap::new(),
            cover_clock: 0,
            visible_covers: HashSet::new(),
            cover_pending: HashSet::new(),
            cover_failed: HashSet::new(),
            pages: HashMap::new(),
            page_pending: HashSet::new(),
            prefetch_blocked: HashSet::new(),
            page_errors: HashMap::new(),
            page: 0,
            page_generation: 0,
            vertical_reader: crate::vertical::VerticalReader::default(),
            vertical_prev: None,
            vertical_next: None,
            next_stream: None,
            saved_position: None,
            persisted_position: None,
            failed_position: None,
            position_save_error: None,
            last_position_save: 0.,
            zoom: 1.,
            pan: Vec2::ZERO,
            controls: true,
            controls_anim: None,
            page_slide: motion::PageSlide::default(),
            slide_turn_gap: 0.,
            zoom_anim: motion::ZoomAnim::default(),
            nav_pill: None,
            last_nav_tab: Tab::Library,
            tab_enter: None,
            focus_book_anim: None,
            progress_tween: None,
            last_thumb_selected: None,
            reader_options_at: None,
            chapter_end: false,
            tap_zones: "lr".into(),
            keep_awake_setting: true,
            perf_overlay: false,
            last_frame_at: None,
            page_target_px: 2048.,
            hires_requested: HashSet::new(),
            awake: false,
            #[cfg(test)]
            awake_log: Vec::new(),
            reader_status: true,
            battery: None,
            battery_at: None,
            reader_drag: 0.,
            reader_drag_y: 0.,
            reader_fit_limit: 0.,
            private: false,
            rtl: false,
            vertical: false,
            spread: false,
            crop: false,
            fit_width: false,
            brightness: 1.,
            spread_offset: false,
            reader_globals: crate::reader_prefs::ReaderPrefs::default(),
            reader_prefs,
            dark: false,
            reduced: false,
            theme: "system".into(),
            color_scheme: "ink".into(),
            library_grid: false,
            shelf_sort: ShelfSort::Recent,
            sort_menu: false,
            shelf_reorder: None,
            shelf_reserve: 0.,
            shelf_stage: Rect::NOTHING,
            shelf_books: Rect::NOTHING,
            font_scale: 1.,
            widget_font_scale: 0.,
            error: None,
            notice: None,
            pending_notice: None,
            exit_notice,
            exit_announced: false,
            confirm_remove: false,
            remove_files: true,
            storage: None,
            storage_opened: false,
            storage_confirm: None,
            chapter_manage: false,
            chapter_selected: Default::default(),
            import_text: String::new(),
            backup_path,
            hitboxes: BTreeMap::new(),
            rendered_book_nodes: 0,
            frame_is_animating: false,
            rendered_page_nodes: 0,
        }
    }
    #[cfg(debug_assertions)]
    fn apply_snapshot(&mut self, name: &str) {
        match name {
            "detail" => {
                if let Some(book) = self.books.first() {
                    self.screen = Screen::Detail(book.id.clone());
                }
            }
            "reader" => {
                if let Some(book) = self.books.first().cloned() {
                    self.flight = None;
                    self.open_reader(&book);
                }
            }
            "sources" => {
                self.tab = Tab::Sources;
                self.screen = Screen::Library;
            }
            "community" => {
                self.tab = Tab::Sources;
                self.screen = Screen::Library;
                self.sources.preview_community();
            }
            "browse" | "filters" | "filtered" => {
                self.tab = Tab::Sources;
                self.screen = Screen::Library;
                if let Err(error) = self.sources.preview_browser(name) {
                    self.error = Some(format!("验收书源准备失败：{error:#}"));
                }
            }
            "settings" => {
                self.tab = Tab::Settings;
                self.screen = Screen::Library;
            }
            "history" => {
                self.tab = Tab::History;
                self.screen = Screen::Library;
            }
            _ => {}
        }
    }
    /// The simulator waits for a real rendered screen instead of a fixed delay.
    #[cfg(debug_assertions)]
    fn report_snapshot_ready(&mut self, ctx: &Context) {
        let Some((name, applied_at)) = self.snapshot_pending.as_ref() else {
            return;
        };
        let now = ctx.input(|i| i.time);
        let content_ready = self.ready
            && self.startup_error.is_none()
            && self.error.is_none()
            && self.cover_pending.is_empty()
            && self.cover_failed.is_empty()
            && (!matches!(name.as_str(), "shelf" | "detail") || !self.covers.is_empty())
            && (name != "community" || self.sources.repository_size() > 0)
            && (!matches!(name.as_str(), "browse" | "filters" | "filtered")
                || self.sources.browser_ready(name))
            && (!std::env::args().any(|arg| arg == "--ui-landscape")
                || ctx.content_rect().width() > ctx.content_rect().height())
            && (!matches!(self.screen, Screen::Reader(_)) || self.page_loaded());
        if !content_ready || now - applied_at < 3. {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
            return;
        }
        let safe = ctx.content_rect();
        let nav_safe = self
            .hitboxes
            .iter()
            .filter(|(id, _)| id.starts_with("nav-") || id.as_str() == "reader-back")
            .all(|(_, rect)| safe.contains_rect(*rect));
        if !nav_safe {
            log::error!("snapshot navigation outside native safe area: {safe:?}");
            ctx.request_repaint_after(std::time::Duration::from_secs(1));
            return;
        }
        let name = name.clone();
        let args: Vec<String> = std::env::args().collect();
        let run_id = args
            .iter()
            .position(|arg| arg == "--ui-snapshot-run")
            .and_then(|index| args.get(index + 1))
            .cloned()
            .unwrap_or_default();
        let report = serde_json::json!({
            "screen": name,
            "run_id": run_id,
            "pid": std::process::id(),
            "books": self.books.len(),
            "book_ids": self.books.iter().map(|book| &book.id).collect::<Vec<_>>(),
            "covers_loaded": self.covers.len(),
            "covers_pending": self.cover_pending.len(),
            "covers_failed": self.cover_failed.len(),
            "archives_available": self.books.iter().filter(|book| book.archive_path.exists()).count(),
            "cover_files_available": self.books.iter().filter(|book| book.cover_path.as_ref().is_some_and(|path| path.is_file())).count(),
            "page_loaded": self.page_loaded(),
            "repository_sources": self.sources.repository_size(),
            "source_browser":self.sources.browser_report(),
            "width": ctx.content_rect().width(),
            "height": ctx.content_rect().height(),
            "safe_area": [safe.left(), safe.top(), safe.right(), safe.bottom()],
            "navigation": self.hitboxes.iter().filter(|(id, _)| id.starts_with("nav-"))
                .map(|(id, rect)| (id.clone(), [rect.left(), rect.top(), rect.right(), rect.bottom()]))
                .collect::<BTreeMap<_, _>>(),
        });
        let dir = self.root.join("ui-snapshots");
        let json = report.to_string();
        let result = std::fs::create_dir_all(&dir)
            .and_then(|()| std::fs::write(dir.join(format!("{name}.tmp")), &json))
            .and_then(|()| {
                std::fs::rename(
                    dir.join(format!("{name}.tmp")),
                    dir.join(format!("{name}.json")),
                )
            });
        match result {
            Ok(()) => {
                // XCTest receives the same report from this running app,
                // rather than guessing which data container holds its files.
                #[cfg(target_os = "ios")]
                if let Ok(json) = std::ffi::CString::new(json) {
                    unsafe extern "C" {
                        fn yejian_ios_snapshot_ready(json: *const std::ffi::c_char);
                    }
                    unsafe {
                        yejian_ios_snapshot_ready(json.as_ptr());
                    }
                }
                log::info!("snapshot ready: {name}");
                self.snapshot_pending = None;
            }
            Err(error) => {
                log::error!("cannot write snapshot readiness: {error}");
                self.snapshot_pending = None;
            }
        }
    }
    fn send(&mut self, command: Command) {
        if let Err(e) = self.worker.send(command) {
            self.error = Some(e);
        }
    }
    fn setting(&mut self, key: &str, value: String) {
        self.send(Command::Setting(key.into(), value));
    }
    fn receive(&mut self, ctx: &Context) {
        while let Ok(event) = self.worker.rx.try_recv() {
            match event {
                Event::Ready(books, settings, links) => {
                    crate::perf::mark(crate::perf::Stage::LibraryOpen);
                    self.replace_links(links);
                    self.replace_books(books, ctx.input(|i| i.time));
                    self.ready = true;
                    let mut app_lock_enabled = self.applock.enabled();
                    let mut app_lock_delay = self.applock.delay_secs();
                    for (key, value) in settings {
                        match key.as_str() {
                            "theme" => self.theme = value,
                            "palette" => self.color_scheme = value,
                            "shelf-grid" => self.library_grid = value == "open",
                            "shelf-sort" => self.shelf_sort = ShelfSort::from_key(&value),
                            "search-scope" => self.sources.set_search_scope(&value),
                            "reduce-motion" => self.reduced = value == "true",
                            "reading-direction" => {
                                self.reader_globals.rtl = value == "rtl";
                                self.rtl = self.reader_globals.rtl;
                            }
                            "private" => {
                                self.private = value == "true";
                                self.sources.set_private(self.private);
                            }
                            "reading-mode" => {
                                self.reader_globals.mode = match value.as_str() {
                                    "vertical" => crate::reader_prefs::Mode::Vertical,
                                    "spread" => crate::reader_prefs::Mode::Spread,
                                    _ => crate::reader_prefs::Mode::Paged,
                                };
                                self.vertical = value == "vertical";
                                self.spread = value == "spread";
                            }
                            "reader-crop" => {
                                self.reader_globals.crop = value == "true";
                                self.crop = self.reader_globals.crop;
                            }
                            "reader-fit" => {
                                self.reader_globals.fit_width = value == "true";
                                self.fit_width = self.reader_globals.fit_width;
                            }
                            "reader-brightness" => {
                                self.reader_globals.brightness =
                                    value.parse::<f32>().unwrap_or(1.);
                                self.brightness = self.reader_globals.brightness();
                            }
                            "reader-spread-offset" => {
                                self.reader_globals.spread_offset = value == "true";
                                self.spread_offset = self.reader_globals.spread_offset;
                            }
                            "font-scale" => {
                                self.font_scale = value.parse::<f32>().unwrap_or(1.).clamp(1., 2.)
                            }
                            "keep-awake" => {
                                self.keep_awake_setting = value != "false";
                                if !self.keep_awake_setting {
                                    self.keep_awake(false);
                                }
                            }
                            "tap-zones" => {
                                if matches!(value.as_str(), "lr" | "next" | "off") {
                                    self.tap_zones = value;
                                }
                            }
                            "reader-status" => {
                                self.reader_status = value != "false";
                            }
                            "perf-overlay" => {
                                self.perf_overlay = value == "true";
                            }
                            "follow-interval" => {
                                let minutes: u64 = value
                                    .parse()
                                    .unwrap_or(crate::sources::DEFAULT_FOLLOW_INTERVAL_MIN);
                                self.sources.set_follow_interval(minutes);
                            }
                            "notify-updates" => {
                                self.notify_enabled = value == "true";
                            }
                            "webdav-enabled" => {
                                self.cloud.enabled = value == "true";
                            }
                            "webdav-url" => {
                                self.cloud_url = value;
                            }
                            "webdav-user" => {
                                self.cloud_user = value;
                            }
                            "webdav-dir" => {
                                self.cloud_dir = if value.is_empty() {
                                    "yejian".into()
                                } else {
                                    value
                                };
                            }
                            "appearance-updated" => {
                                self.appearance_ts = value.parse().unwrap_or(0);
                            }
                            "auto-download" => {
                                self.sources.set_auto_download(&value);
                            }
                            "source-repos" => {
                                self.sources.set_repos_setting(&value);
                            }
                            "source-repos-checked-at" => {
                                self.sources
                                    .set_repos_checked_day(value.parse::<i64>().ok());
                            }
                            "history-hidden" => {
                                if let Ok(entries) = serde_json::from_str::<
                                    Vec<serde_json::Value>,
                                >(&value)
                                {
                                    self.history_hidden = entries
                                        .iter()
                                        .filter_map(|entry| {
                                            Some((
                                                entry.get("key")?.as_str()?.to_string(),
                                                entry.get("hidden_at")?.as_i64()?,
                                            ))
                                        })
                                        .collect();
                                }
                            }
                            "app-lock" => {
                                app_lock_enabled = value == "true";
                            }
                            "app-lock-delay" => {
                                app_lock_delay = value.parse().unwrap_or(0);
                            }
                            "search-history" => {
                                if let Ok(history) =
                                    serde_json::from_str::<Vec<String>>(&value)
                                {
                                    self.sources.set_search_history(history);
                                }
                            }
                            "categories" => {
                                if let Ok(categories) =
                                    serde_json::from_str::<Vec<String>>(&value)
                                {
                                    self.user_categories = categories;
                                }
                            }
                            _ => {}
                        }
                    }
                    // The lock is decided here — before the first painted
                    // shelf frame — so an enabled app never flashes content.
                    self.applock.apply_settings(app_lock_enabled, app_lock_delay);
                    self.applock.cold_start();
                    yejian_platform::set_privacy_shield(self.applock.enabled());
                    yejian_platform::set_refresh_interval(
                        self.sources.follow_interval().max(60) * 60,
                    );
                    self.cloud.config.url = self.cloud_url.clone();
                    self.cloud.config.user = self.cloud_user.clone();
                    self.cloud.config.password = self.cloud_password.clone();
                    self.cloud.config.dir = self.cloud_dir.clone();
                    self.cloud.config.enabled = self.cloud.enabled;
                    self.cloud.enabled = self.cloud.config.enabled;
                    self.sources.set_private(self.private);
                    // 启动即同步一次（配置完整时）。
                    self.cloud.sync_now(self.cloud_snapshot());
                }
                Event::Library(books, links) => {
                    self.replace_links(links);
                    self.replace_books(books, ctx.input(|i| i.time));
                }
                Event::AppState(bytes) => {
                    match serde_json::from_slice::<crate::cloud::Snapshot>(&bytes) {
                        Ok(snapshot) => {
                            self.apply_cloud_snapshot(&snapshot);
                            self.pending_notice =
                                Some("已恢复备份中的应用数据".into());
                        }
                        Err(error) => {
                            self.error = Some(format!("备份中的应用数据损坏：{error}"));
                        }
                    }
                }
                Event::Image {
                    id,
                    page,
                    cover,
                    generation,
                    result,
                } => {
                    if cover {
                        self.cover_pending.remove(&id);
                        match result {
                            Ok(image) => {
                                if let Some(tile) = image.tiles.into_iter().next() {
                                    self.covers.insert(
                                        id.clone(),
                                        ctx.load_texture(
                                            format!("cover-{id}"),
                                            tile,
                                            TextureOptions::LINEAR,
                                        ),
                                    );
                                }
                            }
                            Err(error) => {
                                log::warn!("cannot load cover {id}: {error}");
                                self.cover_failed.insert(id);
                            }
                        }
                    } else {
                        if generation != self.page_generation {
                            continue;
                        }
                        self.page_pending.remove(&(id.clone(), page));
                        self.hires_requested.remove(&(id.clone(), page));
                        if result.as_ref().is_err_and(|error| error == PAGE_NOT_READY) {
                            if let Some(stream) = self.stream_for_mut(&id) {
                                stream.waiting.insert(page);
                            }
                            continue;
                        }
                        if !self.wants_page(&id, page) {
                            continue;
                        }
                        match result {
                            Ok(image) => {
                                let size = vec2(image.size[0] as f32, image.size[1] as f32);
                                let tiles = image
                                    .tiles
                                    .into_iter()
                                    .enumerate()
                                    .map(|(i, t)| {
                                        ctx.load_texture(
                                            format!("page-{id}-{page}-{i}"),
                                            t,
                                            TextureOptions::LINEAR,
                                        )
                                    })
                                    .collect();
                                self.pages.insert(
                                    (id.clone(), page),
                                    PageTexture {
                                        tiles,
                                        size,
                                        content: Rect::from_min_max(
                                            pos2(image.content[0], image.content[1]),
                                            pos2(image.content[2], image.content[3]),
                                        ),
                                    },
                                );
                                self.trim_pages();
                                if matches!(&self.screen,Screen::Reader(current) if current==&id)
                                    && self.page == page
                                {
                                    self.record_page(&id, page);
                                }
                            }
                            Err(e) => {
                                log::warn!("cannot load page {id}/{page}: {e}");
                                self.page_errors.insert((id, page), e);
                            }
                        }
                    }
                }
                Event::Notice(text) => self.pending_notice = Some(text),
                Event::ShareReady(result) => match result {
                    Ok(path) => {
                        self.last_shared = Some(path.clone());
                        match yejian_platform::share_file(&path) {
                            Ok(()) => {}
                            // Desktop has no share sheet; the staged file is
                            // the acceptance-verifiable outcome there.
                            Err(yejian_platform::PlatformError::Unsupported(_)) => {}
                            Err(error) => {
                                self.error = Some(format!("无法打开分享：{error}"))
                            }
                        }
                    }
                    Err(error) => {
                        self.error = Some(if error == PAGE_NOT_READY {
                            "页面还没下载完，稍后再试".into()
                        } else {
                            format!("无法读取页面：{error}")
                        });
                    }
                },
                Event::Storage(report) => self.storage = Some(report),
                Event::Error(text) => self.error = Some(text),
                Event::StartupError(text) => self.startup_error = Some(text),
                Event::ProgressSaved { id, page, fraction } => {
                    if self
                        .failed_position
                        .as_ref()
                        .is_some_and(|(failed, _, _, _)| failed == &id)
                    {
                        self.failed_position = None;
                        if self.error == self.position_save_error {
                            self.error = None;
                        }
                        self.position_save_error = None;
                    }
                    self.persisted_position = Some((id, page, fraction));
                }
                Event::ProgressSaveFailed {
                    id,
                    page,
                    fraction,
                    private,
                    error,
                } => {
                    self.failed_position = Some((id, page, fraction, private));
                    self.position_save_error = Some(error.clone());
                    self.error = Some(error);
                }
                Event::ChapterImported(id) => self.sources.ack_downloaded(&id),
                Event::ChapterImportFailed { id, error } => {
                    self.sources.import_failed(&id, &error);
                    self.error = Some(format!(
                        "漫画已下载，但加入书架失败：{error}\n可在书源页重试加入，不会重新下载。"
                    ));
                }
                Event::BackupReady(path) => {
                    #[cfg(any(target_os = "android", target_os = "ios"))]
                    if let Err(error) = yejian_platform::share_file(&path) {
                        self.error = Some(format!("备份已保存，打开分享面板失败：{error}"));
                    }
                    #[cfg(not(any(target_os = "android", target_os = "ios")))]
                    let _ = path;
                }
            }
        }
    }
    fn back(&mut self, now: f64, area: Rect) {
        match self.screen.clone() {
            Screen::Reader(_) => {
                self.flush_position();
                let epoch = crate::follows::now() as f64;
                self.stats.set_reader_open(false, epoch);
                self.stats.tick(epoch);
                self.stats.flush(&self.root);
                self.keep_awake(false);
                self.chapter_end = false;
                self.vertical_reader.stop();
                self.vertical_prev = None;
                self.vertical_next = None;
                self.next_stream = None;
                self.page_generation += 1;
                self.worker
                    .generation
                    .store(self.page_generation, Ordering::Relaxed);
                self.page_pending.clear();
                self.screen = self.reader_return.clone();
                self.cloud.queue_debounced();
                self.reader_options = false;
                self.pending_reader_tap = None;
                self.send(Command::Refresh);
            }
            Screen::Detail(id) => {
                let books = self.visible_books();
                self.shelf_viewport = area;
                let shelf_area = self.shelf_area(area.translate(vec2(0., self.shelf_shift)));
                if let Some(index) = books.iter().position(|b| self.same_series(&b.id, &id)) {
                    self.motion.select(index as f32, now, true);
                }
                let volumes = shelf::geometry(shelf_area, books.len(), self.motion.focus, 1.);
                if self.tab == Tab::Library
                    && let Some(volume) = volumes.iter().find(|v| {
                        v.lane == self.motion.focus.round() as i32
                            && self.same_series(&books[v.index].id, &id)
                    })
                {
                    if !self.reduced && self.covers.contains_key(&self.cover_id(&id)) {
                        let (started, source) = if let Some(out) = self
                            .flight
                            .as_ref()
                            .filter(|f| !f.returning && f.book_id == id)
                        {
                            let progress = ((now - out.started) / motion::OPEN).clamp(0., 1.);
                            (now - (1. - progress) * motion::CLOSE, out.from)
                        } else {
                            (now, volume.cover)
                        };
                        self.flight = Some(Flight {
                            id: self.cover_id(&id),
                            book_id: id.clone(),
                            started,
                            returning: true,
                            from: source,
                            to: Plane::rect(self.detail_cover_rect(area)),
                            foreground: shelf::foreground(&volumes, volume.lane, source),
                            clip: shelf_area,
                        });
                    }
                }
                self.screen = Screen::Library;
            }
            Screen::Library => {
                self.search = false;
                self.query.clear();
                self.selected.clear();
                self.tab = Tab::Library;
            }
        }
    }
    fn request_back(&mut self, now: f64, area: Rect, system: bool) {
        if self.error.take().is_some() {
            return;
        }
        if self.confirm_remove {
            self.confirm_remove = false;
            return;
        }
        if self.chapter_dialog.take().is_some() {
            return;
        }
        if self.reader_options {
            self.reader_options = false;
            return;
        }
        if matches!(self.screen, Screen::Library) && self.tab == Tab::Sources && self.sources.back()
        {
            if self.sources.take_shelf_return() {
                self.tab = Tab::Library;
            }
            return;
        }
        let at_root = matches!(self.screen, Screen::Library)
            && self.tab == Tab::Library
            && !self.search
            && self.selected.is_empty();
        if at_root && system {
            #[cfg(target_os = "android")]
            if let Err(error) = yejian_platform::finish() {
                self.error = Some(error.to_string());
            }
        } else {
            self.back(now, area);
        }
    }
    /// True while any bounded animation still needs repaints. Every `Option`
    /// it reads is cleared once the motion finishes, so an idle frame leaves
    /// `frame_is_animating` false and the repaint delay long.
    fn animating(&self, now: f64, entrance: f32) -> bool {
        let reader_motion = matches!(self.screen, Screen::Reader(_))
            && (self.page_slide.active(now)
                || self.zoom_anim.active(now)
                || self.controls_anim.is_some_and(|t| !t.done(now)));
        self.motion.active()
            || entrance < 1.
            || self.flight.is_some()
            || self.shelf_reorder.is_some()
            || reader_motion
            || (matches!(self.screen, Screen::Reader(_))
                && self.vertical
                && self.vertical_reader.active())
            || self.nav_pill.is_some_and(|t| !t.done(now))
            || self.tab_enter.is_some()
            || self
                .focus_book_anim
                .as_ref()
                .is_some_and(|(_, t0)| now - *t0 < 0.12)
            || self.progress_tween.is_some_and(|t| !t.done(now))
            || self
                .reader_options_at
                .is_some_and(|t0| now - t0 < 0.16)
            || (matches!(self.screen, Screen::Library)
                && self.tab == Tab::Sources
                && self.sources.is_animating(now))
    }
    fn colors(&self) -> (Color32, Color32, Color32) {
        let p = crate::design::palette(self.dark);
        (p.paper, p.ink, p.muted)
    }
    fn label(&self, p: &Painter, pos: Pos2, text: &str, size: f32, color: Color32) {
        p.text(
            pos,
            Align2::LEFT_TOP,
            text,
            FontId::proportional(size * self.font_scale),
            color,
        );
    }
    fn button(&mut self, ui: &mut Ui, id: &str, rect: Rect, label: &str, primary: bool) -> bool {
        self.hitboxes.insert(id.into(), rect);
        let response = ui.interact(rect, Id::unique(id), Sense::click());
        response.widget_info(|| WidgetInfo::labeled(Role::Button, true, label));
        let palette = crate::design::palette(self.dark);
        let pressed = response.is_pointer_button_down_on();
        let rect = crate::design::press_rect(rect, pressed && !self.reduced);
        let fill = if primary {
            if pressed {
                palette.primary.gamma_multiply(0.78)
            } else if response.hovered() {
                palette.primary.gamma_multiply(0.88)
            } else {
                palette.primary
            }
        } else if pressed {
            palette.accent_soft
        } else if response.hovered() {
            palette.soft
        } else {
            palette.surface
        };
        ui.painter().rect_filled(rect, 10., fill);
        if !primary {
            ui.painter()
                .rect_stroke(rect, 10., Stroke::new(1., palette.line), StrokeKind::Inside);
        }
        let text_color = if primary {
            palette.on_primary
        } else {
            palette.ink
        };
        let mut text = egui::text::LayoutJob::simple(
            label.to_owned(),
            FontId::proportional(15. * self.font_scale),
            text_color,
            (rect.width() - 20.).max(1.),
        );
        text.wrap.max_rows = 1;
        let galley = ui.painter().layout_job(text);
        ui.painter()
            .galley(rect.center() - galley.size() / 2., galley, text_color);
        response.clicked()
    }
    /// The shelf tab list: fixed entries, stored user categories in order,
    /// then any category found on books that is not stored yet.
    /// `None` marks the trailing 管理 tab.
    pub fn draw(&mut self, ui: &mut Ui) {
        crate::perf::mark(crate::perf::Stage::RendererReady);
        {
            let now = std::time::Instant::now();
            if matches!(self.screen, Screen::Reader(_))
                && let Some(last) = self.last_frame_at
            {
                crate::perf::record_frame(now.duration_since(last).as_secs_f64() * 1000.);
            }
            self.last_frame_at = Some(now);
        }
        let ctx = ui.ctx().clone();
        self.visible_covers.clear();
        self.receive(&ctx);
        #[cfg(debug_assertions)]
        if self.snapshot_request.is_some() && self.ready && !self.books.is_empty() {
            let name = self.snapshot_request.take().unwrap_or_default();
            self.apply_snapshot(&name);
            self.snapshot_pending = Some((name, ctx.input(|i| i.time)));
        }
        #[cfg(any(target_os = "android", target_os = "ios"))]
        {
            loop {
                match yejian_platform::poll_import() {
                    Ok(Some(batch)) => {
                        if !batch.failed.is_empty() {
                            self.error = Some(
                                batch
                                    .failed
                                    .iter()
                                    .map(|f| format!("{}：{}", f.name, f.reason))
                                    .collect::<Vec<_>>()
                                    .join("\n"),
                            );
                        }
                        if batch.kind == yejian_platform::PickKind::Backup {
                            for file in batch.files {
                                self.send(Command::Restore(file.path));
                            }
                        } else {
                            self.import_files(batch.files.into_iter().map(|f| f.path).collect());
                        }
                    }
                    Ok(None) => break,
                    Err(error) => {
                        self.error = Some(format!("读取导入结果失败：{error}"));
                        break;
                    }
                }
            }
        }
        self.sources.poll();
        self.sources.set_reduced(self.reduced);
        if let Some(scope) = self.sources.take_scope_change() {
            self.setting("search-scope", scope);
        }
        if let Some(history) = self.sources.take_history_change() {
            if let Ok(json) = serde_json::to_string(&history) {
                self.setting("search-history", json);
            }
        }
        for (key, value) in self.sources.take_setting_writes() {
            self.setting(&key, value);
        }
        if let Some(post) = self.sources.take_migrate_post() {
            if post.resume_unparsed {
                self.pending_notice =
                    Some("已换源，但没能从旧阅读记录解析出话数，将从第一章开始".into());
            }
            // One binding key covers whichever tracker the work is bound to;
            // the command moves it to the new source's key.
            self.trackers.submit(crate::trackers::TrackCommand::Rebind {
                old: post.rebind.0,
                new: post.rebind.1,
            });
            if let Some((source, manga)) = post.purge {
                let ids: Vec<String> = self
                    .chapters
                    .iter()
                    .filter(|link| {
                        link.metadata.source_id == source
                            && link.metadata.manga_id == manga
                    })
                    .map(|link| link.book_id.clone())
                    .collect::<std::collections::BTreeSet<_>>()
                    .into_iter()
                    .collect();
                if !ids.is_empty() {
                    self.send(Command::PurgeBooks(ids));
                }
            }
        }
        if let Some(notice) = self.sources.follow_notice.take() {
            self.pending_notice = Some(notice);
        }
        if let Some(chapter) = self.sources.take_stream() {
            self.open_stream(chapter);
        }
        self.update_stream();
        if let Some(chapter) = self.sources.take_open_chapter() {
            let book = self
                .chapters
                .iter()
                .find(|link| {
                    link.metadata.source_id == chapter.source_id
                        && link.metadata.manga_id == chapter.manga_id
                        && link.metadata.chapter_id == chapter.chapter_id
                })
                .and_then(|link| self.books.iter().find(|book| book.id == link.book_id))
                .cloned();
            if let Some(book) = book {
                self.open_reader(&book);
            } else {
                self.send(Command::Refresh);
                self.error = Some("这章还没有加入书架，正在刷新书库。请稍后再点「阅读」。".into());
            }
        }
        self.sync.poll();
        if self.sync.changed {
            self.sync.changed = false;
            self.send(Command::Refresh);
        }
        self.trackers.poll();
        for notice in self.trackers.notices.drain(..) {
            self.pending_notice = Some(notice);
        }
        while let Some(chapter) = self.sources.take_downloaded() {
            let id = chapter.id.clone();
            if let Err(error) = self.worker.send(Command::ImportChapter {
                task_id: chapter.id,
                path: chapter.path,
                metadata: chapter.metadata,
                read_page: chapter.read_page,
            }) {
                self.sources.import_failed(&id, &error);
                self.error = Some(error);
            }
        }
        let now = ctx.input(|i| i.time);
        let now_secs = now as u64;
        while let Some(event) = yejian_platform::take_foreground_event() {
            match event {
                yejian_platform::ForegroundEvent::Background => {
                    self.applock.background(now_secs);
                    self.app_foreground = false;
                    // iOS may kill the process while suspended: a marker left
                    // behind would read as an abnormal exit next launch.
                    crate::diagnostics::clear_running();
                    // Settle and persist the reading clock while the app is
                    // still allowed to run — the process may never see
                    // another frame once it is suspended.
                    let epoch = crate::follows::now() as f64;
                    self.stats.set_foreground(false, epoch);
                    self.stats.tick(epoch);
                    self.stats.flush(&self.root);
                }
                yejian_platform::ForegroundEvent::Foreground => {
                    self.applock.foreground(now_secs);
                    self.app_foreground = true;
                    crate::diagnostics::mark_running();
                    self.cloud.sync_now(self.cloud_snapshot());
                }
            }
        }
        while let Some(result) = yejian_platform::poll_auth() {
            self.applock.auth_finished(result);
            if let Some(on) = self.applock.take_toggle_applied() {
                self.setting("app-lock", on.to_string());
                yejian_platform::set_privacy_shield(on);
            }
        }
        // 新章节通知: permission answer → keep or revert the toggle.
        while let Some(granted) = yejian_platform::poll_notification_permission() {
            if self.notify_permission_pending {
                self.notify_permission_pending = false;
                if granted {
                    self.pending_notice = Some("新章节通知已开启".into());
                } else {
                    self.notify_enabled = false;
                    self.setting("notify-updates", "false".into());
                    self.pending_notice =
                        Some("系统拒绝了通知权限，请在系统设置中允许页间的通知".into());
                }
            }
        }
        // A tapped notification routes to the updated work or the follows list.
        while let Some(payload) = yejian_platform::take_notification_open() {
            match crate::notify::route(&payload) {
                Some(crate::notify::Route::Follow(source, manga)) => {
                    self.tab = Tab::Sources;
                    self.screen = Screen::Library;
                    self.sources.open_history_work(&source, &manga);
                }
                Some(crate::notify::Route::Follows) => {
                    self.tab = Tab::Sources;
                    self.screen = Screen::Library;
                }
                None => {}
            }
        }
        // Fresh chapters collected by checks: dedupe keys are marked when
        // surfaced; the system note only goes out while backgrounded —
        // foregrounded frames already show the in-app notice.
        // How the previous run ended, announced once the shelf is up.
        if self.ready && !self.exit_announced {
            self.exit_announced = true;
            if let Some(kind) = &self.exit_notice {
                self.pending_notice = Some(match kind {
                    crate::diagnostics::ExitNotice::Crash => {
                        "上次意外退出，已保存崩溃日志".into()
                    }
                    crate::diagnostics::ExitNotice::Oom => {
                        "上次可能因内存不足被系统关闭".into()
                    }
                });
            }
        }
        // Supervised workers report automatic restarts (or restart advice).
        let recoveries = crate::diagnostics::take_recoveries();
        if !recoveries.is_empty() {
            if recoveries.iter().any(|n| n.contains("下载")) {
                self.sources.reload_download_queue();
            }
            self.pending_notice = Some(recoveries.join("；"));
        }
        let notify_updates = self.sources.take_notify_updates();
        if !notify_updates.is_empty() {
            for update in &notify_updates {
                self.notify_sent.mark_all(
                    &crate::notify::follow_key(&update.source, &update.manga),
                    &update.added,
                );
            }
            if self.notify_enabled && !self.app_foreground {
                for note in crate::notify::summarize(&notify_updates) {
                    let _ = yejian_platform::post_notification(&note);
                }
            }
        }
        // 云同步: a finished round applies remote-won groups through the
        // UI-side owners; the debounce re-collects a fresh snapshot.
        if self.cloud.debounce_due() {
            self.cloud.clear_debounce();
            self.cloud.sync_now(self.cloud_snapshot());
        }
        self.cloud.poll();
        if let Some(outcome) = self.cloud.take_outcome() {
            let applied = self.apply_cloud_snapshot(&outcome.merged);
            self.cloud_missing = outcome.report.remote_sources.clone();
            self.pending_notice = Some(if applied > 0 {
                format!("云同步完成 · 应用了 {applied} 项远端更新")
            } else {
                "云同步完成".into()
            });
            if !self.cloud_missing.is_empty() {
                self.pending_notice = Some(format!(
                    "云同步完成 · 发现 {} 个云端书源（书源页可一键安装）",
                    self.cloud_missing.len()
                ));
            }
        }
        if self.cloud.busy() {
            ctx.request_repaint_after(std::time::Duration::from_millis(500));
        }
        // 阅读统计: conditions are pushed every frame; the module settles
        // the interval itself so no span is credited to the wrong state.
        {
            let epoch = crate::follows::now() as f64;
            let reading = matches!(self.screen, Screen::Reader(_));
            self.stats.set_reader_open(reading, epoch);
            self.stats.set_private(self.private, epoch);
            self.stats.set_foreground(self.app_foreground, epoch);
            self.stats.set_locked(self.applock.locked(), epoch);
            self.stats.tick(epoch);
            self.stats.flush_due(epoch, &self.root);
        }
        if self.applock.auth_in_flight() {
            ctx.request_repaint_after(std::time::Duration::from_millis(200));
        }
        let entrance = if self.books.is_empty() || self.reduced {
            1.
        } else {
            let began = *self.first_frame.get_or_insert(now);
            ((now - began) / motion::ENTRANCE).clamp(0., 1.) as f32
        };
        if let Some(message) = self.pending_notice.take() {
            self.notice = Some((message, now));
            ctx.request_repaint_after(std::time::Duration::from_secs(5));
        }
        self.dark = match self.theme.as_str() {
            "dark" => true,
            "light" => false,
            _ => ctx.input(|i| i.raw.system_theme == Some(Theme::Dark)),
        };
        crate::design::set_scheme(crate::design::Scheme::from_key(&self.color_scheme));
        let visuals = crate::design::visuals(self.dark);
        ctx.set_visuals(visuals.clone());
        ui.style_mut().visuals = visuals;
        if self.widget_font_scale != self.font_scale {
            let fonts = crate::text_styles(self.font_scale);
            for theme in [Theme::Light, Theme::Dark] {
                ctx.style_mut_of(theme, |style| style.text_styles = fonts.clone());
            }
            ui.style_mut().text_styles = fonts;
            self.widget_font_scale = self.font_scale;
        }
        self.hitboxes.clear();
        let viewport = ui.max_rect().intersect(ctx.viewport_rect());
        // eframe already obtains UIKit's safe content rectangle. Intersect it
        // with the shell's rectangle so either provider can protect the home
        // indicator, without subtracting the same inset twice.
        let full = viewport.intersect(ctx.content_rect());
        let full = match yejian_platform::safe_insets() {
            Ok(insets) => {
                let scale = ctx.pixels_per_point();
                full.intersect(Rect::from_min_max(
                    viewport.min + vec2(insets.left as f32 / scale, insets.top as f32 / scale),
                    viewport.max - vec2(insets.right as f32 / scale, insets.bottom as f32 / scale),
                ))
            }
            Err(_) => full,
        };
        self.page_target_px = full.width() * ctx.pixels_per_point();
        let (background, ink, _) = self.colors();
        ui.painter().rect_filled(full, 0., background);
        let tablet = Self::side_navigation(full);
        let rail_width = if full.width() >= 1280. { 176. } else { 88. };
        let content = if tablet {
            Rect::from_min_max(full.min + vec2(rail_width, 0.), full.max)
        } else {
            full
        };
        let system_back = !self.applock.locked()
            && (yejian_platform::take_back_requested()
                || ctx.input(|i| i.key_pressed(Key::BrowserBack)));
        if system_back || (!self.applock.locked() && ctx.input(|i| i.key_pressed(Key::Escape))) {
            self.request_back(now, content, system_back);
        }
        // Clear every finished bounded tween so an idle frame reports
        // `frame_is_animating == false` and the repaint delay stays long.
        if self.nav_pill.is_some_and(|t| t.done(now)) {
            self.nav_pill = None;
        }
        if self.tab_enter.is_some_and(|t0| now - t0 >= 0.14) {
            self.tab_enter = None;
        }
        if self.progress_tween.is_some_and(|t| t.done(now)) {
            self.progress_tween = None;
        }
        if self.controls_anim.is_some_and(|t| t.done(now)) {
            self.controls_anim = None;
        }
        if !self.reader_options {
            self.reader_options_at = None;
        }
        let tab_alpha = if let Some(t0) = self.tab_enter {
            let k = ((now - t0) / 0.14).clamp(0., 1.) as f32;
            if k >= 1. {
                1.
            } else {
                0.45 + 0.55 * motion::smooth(k)
            }
        } else {
            1.
        };
        let is_reader = matches!(self.screen, Screen::Reader(_));
        self.motion.tick(now);
        self.frame_is_animating = self.animating(now, entrance);
        if self.frame_is_animating {
            ctx.request_repaint();
        }
        if is_reader {
            if let Screen::Reader(id) = self.screen.clone() {
                self.reader_ui(ui, full, &id, now);
            }
        } else {
            let transition = self.flight.as_ref().map(|flight| {
                let duration = if flight.returning {
                    motion::CLOSE
                } else {
                    motion::OPEN
                };
                let elapsed = ((now - flight.started) / duration).clamp(0., 1.) as f32;
                let progress = if flight.returning {
                    1. - elapsed
                } else {
                    elapsed
                };
                (flight.book_id.clone(), motion::travel(progress))
            });
            let flight_reveal = transition.as_ref().map(|(_, reveal)| *reveal);
            if !matches!(self.screen, Screen::Library) {
                // The back affordance fades in late during the cover flight
                // instead of snapping onto the detail page.
                let back_alpha = flight_reveal
                    .map(|reveal| motion::smooth(((reveal - 0.35) / 0.45).clamp(0., 1.)))
                    .unwrap_or(1.);
                if back_alpha > 0.001 {
                    let mut go_back = false;
                    ui.scope(|ui| {
                        ui.set_opacity(back_alpha);
                        go_back = self.icon(
                            ui,
                            "back",
                            Rect::from_min_size(content.min + vec2(24., 14.), vec2(44., 44.)),
                            "back",
                            "返回书架",
                        );
                    });
                    if go_back {
                        self.back(now, content);
                    }
                }
            } else {
                // The library top bar crossfades in as the cover flies home.
                let bar_alpha = flight_reveal
                    .map(|reveal| 1. - motion::smooth((reveal / 0.42).min(1.)))
                    .unwrap_or(1.);
                if bar_alpha > 0.001 {
                    let mut toggled = false;
                    let mut import = false;
                    ui.scope(|ui| {
                        ui.set_opacity(bar_alpha);
                        if flight_reveal.is_some() {
                            ui.disable();
                        }
                        let title = match self.tab {
                            Tab::Library => "书架",
                            Tab::Sources => "发现书源",
                            Tab::History => "阅读足迹",
                            Tab::Settings => "设置",
                        };
                        ui.painter().text(
                            content.min + vec2(24., 37.),
                            Align2::LEFT_CENTER,
                            title,
                            crate::typography::display(30. * self.font_scale.min(1.3)),
                            ink,
                        );
                        if self.tab == Tab::Library
                            && self.icon(
                                ui,
                                "search",
                                Rect::from_min_size(
                                    pos2(content.right() - 68., content.top() + 16.),
                                    vec2(44., 44.),
                                ),
                                "search",
                                "搜索书库",
                            )
                        {
                            toggled = true;
                        }
                        if self.tab == Tab::Library
                            && self.icon(
                                ui,
                                "import",
                                Rect::from_min_size(
                                    pos2(content.right() - 120., content.top() + 16.),
                                    vec2(44., 44.),
                                ),
                                "plus",
                                "导入漫画",
                            )
                        {
                            import = true;
                        }
                    });
                    if toggled {
                        self.search = !self.search;
                    }
                    if import {
                        self.pick_import();
                    }
                }
            }
            if let Some((id, reveal)) = transition {
                ui.scope(|ui| {
                    ui.disable();
                    ui.set_opacity(1. - motion::smooth((reveal / 0.5).min(1.)));
                    self.library_viewport(ui, content, now, 1.);
                });
                ui.scope(|ui| {
                    ui.disable();
                    ui.set_opacity(motion::smooth(((reveal - 0.5) / 0.5).max(0.)));
                    self.detail_ui(ui, content, &id);
                });
            } else {
                match self.screen.clone() {
                    Screen::Library => {
                        // Opening 设置 re-runs the storage scan once.
                        if self.tab != Tab::Settings {
                            self.storage_opened = false;
                        }
                        ui.scope(|ui| {
                            ui.set_opacity(tab_alpha);
                            match self.tab {
                                Tab::Library => {
                                    self.library_viewport(ui, content, now, entrance);
                                    crate::perf::mark(crate::perf::Stage::FirstShelfFrame);
                                }
                                Tab::History => self.history_ui(ui, content),
                                Tab::Settings => self.settings_ui(ui, content),
                                Tab::Sources => self.sources_ui(ui, content),
                            }
                        });
                    }
                    Screen::Detail(id) => self.detail_ui(ui, content, &id),
                    _ => {}
                }
            }
            if tablet || matches!(self.screen, Screen::Library) {
                self.navigation(ui, full, tablet, now);
            }
            if let Some(flight) = &self.flight {
                let duration = if flight.returning {
                    motion::CLOSE
                } else {
                    motion::OPEN
                };
                let t = ((now - flight.started) / duration).clamp(0., 1.) as f32;
                let progress = if flight.returning { 1. - t } else { t };
                if let Some(texture) = self.covers.get(&flight.id) {
                    shelf::paint_flight(
                        ui.painter(),
                        flight.from,
                        flight.to,
                        progress,
                        texture,
                        self.dark,
                    );
                    if progress < 0.12 {
                        let mut mask = ui.painter().with_clip_rect(flight.clip);
                        mask.multiply_opacity(
                            1. - motion::cubic(progress / 0.12, 0.42, 0., 0.58, 1.),
                        );
                        let books = self.visible_books();
                        for volume in &flight.foreground {
                            if let Some(book) = books.get(volume.index) {
                                let cover = self.cover_id(&book.id);
                                shelf::paint_volume(
                                    &mask,
                                    volume,
                                    &book.title,
                                    self.covers.get(&cover),
                                    self.dark,
                                    false,
                                    self.selected.contains(&book.id),
                                    None,
                                );
                            }
                        }
                    }
                }
                if t >= 1. {
                    self.flight = None;
                }
            }
        }
        if let Some((text, at)) = self.notice.clone() {
            let age = (now - at) as f32;
            if age < 5. {
                // 0.18s slide/fade in, 0.22s fade out before the 5s expiry;
                // repaints are requested only during those two windows.
                let enter = if self.reduced {
                    1.
                } else {
                    motion::smooth((age / 0.18).clamp(0., 1.))
                };
                let exit = if self.reduced {
                    1.
                } else {
                    (1. - (age - (5. - 0.22)) / 0.22).clamp(0., 1.)
                };
                let alpha = (enter * exit).clamp(0., 1.);
                if alpha > 0.001 {
                    let crash = matches!(
                        self.exit_notice,
                        Some(crate::diagnostics::ExitNotice::Crash)
                    ) && text.contains("意外退出");
                    let fg = self.colors().0;
                    let height = if crash { 54. } else { 42. };
                    let rect = Rect::from_center_size(
                        pos2(full.center().x, full.bottom() - 100. + 14. * (1. - enter)),
                        vec2((full.width() - 32.).min(560.), height),
                    );
                    let mut toast = ui.painter().clone();
                    toast.multiply_opacity(alpha);
                    crate::design::soft_shadow(&toast, rect, height / 2., 1., self.dark);
                    toast.rect_filled(rect, height / 2., ink);
                    let text_rect = if crash {
                        rect.shrink2(vec2(16., 0.)).with_max_x(rect.max.x - 108.)
                    } else {
                        rect.shrink2(vec2(16., 0.))
                    };
                    let mut job = egui::text::LayoutJob::simple(
                        text,
                        FontId::proportional(12.5),
                        fg,
                        text_rect.width().max(40.),
                    );
                    job.wrap.max_rows = 1;
                    job.wrap.break_anywhere = true;
                    let galley = ui.painter().layout_job(job);
                    let text_pos = if crash {
                        pos2(text_rect.min.x, rect.center().y - galley.size().y * 0.5)
                    } else {
                        pos2(
                            text_rect.center().x - galley.size().x * 0.5,
                            rect.center().y - galley.size().y * 0.5,
                        )
                    };
                    toast.galley(text_pos, galley, fg);
                    if crash {
                        let btn_rect = Rect::from_center_size(
                            pos2(rect.max.x - 54., rect.center().y),
                            vec2(92., 44.),
                        );
                        let button = ui.put(
                            btn_rect,
                            Button::new(RichText::new("导出日志").size(12.5).color(fg))
                                .fill(fg.gamma_multiply(0.16))
                                .stroke(Stroke::new(1., fg))
                                .corner_radius(8.),
                        );
                        self.hitboxes.insert("crash-export".into(), button.rect);
                        if button.clicked() {
                            self.exit_notice = None;
                            self.export_diagnostics();
                        }
                    }
                }
                if !self.reduced && (age < 0.18 || age > 5. - 0.22) {
                    ctx.request_repaint();
                }
            }
        }
        self.dialogs(&ctx);
        if self.applock.locked() {
            self.lock_ui(ui, full);
        }
        if self.perf_overlay {
            let text = crate::perf::overlay_text();
            let font = FontId::monospace(11.);
            let fg = Color32::from_rgb(242, 239, 233);
            let galley = ui.painter().layout_no_wrap(text, font, fg);
            let size = galley.size() + vec2(16., 10.);
            let rect = Rect::from_min_size(
                pos2((full.max.x - 12. - size.x).max(full.min.x + 8.), full.min.y + 12.),
                size,
            );
            ui.painter()
                .rect_filled(rect, 8., Color32::from_rgb(28, 26, 24));
            ui.painter().rect_stroke(
                rect,
                8.,
                Stroke::new(1., Color32::from_rgb(68, 64, 60)),
                StrokeKind::Inside,
            );
            ui.painter().galley(rect.min + vec2(8., 5.), galley, fg);
            ctx.request_repaint_after(std::time::Duration::from_millis(500));
        }
        // Covers are "all here" once the shelf's pending requests drain.
        if self.ready && self.cover_pending.is_empty() && !self.books.is_empty() {
            crate::perf::mark(crate::perf::Stage::CoversReady);
        }
        self.trim_covers();
        self.frame_is_animating = self.animating(now, entrance);
        if self.frame_is_animating {
            ctx.request_repaint();
        }
    }
}
impl eframe::App for ReaderApp {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        self.draw(ui);
        #[cfg(debug_assertions)]
        self.report_snapshot_ready(ui.ctx());
    }
    fn on_exit(&mut self) {
        self.worker.shutdown();
        self.keep_awake(false);
        self.flush_position();
        let epoch = crate::follows::now() as f64;
        self.stats.set_reader_open(false, epoch);
        self.stats.tick(epoch);
        self.stats.flush(&self.root);
        self.page_generation += 1;
        self.worker
            .generation
            .store(self.page_generation, Ordering::Relaxed);
        if let Err(error) = self.worker.flush(std::time::Duration::from_secs(5)) {
            eprintln!("页间关闭时保存未完成：{error}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::reader_page::spread_pair;
    use yejian_core::ChapterMetadata;
    use crate::sources::STREAM_PREFIX;
    use crate::sources::StreamChapter;
    use crate::follows;

    fn book(id: &str) -> Book {
        Book {
            id: id.into(),
            title: id.into(),
            author: String::new(),
            archive_path: PathBuf::new(),
            cover_path: None,
            page_count: 20,
            current_page: 0,
            page_fraction: 0.,
            updated_at: 1,
            added_at: 1,
            last_read_at: None,
            category: String::new(),
        }
    }
    fn page(ctx: &Context) -> PageTexture {
        PageTexture {
            tiles: vec![ctx.load_texture(
                "test",
                ColorImage::filled([2, 2], Color32::WHITE),
                TextureOptions::LINEAR,
            )],
            size: vec2(2., 2.),
            content: Rect::from_min_max(Pos2::ZERO, pos2(1., 1.)),
        }
    }
    #[test]
    fn ipad_navigation_stays_inside_native_safe_area() {
        let root = tempfile::tempdir().unwrap();
        let ctx = Context::default();
        crate::configure_fonts(&ctx);
        let mut app = ReaderApp::new(root.path().into(), ctx.clone());
        let input = RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(834., 1210.))),
            safe_area_insets: Some(SafeAreaInsets(egui::epaint::MarginF32 {
                top: 24.,
                bottom: 25.,
                left: 0.,
                right: 0.,
            })),
            ..Default::default()
        };
        ctx.run_ui(input, |ui| app.draw(ui))
            .drop_without_applying_deltas();
        let nav = app.hitboxes["nav-library"];
        assert!(
            nav.bottom() <= 1185.,
            "home indicator overlaps navigation: {nav:?}"
        );
        assert!(
            nav.bottom() >= 1170.,
            "safe inset was deducted twice: {nav:?}"
        );
        assert!(nav.height() >= 44.);
    }
    #[test]
    fn cover_cache_keeps_visible_books_and_evicts_old_entries() {
        let root = tempfile::tempdir().unwrap();
        let ctx = Context::default();
        let mut app = ReaderApp::new(root.path().into(), ctx.clone());
        // Covers evict on a byte LRU (64 MB cap); each of these is 4 MB.
        for index in 0..18 {
            let id = format!("book-{index}");
            app.covers.insert(id.clone(), ctx.load_texture(
                id.clone(), ColorImage::filled([1024, 1024], Color32::WHITE), TextureOptions::LINEAR,
            ));
            app.cover_usage.insert(id, index);
        }
        app.visible_covers.insert("book-0".into());
        app.trim_covers();
        assert_eq!(app.covers.len(), 16);
        assert!(app.covers.contains_key("book-0"));
        assert!(app.covers.contains_key("book-17"));
        assert!(!app.covers.contains_key("book-1"));
    }
    #[test]
    fn history_only_requests_covers_for_visible_rows() {
        let root = tempfile::tempdir().unwrap();
        let ctx = Context::default();
        crate::configure_fonts(&ctx);
        let mut app = ReaderApp::new(root.path().into(), ctx.clone());
        app.books = (0..500).map(|index| {
            let mut book = book(&format!("book-{index}"));
            book.last_read_at = Some(index);
            book
        }).collect();
        ctx.run_ui(RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(390., 844.))),
            ..Default::default()
        }, |ui| app.history_ui(ui, ui.max_rect())).drop_without_applying_deltas();
        assert!(!app.cover_pending.is_empty());
        assert!(app.cover_pending.len() <= 6, "hidden history rows requested {} covers", app.cover_pending.len());
    }
    #[test]
    fn online_chapter_waits_for_pages_and_hands_over_to_the_library_copy_at_the_same_page() {
        let root = tempfile::tempdir().unwrap();
        let ctx = Context::default();
        let mut app = ReaderApp::new(root.path().into(), ctx.clone());
        let metadata = ChapterMetadata {
            source_id: "source.a".into(),
            manga_id: "story".into(),
            manga_title: "Story".into(),
            chapter_id: "c1".into(),
            chapter_title: "第 1 话".into(),
            chapter_number: Some(1.),
            volume_number: None,
        };
        app.open_stream(StreamChapter {
            id: "0123456789abcdef".into(),
            metadata: metadata.clone(),
            from_end: false,
        });
        let id = format!("{STREAM_PREFIX}0123456789abcdef");
        assert_eq!(app.screen_name(), "reader");
        assert_eq!(app.page_pending, HashSet::from([(id.clone(), 0)]));
        app.page_pending.clear();
        app.stream.as_mut().unwrap().waiting.insert(0);
        app.ask_page(&id, 0);
        assert!(app.page_pending.is_empty(), "a missing page is not requested every frame");
        assert!(app.stream_waiting_text(&id, 0).is_some());

        app.stream.as_mut().unwrap().book.page_count = 20;
        app.pages.insert((id.clone(), 5), page(&ctx));
        app.page = 5;
        app.record_page(&id, 5);
        assert_eq!(app.stream.as_ref().unwrap().book.current_page, 5);

        app.books = vec![book("imported")];
        app.replace_links(vec![ChapterLink { book_id: "imported".into(), metadata }]);
        app.update_stream();
        assert!(app.stream.is_none());
        assert_eq!(app.current_book_id(), Some("imported"));
        assert_eq!(app.page, 5);
        assert!(app.pages.contains_key(&("imported".to_string(), 5)), "no reload flash");
    }
    #[test]
    fn returning_closes_dialog_before_leaving_the_reader() {
        let root = tempfile::tempdir().unwrap();
        let mut app = ReaderApp::new(root.path().into(), Context::default());
        app.reader_return = Screen::Detail("book".into());
        app.screen = Screen::Reader("book".into());
        app.error = Some("page decode failed".into());
        let area = Rect::from_min_size(Pos2::ZERO, vec2(390., 844.));
        app.request_back(1., area, true);
        assert!(app.error.is_none());
        assert_eq!(app.screen_name(), "reader");
        app.confirm_remove = true;
        app.request_back(2., area, true);
        assert!(!app.confirm_remove);
        assert_eq!(app.screen_name(), "reader");
        app.reader_options = true;
        app.request_back(3., area, true);
        assert!(!app.reader_options);
        assert_eq!(app.screen_name(), "reader");
        app.request_back(4., area, true);
        assert_eq!(app.screen_name(), "detail");
    }
    #[test]
    fn opening_another_book_releases_old_page_textures_and_requests() {
        let root = tempfile::tempdir().unwrap();
        let ctx = Context::default();
        let mut app = ReaderApp::new(root.path().into(), ctx.clone());
        app.books = vec![book("old"), book("new")];
        app.pages.insert(("old".into(), 0), page(&ctx));
        app.pages.insert(("new".into(), 14), page(&ctx));
        app.page_pending.insert(("old".into(), 1));
        let next = book("new");
        app.open_reader(&next);
        assert!(app.pages.is_empty());
        assert_eq!(app.page_pending, HashSet::from([("new".into(), 0)]));
        let generation = app.page_generation;
        app.next_page("new", 10, 0., 800.);
        assert!(app.page_generation > generation);
        assert_eq!(app.page_pending, HashSet::from([("new".into(), 10)]));
    }
    #[test]
    fn downloaded_chapters_group_by_source_and_keep_fractional_chapter_order() {
        let root = tempfile::tempdir().unwrap();
        let mut app = ReaderApp::new(root.path().into(), Context::default());
        app.books = vec![book("one"), book("extra"), book("two"), book("foreign")];
        let metadata = |source: &str, chapter: &str, n: f64| ChapterMetadata {
            source_id: source.into(),
            manga_id: "story".into(),
            manga_title: "同名漫画".into(),
            chapter_id: chapter.into(),
            chapter_title: format!("第 {n} 话"),
            chapter_number: Some(n),
            volume_number: None,
        };
        app.replace_links(vec![
            ChapterLink {
                book_id: "one".into(),
                metadata: metadata("source-a", "1", 1.),
            },
            ChapterLink {
                book_id: "two".into(),
                metadata: metadata("source-a", "2", 2.),
            },
            ChapterLink {
                book_id: "extra".into(),
                metadata: metadata("source-a", "2.5", 2.5),
            },
            ChapterLink {
                book_id: "foreign".into(),
                metadata: metadata("source-b", "1", 1.),
            },
        ]);
        assert_eq!(app.shelf_book_count(), 2);
        assert_eq!(app.cover_id("extra"), "one");
        assert_eq!(app.cover_id("foreign"), "foreign");
        assert_eq!(app.adjacent_chapter("two", 1).unwrap().id, "extra");
        assert!(app.adjacent_chapter("extra", 1).is_none());
        app.books.swap(0, 2);
        assert_eq!(app.visible_books()[0].id, "one");
        app.books
            .iter_mut()
            .find(|b| b.id == "extra")
            .unwrap()
            .last_read_at = Some(123);
        assert_eq!(app.visible_books()[0].id, "extra");
        app.selected.insert("two".into());
        let selected = app.selected_books();
        assert_eq!(selected.len(), 3);
        assert!(!selected.contains(&"foreign".into()));
    }
    #[test]
    fn reader_prefs_save_per_work_and_beat_the_viewer_hint() {
        let root = tempfile::tempdir().unwrap();
        let ctx = Context::default();
        let mut app = ReaderApp::new(root.path().into(), ctx.clone());
        app.books = vec![book("one"), book("two"), book("other")];
        let metadata = |chapter: &str| ChapterMetadata {
            source_id: "source.a".into(),
            manga_id: "story".into(),
            manga_title: "Story".into(),
            chapter_id: chapter.into(),
            chapter_title: chapter.into(),
            chapter_number: None,
            volume_number: None,
        };
        app.replace_links(vec![
            ChapterLink {
                book_id: "one".into(),
                metadata: metadata("c1"),
            },
            ChapterLink {
                book_id: "two".into(),
                metadata: metadata("c2"),
            },
        ]);
        let mut stored = yejian_sources::Manga {
            source_key: "source.a".into(),
            key: "story".into(),
            viewer: yejian_sources::Viewer::Vertical,
            ..Default::default()
        };
        crate::sources::save_manga_snapshot(root.path(), &stored).unwrap();

        // No saved entry: the source hint chooses vertical.
        app.open_reader(&book("one"));
        assert!(app.vertical, "viewer hint should apply");

        // A reader-side change lands on the work and beats the hint.
        app.set_reading_mode("paged");
        assert!(!app.vertical);
        app.open_reader(&book("two"));
        assert!(!app.vertical, "chapters of one work share settings");
        stored.viewer = yejian_sources::Viewer::Unknown;
        app.open_reader(&book("other"));
        assert!(!app.vertical && !app.spread, "no link means global defaults");
        assert!(root.path().join("reader-prefs.json").is_file());

        // A fresh instance reloads the same per-work entry.
        let mut reopened = ReaderApp::new(root.path().into(), ctx);
        reopened.books = vec![book("one")];
        reopened.replace_links(vec![ChapterLink {
            book_id: "one".into(),
            metadata: metadata("c1"),
        }]);
        reopened.open_reader(&book("one"));
        assert!(!reopened.vertical && !reopened.spread);
    }

    #[test]
    fn spread_offset_shows_page_zero_alone_then_odd_pairs() {
        assert_eq!(spread_pair(0, true), (0, None));
        assert_eq!(spread_pair(1, true), (1, Some(2)));
        assert_eq!(spread_pair(2, true), (1, Some(2)));
        assert_eq!(spread_pair(3, true), (3, Some(4)));
        assert_eq!(spread_pair(4, true), (3, Some(4)));
        assert_eq!(spread_pair(0, false), (0, Some(1)));
        assert_eq!(spread_pair(2, false), (2, Some(3)));
    }

    #[test]
    fn page_cache_never_evicts_the_visible_spread_partner() {
        let root = tempfile::tempdir().unwrap();
        let ctx = Context::default();
        let mut app = ReaderApp::new(root.path().into(), ctx.clone());
        app.spread = true;
        app.screen = Screen::Reader("book".into());
        for index in 0..3 {
            let mut texture = page(&ctx);
            texture.size = vec2(3000., 9000.);
            app.pages.insert(("book".into(), index), texture);
        }
        app.trim_pages();
        assert!(app.pages.contains_key(&("book".into(), 0)));
        assert!(app.pages.contains_key(&("book".into(), 1)));
        assert!(!app.pages.contains_key(&("book".into(), 2)));
        assert!(app.prefetch_blocked.contains(&("book".into(), 2)));
    }

    fn linked(book_id: &str, chapter: &str) -> ChapterLink {
        ChapterLink {
            book_id: book_id.into(),
            metadata: ChapterMetadata {
                source_id: "source.a".into(),
                manga_id: "story".into(),
                manga_title: "Story".into(),
                chapter_id: chapter.into(),
                chapter_title: chapter.into(),
                chapter_number: None,
                volume_number: None,
            },
        }
    }
    fn vertical_frame(ctx: &Context, app: &mut ReaderApp) {
        ctx.run_ui(RawInput::default(), |ui| {
            let content = Rect::from_min_size(Pos2::ZERO, vec2(800., 600.));
            let gesture = ui.interact(
                content,
                egui::Id::unique("vertical-gesture"),
                egui::Sense::click_and_drag(),
            );
            let (id, count) = match &app.screen {
                Screen::Reader(id) => (
                    id.clone(),
                    app.reader_book(id)
                        .map(|book| book.page_count)
                        .unwrap_or(0),
                ),
                _ => (String::new(), 0),
            };
            app.vertical_ui(ui, content, &id, count, 1.0, &gesture);
        })
        .drop_without_applying_deltas();
    }
    #[test]
    fn reader_options_stay_on_screen_at_large_text() {
        let root = tempfile::tempdir().unwrap();
        let ctx = Context::default();
        crate::configure_fonts(&ctx);
        let size = [320., 1024.];
        yejian_platform::set_insets_override(Some(yejian_platform::Insets {
            left: 0,
            top: 24,
            right: 0,
            bottom: 20,
        }));
        let mut app = ReaderApp::new(root.path().into(), ctx.clone());
        app.books = vec![book("one")];
        app.font_scale = 2.;
        app.open_reader(&app.books[0].clone());
        app.reader_options = true;
        for _ in 0..6 {
            ctx.run_ui(
                RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, size.into())),
                    ..Default::default()
                },
                |ui| app.draw(ui),
            )
            .drop_without_applying_deltas();
        }
        yejian_platform::set_insets_override(None);
        let screen = ctx
            .content_rect()
            .intersect(Rect::from_min_max(pos2(0., 24.), pos2(320., 1004.)));
        for id in [
            "reader-mode-paged",
            "reader-mode-spread",
            "reader-mode-vertical",
            "reader-direction",
            "reader-options-close",
        ] {
            let rect = app
                .hitboxes
                .get(id)
                .unwrap_or_else(|| panic!("missing hitbox: {id}"));
            assert!(
                screen.contains_rect(*rect),
                "{id} escaped the screen: {rect:?} vs {screen:?}"
            );
        }
    }
    #[test]
    fn vertical_strip_flows_into_the_next_chapter_without_a_jump() {
        let root = tempfile::tempdir().unwrap();
        let ctx = Context::default();
        let mut app = ReaderApp::new(root.path().into(), ctx.clone());
        let mut one = book("one");
        one.page_count = 3;
        let mut two = book("two");
        two.page_count = 2;
        app.books = vec![one.clone(), two.clone()];
        app.replace_links(vec![linked("one", "c1"), linked("two", "c2")]);
        app.reader_globals.mode = crate::reader_prefs::Mode::Vertical;
        app.open_reader(&one.clone());
        for index in 0..3 {
            app.pages.insert(("one".into(), index), page(&ctx));
        }
        for index in 0..2 {
            app.pages.insert(("two".into(), index), page(&ctx));
        }
        vertical_frame(&ctx, &mut app);
        assert_eq!(app.vertical_next, Some(("two".into(), 2)));

        // Reach the last page of chapter one like a real reader does; the
        // keep window then holds the first pages of chapter two as well.
        app.vertical_reader.scroll(1600.);
        vertical_frame(&ctx, &mut app);
        assert!(matches!(&app.screen, Screen::Reader(id) if id == "one"));
        assert_eq!(app.vertical_reader.page, 2);
        assert_eq!(app.page, 2);
        // Stand-ins for the prefetched textures of chapter two.
        for index in 0..2 {
            app.pages.insert(("two".into(), index), page(&ctx));
        }

        // Scrolling past the last page of chapter one keeps the strip's
        // position and hands over to chapter two: current book, per-chapter
        // progress and still-loaded textures all move with it.
        app.vertical_reader.scroll(10_000.);
        vertical_frame(&ctx, &mut app);
        assert!(matches!(&app.screen, Screen::Reader(id) if id == "two"));
        assert_eq!(app.vertical_prev, Some(("one".into(), 3)));
        assert_eq!(app.vertical_reader.page, 4, "global index across the seam");
        assert_eq!(app.page, 1, "last page of chapter two");
        assert_eq!(app.books[0].current_page, 2, "chapter one left at its end");
        assert_eq!(app.books[1].current_page, 1);
        assert!(app.pages.contains_key(&("one".into(), 2)));
        assert!(app.pages.contains_key(&("two".into(), 0)));

        // The previous chapter's still-loaded pages stay reachable by
        // scrolling back; the current book itself does not switch back.
        app.vertical_reader.scroll(-1600.);
        vertical_frame(&ctx, &mut app);
        assert!(matches!(&app.screen, Screen::Reader(id) if id == "two"));
        assert_eq!(app.vertical_reader.page, 2, "anchor back in chapter one");
        assert!(app.hitboxes.contains_key("vertical-page-2"));
        assert_eq!(
            app.saved_position
                .as_ref()
                .map(|(id, page, _)| (id.as_str(), *page)),
            Some(("one", 2)),
            "the anchor chapter's own position is saved"
        );
        assert_eq!(app.page, 1, "self.page still tracks the current chapter");
    }
    #[test]
    fn vertical_strip_waits_for_the_next_online_chapters_page_count() {
        let root = tempfile::tempdir().unwrap();
        let ctx = Context::default();
        let mut app = ReaderApp::new(root.path().into(), ctx.clone());
        let mut one = book("one");
        one.page_count = 2;
        app.books = vec![one.clone()];
        app.replace_links(vec![linked("one", "c1")]);
        app.reader_globals.mode = crate::reader_prefs::Mode::Vertical;
        app.sources.seed_online_detail(
            yejian_sources::Manga {
                source_key: "source.a".into(),
                key: "story".into(),
                title: "Story".into(),
                ..Default::default()
            },
            vec![
                yejian_sources::Chapter {
                    key: "c1".into(),
                    title: Some("第一话".into()),
                    ..Default::default()
                },
                yejian_sources::Chapter {
                    key: "c2".into(),
                    title: Some("第二话".into()),
                    ..Default::default()
                },
            ],
        );
        app.open_reader(&one.clone());
        for index in 0..2 {
            app.pages.insert(("one".into(), index), page(&ctx));
        }
        vertical_frame(&ctx, &mut app);
        // The next chapter's page count is unknown: no segment yet.
        assert!(app.vertical_next.is_none());

        // Near the end the chapter gets queued; once the task reports its
        // total the segment joins the strip as a stream book.
        app.vertical_reader.scroll(10_000.);
        vertical_frame(&ctx, &mut app);
        assert!(app.vertical_next.is_none(), "still no known page count");
        let task = crate::sources::identity_id("source.a", "story", "c2");
        assert!(
            app.sources.stream_state(&task).is_some(),
            "the next chapter was queued behind the reader"
        );
        app.sources.force_pages(&task, 2, 0);
        vertical_frame(&ctx, &mut app);
        let seg = format!("{STREAM_PREFIX}{task}");
        assert_eq!(app.vertical_next, Some((seg.clone(), 2)));
        assert!(app.next_stream.is_some());

        // Crossing into the stream segment switches both the screen book
        // and the stream bookkeeping without resetting the offset.
        for index in 0..2 {
            app.pages.insert((seg.clone(), index), page(&ctx));
        }
        app.vertical_reader.scroll(10_000.);
        vertical_frame(&ctx, &mut app);
        assert!(matches!(&app.screen, Screen::Reader(id) if id == &seg));
        assert_eq!(app.vertical_prev, Some(("one".into(), 2)));
        assert_eq!(app.vertical_reader.page, 3);
        assert!(
            app.stream.as_ref().is_some_and(|s| s.book.id == seg),
            "stream bookkeeping moved to the new chapter"
        );
        assert_eq!(app.page, 1);
        assert_eq!(app.books[0].current_page, 1, "chapter one left at its end");
    }

    fn link(book_id: &str, chapter_id: &str, number: f64) -> ChapterLink {
        ChapterLink {
            book_id: book_id.into(),
            metadata: ChapterMetadata {
                source_id: "source.a".into(),
                manga_id: "story".into(),
                manga_title: "Story".into(),
                chapter_id: chapter_id.into(),
                chapter_title: chapter_id.into(),
                chapter_number: Some(number),
                volume_number: None,
            },
        }
    }

    #[test]
    fn shelf_sorts_keep_recent_order_and_rank_progress_and_added() {
        let root = tempfile::tempdir().unwrap();
        let mut app = ReaderApp::new(root.path().into(), Context::default());
        let mut done = book("done");
        done.current_page = 19;
        done.last_read_at = Some(10);
        let mut half = book("half");
        half.current_page = 9;
        half.last_read_at = Some(20);
        let mut most = book("most");
        most.current_page = 15;
        most.last_read_at = Some(5);
        let fresh = book("fresh");
        app.books = vec![done, half, most, fresh];
        let order = |app: &ReaderApp| {
            app.visible_books()
                .iter()
                .map(|book| book.id.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            order(&app),
            ["done", "half", "most", "fresh"],
            "the default order must be the stored order exactly"
        );
        app.shelf_sort = ShelfSort::Progress;
        assert_eq!(
            order(&app),
            ["most", "half", "done", "fresh"],
            "in-progress by fraction, then finished, then unread"
        );
        for (index, book) in app.books.iter_mut().enumerate() {
            book.added_at = (index as i64 + 1) * 100;
        }
        app.shelf_sort = ShelfSort::Added;
        assert_eq!(order(&app), ["fresh", "most", "half", "done"]);
    }

    #[test]
    fn shelf_sort_reads_series_progress_and_earliest_added() {
        let root = tempfile::tempdir().unwrap();
        let mut app = ReaderApp::new(root.path().into(), Context::default());
        let mut chapter1 = book("s1");
        chapter1.current_page = 19;
        chapter1.last_read_at = Some(3);
        chapter1.added_at = 300;
        let mut chapter2 = book("s2");
        chapter2.current_page = 9;
        chapter2.last_read_at = Some(40);
        chapter2.added_at = 100;
        let mut chapter3 = book("s3");
        chapter3.added_at = 400;
        app.books = vec![chapter1, chapter2, chapter3];
        app.chapters = vec![link("s1", "c1", 1.), link("s2", "c2", 2.), link("s3", "c3", 3.)];
        app.chapter_indices = app
            .chapters
            .iter()
            .enumerate()
            .map(|(i, link)| (link.book_id.clone(), i))
            .collect();
        // Representative = latest read chapter (s2): progress (1 + 0.5)/3.
        app.shelf_sort = ShelfSort::Progress;
        let visible = app.visible_books();
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].id, "s2");
        let ordinals = app.series_ordinals();
        let (group, fraction) = app.work_progress(&visible[0], &ordinals);
        assert_eq!(group, 0, "a half-read middle chapter is in progress");
        assert!((fraction - 0.5).abs() < 0.001);
        // Finish the last chapter: the work counts as finished.
        app.books[2].current_page = 19;
        app.books[2].last_read_at = Some(50);
        let visible = app.visible_books();
        let ordinals = app.series_ordinals();
        let (group, _) = app.work_progress(&visible[0], &ordinals);
        assert_eq!(group, 1);
        // Added sort takes the earliest chapter's join date.
        app.shelf_sort = ShelfSort::Added;
        let mut solo = book("solo");
        solo.added_at = 200;
        app.books.push(solo);
        let order: Vec<String> = app.visible_books().iter().map(|b| b.id.clone()).collect();
        assert_eq!(
            order,
            ["solo", "s3"],
            "series added_at is the earliest chapter's (100 < solo's 200)"
        );
    }

    #[test]
    fn sorting_jumps_to_first_book_and_animates_unless_reduced() {
        let root = tempfile::tempdir().unwrap();
        let mut app = ReaderApp::new(root.path().into(), Context::default());
        app.books = vec![book("a"), book("b"), book("c")];
        app.motion.focus = 2.;
        app.set_shelf_sort(ShelfSort::Added, 10.);
        assert_eq!(app.motion.focus, 0.);
        let reorder = app.shelf_reorder.expect("a sort change must animate");
        assert_eq!(reorder.from_ids, ["a", "b", "c"]);
        // No animation when reduce-motion is on.
        app.reduced = true;
        app.shelf_reorder = None;
        app.set_shelf_sort(ShelfSort::Progress, 11.);
        assert!(app.shelf_reorder.is_none());
        // A same-set reorder from a library refresh also animates.
        app.reduced = false;
        app.shelf_sort = ShelfSort::Recent;
        app.motion.focus = 1.;
        let mut swapped = app.books.clone();
        swapped.swap(0, 2);
        app.replace_books(swapped, 12.);
        let reorder = app
            .shelf_reorder
            .expect("same books in a new order must animate");
        assert_eq!(reorder.from_ids, ["a", "b", "c"]);
        assert_eq!(reorder.from_focus, 1.);
        // A real change does not animate.
        app.shelf_reorder = None;
        let mut grown = app.books.clone();
        grown.push(book("d"));
        app.replace_books(grown, 13.);
        assert!(app.shelf_reorder.is_none());
    }

    fn online_app(root: &tempfile::TempDir) -> ReaderApp {
        crate::follows::save(
            root.path(),
            &[
                follows::Follow {
                    source_id: "source.a".into(),
                    manga_id: "story".into(),
                    title: "Story".into(),
                    snapshot: String::new(),
                    known: vec!["c1".into()],
                    total: 1,
                    followed_at: Some(7),
                    ..Default::default()
                },
                follows::Follow {
                    source_id: "source.b".into(),
                    manga_id: "web".into(),
                    title: "网络连载".into(),
                    snapshot: String::new(),
                    known: vec!["a".into(), "b".into()],
                    fresh: vec!["c".into()],
                    total: 3,
                    checked_at: Some(20),
                    ..Default::default()
                },
            ],
        )
        .unwrap();
        ReaderApp::new(root.path().into(), Context::default())
    }

    #[test]
    fn online_follows_join_the_shelf_without_duplicating_local_works() {
        let root = tempfile::tempdir().unwrap();
        let mut app = online_app(&root);
        app.books = vec![book("local"), book("linked")];
        app.replace_links(vec![linked("linked", "c1")]);
        let books = app.visible_books();
        let ids: Vec<String> = books.iter().map(|b| b.id.clone()).collect();
        // The local-linked work is not duplicated as an online entry.
        assert!(!ids.iter().any(|id| id.starts_with("online:source.a")));
        let online = books
            .iter()
            .find(|book| book.id == "online:source.b\u{1f}web")
            .expect("the followed work joins the shelf");
        assert_eq!(online.title, "网络连载");
        assert_eq!(online.category, "在线追更");
        assert_eq!(online.page_count, 3);
        assert_eq!(online.added_at, 20, "followed_at falls back to checked_at");
        assert_eq!(online.last_read_at, None);
        assert_eq!(app.online_badge(online), Some(1));
        // The badge rides the local representative of a followed work instead.
        let linked_book = books.iter().find(|book| book.id == "linked").unwrap();
        assert_eq!(app.online_badge(linked_book), None);
        // The category filter can isolate online works.
        app.category = "在线追更".into();
        let ids: Vec<String> = app
            .visible_books()
            .iter()
            .map(|book| book.id.clone())
            .collect();
        assert_eq!(ids, ["online:source.b\u{1f}web"]);
    }

    #[test]
    fn online_entries_participate_in_all_three_sorts() {
        let root = tempfile::tempdir().unwrap();
        let mut app = online_app(&root);
        let mut local = book("local");
        local.last_read_at = Some(5);
        app.books = vec![local];
        let first = |app: &ReaderApp| app.visible_books()[0].id.clone();
        // Recent keeps library order, online entries appended by follow order.
        assert_eq!(first(&app), "local");
        app.shelf_sort = ShelfSort::Added;
        // added_at 20 beats the fixture's 1.
        assert_eq!(first(&app), "online:source.b\u{1f}web");
        app.shelf_sort = ShelfSort::Progress;
        // Unread online entries sort into the not-started group, i.e. last.
        assert_eq!(first(&app), "local");
    }

    #[test]
    fn removing_an_online_entry_unfollows_it() {
        let root = tempfile::tempdir().unwrap();
        let mut app = online_app(&root);
        assert!(app.sources.unfollow("source.b", "web"));
        assert!(!app.sources.unfollow("source.b", "missing"));
        assert_eq!(crate::follows::load(root.path()).unwrap().len(), 1);
        let ids: Vec<String> = app
            .visible_books()
            .iter()
            .map(|book| book.id.clone())
            .collect();
        // The removed work leaves; unrelated follows stay on the shelf.
        assert_eq!(ids, ["online:source.a\u{1f}story"]);
    }

    #[test]
    fn online_progress_uses_the_reading_order_and_chapter_name() {
        let root = tempfile::tempdir().unwrap();
        crate::follows::save(
            root.path(),
            &[follows::Follow {
                source_id: "source.a".into(),
                manga_id: "story".into(),
                title: "Story".into(),
                snapshot: String::new(),
                // The source lists newest first; the stored order does not.
                known: vec!["c3".into(), "c2".into(), "c1".into()],
                reading_order: vec!["c1".into(), "c2".into(), "c3".into()],
                total: 3,
                ..Default::default()
            }],
        )
        .unwrap();
        let mut app = ReaderApp::new(root.path().into(), Context::default());
        app.sources
            .seed_read_at("source.a", "story", "c2", "第 2 话", 10);
        let book = app.visible_books()[0].clone();
        assert_eq!(
            book.current_page, 1,
            "progress is the index in reading order, not in source order"
        );
        assert_eq!(book.page_fraction, 0.);
        assert_eq!(book.last_read_at, Some(10));
        assert_eq!(
            app.online_progress_label(app.online_follow(&book.id).unwrap()),
            "在线 · 读到 第 2 话"
        );
    }

    #[test]
    fn online_progress_falls_back_when_the_reading_order_is_unknown() {
        let root = tempfile::tempdir().unwrap();
        crate::follows::save(
            root.path(),
            &[
                // An older record without the order kept, but already read.
                follows::Follow {
                    source_id: "source.a".into(),
                    manga_id: "old".into(),
                    title: "Old".into(),
                    snapshot: String::new(),
                    known: vec!["c2".into(), "c1".into()],
                    total: 2,
                    ..Default::default()
                },
                // A new record read halfway in its stored order.
                follows::Follow {
                    source_id: "source.a".into(),
                    manga_id: "new".into(),
                    title: "New".into(),
                    snapshot: String::new(),
                    known: vec!["c2".into(), "c1".into()],
                    reading_order: vec!["c1".into(), "c2".into()],
                    total: 2,
                    ..Default::default()
                },
            ],
        )
        .unwrap();
        let mut app = ReaderApp::new(root.path().into(), Context::default());
        app.sources.seed_read_at("source.a", "old", "c1", "第 1 话", 5);
        app.sources.seed_read_at("source.a", "new", "c1", "第 1 话", 5);
        let books = app.visible_books();
        assert_eq!(
            books
                .iter()
                .find(|book| book.id == "online:source.a\u{1f}old")
                .unwrap()
                .page_fraction,
            -1.,
            "an unresolvable order marks progress as unknown"
        );
        assert_eq!(
            books
                .iter()
                .find(|book| book.id == "online:source.a\u{1f}new")
                .unwrap()
                .page_fraction,
            0.
        );
        app.shelf_sort = ShelfSort::Progress;
        let ids: Vec<String> = app
            .visible_books()
            .iter()
            .map(|book| book.id.clone())
            .collect();
        assert_eq!(
            ids,
            ["online:source.a\u{1f}new", "online:source.a\u{1f}old"],
            "unknown progress sorts behind known progress, not ahead"
        );
        // The label still names the chapter even when the order is unknown.
        assert_eq!(
            app.online_progress_label(
                app.online_follow("online:source.a\u{1f}old").unwrap()
            ),
            "在线 · 读到 第 1 话"
        );
    }

    #[test]
    fn tap_zones_respect_mode_and_rtl() {
        let root = tempfile::tempdir().unwrap();
        let mut app = ReaderApp::new(root.path().into(), Context::default());
        let mut target = book("one");
        target.current_page = 5;
        app.books = vec![target.clone()];
        app.open_reader(&target);
        // lr follows the reading direction.
        app.tap_zones = "lr".into();
        app.reader_tap("one", 0.9, 1, 0., 800.);
        assert_eq!(app.page, 6);
        app.reader_tap("one", 0.1, 1, 0., 800.);
        assert_eq!(app.page, 5);
        app.rtl = true;
        app.reader_tap("one", 0.1, 1, 0., 800.);
        assert_eq!(app.page, 6, "rtl left edge advances");
        app.reader_tap("one", 0.9, 1, 0., 800.);
        assert_eq!(app.page, 5, "rtl right edge retreats");
        app.rtl = false;
        // next: both outer zones advance.
        app.tap_zones = "next".into();
        app.reader_tap("one", 0.1, 1, 0., 800.);
        assert_eq!(app.page, 6);
        app.reader_tap("one", 0.9, 1, 0., 800.);
        assert_eq!(app.page, 7);
        // off: outer taps do nothing, the center still toggles controls.
        app.tap_zones = "off".into();
        app.reader_tap("one", 0.1, 1, 0., 800.);
        app.reader_tap("one", 0.9, 1, 0., 800.);
        assert_eq!(app.page, 7);
        let controls = app.controls;
        app.reader_tap("one", 0.5, 1, 0., 800.);
        assert_eq!(app.controls, !controls);
        app.reader_tap("one", 0.5, 1, 0., 800.);
        assert_eq!(app.controls, controls);
    }

    #[test]
    fn chapter_end_card_enters_leaves_and_never_saves_past_the_last_page() {
        let root = tempfile::tempdir().unwrap();
        let mut app = ReaderApp::new(root.path().into(), Context::default());
        let mut one = book("one");
        one.current_page = 19;
        let two = book("two");
        app.books = vec![one.clone(), two];
        app.chapters = vec![link("one", "c1", 1.), link("two", "c2", 2.)];
        app.chapter_indices = app
            .chapters
            .iter()
            .enumerate()
            .map(|(i, link)| (link.book_id.clone(), i))
            .collect();
        app.open_reader(&one);
        assert_eq!(app.page, 19);
        // The first advance past the last page shows the card instead of
        // moving — progress stays on the real last page.
        app.next_page("one", 1, 0., 800.);
        assert!(app.chapter_end);
        assert_eq!(app.page, 19);
        assert_eq!(app.current_book_id(), Some("one"));
        // Back returns to the last page without a card.
        app.next_page("one", -1, 0., 800.);
        assert!(!app.chapter_end);
        assert_eq!(app.page, 19);
        // Forward again, then continue into the next linked chapter.
        app.next_page("one", 1, 0., 800.);
        assert!(app.chapter_end);
        app.next_page("one", 1, 0., 800.);
        assert_eq!(app.current_book_id(), Some("two"));
        assert_eq!(app.page, 0);
        assert!(!app.chapter_end);
        // The last chapter's card stays when there is no next chapter.
        let mut tail = book("two");
        tail.current_page = 19;
        app.open_reader(&tail);
        app.next_page("two", 1, 0., 800.);
        assert!(app.chapter_end);
        app.next_page("two", 1, 0., 800.);
        assert!(app.chapter_end && app.page == 19);
        assert_eq!(app.current_book_id(), Some("two"));
    }

    #[test]
    fn keep_awake_only_fires_on_state_transitions() {
        let root = tempfile::tempdir().unwrap();
        let mut app = ReaderApp::new(root.path().into(), Context::default());
        let target = book("one");
        app.books = vec![target.clone()];
        app.open_reader(&target);
        assert_eq!(app.awake_log, [true]);
        // Re-entering the reader while already awake does not repeat.
        app.open_reader(&target);
        assert_eq!(app.awake_log, [true]);
        app.back(1.0, Rect::from_min_size(Pos2::ZERO, vec2(390., 844.)));
        assert_eq!(app.awake_log, [true, false]);
        app.open_reader(&target);
        assert_eq!(app.awake_log, [true, false, true]);
        eframe::App::on_exit(&mut app);
        assert_eq!(app.awake_log, [true, false, true, false]);
    }

    #[test]
    fn reader_status_formats_time_and_battery() {
        assert_eq!(ReaderApp::status_text("21:34", Some(0.85)), "21:34 · 85%");
        assert_eq!(ReaderApp::status_text("21:34", Some(0.05)), "21:34 · 5%");
        assert_eq!(ReaderApp::status_text("21:34", None), "21:34");
        assert_eq!(ReaderApp::status_text("21:34", Some(1.4)), "21:34 · 100%");
    }

    #[test]
    fn category_tabs_merge_user_and_derived_and_fixed_stay_first() {
        let root = tempfile::tempdir().unwrap();
        let mut app = ReaderApp::new(root.path().into(), Context::default());
        app.user_categories = vec!["热血".into(), "悬疑".into()];
        let mut old = book("old");
        old.category = "旧分类".into();
        let mut member = book("member");
        member.category = "热血".into();
        app.books = vec![old, member];
        let tabs: Vec<String> = app
            .category_tabs()
            .iter()
            .map(|(label, _)| label.clone())
            .collect();
        assert_eq!(
            tabs,
            ["全部", "未分类", "在线追更", "热血", "悬疑", "旧分类", "管理"]
        );
        // 管理 is the only tab without a filter value; fixed names are not
        // manageable entries either.
        assert_eq!(
            app.managed_categories(),
            vec!["热血".to_string(), "悬疑".to_string(), "旧分类".to_string()]
        );
        assert!(app.add_category("全部").is_none());
        assert!(app.add_category("未分类").is_none());
        assert!(app.add_category("在线追更").is_none());
        assert!(app.add_category("热血").is_none());
        assert_eq!(app.add_category(" 日常 ").as_deref(), Some("日常"));
        assert_eq!(app.user_categories.last().unwrap(), "日常");
    }

    #[test]
    fn bookmark_toggle_and_cross_chapter_jump() {
        let root = tempfile::tempdir().unwrap();
        let ctx = Context::default();
        let mut app = ReaderApp::new(root.path().into(), ctx);
        app.books = vec![book("one"), book("two")];
        app.chapters = vec![linked("one", "c1"), linked("two", "c2")];
        app.chapter_indices = app
            .chapters
            .iter()
            .enumerate()
            .map(|(index, link)| (link.book_id.clone(), index))
            .collect();
        app.screen = Screen::Reader("one".into());
        app.page = 3;
        app.bookmark_toggle("one");
        assert!(app.bookmarked("one"));
        app.bookmark_toggle("one");
        assert!(!app.bookmarked("one"));
        app.bookmark_toggle("one");
        // Cross-chapter bookmark: resolves the linked local book.
        app.bookmarks.toggle(crate::bookmarks::Bookmark {
            work: "source.a\u{1f}story".into(),
            chapter: "c2".into(),
            page: 5,
            title: "c2".into(),
            created_at: 1,
        });
        let mark = app
            .bookmarks
            .for_work("source.a\u{1f}story")
            .into_iter()
            .find(|mark| mark.chapter == "c2")
            .unwrap()
            .clone();
        assert_eq!(app.bookmark_book(&mark).unwrap().id, "two");
        app.bookmark_jump(mark, "one", 0., 800.);
        assert!(matches!(&app.screen, Screen::Reader(id) if id == "two"));
        assert_eq!(app.page, 5);
        // Private mode disables bookmarks entirely.
        app.private = true;
        app.screen = Screen::Reader("one".into());
        app.page = 0;
        app.bookmark_toggle("one");
        assert!(!app.bookmarked("one"));
        assert!(app.pending_notice.is_some());
        // An online key without a task reports a friendly error.
        app.error = None;
        app.bookmark_jump(
            crate::bookmarks::Bookmark {
                work: "source.b\u{1f}m2".into(),
                chapter: "c9".into(),
                page: 0,
                title: "c9".into(),
                created_at: 1,
            },
            "one",
            0.,
            800.,
        );
        assert!(app.error.is_some());
    }

    #[test]
    fn startup_clears_staged_share_files() {
        let root = tempfile::tempdir().unwrap();
        let share = root.path().join("share");
        std::fs::create_dir_all(&share).unwrap();
        std::fs::write(share.join("old.png"), b"x").unwrap();
        let _app = ReaderApp::new(root.path().into(), Context::default());
        assert!(!share.exists());
    }
}

