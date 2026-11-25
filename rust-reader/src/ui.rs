use crate::{
    motion::{self, ShelfMotion},
    shelf::{self, Plane},
    sources::{STREAM_PREFIX, StreamChapter},
    worker::{Command, Event, PAGE_NOT_READY, Worker},
};
use egui::*;
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    path::PathBuf,
    sync::atomic::Ordering,
};
use yejian_core::{Book, ChapterLink, ChapterMetadata};

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
struct Flight {
    id: String,
    book_id: String,
    started: f64,
    returning: bool,
    from: Plane,
    to: Plane,
    foreground: Vec<shelf::Volume>,
    /// 起点书所在 lane：foreground 里 lane 更小的是"被遮挡的后景书"，
    /// 重绘时给半透明，让剪影完整但仍在飞行封面之下（P2-4）。
    origin_lane: i32,
    clip: Rect,
}
/// Snap page rect to the pixel grid: shared tile/spread edges coincide (S4).
fn snap_page_rect(painter: &Painter, r: Rect) -> Rect {
    let ppp = painter.ctx().pixels_per_point();
    let s = |v: f32| (v * ppp).round() / ppp;
    Rect::from_min_max(pos2(s(r.left()), s(r.top())), pos2(s(r.right()), s(r.bottom())))
}
fn page_label(book: &Book, page: usize, private: bool) -> String {
    format!("{} / {}{}", page + 1, book.page_count, if private { " · 私密阅读" } else { "" })
}
struct ReaderChrome { // chrome rects shared by live controls and ghosts
    top: Rect, dock: Rect, back: Rect, settings: Rect, chapters: Rect, title_clip: Rect,
    prev: Rect, next: Rect, badge: Rect, slider: Rect, }


struct PageTexture {
    tiles: Vec<TextureHandle>,
    size: Vec2,
    /// Drawn content inside the full image, normalized to 0..1.
    content: Rect,
}
impl PageTexture {
    /// Pixels the layout should measure when white/black margins are cropped.
    fn shown_size(&self, crop: bool) -> Vec2 {
        if crop {
            vec2(
                self.size.x * self.content.width(),
                self.size.y * self.content.height(),
            )
        } else {
            self.size
        }
    }
    /// Paints the page; with `crop` only the content sub-rect is drawn,
    /// stretched to fill `rect`. Tiles still slice vertically.
    fn paint(&self, painter: &Painter, rect: Rect, crop: bool) {
        let content = if crop {
            self.content
        } else {
            Rect::from_min_max(Pos2::ZERO, pos2(1., 1.))
        };
        let rect = snap_page_rect(painter, rect);
        let mut top = 0.;
        for tile in &self.tiles {
            let tile_height = tile.size()[1] as f32 / self.size.y;
            let (tile_top, tile_bottom) = (top, top + tile_height);
            top = tile_bottom;
            let (visible_top, visible_bottom) = (
                tile_top.max(content.top()),
                tile_bottom.min(content.bottom()),
            );
            if visible_bottom <= visible_top {
                continue;
            }
            let uv = Rect::from_min_max(
                pos2(content.left(), (visible_top - tile_top) / tile_height),
                pos2(content.right(), (visible_bottom - tile_top) / tile_height),
            );
            let target = Rect::from_min_size(
                pos2(
                    rect.left(),
                    rect.top()
                        + (visible_top - content.top()) / content.height() * rect.height(),
                ),
                vec2(
                    rect.width(),
                    (visible_bottom - visible_top) / content.height() * rect.height(),
                ),
            );
            if target.intersects(painter.clip_rect()) {
                painter.image(tile.id(), target, uv, Color32::WHITE);
            }
        }
    }
    /// Paints the page as a textured mesh instead of a flat image: each
    /// column lifts by [`motion::flip_depth`] and scales like the cover
    /// paper (`300/(300-depth)`), with finite-difference shading.
    /// `phase` is `|offset|/W`; `mirror` flips the ridge to the `u = 0`
    /// edge for pages whose reveal seam is on the left. When `phase`
    /// is 0 the vertices land on the flat rect, matching `paint`.
    fn paint_flip(&self, painter: &Painter, rect: Rect, crop: bool, phase: f32, mirror: bool) {
        const COLUMNS: u32 = 17;
        let content = if crop {
            self.content
        } else {
            Rect::from_min_max(Pos2::ZERO, pos2(1., 1.))
        };
        let f = crate::params::params().flip.clone();
        let rect = snap_page_rect(painter, rect);
        let mut top = 0.;
        for tile in &self.tiles {
            let tile_height = tile.size()[1] as f32 / self.size.y;
            let (tile_top, tile_bottom) = (top, top + tile_height);
            top = tile_bottom;
            let (visible_top, visible_bottom) = (
                tile_top.max(content.top()),
                tile_bottom.min(content.bottom()),
            );
            if visible_bottom <= visible_top {
                continue;
            }
            let uv = Rect::from_min_max(
                pos2(content.left(), (visible_top - tile_top) / tile_height),
                pos2(content.right(), (visible_bottom - tile_top) / tile_height),
            );
            let target = Rect::from_min_size(
                pos2(
                    rect.left(),
                    rect.top()
                        + (visible_top - content.top()) / content.height() * rect.height(),
                ),
                vec2(
                    rect.width(),
                    (visible_bottom - visible_top) / content.height() * rect.height(),
                ),
            );
            if !target.intersects(painter.clip_rect()) {
                continue;
            }
            let mut mesh = Mesh::with_texture(tile.id());
            mesh.vertices.reserve((COLUMNS * 2) as usize);
            for row in 0..2 {
                let v = row as f32;
                for col in 0..COLUMNS {
                    let u = col as f32 / (COLUMNS - 1) as f32;
                    let um = if mirror { 1. - u } else { u };
                    let depth = motion::flip_depth(um, phase, f.depth_px);
                    let scale = 300. / (300. - depth.min(200.));
                    let flat = target.lerp_inside(vec2(u, v));
                    let pos = target.center() + (flat - target.center()) * scale;
                    let fd = motion::flip_depth((um + 0.03).min(1.), phase, f.depth_px) - depth;
                    let brightness =
                        (1. - fd * f.edge_shade * 2. - depth / 28. * f.edge_shade)
                            .clamp(0.72, 1.06);
                    let channel = (brightness * 255.).round().clamp(0., 255.) as u8;
                    mesh.vertices.push(egui::epaint::Vertex {
                        pos,
                        uv: uv.lerp_inside(vec2(u, v)),
                        color: Color32::from_rgb(channel, channel, channel),
                    });
                }
            }
            for col in 0..COLUMNS - 1 {
                let a = col;
                let b = col + COLUMNS;
                mesh.indices.extend([a, b, a + 1, a + 1, b, b + 1]);
            }
            painter.add(Shape::mesh(mesh));
        }
    }
}

/// What follows the current chapter in the vertical strip.
enum Peek {
    /// No next chapter.
    Missing,
    /// A next chapter exists but its page count is not known yet.
    Loading,
    /// `(segment id, page count, stream metadata when it is an online task)`.
    Ready(String, usize, Option<ChapterMetadata>),
}

/// The pair shown in two-page mode: with `spread_offset` page 0 stands
/// alone and the following spreads start on odd pages.
fn spread_pair(page: usize, spread_offset: bool) -> (usize, Option<usize>) {
    if spread_offset && page == 0 {
        (0, None)
    } else if spread_offset && page % 2 == 0 {
        (page - 1, Some(page))
    } else {
        (page, Some(page + 1))
    }
}

/// A chapter read while its download is still running. Its position lives
/// only in memory until the finished chapter joins the library.
struct StreamBook {
    book: Book,
    task: String,
    metadata: ChapterMetadata,
    done: usize,
    stopped: Option<String>,
    signature: Option<String>,
    /// Page to restore once the page count is known (`usize::MAX`: last).
    resume: Option<usize>,
    waiting: HashSet<usize>,
}

#[derive(Clone, Copy)]
struct ShelfLayout {
    stage: Rect,
    books: Rect,
    info: Rect,
    browse: Rect,
    side_by_side: bool,
    short_landscape: bool,
    /// P0-1 横屏大档（左栏导航 + 非矮视口）：书群放大、扇形放开、群心右移。
    wide: bool,
}

pub struct ReaderApp {
    root: PathBuf,
    worker: Worker,
    books: Vec<Book>,
    chapters: Vec<ChapterLink>,
    chapter_indices: HashMap<String, usize>,
    chapter_dialog: Option<String>,
    /// 详情页内联章节卡的排序偏好：false=正序（从头），true=倒序（最新）。
    detail_desc: bool,
    ready: bool,
    startup_error: Option<String>,
    query: String,
    search: bool,
    category: String,
    category_edit: String,
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
    /// CPU mip tiers per cover (see `shelf::CoverMips`): the oblique shelf
    /// face picks a downscaled level instead of aliasing the full artwork.
    cover_mips: HashMap<String, shelf::CoverMips>,
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
    /// Detail -> reader peel-entry clock: `INFINITY` means armed (starts
    /// on the first reader frame with a decoded page), then the start
    /// timestamp, cleared when the entry finishes.
    reader_entry: Option<f64>,
    /// Reader -> target peel-exit clock: (start, outgoing book id). Armed
    /// by back() with `INFINITY` (starts on the first exit frame), run to
    /// raw=1 in `close.exit_ms`, then cleared. While set, the target
    /// screen paints underneath shrinking spread bands.
    reader_exit: Option<(f64, String)>,
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
    reader_bg: crate::reader_prefs::ReaderBg,
    warm_tint: bool,
    gutter_style: crate::reader_prefs::GutterStyle,
    tap_preset: crate::reader_prefs::TapPreset,
    show_corner_hud: bool,
    page_anim: crate::reader_prefs::PageAnim,
    bookmarks: Vec<usize>,
    reader_globals: crate::reader_prefs::ReaderPrefs,
    reader_prefs: HashMap<String, crate::reader_prefs::ReaderPrefs>,
    dark: bool,
    reduced: bool,
    theme: String,
    font_scale: f32,
    widget_font_scale: f32,
    error: Option<String>,
    notice: Option<(String, f64)>,
    pending_notice: Option<String>,
    confirm_remove: bool,
    import_text: String,
    backup_path: String,
    sources: crate::sources::Sources,
    stream: Option<StreamBook>,
    sync: crate::sync::ProgressSync,
    trackers: crate::trackers::Trackers,
    tracker_anilist_token: String,
    tracker_kitsu_user: String,
    tracker_kitsu_pass: String,
    track_panel: Option<String>,
    track_query: String,
    pub hitboxes: BTreeMap<String, Rect>,
    pub rendered_book_nodes: usize,
    pub frame_is_animating: bool,
    pub rendered_page_nodes: usize,
}

impl ReaderApp {
    pub fn new(root: PathBuf, ctx: Context) -> Self {
        let backup_path = root.join("backup.yejian.zip").to_string_lossy().to_string();
        let reader_prefs = crate::reader_prefs::load(&root).unwrap_or_default();
        let reader_defaults = crate::reader_prefs::ReaderPrefs::default();
        Self {
            worker: Worker::start(root.clone(), ctx.clone()),
            sync: crate::sync::ProgressSync::new(root.clone(), ctx.clone()),
            trackers: crate::trackers::Trackers::new(root.clone(), ctx.clone()),
            tracker_anilist_token: String::new(),
            tracker_kitsu_user: String::new(),
            tracker_kitsu_pass: String::new(),
            track_panel: None,
            track_query: String::new(),
            sources: crate::sources::Sources::new(root.clone(), ctx),
            root,
            stream: None,
            books: vec![],
            chapters: Vec::new(),
            chapter_indices: HashMap::new(),
            chapter_dialog: None,
            detail_desc: false,
            ready: false,
            startup_error: None,
            query: String::new(),
            search: false,
            category: String::new(),
            category_edit: String::new(),
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
            cover_mips: HashMap::new(),
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
            reader_entry: None,
            reader_exit: None,
            zoom_anim: motion::ZoomAnim::default(),
            nav_pill: None,
            last_nav_tab: Tab::Library,
            tab_enter: None,
            focus_book_anim: None,
            progress_tween: None,
            last_thumb_selected: None,
            reader_options_at: None,
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
            reader_bg: reader_defaults.bg,
            warm_tint: reader_defaults.warm_tint,
            gutter_style: reader_defaults.gutter,
            tap_preset: reader_defaults.tap_preset,
            show_corner_hud: reader_defaults.show_corner_hud,
            page_anim: reader_defaults.page_anim,
            bookmarks: reader_defaults.bookmarks.clone(),
            reader_globals: reader_defaults,
            reader_prefs,
            dark: false,
            reduced: false,
            theme: "system".into(),
            font_scale: 1.,
            widget_font_scale: 0.,
            error: None,
            notice: None,
            pending_notice: None,
            confirm_remove: false,
            import_text: String::new(),
            backup_path,
            hitboxes: BTreeMap::new(),
            rendered_book_nodes: 0,
            frame_is_animating: false,
            rendered_page_nodes: 0,
        }
    }
    pub fn import_files(&mut self, paths: Vec<PathBuf>) {
        if !paths.is_empty() {
            self.send(Command::Import(paths));
        }
    }
    /// Simulator snapshots: jump to a screen once the library is readable.
    #[cfg(debug_assertions)]
    pub fn request_snapshot(&mut self, name: String) {
        self.snapshot_request = Some(name);
    }
    #[cfg(debug_assertions)]
    pub fn prepare_source_acceptance(&mut self, mode: &str) -> anyhow::Result<()> {
        self.tab = Tab::Sources;
        self.screen = Screen::Library;
        self.sources.preview_browser(mode)
    }
    #[cfg(debug_assertions)]
    pub fn source_browser_report(&self) -> serde_json::Value {
        self.sources.browser_report()
    }
    #[cfg(debug_assertions)]
    pub fn source_browser_ready(&self) -> bool {
        self.sources.browser_ready("browse")
    }
    #[cfg(debug_assertions)]
    pub fn prepare_installed_source_acceptance(&mut self, id: &str) -> anyhow::Result<()> {
        self.tab = Tab::Sources;
        self.screen = Screen::Library;
        self.sources.preview_installed_source(id)
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
    fn reader_book(&self, id: &str) -> Option<Book> {
        self.books
            .iter()
            .find(|b| b.id == id)
            .or_else(|| self.stream_for(id).map(|s| &s.book).filter(|b| b.id == id))
            .cloned()
    }
    fn reading_metadata(&self, id: &str) -> Option<ChapterMetadata> {
        self.chapter(id)
            .cloned()
            .or_else(|| self.stream_for(id).map(|stream| stream.metadata.clone()))
            .or_else(|| {
                id.strip_prefix(STREAM_PREFIX)
                    .and_then(|task| self.sources.stream_metadata(task))
            })
    }
    fn open_stream(&mut self, chapter: StreamChapter) {
        let id = format!("{STREAM_PREFIX}{}", chapter.id);
        let state = self.sources.stream_state(&chapter.id).unwrap_or_default();
        let wanted = if chapter.from_end {
            usize::MAX
        } else {
            state.page.unwrap_or(0)
        };
        let resume = (state.total == 0 && wanted > 0).then_some(wanted);
        let book = Book {
            id: id.clone(),
            title: format!(
                "{} · {}",
                chapter.metadata.manga_title, chapter.metadata.chapter_title
            ),
            author: String::new(),
            archive_path: PathBuf::new(),
            cover_path: None,
            page_count: state.total.max(1),
            current_page: wanted.min(state.total.saturating_sub(1)),
            page_fraction: 0.,
            updated_at: 0,
            last_read_at: None,
            category: String::new(),
        };
        self.stream = Some(StreamBook {
            book: book.clone(),
            task: chapter.id,
            metadata: chapter.metadata,
            done: state.done,
            stopped: state.stopped,
            signature: state.signature,
            resume,
            waiting: HashSet::new(),
        });
        self.page_slide = motion::PageSlide::default();
        self.open_reader(&book);
    }
    /// Follows the online chapter's download, and hands the reader over to
    /// the library copy (same page) once it has been imported.
    /// The vertical strip's segments in order: the still-loaded previous
    /// chapter (kept for scrolling back), the current one, and the next one
    /// once its page count is known.
    fn strip_segments(&self) -> Vec<(String, usize)> {
        let mut segments = Vec::with_capacity(3);
        if let Some(prev) = &self.vertical_prev {
            segments.push(prev.clone());
        }
        if let Screen::Reader(id) = &self.screen {
            let count = self
                .reader_book(id)
                .map(|book| book.page_count)
                .unwrap_or(0);
            segments.push((id.clone(), count));
        }
        if let Some(next) = &self.vertical_next {
            segments.push(next.clone());
        }
        segments
    }
    /// Resolves a global strip index to `(segment id, local page)`.
    fn locate<'a>(segments: &'a [(String, usize)], global: usize) -> Option<(&'a str, usize)> {
        let mut base = 0;
        for (id, count) in segments {
            if global < base + count {
                return Some((id.as_str(), global - base));
            }
            base += count;
        }
        None
    }
    /// Rendered heights for all loaded strip pages, keyed by global index.
    fn strip_heights(&self, segments: &[(String, usize)], width: f32) -> HashMap<usize, f32> {
        let mut heights = HashMap::new();
        let mut base = 0;
        for (id, count) in segments {
            for ((_, page), image) in self.pages.iter().filter(|((key, _), _)| key == id) {
                let size = image.shown_size(self.crop);
                heights.insert(base + *page, size.y / size.x * width);
            }
            base += count;
        }
        heights
    }
    /// An `(id, page)` key's global strip index, or `usize::MAX` when the
    /// page is not part of the strip any more.
    fn global_page_index(&self, id: &str, page: usize) -> usize {
        let mut base = 0;
        for (seg_id, count) in self.strip_segments() {
            if seg_id == id {
                return if page < count { base + page } else { usize::MAX };
            }
            base += count;
        }
        usize::MAX
    }
    /// Whether an arriving page texture belongs in the reader's working set.
    fn wants_page(&self, id: &str, page: usize) -> bool {
        if !self.vertical {
            return matches!(&self.screen, Screen::Reader(current) if current == id)
                && page.abs_diff(self.page) <= 2;
        }
        let global = self.global_page_index(id, page);
        global != usize::MAX && global.abs_diff(self.vertical_reader.page) <= 3
    }
    /// `(near, essential)` page keys: `near` is the keep window around the
    /// anchor across segments, `essential` the pages the current view may
    /// show, which eviction never removes.
    fn page_keep_sets(&self) -> (HashSet<(String, usize)>, HashSet<(String, usize)>) {
        let mut near = HashSet::new();
        let mut essential = HashSet::new();
        let Screen::Reader(id) = &self.screen else {
            return (near, essential);
        };
        if self.vertical {
            let segments = self.strip_segments();
            let anchor = self.vertical_reader.page;
            for global in anchor.saturating_sub(2)..=anchor + 2 {
                if let Some((seg, local)) = Self::locate(&segments, global) {
                    near.insert((seg.to_string(), local));
                }
            }
            for global in anchor.saturating_sub(1)..=anchor + 1 {
                if let Some((seg, local)) = Self::locate(&segments, global) {
                    essential.insert((seg.to_string(), local));
                }
            }
        } else {
            for page in self.page.saturating_sub(2)..=self.page + 2 {
                near.insert((id.clone(), page));
            }
            essential.insert((id.clone(), self.page));
            if self.spread {
                let (base, second) = spread_pair(self.page, self.spread_offset);
                essential.insert((id.clone(), base));
                if let Some(second) = second {
                    essential.insert((id.clone(), second));
                }
            }
        }
        (near, essential)
    }
    /// Distance of a page key from the reader's anchor; used for eviction.
    fn page_distance(&self, id: &str, page: usize) -> usize {
        if self.vertical {
            self.global_page_index(id, page)
                .abs_diff(self.vertical_reader.page)
        } else {
            page.abs_diff(self.page)
        }
    }
    /// Whether a chapter exists before/after `id` (library or online).
    fn has_neighbor(&self, id: &str, delta: i32) -> bool {
        if let Some(metadata) = self.reading_metadata(id)
            && let Some(key) = self.sources.adjacent_online_key(&metadata, delta)
        {
            let local = self.chapters.iter().any(|link| {
                link.metadata.source_id == metadata.source_id
                    && link.metadata.manga_id == metadata.manga_id
                    && link.metadata.chapter_id == key
                    && self.books.iter().any(|book| book.id == link.book_id)
            });
            return local || self.sources.has_adjacent_online(&metadata, delta);
        }
        self.adjacent_chapter(id, delta).is_some()
    }
    /// Looks up the chapter that follows `id`, queuing its online download
    /// when `enqueue` is set (the anchor is within the last pages). The
    /// segment becomes `Ready` only once its page count is known.
    fn peek_next(&mut self, id: &str, enqueue: bool) -> Peek {
        if let Some(metadata) = self.reading_metadata(id)
            && let Some(key) = self.sources.adjacent_online_key(&metadata, 1)
        {
            let local = self
                .chapters
                .iter()
                .find(|link| {
                    link.metadata.source_id == metadata.source_id
                        && link.metadata.manga_id == metadata.manga_id
                        && link.metadata.chapter_id == key
                })
                .and_then(|link| self.books.iter().find(|book| book.id == link.book_id))
                .cloned();
            if let Some(book) = local {
                return Peek::Ready(book.id, book.page_count, None);
            }
            if !self.sources.has_adjacent_online(&metadata, 1) {
                return Peek::Missing;
            }
            if enqueue
                && let Some((task, task_meta)) = self.sources.prepare_next(&metadata)
                && let Some(state) = self.sources.stream_state(&task)
                && state.total > 0
            {
                return Peek::Ready(
                    format!("{STREAM_PREFIX}{task}"),
                    state.total,
                    Some(task_meta),
                );
            }
            return Peek::Loading;
        }
        match self.adjacent_chapter(id, 1) {
            Some(book) => Peek::Ready(book.id, book.page_count, None),
            None => Peek::Missing,
        }
    }
    /// Stream bookkeeping for a `stream:` strip segment.
    fn stream_segment_book(&self, seg_id: &str, metadata: &ChapterMetadata) -> Option<StreamBook> {
        let task = seg_id.strip_prefix(STREAM_PREFIX)?;
        let state = self.sources.stream_state(task).unwrap_or_default();
        Some(StreamBook {
            book: Book {
                id: seg_id.to_string(),
                title: format!("{} · {}", metadata.manga_title, metadata.chapter_title),
                author: String::new(),
                archive_path: PathBuf::new(),
                cover_path: None,
                page_count: state.total.max(1),
                current_page: 0,
                page_fraction: 0.,
                updated_at: 0,
                last_read_at: None,
                category: String::new(),
            },
            task: task.to_string(),
            metadata: metadata.clone(),
            done: state.done,
            stopped: state.stopped,
            signature: state.signature,
            resume: None,
            waiting: HashSet::new(),
        })
    }
    /// The library book a downloaded task's metadata resolved to.
    fn imported_book(&self, metadata: &ChapterMetadata) -> Option<Book> {
        self.chapters
            .iter()
            .find(|link| {
                link.metadata.source_id == metadata.source_id
                    && link.metadata.manga_id == metadata.manga_id
                    && link.metadata.chapter_id == metadata.chapter_id
            })
            .and_then(|link| self.books.iter().find(|book| book.id == link.book_id))
            .cloned()
    }
    /// Moves every page-scoped map entry from a `stream:` id to its imported
    /// library book id, keeping decoded textures across the handover.
    fn remap_page_keys(&mut self, from: &str, to: &str) {
        for key in self
            .pages
            .keys()
            .filter(|(id, _)| id == from)
            .cloned()
            .collect::<Vec<_>>()
        {
            if let Some(texture) = self.pages.remove(&key) {
                self.pages.insert((to.to_string(), key.1), texture);
            }
        }
        for key in self
            .page_errors
            .keys()
            .filter(|(id, _)| id == from)
            .cloned()
            .collect::<Vec<_>>()
        {
            if let Some(error) = self.page_errors.remove(&key) {
                self.page_errors.insert((to.to_string(), key.1), error);
            }
        }
        for key in self
            .page_pending
            .iter()
            .filter(|(id, _)| id == from)
            .cloned()
            .collect::<Vec<_>>()
        {
            self.page_pending.remove(&key);
            self.page_pending.insert((to.to_string(), key.1));
        }
        for key in self
            .prefetch_blocked
            .iter()
            .filter(|(id, _)| id == from)
            .cloned()
            .collect::<Vec<_>>()
        {
            self.prefetch_blocked.remove(&key);
            self.prefetch_blocked.insert((to.to_string(), key.1));
        }
    }
    /// The vertical anchor entered the following segment: that chapter
    /// becomes the reader's book, keeping textures and the exact scroll
    /// offset. Returns the new current `(id, page count)`.
    fn cross_vertical(&mut self, old_id: &str, old_count: usize) -> (String, usize) {
        let prev_count = self.vertical_prev.as_ref().map(|(_, c)| *c).unwrap_or(0);
        let Some((next_id, next_count)) = self.vertical_next.take() else {
            return (old_id.to_string(), old_count);
        };
        let global = self.vertical_reader.page;
        let local = global - prev_count - old_count;
        if let Some(task) = old_id.strip_prefix(STREAM_PREFIX) {
            if !self.private {
                self.sources
                    .set_stream_page(task, old_count.saturating_sub(1));
            }
        } else {
            self.record_position(old_id, old_count.saturating_sub(1), 0.);
        }
        self.vertical_prev = Some((old_id.to_string(), old_count));
        self.stream = self.next_stream.take();
        self.screen = Screen::Reader(next_id.clone());
        self.page = local;
        self.vertical_reader.page = global - prev_count;
        self.prefetch_blocked.clear();
        self.record_position(&next_id, local, self.vertical_reader.fraction);
        (next_id, next_count)
    }
    /// Saves the strip anchor's position for whichever chapter holds it.
    fn record_anchor(&mut self, id: &str, local: usize) {
        let fraction = self.vertical_reader.fraction;
        if id.starts_with(STREAM_PREFIX)
            && !self.stream.as_ref().is_some_and(|s| s.book.id == id)
        {
            if !self.private {
                self.sources.set_stream_page(&id[STREAM_PREFIX.len()..], local);
            }
        } else {
            self.record_position(id, local, fraction);
        }
    }
    fn update_stream(&mut self) {
        if let Some(stream) = self.stream.as_ref() {
            let id = stream.book.id.clone();
            if !matches!(&self.screen, Screen::Reader(current) if current == &id) {
                self.stream = None;
            } else if let Some(book) = self.imported_book(&stream.metadata) {
                self.remap_page_keys(&id, &book.id);
                self.stream = None;
                if self.vertical {
                    // In the strip only the id behind the segment changes;
                    // textures, offset and the strip itself stay put.
                    self.screen = Screen::Reader(book.id);
                } else {
                    let mut book = book;
                    book.current_page = self.page.min(book.page_count.saturating_sub(1));
                    book.page_fraction = 0.;
                    self.open_reader(&book);
                }
            } else {
                let task = stream.task.clone();
                if let Some(state) = self.sources.stream_state(&task) {
                    let Some(stream) = self.stream.as_mut() else {
                        return;
                    };
                    if state.total > 0 {
                        stream.book.page_count = state.total;
                    }
                    if state.signature != stream.signature {
                        stream.signature = state.signature.clone();
                        stream.waiting.clear();
                        self.pages.retain(|(key, _), _| key != &id);
                        self.page_errors.retain(|(key, _), _| key != &id);
                        self.page_pending.retain(|(key, _)| key != &id);
                        self.prefetch_blocked.retain(|(key, _)| key != &id);
                        self.page_generation += 1;
                        self.worker
                            .generation
                            .store(self.page_generation, Ordering::Relaxed);
                    }
                    let Some(stream) = self.stream.as_mut() else {
                        return;
                    };
                    if state.done != stream.done || state.stopped != stream.stopped {
                        stream.done = state.done;
                        stream.stopped = state.stopped;
                        stream.waiting.clear();
                    }
                    if let Some(wanted) = stream.resume.filter(|_| state.total > 0) {
                        stream.resume = None;
                        stream.book.current_page = wanted.min(state.total - 1);
                        stream.book.page_fraction = 0.;
                        let book = stream.book.clone();
                        self.open_reader(&book);
                        return;
                    }
                }
            }
        }
        // The following segment's stream hands over to its library copy the
        // same way once its task is imported.
        if let Some((stream_id, book)) = self.next_stream.as_ref().and_then(|next| {
            self.imported_book(&next.metadata)
                .map(|book| (next.book.id.clone(), book))
        }) {
            self.remap_page_keys(&stream_id, &book.id);
            if self
                .vertical_next
                .as_ref()
                .is_some_and(|(seg, _)| seg == &stream_id)
            {
                self.vertical_next = Some((book.id, book.page_count));
            }
            self.next_stream = None;
        }
        if let Some(next) = self.next_stream.as_mut()
            && let Some(state) = self.sources.stream_state(&next.task)
        {
            if state.total > 0 {
                next.book.page_count = state.total;
                if let Some(seg) = self.vertical_next.as_mut()
                    && seg.0 == next.book.id
                {
                    seg.1 = state.total;
                }
            }
            if state.signature != next.signature {
                next.signature = state.signature.clone();
                next.waiting.clear();
                let id = next.book.id.clone();
                self.pages.retain(|(key, _), _| key != &id);
                self.page_errors.retain(|(key, _), _| key != &id);
                self.page_pending.retain(|(key, _)| key != &id);
                self.prefetch_blocked.retain(|(key, _)| key != &id);
            }
            if state.done != next.done || state.stopped != next.stopped {
                next.done = state.done;
                next.stopped = state.stopped;
                next.waiting.clear();
            }
        }
        // A finished previous chapter swaps its stream id for the book id so
        // its still-loaded pages keep resolving.
        if let Some((prev_id, prev_count)) = self.vertical_prev.clone()
            && let Some(task) = prev_id.strip_prefix(STREAM_PREFIX)
            && let Some(metadata) = self.sources.stream_metadata(task)
            && let Some(book) = self.imported_book(&metadata)
        {
            self.remap_page_keys(&prev_id, &book.id);
            self.vertical_prev = Some((book.id, prev_count.min(book.page_count)));
        }
    }
    /// Opens the previous/next chapter in the source's order when that list
    /// is known (library copy if present, online otherwise), else the
    /// neighbouring library chapter.
    fn open_neighbor(&mut self, id: &str, delta: i32) -> bool {
        let start = |book: &mut Book| {
            book.current_page = if delta < 0 {
                book.page_count.saturating_sub(1)
            } else {
                0
            };
            book.page_fraction = 0.;
        };
        if let Some(metadata) = self.reading_metadata(id)
            && let Some(key) = self.sources.adjacent_online_key(&metadata, delta)
        {
            let local = self
                .chapters
                .iter()
                .find(|link| {
                    link.metadata.source_id == metadata.source_id
                        && link.metadata.manga_id == metadata.manga_id
                        && link.metadata.chapter_id == key
                })
                .and_then(|link| self.books.iter().find(|book| book.id == link.book_id))
                .cloned();
            if let Some(mut book) = local {
                start(&mut book);
                self.open_reader(&book);
                return true;
            }
            return self.sources.open_adjacent_online(&metadata, delta);
        }
        if let Some(mut adjacent) = self.adjacent_chapter(id, delta) {
            start(&mut adjacent);
            self.open_reader(&adjacent);
            return true;
        }
        false
    }
    fn stream_for(&self, id: &str) -> Option<&StreamBook> {
        self.stream
            .as_ref()
            .filter(|stream| stream.book.id == id)
            .or(self
                .next_stream
                .as_ref()
                .filter(|stream| stream.book.id == id))
    }
    fn stream_for_mut(&mut self, id: &str) -> Option<&mut StreamBook> {
        if self.stream.as_mut().is_some_and(|s| s.book.id == id) {
            return self.stream.as_mut();
        }
        self.next_stream.as_mut().filter(|s| s.book.id == id)
    }
    fn stream_waiting_text(&self, id: &str, page: usize) -> Option<String> {
        let stream = self.stream_for(id)?;
        if !stream.waiting.contains(&page) {
            return None;
        }
        Some(if let Some(reason) = &stream.stopped {
            format!("第 {} 页还没下载
{reason}", page + 1)
        } else if stream.book.page_count > 1 || stream.done > 0 {
            format!(
                "正在下载第 {} 页…（{}/{}）",
                page + 1,
                stream.done,
                stream.book.page_count
            )
        } else {
            "正在获取章节页面…".into()
        })
    }
    fn visible_books(&self) -> Vec<Book> {
        let query = self.query.trim().to_lowercase();
        let mut representatives: Vec<&Book> = Vec::new();
        let mut works = HashMap::<(&str, &str), usize>::new();
        for book in &self.books {
            let chapter = self.chapter(&book.id);
            if (!self.category.is_empty() && book.category != self.category)
                || (!query.is_empty()
                    && !book.title.to_lowercase().contains(&query)
                    && !book.author.to_lowercase().contains(&query)
                    && !chapter.is_some_and(|m| m.manga_title.to_lowercase().contains(&query)))
            {
                continue;
            }
            if let Some(chapter) = chapter {
                let key = (chapter.source_id.as_str(), chapter.manga_id.as_str());
                let slot = *works.entry(key).or_insert(representatives.len());
                if let Some(current) = representatives.get_mut(slot) {
                    // An unread work starts at its first available chapter, even
                    // when the last chapter was the most recent download.
                    if book.last_read_at > current.last_read_at
                        || (book.last_read_at == current.last_read_at
                            && self.chapter_indices[&book.id] < self.chapter_indices[&current.id])
                    {
                        *current = book;
                    }
                    continue;
                }
            }
            representatives.push(book);
        }
        representatives
            .into_iter()
            .map(|book| {
                let mut entry = book.clone();
                if let Some(chapter) = self.chapter(&book.id) {
                    entry.title.clone_from(&chapter.manga_title);
                }
                entry
            })
            .collect()
    }
    fn chapter(&self, id: &str) -> Option<&ChapterMetadata> {
        self.chapter_indices
            .get(id)
            .map(|&index| &self.chapters[index].metadata)
    }
    fn same_series(&self, a: &str, b: &str) -> bool {
        match (self.chapter(a), self.chapter(b)) {
            (Some(a), Some(b)) => a.source_id == b.source_id && a.manga_id == b.manga_id,
            _ => a == b,
        }
    }
    fn replace_links(&mut self, links: Vec<ChapterLink>) {
        self.sources.set_library_chapters(&links);
        self.chapter_indices = links
            .iter()
            .enumerate()
            .map(|(i, link)| (link.book_id.clone(), i))
            .collect();
        self.chapters = links;
    }
    fn chapters_for(&self, id: &str) -> Vec<Book> {
        self.chapters
            .iter()
            .filter(|link| self.same_series(id, &link.book_id))
            .filter_map(|link| {
                self.books
                    .iter()
                    .find(|book| book.id == link.book_id)
                    .cloned()
            })
            .collect()
    }
    fn adjacent_chapter(&self, id: &str, delta: i32) -> Option<Book> {
        let chapters = self.chapters_for(id);
        let index = chapters.iter().position(|book| book.id == id)? as i32 + delta;
        if index < 0 {
            return None;
        }
        chapters.get(index as usize).cloned()
    }
    fn cover_id(&self, id: &str) -> String {
        if self.chapter(id).is_none() {
            return id.to_string();
        }
        self.chapters
            .iter()
            .find(|link| self.same_series(id, &link.book_id))
            .map(|link| link.book_id.clone())
            .unwrap_or_else(|| id.to_string())
    }
    fn reading_title(&self, book: &Book) -> String {
        self.chapter(&book.id)
            .map(|m| format!("{} · {}", m.manga_title, m.chapter_title))
            .unwrap_or_else(|| book.title.clone())
    }
    /// 历史行副标里的相对时间（last_read_at 是 Unix 毫秒）。只读展示，不写库。
    fn reading_ago(at: i64) -> String {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(at);
        let mins = (now - at).max(0) / 60_000;
        if mins < 1 {
            "刚刚".into()
        } else if mins < 60 {
            format!("{mins} 分钟前")
        } else if mins < 60 * 24 {
            format!("{} 小时前", mins / 60)
        } else if mins < 60 * 24 * 30 {
            format!("{} 天前", mins / (60 * 24))
        } else {
            format!("{} 个月前", mins / (60 * 24 * 30))
        }
    }
    fn selected_books(&self) -> Vec<String> {
        self.books
            .iter()
            .filter(|book| {
                self.selected
                    .iter()
                    .any(|id| self.same_series(id, &book.id))
            })
            .map(|book| book.id.clone())
            .collect()
    }
    fn replace_books(&mut self, books: Vec<Book>) {
        let previous = self.visible_books();
        let selected = if previous.is_empty() {
            None
        } else {
            Some(
                previous
                    [(self.motion.focus.round() as i32).rem_euclid(previous.len() as i32) as usize]
                    .id
                    .clone(),
            )
        };
        self.books = books;
        let visible = self.visible_books();
        if let Some(id) = selected {
            if let Some(i) = visible.iter().position(|b| self.same_series(&b.id, &id)) {
                self.motion.focus = i as f32;
            }
        } else if let Some((i, _)) = visible
            .iter()
            .enumerate()
            .filter(|(_, b)| b.last_read_at.is_some())
            .max_by_key(|(_, b)| b.last_read_at)
        {
            self.motion.focus = i as f32;
        }
        self.selected
            .retain(|id| self.books.iter().any(|b| &b.id == id));
    }
    fn receive(&mut self, ctx: &Context) {
        while let Ok(event) = self.worker.rx.try_recv() {
            match event {
                Event::Ready(books, settings, links) => {
                    self.replace_links(links);
                    self.replace_books(books);
                    self.ready = true;
                    for (key, value) in settings {
                        match key.as_str() {
                            "theme" => self.theme = value,
                            "reduce-motion" => self.reduced = value == "true",
                            "reading-direction" => {
                                self.reader_globals.rtl = value == "rtl";
                                self.rtl = self.reader_globals.rtl;
                            }
                            "private" => self.private = value == "true",
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
                            "follow-interval" => {
                                let minutes: u64 = value
                                    .parse()
                                    .unwrap_or(crate::sources::DEFAULT_FOLLOW_INTERVAL_MIN);
                                self.sources.set_follow_interval(minutes);
                            }
                            _ => {}
                        }
                    }
                }
                Event::Library(books, links) => {
                    self.replace_links(links);
                    self.replace_books(books);
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
                                    self.cover_mips.insert(
                                        id.clone(),
                                        shelf::CoverMips::upload(ctx, &format!("cover-{id}"), &tile),
                                    );
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
    fn record_page(&mut self, id: &str, page: usize) {
        let fraction = if self.vertical && self.vertical_reader.page == page {
            self.vertical_reader.fraction
        } else {
            0.
        };
        self.record_position(id, page, fraction);
    }
    fn record_position(&mut self, id: &str, page: usize, fraction: f32) {
        if self
            .saved_position
            .as_ref()
            .is_some_and(|(key, p, f)| key == id && *p == page && (*f - fraction).abs() < 0.0001)
        {
            return;
        }
        self.saved_position = Some((id.to_string(), page, fraction));
        // Auto-sync: a non-private reader that reached the last page (or a
        // chapter seam) pushes the remote progress forward. `chapter_done`
        // itself enforces the auto flag and the monotone floor.
        if !self.private {
            let last = self
                .reader_book(id)
                .map(|book| book.page_count.saturating_sub(1))
                .unwrap_or(usize::MAX);
            if page >= last {
                if let Some(metadata) = self.reading_metadata(id) {
                    let key = crate::trackers::binding_key(
                        Some((&metadata.source_id, &metadata.manga_id)),
                        None,
                    );
                    let chapter = metadata
                        .chapter_number
                        .filter(|n| n.is_finite() && *n >= 1.)
                        .map(|n| n.floor() as i64)
                        .or_else(|| crate::trackers::chapter_number(&metadata.chapter_title));
                    if let Some(chapter) = chapter {
                        self.trackers.chapter_done(&key, chapter);
                    }
                } else {
                    let key = crate::trackers::binding_key(None, Some(id));
                    self.trackers.chapter_done(&key, 0);
                }
            }
        }
        if let Some(stream) = self.stream.as_mut().filter(|stream| stream.book.id == id) {
            stream.book.current_page = page;
            stream.book.page_fraction = fraction;
            if stream.resume.is_none() && !self.private {
                let task = stream.task.clone();
                self.sources.set_stream_page(&task, page);
            }
            return;
        }
        self.send(Command::Progress {
            id: id.into(),
            page,
            fraction,
            private: self.private,
        });
        if !self.private {
            if let Some(b) = self.books.iter_mut().find(|b| b.id == id) {
                b.current_page = page;
                b.page_fraction = fraction;
                b.last_read_at = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .ok()
                    .map(|d| d.as_millis() as i64);
            }
        }
    }
    fn flush_position(&mut self) {
        if let Screen::Reader(id) = self.screen.clone() {
            if self.pages.contains_key(&(id.clone(), self.page)) {
                self.record_page(&id, self.page);
            }
        }
    }
    fn ask_cover(&mut self, id: &str) {
        self.visible_covers.insert(id.into());
        self.cover_clock += 1;
        self.cover_usage.insert(id.into(), self.cover_clock);
        if !self.covers.contains_key(id)
            && !self.cover_failed.contains(id)
            && self.cover_pending.insert(id.into())
        {
            self.send(Command::Image {
                id: id.into(),
                page: 0,
                cover: true,
                generation: 0,
            });
        }
    }
    fn trim_covers(&mut self) {
        // covers holds each base; cover_mips holds the extra downscaled
        // tiers (its level 0 is a separate upload, counted only in mips).
        let mip_bytes = |mips: &shelf::CoverMips| {
            let base = mips.base().size();
            mips.bytes().saturating_sub(base[0] * base[1] * 4)
        };
        let mut bytes: usize = self
            .covers
            .values()
            .map(|texture| texture.size()[0] * texture.size()[1] * 4)
            .sum::<usize>()
            + self.cover_mips.values().map(&mip_bytes).sum::<usize>();
        while self.covers.len() > 48 || bytes > 64 * 1024 * 1024 {
            let candidate = self.covers.keys()
                .filter(|id| !self.visible_covers.contains(*id)
                    && !self.flight.as_ref().is_some_and(|flight| &flight.id == *id))
                .min_by_key(|id| self.cover_usage.get(*id).copied().unwrap_or(0))
                .cloned();
            let Some(id) = candidate else { break };
            if let Some(texture) = self.covers.remove(&id) {
                bytes -= texture.size()[0] * texture.size()[1] * 4;
            }
            if let Some(mips) = self.cover_mips.remove(&id) {
                bytes -= mip_bytes(&mips);
            }
            self.cover_usage.remove(&id);
        }
    }
    fn ask_page(&mut self, id: &str, page: usize) {
        let key = (id.to_string(), page);
        if page != self.page && self.prefetch_blocked.contains(&key) {
            return;
        }
        if self
            .stream_for(id)
            .is_some_and(|stream| stream.waiting.contains(&page))
        {
            return;
        }
        if !self.pages.contains_key(&key)
            && !self.page_errors.contains_key(&key)
            && self.page_pending.insert(key)
        {
            self.send(Command::Image {
                id: id.into(),
                page,
                cover: false,
                generation: self.page_generation,
            });
        }
    }
    fn trim_pages(&mut self) {
        let (near, essential) = self.page_keep_sets();
        self.pages
            .retain(|key, _| near.contains(key) || essential.contains(key));
        while self
            .pages
            .values()
            .map(|page| page.size.x as u64 * page.size.y as u64 * 4)
            .sum::<u64>()
            > 128 * 1024 * 1024
        {
            let Some(key) = self
                .pages
                .keys()
                .filter(|key| !essential.contains(*key))
                .max_by_key(|(id, page)| self.page_distance(id, *page))
                .cloned()
            else {
                break;
            };
            self.pages.remove(&key);
            self.prefetch_blocked.insert(key);
        }
    }
    /// The effective reader settings of one book: a saved per-work entry
    /// wins, then the source's viewer hint, then the global defaults.
    fn apply_reader_prefs(&mut self, book: &Book) {
        let metadata = self.reading_metadata(&book.id);
        let key = crate::reader_prefs::key_for(metadata.as_ref(), &book.id);
        let mut prefs = self.reader_prefs.get(&key).cloned().unwrap_or_else(|| {
            let mut prefs = self.reader_globals.clone();
            if let Some(viewer) = metadata.and_then(|metadata| {
                self.sources
                    .viewer_hint(&metadata.source_id, &metadata.manga_id)
            }) {
                match viewer {
                    yejian_sources::Viewer::RightToLeft => {
                        prefs.mode = crate::reader_prefs::Mode::Paged;
                        prefs.rtl = true;
                    }
                    yejian_sources::Viewer::LeftToRight => {
                        prefs.mode = crate::reader_prefs::Mode::Paged;
                        prefs.rtl = false;
                    }
                    yejian_sources::Viewer::Vertical | yejian_sources::Viewer::Webtoon => {
                        prefs.mode = crate::reader_prefs::Mode::Vertical;
                    }
                    yejian_sources::Viewer::Unknown => {}
                }
            }
            prefs
        });
        prefs.brightness = prefs.brightness();
        self.vertical = prefs.mode == crate::reader_prefs::Mode::Vertical;
        self.spread = prefs.mode == crate::reader_prefs::Mode::Spread;
        self.rtl = prefs.rtl;
        self.crop = prefs.crop;
        self.fit_width = prefs.fit_width;
        self.brightness = prefs.brightness;
        self.spread_offset = prefs.spread_offset;
        self.reader_bg = prefs.bg;
        self.warm_tint = prefs.warm_tint;
        self.gutter_style = prefs.gutter;
        self.tap_preset = prefs.tap_preset;
        self.show_corner_hud = prefs.show_corner_hud;
        self.page_anim = prefs.page_anim;
        self.bookmarks = prefs.bookmarks;
    }
    fn reader_canvas_color(&self) -> Color32 {
        let [r, g, b] = self.reader_bg.rgb();
        Color32::from_rgb(r, g, b)
    }
    fn current_prefs(&self) -> crate::reader_prefs::ReaderPrefs {
        crate::reader_prefs::ReaderPrefs {
            mode: if self.vertical {
                crate::reader_prefs::Mode::Vertical
            } else if self.spread {
                crate::reader_prefs::Mode::Spread
            } else {
                crate::reader_prefs::Mode::Paged
            },
            rtl: self.rtl,
            crop: self.crop,
            fit_width: self.fit_width,
            brightness: self.brightness,
            spread_offset: self.spread_offset,
            bg: self.reader_bg,
            warm_tint: self.warm_tint,
            gutter: self.gutter_style,
            tap_preset: self.tap_preset,
            show_corner_hud: self.show_corner_hud,
            page_anim: self.page_anim,
            bookmarks: self.bookmarks.clone(),
        }
    }
    /// Changes made inside the reader always land on the current work.
    fn save_reader_prefs(&mut self) {
        let Screen::Reader(id) = self.screen.clone() else {
            return;
        };
        let key = crate::reader_prefs::key_for(self.reading_metadata(&id).as_ref(), &id);
        self.reader_prefs.insert(key, self.current_prefs());
        if let Err(error) = crate::reader_prefs::save(&self.root, &self.reader_prefs) {
            self.error = Some(format!("阅读设置保存失败：{error:#}"));
        }
    }
    /// The "default for every work" action copies the live settings into the
    /// global keys works without their own entry resolve from.
    fn save_reader_globals(&mut self) {
        self.reader_globals = self.current_prefs();
        self.setting(
            "reading-mode",
            match self.reader_globals.mode {
                crate::reader_prefs::Mode::Vertical => "vertical",
                crate::reader_prefs::Mode::Spread => "spread",
                crate::reader_prefs::Mode::Paged => "paged",
            }
            .into(),
        );
        self.setting(
            "reading-direction",
            if self.rtl { "rtl" } else { "ltr" }.into(),
        );
        self.setting("reader-crop", self.crop.to_string());
        self.setting("reader-fit", self.fit_width.to_string());
        self.setting("reader-brightness", self.brightness.to_string());
        self.setting("reader-spread-offset", self.spread_offset.to_string());
    }
    fn open_reader(&mut self, book: &Book) {
        self.flush_position();
        self.apply_reader_prefs(book);
        if !matches!(self.screen, Screen::Reader(_)) {
            self.reader_return = self.screen.clone();
            // Arm the peel-in entry; it starts on the first reader frame
            // so a chapter-to-chapter handoff inside the reader skips it.
            // Vertical and reduced-motion readers get no peel to show.
            self.reader_entry = (!self.reduced && !self.vertical).then_some(f64::INFINITY);
        } else {
            self.reader_entry = None;
        }
        // Re-entering the reader (e.g. from a button exposed mid-exit)
        // cancels a closing book outright.
        self.reader_exit = None;
        self.page = book.current_page.min(book.page_count.saturating_sub(1));
        self.screen = Screen::Reader(book.id.clone());
        self.vertical_reader.reset(self.page, book.page_fraction);
        self.vertical_prev = None;
        self.vertical_next = None;
        self.next_stream = None;
        self.saved_position = None;
        self.chapter_dialog = None;
        self.reader_options = false;
        self.pending_reader_tap = None;
        self.pages
            .retain(|(id, page), _| id == &book.id && page.abs_diff(self.page) <= 2);
        self.zoom = 1.;
        self.pan = Vec2::ZERO;
        self.controls = true;
        self.controls_anim = None;
        self.page_slide = motion::PageSlide::default();
        self.page_slide.page = self.page;
        self.zoom_anim.cancel();
        self.page_generation += 1;
        self.worker
            .generation
            .store(self.page_generation, Ordering::Relaxed);
        self.page_pending.clear();
        self.prefetch_blocked.clear();
        self.page_errors.clear();
        self.ask_page(&book.id, self.page);
        if self.pages.contains_key(&(book.id.clone(), self.page)) {
            self.record_page(&book.id, self.page);
        }
    }
    fn next_page(&mut self, id: &str, delta: i32, now: f64, viewport_w: f32) {
        self.pending_reader_tap = None;
        let old_page = self.page;
        let Some(book) = self.reader_book(id) else {
            return;
        };
        let target = self.page as i64 + delta as i64;
        let streaming_unknown = self
            .stream
            .as_ref()
            .is_some_and(|stream| stream.book.id == id && stream.done == 0 && book.page_count <= 1);
        if (target < 0 || target >= book.page_count as i64) && !streaming_unknown {
            if self.open_neighbor(id, if target < 0 { -1 } else { 1 }) {
                return;
            }
        }
        let page = (self.page as i64 + delta as i64)
            .clamp(0, book.page_count.saturating_sub(1) as i64) as usize;
        if page == self.page {
            return;
        }
        self.page = page;
        let base = self.vertical_prev.as_ref().map(|(_, c)| *c).unwrap_or(0);
        self.vertical_reader.reset(base + page, 0.);
        self.pan = Vec2::ZERO;
        self.page_generation += 1;
        self.worker
            .generation
            .store(self.page_generation, Ordering::Relaxed);
        self.page_pending.clear();
        self.prefetch_blocked.clear();
        self.pages
            .retain(|(key, p), _| key == id && p.abs_diff(page) <= 2);
        self.page_errors
            .retain(|(key, p), _| key == id && p.abs_diff(page) <= 2);
        self.ask_page(id, page);
        if self.pages.contains_key(&(id.to_string(), page)) {
            self.record_page(id, page);
        }
        // Tap, button and keyboard turns ease the incoming page in from a
        // small strip offset (instant under reduced motion); a committed
        // finger drag re-arms the turn with the real drag offset instead.
        if matches!(&self.screen, Screen::Reader(current) if current == id)
            && !self.vertical
            && self.zoom <= 1.
        {
            let dir_sign = if (delta > 0) ^ self.rtl { 1. } else { -1. };
            let start_offset =
                dir_sign * viewport_w * crate::params::params().flip.tap_strip_pct;
            self.page_slide.page = page;
            self.slide_turn_gap = start_offset;
            self.page_slide
                .animate_turn(old_page, start_offset, now, self.reduced);
        }
    }
    /// True while any bounded animation still needs repaints. Every `Option`
    /// it reads is cleared once the motion finishes, so an idle frame leaves
    /// `frame_is_animating` false and the repaint delay long.
    fn animating(&self, now: f64, entrance: f32) -> bool {
        let entry_active = self.reader_entry.is_some_and(|t| {
            t.is_infinite()
                || now - t < crate::params::params().flip.entry_ms.max(0.) as f64 / 1000.
        });
        let reader_motion = matches!(self.screen, Screen::Reader(_))
            && (self.page_slide.active(now)
                || self.zoom_anim.active(now)
                || self.controls_anim.is_some_and(|t| !t.done(now))
                || entry_active);
        self.motion.active()
            || entrance < 1.
            || self.flight.is_some()
            || self.reader_exit.is_some()
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
    fn back(&mut self, now: f64, area: Rect) {
        match self.screen.clone() {
            Screen::Reader(id) => {
                self.flush_position();
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
                self.reader_options = false;
                self.pending_reader_tap = None;
                self.reader_entry = None;
                // The slide/zoom tweens are left running: the overlay reads
                // them live so a back mid-turn hands off with no snap.
                // R2: a flight still playing under the reader would keep
                // drawing its cover after the exit bands open — drop it.
                self.flight = None;
                // R5: vertical/reduced cut straight to the target.
                // 裁决：screen 立即切到 reader_return，exit 是纯叠加层（
                // screen_name 报目标名）——两段连打与既有断言都要求这样。
                self.reader_exit = if self.reduced || self.vertical {
                    None
                } else {
                    Some((f64::INFINITY, id))
                };
                self.send(Command::Refresh);
            }
            Screen::Detail(id) => {
                let books = self.visible_books();
                self.shelf_viewport = area;
                let shifted = area.translate(vec2(0., self.shelf_shift));
                let layout = self.shelf_layout(shifted);
                let shelf_area = self.shelf_paint_area(shifted);
                if let Some(index) = books.iter().position(|b| self.same_series(&b.id, &id)) {
                    self.motion.select(index as f32, now, true);
                }
                let volumes = shelf::geometry_wide(
                    shelf_area,
                    books.len(),
                    self.motion.focus,
                    1.,
                    layout.wide,
                );
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
                            (now - (1. - progress) * motion::close_travel_seconds(), out.from)
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
                            origin_lane: volume.lane,
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
                palette.accent.gamma_multiply(0.78)
            } else if response.hovered() {
                palette.accent.gamma_multiply(0.88)
            } else {
                palette.accent
            }
        } else if pressed {
            palette.accent_soft
        } else if response.hovered() {
            palette.accent_soft
        } else {
            palette.soft
        };
        // 饱满胶囊（短边全圆），主按钮加轻落影浮起（S2b）。
        let radius = rect.height() / 2.;
        if primary && !response.is_pointer_button_down_on() {
            crate::design::soft_shadow(ui.painter(), rect, radius, 0.5, self.dark);
        }
        ui.painter().rect_filled(rect, radius, fill);
        if !primary {
            ui.painter()
                .rect_stroke(rect, radius, Stroke::new(1., palette.line), StrokeKind::Inside);
        }
        let text_color = if primary {
            palette.on_accent
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
    fn icon(&mut self, ui: &mut Ui, id: &str, rect: Rect, kind: &str, label: &str) -> bool {
        self.hitboxes.insert(id.into(), rect);
        let response = ui.interact(rect, Id::unique(id), Sense::click());
        response.widget_info(|| WidgetInfo::labeled(Role::Button, true, label));
        let palette = crate::design::palette(self.dark);
        let is_reader = matches!(self.screen, Screen::Reader(_));
        let active = match id {
            "nav-library" => self.tab == Tab::Library,
            "nav-search" => self.tab == Tab::Sources,
            "nav-history" => self.tab == Tab::History,
            "nav-settings" => self.tab == Tab::Settings,
            _ => false,
        };
        let color = if is_reader {
            Color32::from_gray(232)
        } else if active {
            palette.accent
        } else {
            palette.muted
        };
        let visual = crate::design::press_rect(
            rect,
            response.is_pointer_button_down_on() && !self.reduced,
        );
        let p = ui.painter();
        if !id.starts_with("nav-") && !is_reader {
            // 圆形/胶囊图标钮：surface 底 + 细描边 + 轻落影（S2b 柔和圆钮）。
            let chip_radius = visual.height() / 2.;
            if !(response.is_pointer_button_down_on() || response.hovered()) {
                crate::design::soft_shadow(p, visual, chip_radius, 0.4, self.dark);
            }
            p.rect_filled(
                visual,
                chip_radius,
                if response.is_pointer_button_down_on() || response.hovered() {
                    palette.accent_soft
                } else {
                    palette.surface
                },
            );
            p.rect_stroke(
                visual,
                chip_radius,
                Stroke::new(1., palette.line),
                StrokeKind::Inside,
            );
        }
        if !is_reader {
            crate::design::draw_icon(p, visual.center(), kind, color, active);
        }
        response.clicked()
    }
    fn shelf_area(&self, area: Rect) -> Rect {
        self.shelf_layout(area).books
    }
    /// 书群实际绘制的区域：宽屏要扣掉底部展签带（+24pt 间距），
    /// 否则横屏书脚压字。与 library_ui 里 shelf_area 的计算同源。
    fn shelf_paint_area(&self, area: Rect) -> Rect {
        let layout = self.shelf_layout(area);
        if layout.side_by_side {
            // 书群可绘制区底部必须在展签顶再让 24pt。
            // 展签顶 = stage.bottom - band - 16 ⇒ 等价于扣 band + 40。
            Rect::from_min_max(
                pos2(layout.books.left(), layout.books.top() + 12.),
                pos2(layout.books.right(), layout.info.top() - 24.),
            )
        } else {
            layout.books
        }
    }
    fn shelf_layout(&self, area: Rect) -> ShelfLayout {
        // iPad 是整屏应用：宽屏时不再留桌面式两侧边距，也不套 1200pt 上限。
        let side_by_side = area.width() >= 660.;
        let width = if side_by_side {
            area.width()
        } else {
            (area.width() - 32.).min(1200.)
        };
        let short_landscape = side_by_side && self.shelf_viewport.height() < 600.;
        // 只有横屏大档走"放大+右移"：竖屏（底栏）与矮视口（手机横屏）
        // 一律维持原布局（P0-1 只点名 ipad-11 / ipad-pro-12.9 两档）。
        let wide = side_by_side
            && Self::side_navigation(self.shelf_viewport)
            && !short_landscape;
        let stacked_header = self.font_scale > 1.4 && area.width() < 520.;
        // 宽屏头部压缩：书架是主体，不该让桌面式大标题区占掉一大块。
        let top = if side_by_side {
            132. + 6. * (self.font_scale - 1.)
        } else {
            144.
                + if stacked_header {
                    84.
                } else {
                    14. * (self.font_scale - 1.)
                }
        };
        let tall_extra = if side_by_side {
            ((self.shelf_viewport.height() - 900.) * 0.30).clamp(0., 100.)
        } else {
            0.
        };
        // 藏书一览网格已下线（2026-11-23 用户决定）：立体书架是主体，
        // 原本留给网格的高度全部并入 stage。
        let browse_extra = if side_by_side {
            236. + tall_extra * 0.5
        } else {
            170.
        } + 28. * (self.font_scale - 1.);
        // 书架是页面主体：宽屏（iPad）时舞台直接吃掉整屏高度。
        // 用 shelf_viewport 而不是 area.bottom()：area 可能是可滚动内容区。
        // 竖屏底部标签栏占 TAB_BAR_HEIGHT：舞台/展签让出栏高，不被压住。
        let viewport = if Self::side_navigation(self.shelf_viewport) {
            self.shelf_viewport
        } else {
            Rect::from_min_max(
                self.shelf_viewport.min,
                self.shelf_viewport.max - vec2(0., Self::TAB_BAR_HEIGHT),
            )
        };
        let height = if side_by_side {
            // 再让出空间给底部导航/安全区，避免展签按钮被底栏吃掉。
            (viewport.height() - top - 104.).clamp(220., 4000.)
        } else if short_landscape {
            224. + (self.font_scale - 1.) * 16. + browse_extra + 28.
        } else {
            384. + (self.font_scale - 1.) * 112. + browse_extra + 28.
        };
        // 宽屏：舞台严格贴住内容视口，避免出现可滚动的溢出（iPad 上不该滚动）。
        let stage = if side_by_side {
            Rect::from_min_size(
                pos2(viewport.left(), viewport.top() + top),
                vec2(viewport.width(), (viewport.height() - top - 8.).max(220.)),
            )
        } else {
            // 分屏/竖屏窄档与宽档统一成无边框满幅舞台：横向上不再把书
            // 圈进 16pt 圆角卡片（P1-2），纵向仍按内容需要的高度，可滚动。
            Rect::from_min_size(
                pos2(viewport.left(), area.top() + top),
                vec2(viewport.width(), height),
            )
        };
        let books = if side_by_side {
            // 右侧信息栏曾经占掉近一半宽度；书架是主体，把它压到 22%。
            Rect::from_min_size(
                stage.min + vec2(12., 12.),
                vec2(width - 24., height - 24.),
            )
        } else {
            Rect::from_min_size(stage.min + vec2(8., 30.), vec2(width - 16., 212.))
        };
        let info = if side_by_side {
            // 右侧信息栏下线；改成底部居中的「展签」：书名 + 进度 + 两个按钮。
            // 高度随舞台自适应，短视口（手机横屏 390pt）也不会被挤出屏幕。
            let info_w = (width * 0.52).clamp(300., 620.);
            let band = (height * 0.30).clamp(110., 200.);
            // 展签中心轴跟着书群右移（geometry 里 +6%），保持同轴。
            let axis =
                stage.center().x + if wide { width * 0.06 } else { 0. };
            Rect::from_min_max(
                pos2(axis - info_w / 2., stage.bottom() - band - 16.),
                pos2(axis + info_w / 2., stage.bottom() - 16.),
            )
        } else {
            Rect::from_min_max(
                pos2(stage.left() + 20., books.bottom() + 8.),
                stage.max - vec2(20., 20.),
            )
        };
        let browse = Rect::from_min_size(
            pos2(stage.left(), stage.bottom() + 28.),
            vec2(
                width,
                if side_by_side {
                    236. + tall_extra * 0.5
                } else {
                    170.
                } + 28. * (self.font_scale - 1.),
            ),
        );
        ShelfLayout {
            stage,
            books,
            info,
            browse,
            side_by_side,
            short_landscape,
            wide,
        }
    }
    fn detail_cover_rect(&self, area: Rect) -> Rect {
        if Self::detail_side_by_side(area) {
            let height = (area.height() - 86.).clamp(60., 290.);
            return Rect::from_center_size(
                pos2(area.left() + area.width() * 0.25, area.center().y + 24.),
                vec2(height * 118. / 166., height),
            );
        }
        if Self::detail_wide(area) {
            // iPad landscape: a large cover on the left, the reading column
            // beside it, both centred in the page.
            let height = (area.height() * 0.62).clamp(320., 620.);
            let width = height * 118. / 166.;
            let top = area.top() + (area.height() - height).max(0.) * 0.46;
            let left = area.center().x - (width + 432.) / 2.;
            return Rect::from_min_size(pos2(left, top), vec2(width, height));
        }
        if area.width() >= 700. {
            // iPad portrait and Split View: a generous centred cover with the
            // title block underneath.
            // Tall iPads can carry a notably larger cover without crowding the
            // title block underneath.
            let width = (area.height() * 0.25).clamp(250., 320.);
            let height = width * 166. / 118.;
            let block = height + 220.;
            let top = 84. + (area.height() - 84. - 24. - block).max(0.) * 0.40;
            return Rect::from_min_size(
                pos2(area.center().x - width / 2., area.top() + top),
                vec2(width, height),
            );
        }
        let width = 164.;
        let top = if area.height() < 640. { 66. } else { 105. };
        Rect::from_min_size(
            pos2(area.center().x - width / 2., area.top() + top),
            vec2(width, width * 166. / 118.),
        )
    }
    fn detail_side_by_side(area: Rect) -> bool {
        area.height() < 600. && area.width() >= 480. && area.width() > area.height()
    }
    /// iPad-sized detail page with a separate column beside the cover.
    fn detail_wide(area: Rect) -> bool {
        area.width() >= 1100. && area.height() >= 620.
    }
    /// A readable column: full width on phones, centred and capped on iPad.
    fn page_column(area: Rect, top: f32, bottom: f32) -> Rect {
        let width = (area.width() - 48.).min(760.);
        Rect::from_min_max(
            pos2(area.center().x - width / 2., area.top() + top),
            pos2(area.center().x + width / 2., area.bottom() - bottom),
        )
    }
    pub fn draw(&mut self, ui: &mut Ui) {
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
        let (background, ink, _) = self.colors();
        ui.painter().rect_filled(full, 0., background);
        let tablet = Self::side_navigation(full);
        let rail_width = if full.width() >= 1400. { 176. } else { 96. };
        let content = if tablet {
            Rect::from_min_max(full.min + vec2(rail_width, 0.), full.max)
        } else {
            // 竖屏底栏盖住内容区底部；书架舞台在 shelf_layout 里单独让位，
            // 其余页沿用与旧浮标胶囊相同的滚动覆盖范围。
            full
        };
        let system_back = yejian_platform::take_back_requested()
            || ctx.input(|i| i.key_pressed(Key::BrowserBack));
        if system_back || ctx.input(|i| i.key_pressed(Key::Escape)) {
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
                    motion::close_travel_seconds()
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
                        ui.scope(|ui| {
                            ui.set_opacity(tab_alpha);
                            match self.tab {
                                Tab::Library => {
                                    self.library_viewport(ui, content, now, entrance)
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
                    motion::close_travel_seconds()
                } else {
                    motion::OPEN
                };
                let t = ((now - flight.started) / duration).clamp(0., 1.) as f32;
                let progress = if flight.returning { 1. - t } else { t };
                // 摊开分工：斜扫 banded copy 揭示封面期间，飞行 mesh 隐藏。
                // 否则 mesh 会盖住整副 banded 副本，斜扫只能露出左侧一条
                // 位移残影——spec §7 的"未揭示区=底色"断言也无处可采样。
                // forward 时 spread>0 即隐藏（banded 副本在卡牌处补位）；
                // returning 时 spread<1 即隐藏，反向扫完由 mesh 接管。
                let spread_p = motion::spread_progress(progress);
                let show_mesh = if flight.returning {
                    spread_p >= 0.999
                } else {
                    spread_p <= 0.001
                };
                if show_mesh && let Some(texture) = self
                    .cover_mips
                    .get(&flight.id)
                    .map(shelf::CoverMips::base)
                    .or_else(|| self.covers.get(&flight.id))
                {
                    shelf::paint_flight(
                        ui.painter(),
                        flight.from,
                        flight.to,
                        progress,
                        texture,
                        self.dark,
                    );
                    if progress < 0.12 {
                        let clip = ui.painter().with_clip_rect(flight.clip);
                        let base_alpha =
                            1. - motion::cubic(progress / 0.12, 0.42, 0., 0.58, 1.);
                        let books = self.visible_books();
                        for volume in &flight.foreground {
                            if let Some(book) = books.get(volume.index) {
                                let cover = self.cover_id(&book.id);
                                let mut mask = clip.clone();
                                // P2-4：被抽书挡住的后景书（lane 更小）以半透明
                                // 重绘——剪影保持完整不被飞行 mesh 切碎，又不会
                                // 盖住飞出来的封面；前景书照旧全透明淡出。
                                mask.multiply_opacity(
                                    base_alpha * if volume.lane < flight.origin_lane { 0.6 } else { 1. },
                                );
                                shelf::paint_volume(
                                    &mask,
                                    volume,
                                    &book.title,
                                    shelf::cover_pick(
                                        self.cover_mips.get(&cover),
                                        self.covers.get(&cover),
                                        volume.cover,
                                        shelf::shelf_face(volume.cover),
                                        ui.ctx().pixels_per_point(),
                                    ),
                                    self.dark,
                                    false,
                                    self.selected.contains(&book.id),
                                );
                            }
                        }
                    }
                }
                if t >= 1. {
                    self.flight = None;
                }
            }
            // The outgoing reader paints last: at p=1 its bands cover the
            // whole screen, so the close-out starts from the exact static
            // reader frame and peels away to reveal what is underneath.
            self.reader_exit_overlay(ui, full, now);
        }
        if let Some((text, at)) = &self.notice {
            let age = (now - *at) as f32;
            if age < 5. {
                // Compact auto-sized capsule: 0.18s slide/fade in, 0.22s fade
                // out before the 5s expiry. Repaint only during those windows
                // — never for the ~4.6s idle middle.
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
                    let font = FontId::proportional(13.5);
                    let galley = ui.painter().layout_no_wrap(
                        text.clone(),
                        font.clone(),
                        self.colors().0,
                    );
                    let width = (galley.size().x + 40.).min(full.width() - 32.);
                    let rect = Rect::from_center_size(
                        pos2(full.center().x, full.bottom() - 100. + 14. * (1. - enter)),
                        vec2(width, 40.),
                    );
                    let mut toast = ui.painter().clone();
                    toast.multiply_opacity(alpha);
                    crate::design::soft_shadow(&toast, rect, 20., 1., self.dark);
                    toast.rect_filled(rect, 20., ink);
                    toast.text(
                        rect.center(),
                        Align2::CENTER_CENTER,
                        text,
                        font,
                        self.colors().0,
                    );
                }
                if !self.reduced && (age < 0.18 || age > 5. - 0.22) {
                    ctx.request_repaint();
                }
            }
        }
        self.dialogs(&ctx);
        self.trim_covers();
        self.frame_is_animating = self.animating(now, entrance);
        if self.frame_is_animating {
            ctx.request_repaint();
        }
    }
    fn navigation(&mut self, ui: &mut Ui, full: Rect, tablet: bool, now: f64) {
        let palette = crate::design::palette(self.dark);
        let tabs = [
            (Tab::Library, "书架", "library"),
            (Tab::Sources, "书源", "search"),
            (Tab::History, "历史", "history"),
            (Tab::Settings, "设置", "settings"),
        ];
        let expanded = tablet && full.width() >= 1400.;
        // 竖屏底部标签栏：可点区只占安全区内 54pt，底图向下延伸盖住
        // home-indicator 区（高度 54 + inset，不写死，避免安全区双重扣减）。
        let bottom_inset = if tablet {
            0.
        } else {
            yejian_platform::safe_insets()
                .map(|insets| insets.bottom as f32 / ui.ctx().pixels_per_point())
                .unwrap_or(0.)
        };
        let rail = if tablet {
            Rect::from_min_size(
                full.min,
                vec2(if expanded { 176. } else { 96. }, full.height()),
            )
        } else {
            Rect::from_min_max(
                pos2(full.left(), full.bottom() - Self::TAB_BAR_HEIGHT),
                pos2(full.right(), full.bottom() + bottom_inset),
            )
        };
        // 毛玻璃浮层：双层柔和落影 + 近不透明暖白 + 顶部微高光细边，
        // 顶栏/书群可以从栏底微微透出（S2b：translucent_bar 首次挂接）。
        crate::design::translucent_bar(ui.painter(), rail, palette, self.dark, 0);
        if tablet {
            ui.painter().line_segment(
                [rail.right_top(), rail.right_bottom()],
                Stroke::new(1., palette.line),
            );
            ui.painter().text(
                pos2(rail.center().x, rail.top() + 38.),
                Align2::CENTER_CENTER,
                "页间",
                crate::typography::display(23.),
                palette.accent,
            );
        }
        let mut slots = [Rect::NOTHING; 4];
        for (i, _) in tabs.iter().enumerate() {
            slots[i] = if tablet {
                Rect::from_min_size(
                    pos2(
                        rail.left() + 10.,
                        rail.top()
                            + if full.height() < 600. {
                                80. + i as f32 * ((full.height() - 150.) / 3.).min(72.)
                            } else {
                                112. + i as f32 * 84.
                            },
                    ),
                    vec2(rail.width() - 20., 64.),
                )
            } else {
                // 4 个等宽标签；可点区 54pt 全部待在安全区内。
                Rect::from_min_size(
                    pos2(rail.left() + i as f32 * rail.width() / 4., rail.top()),
                    vec2(rail.width() / 4., Self::TAB_BAR_HEIGHT),
                )
            };
        }
        let tab_index = tabs
            .iter()
            .position(|(tab, _, _)| *tab == self.tab)
            .unwrap_or(0);
        if self.tab != self.last_nav_tab {
            let from = tabs
                .iter()
                .position(|(tab, _, _)| *tab == self.last_nav_tab)
                .unwrap_or(tab_index);
            self.nav_pill = Some(motion::TimedTween::new(
                now,
                if self.reduced { 0. } else { 0.20 },
                from as f32,
                tab_index as f32,
            ));
            self.tab_enter = (!self.reduced).then_some(now);
            self.last_nav_tab = self.tab;
        }
        let pill_pos = self
            .nav_pill
            .map(|t| t.value(now))
            .unwrap_or(tab_index as f32);
        if self.nav_pill.is_some_and(|t| t.done(now)) {
            self.nav_pill = None;
        }
        let pill = Self::nav_pill_rect(&slots, pill_pos);
        self.hitboxes.insert("nav-pill".into(), pill);
        // 侧栏与底栏激活态统一为同一种几何：同暖琥珀软底、同内边距、同
        // 「胶囊」圆角（短边全圆），不再一处圆角矩形一处胶囊（P1-4）；
        // 药丸加一层极轻的落影，浮在毛玻璃栏面上（S2b）。
        crate::design::soft_shadow(ui.painter(), pill, pill.height() / 2., 0.35, self.dark);
        ui.painter()
            .rect_filled(pill, pill.height() / 2., palette.accent_soft);
        for (i, (tab, label, kind)) in tabs.into_iter().enumerate() {
            let rect = slots[i];
            // The whole item is the tap target: the glyph alone is only
            // 32x29 pt, below the 44 pt Apple asks for.
            let row_clicked = ui
                .interact(rect, Id::unique(("nav-row", i)), Sense::click())
                .clicked();
            let icon_rect = if expanded {
                Rect::from_center_size(pos2(rect.left() + 28., rect.center().y), vec2(32., 29.))
            } else if tablet {
                Rect::from_center_size(rect.center() - vec2(0., 9.), vec2(32., 29.))
            } else {
                // 底部标签栏：26×26 图标压在 54pt 可点区上半部。
                Rect::from_center_size(rect.center() - vec2(0., 7.), vec2(26., 26.))
            };
            let clicked = self.icon(ui, &format!("nav-{kind}"), icon_rect, kind, label);
            self.hitboxes.insert(format!("nav-{kind}"), rect);
            let fresh = if tab == Tab::Sources { self.sources.fresh_follows() } else { 0 };
            if fresh > 0 {
                let badge = Rect::from_center_size(icon_rect.right_top() + vec2(2., 2.), vec2(20., 18.));
                ui.painter().rect_filled(badge, 9., Color32::from_rgb(214, 69, 65));
                ui.painter().text(
                    badge.center(),
                    Align2::CENTER_CENTER,
                    fresh.min(99).to_string(),
                    FontId::proportional(11.),
                    Color32::WHITE,
                );
            }
            let label_pos = if expanded {
                pos2(rect.left() + 55., rect.center().y)
            } else if tablet {
                rect.center() + vec2(0., 8.)
            } else {
                rect.center() + vec2(0., 11.)
            };
            let label_color = if tab == self.tab {
                palette.accent
            } else {
                palette.muted
            };
            let label_font = FontId::proportional(if expanded { 16. } else { 12. });
            // 激活标签半粗感：字体没有真粗体时以 0.7pt 偏移重绘一遍。
            if tab == self.tab {
                ui.painter().text(
                    label_pos + vec2(0.7, 0.),
                    if expanded {
                        Align2::LEFT_CENTER
                    } else {
                        Align2::CENTER_TOP
                    },
                    label,
                    label_font.clone(),
                    label_color,
                );
            }
            ui.painter().text(
                label_pos,
                if expanded {
                    Align2::LEFT_CENTER
                } else {
                    Align2::CENTER_TOP
                },
                label,
                label_font,
                label_color,
            );
            let text_clicked = ui
                .interact(
                    Rect::from_min_max(pos2(rect.left(), rect.center().y + 5.), rect.max),
                    Id::unique(("nav-text", i)),
                    Sense::click(),
                )
                .clicked();
            if clicked || row_clicked || text_clicked {
                self.tab = tab;
                self.screen = Screen::Library;
                self.flight = None;
            }
        }
    }
    /// 竖屏底部标签栏在安全区内的高度；底图再向下加底部安全区。
    const TAB_BAR_HEIGHT: f32 = 54.;
    /// 选中标签 pill 相对标签槽的水平/垂直内边距（pt）。320pt 分屏
    /// 下槽宽 80pt，5pt 内边距让 pill 视觉上贴住栏边，故水平用 8pt。
    const NAV_PILL_INSET: egui::Vec2 = vec2(8., 5.);
    /// 选中标签高亮 pill 的矩形：`pill_pos` 在槽下标间插值。
    fn nav_pill_rect(slots: &[Rect; 4], pill_pos: f32) -> Rect {
        let i0 = pill_pos.floor().clamp(0., 3.) as usize;
        let i1 = (i0 + 1).min(3);
        let frac = (pill_pos - i0 as f32).clamp(0., 1.);
        let r0 = slots[i0].shrink2(Self::NAV_PILL_INSET);
        let r1 = slots[i1].shrink2(Self::NAV_PILL_INSET);
        Rect::from_min_max(
            r0.min + (r1.min - r0.min) * frac,
            r0.max + (r1.max - r0.max) * frac,
        )
    }
    /// 只有横屏才用左侧竖栏——宽 1024 的 iPad 竖屏此前被误判成桌面布局。
    fn side_navigation(area: Rect) -> bool {
        area.width() > area.height() && (area.width() >= 1000. || area.height() < 600.)
    }
    fn library_viewport(&mut self, ui: &mut Ui, area: Rect, now: f64, entrance: f32) {
        self.shelf_viewport = area;
        // 空态、加载与启动错误都不画书架，内容只有一屏高。仍走滚动容器
        // 会用虚拟总高做居中基准，把「清除搜索」按钮压出视口（hitbox 照
        // 常登记但 ui.interact 被裁剪，点击落空）——回归
        // 「clear search did not restore the shelf」的根因。
        if self.startup_error.is_some() || !self.ready || self.visible_books().is_empty() {
            self.shelf_shift = 0.;
            self.library_ui(ui, area, now, entrance);
            return;
        }
        let height = self.shelf_layout(area).browse.bottom() - area.top() + 24.;
        let bottom = if Self::side_navigation(ui.ctx().content_rect()) {
            16.
        } else {
            Self::TAB_BAR_HEIGHT + 16.
        };
        if area.height() >= height + bottom {
            self.shelf_shift = 0.;
            self.library_ui(ui, area, now, entrance);
            return;
        }
        let rect = Rect::from_min_max(area.min + vec2(0., 70.), area.max - vec2(0., bottom));
        ui.scope_builder(UiBuilder::new().max_rect(rect), |ui| {
            ScrollArea::vertical()
                .id_salt("library-scroll")
                .show(ui, |ui| {
                    let size = vec2(area.width(), height);
                    let (inner, _) = ui.allocate_exact_size(size - vec2(0., 70.), Sense::hover());
                    self.shelf_shift = inner.top() - 70. - area.top();
                    self.library_ui(
                        ui,
                        Rect::from_min_size(inner.min - vec2(0., 70.), size),
                        now,
                        entrance,
                    );
                });
        });
    }
    fn library_ui(&mut self, ui: &mut Ui, area: Rect, now: f64, entrance: f32) {
        let (_, ink, muted) = self.colors();
        if self.search {
            let rect =
                Rect::from_min_size(area.min + vec2(24., 86.), vec2(area.width() - 48., 44.));
            self.hitboxes.insert("search-field".into(), rect);
            if ui
                .put(
                    rect,
                    TextEdit::singleline(&mut self.query).hint_text("搜索书名或作者"),
                )
                .changed()
            {
                self.motion.focus = 0.;
            }
        } else {
            self.label(
                ui.painter(),
                area.min + vec2(24., 97.),
                &format!("我的收藏 · {} 本", self.shelf_book_count()),
                14.,
                muted,
            );
            let stacked = self.font_scale > 1.4 && area.width() < 520.;
            let category_width = (80. * self.font_scale + 26.).min(area.width() - 48.);
            let category_rect = Rect::from_min_size(
                if stacked {
                    area.min + vec2(24., 142.)
                } else {
                    pos2(area.right() - 24. - category_width, area.top() + 82.)
                },
                vec2(category_width, (24. * self.font_scale + 20.).max(44.)),
            );
            if self.button(
                ui,
                "categories",
                category_rect,
                &if self.category.is_empty() {
                    "全部分类".into()
                } else {
                    self.category.clone()
                },
                false,
            ) {
                let mut categories: Vec<_> = self
                    .books
                    .iter()
                    .map(|b| b.category.clone())
                    .filter(|s| !s.is_empty())
                    .collect();
                categories.sort();
                categories.dedup();
                let next = categories
                    .iter()
                    .position(|x| x == &self.category)
                    .map(|i| i + 1)
                    .unwrap_or(0);
                self.category = categories.get(next).cloned().unwrap_or_default();
                self.motion.focus = 0.;
            }
        }
        if let Some(error) = self.startup_error.clone() {
            self.label(
                ui.painter(),
                area.min + vec2(24., 170.),
                "书库暂时无法打开",
                21.,
                ink,
            );
            ui.put(
                Rect::from_min_size(area.min + vec2(24., 220.), vec2(area.width() - 48., 140.)),
                Label::new(error).wrap(),
            );
            if self.button(
                ui,
                "retry-startup",
                Rect::from_min_size(area.min + vec2(24., 375.), vec2(136., 44.)),
                "重新打开",
                true,
            ) {
                self.worker = Worker::start(self.root.clone(), ui.ctx().clone());
                self.startup_error = None;
            }
            return;
        }
        if !self.ready {
            self.label(
                ui.painter(),
                area.min + vec2(24., 180.),
                "正在打开书库…",
                16.,
                muted,
            );
            return;
        }
        let books = self.visible_books();
        if books.len() <= 1 {
            self.motion.select(0., now, true);
        }
        if books.is_empty() {
            // P2-1：空状态块在页头与底栏之间整体纵向居中，而不是挂在 32% 处，
            // 底部不再剩 2/3 死白。
            let bottom_gap = if Self::side_navigation(area) {
                56.
            } else {
                Self::TAB_BAR_HEIGHT + 16.
            };
            let empty = Rect::from_min_max(
                pos2(area.left() + 32., area.top() + 96.),
                pos2(area.right() - 32., area.bottom() - bottom_gap),
            );
            ui.scope_builder(UiBuilder::new().max_rect(empty), |ui| {
                // 块高约 24·1.4fs + 12 + 13·1.4fs + 24 + 44 ≈ 52fs + 80；
                // 居中 = (可用高 - 块高) / 2，留少许偏上更自然。
                let block = 52. * self.font_scale + 80.;
                ui.add_space(((empty.height() - block) * 0.5 - 8.).max(0.));
                ui.add(
                    Label::new(
                        RichText::new(if self.query.is_empty() {
                            "把喜欢的故事放进来"
                        } else {
                            "没有找到这本书"
                        })
                        .size(24. * self.font_scale)
                        .color(ink),
                    )
                    .wrap(),
                );
                ui.add_space(12.);
                ui.add(
                    Label::new(
                        RichText::new(if self.query.is_empty() {
                            "导入漫画文件，开始第一段阅读。"
                        } else {
                            "换一个书名，或清除搜索。"
                        })
                        .size(13. * self.font_scale)
                        .color(muted),
                    )
                    .wrap(),
                );
                ui.add_space(24.);
                let (rect, _) = ui.allocate_exact_size(vec2(144., 44.), Sense::hover());
                if self.query.is_empty() {
                    if self.button(ui, "empty-import", rect, "导入漫画", true) {
                        self.pick_import();
                    }
                } else if self.button(ui, "clear-search", rect, "清除搜索", true) {
                    self.query.clear();
                    self.search = false;
                }
            });
            return;
        }
        let layout = self.shelf_layout(area);
        // 宽屏：把底部展签让出来，让扇形在"上方可用区域"里垂直居中，
        // 避免书全都压在屏幕底部（2026-11-23 用 analyze-layout.ps1 量出来的问题）。
        let shelf_area = self.shelf_paint_area(area);
        // S2b：窄档不再垫大灰黄舞台色块——立体书群带各自接触阴影，
        // 直接立在统一的暖白纸面上；分屏不再出现割裂的色带（旧
        // stage_color 226,224,213/35,38,34 硬编码已移除）。
        // 右侧栏与竖分隔线已下线：书架占满，信息改到底部展签。
        let volumes =
            shelf::geometry_wide(shelf_area, books.len(), self.motion.focus, entrance, layout.wide);
        self.rendered_book_nodes = volumes.len();
        let mut keep = HashSet::new();
        for volume in &volumes {
            if (volume.lane as f32 - self.motion.focus).abs() <= 3. {
                let cover = self.cover_id(&books[volume.index].id);
                keep.insert(cover.clone());
                self.ask_cover(&cover);
            }
        }
        if self.covers.len() > 40 {
            self.covers.retain(|id, _| keep.contains(id));
            self.cover_mips.retain(|id, _| keep.contains(id));
        }
        // P0-1：横屏大档书放大后焦点书探出 shelf_area 上缘（抬升余量
        // 0.60→0.32 换实高），clip 放宽到视口顶让它完整画出来——
        // 顶部留白只有页头标题/分页器，不与书群列相交。
        let clip_top = if layout.wide {
            self.shelf_viewport.top()
        } else {
            shelf_area.top()
        };
        let p = ui.painter().with_clip_rect(Rect::from_min_max(
            pos2(shelf_area.left(), clip_top),
            shelf_area.max,
        ));
        shelf::paint_floor(&p, &volumes, self.dark);
        for volume in &volumes {
            let book = &books[volume.index];
            let is_central = volume.lane == self.motion.focus.round() as i32;
            let cover = self.cover_id(&book.id);
            let hidden = self.flight.as_ref().is_some_and(|f| f.id == cover) && is_central;
            shelf::paint_volume(
                &p,
                volume,
                &book.title,
                shelf::cover_pick(
                    self.cover_mips.get(&cover),
                    self.covers.get(&cover),
                    volume.cover,
                    shelf::shelf_face(volume.cover),
                    ui.ctx().pixels_per_point(),
                ),
                self.dark,
                hidden,
                self.selected.contains(&book.id),
            );
            if is_central {
                self.hitboxes.insert("shelf-center".into(), volume.bounds());
            }
        }
        // The row is an endless shelf, so spines must dissolve into the page
        // instead of ending on a hard clip line.
        {
            // 舞台色块下线后，左右渐隐带统一溶解进纸面底色（S2b）。
            let background = self.colors().0;
            let clear =
                Color32::from_rgba_unmultiplied(background.r(), background.g(), background.b(), 0);
            let fade = 30.;
            let mut mesh = egui::Mesh::default();
            mesh.colored_vertex(pos2(shelf_area.left(), shelf_area.top()), background);
            mesh.colored_vertex(pos2(shelf_area.left() + fade, shelf_area.top()), clear);
            mesh.colored_vertex(pos2(shelf_area.left() + fade, shelf_area.bottom()), clear);
            mesh.colored_vertex(pos2(shelf_area.left(), shelf_area.bottom()), background);
            mesh.add_triangle(0, 1, 2);
            mesh.add_triangle(0, 2, 3);
            mesh.colored_vertex(pos2(shelf_area.right() - fade, shelf_area.top()), clear);
            mesh.colored_vertex(pos2(shelf_area.right(), shelf_area.top()), background);
            mesh.colored_vertex(pos2(shelf_area.right(), shelf_area.bottom()), background);
            mesh.colored_vertex(pos2(shelf_area.right() - fade, shelf_area.bottom()), clear);
            mesh.add_triangle(4, 5, 6);
            mesh.add_triangle(4, 6, 7);
            ui.painter().add(Shape::mesh(mesh));
        }
        let gesture = ui.interact(
            shelf_area,
            Id::unique("shelf-gesture"),
            Sense::click_and_drag(),
        );
        let pitch = if layout.wide {
            shelf::lane_pitch_wide(shelf_area, books.len())
        } else {
            shelf::lane_pitch_for(shelf_area, books.len())
        };
        if gesture.dragged() && books.len() > 1 {
            self.motion.drag(-ui.input(|i| i.pointer.delta().x) / pitch);
        }
        if gesture.drag_stopped() && books.len() > 1 {
            self.motion.release(
                -ui.input(|i| i.pointer.velocity().x) / pitch,
                pitch,
                now,
                self.reduced,
            );
        }
        if gesture.hovered() && books.len() > 1 {
            let scroll = ui.input(|i| i.smooth_scroll_delta);
            if scroll.x.abs() > 0.1 || scroll.y.abs() > 0.1 {
                self.motion.select(
                    self.motion.focus.round() + if scroll.x + scroll.y > 0. { -1. } else { 1. },
                    now,
                    self.reduced,
                );
            }
        }
        if (gesture.clicked() || gesture.secondary_clicked() || gesture.long_touched())
            && self.flight.is_none()
        {
            if let Some(pos) = gesture.interact_pointer_pos() {
                if let Some(volume) = volumes.iter().rev().find(|v| v.contains(pos)) {
                    let book = &books[volume.index];
                    if !self.selected.is_empty()
                        || gesture.secondary_clicked()
                        || gesture.long_touched()
                    {
                        if !self.selected.insert(book.id.clone()) {
                            self.selected.remove(&book.id);
                        }
                    } else if volume.lane != self.motion.focus.round() as i32 {
                        self.motion.select(volume.lane as f32, now, self.reduced);
                    } else {
                        self.open_detail(book, volume, &volumes, area, now);
                    }
                }
            }
        }
        let index = (self.motion.focus.round() as i32).rem_euclid(books.len() as i32) as usize;
        let book = &books[index];
        // Focus info: a new central book settles the title block in with a
        // crisp 0.12s opacity fade while the progress bar tweens over. The
        // `(id, started)` pair doubles as the "last focused" marker, so the
        // fade arms exactly once per change and then stays finished.
        let focus_changed = !matches!(
            &self.focus_book_anim,
            Some((anim_id, _)) if anim_id == &book.id
        );
        if focus_changed {
            self.focus_book_anim = Some((
                book.id.clone(),
                if self.reduced { f64::MIN } else { now },
            ));
        }
        let focus_new = focus_changed;
        let focus_alpha = self
            .focus_book_anim
            .as_ref()
            .map(|(_, started)| {
                let k = ((now - *started) / 0.12).clamp(0., 1.) as f32;
                0.4 + 0.6 * motion::smooth(k)
            })
            .unwrap_or(1.);
        let mut info_painter = ui.painter().clone();
        info_painter.multiply_opacity(focus_alpha);
        let info = layout.info.min;
        let info_right = layout.info.right();
        let title_width = layout.info.width();
        let title_size = if layout.short_landscape {
            22.
        } else if layout.side_by_side {
            34.
        } else {
            26.
        } * self.font_scale;
        let mut title = egui::text::LayoutJob::simple(
            book.title.clone(),
            crate::typography::display(title_size),
            ink,
            title_width,
        );
        title.wrap.max_rows = 1;
        let title = ui.painter().layout_job(title);
        // 展签内部全部水平居中：标题、分类胶囊、进度、按钮行都锚到展签中线。
        let info_cx = layout.info.center().x;
        info_painter.galley(
            pos2(info_cx - title.size().x / 2., info.y),
            title.clone(),
            ink,
        );
        let palette = crate::design::palette(self.dark);
        // 标题与副标题（章节/进度）间留白加大，字重层级更舒展（S2b）。
        let mut progress_y = info.y + title.size().y + 12.;
        // A quiet metadata pill rides under the title on wide layouts.
        // 展签太矮时先让位：iPad-11 横屏 pill 会顶掉分隔线与按钮的空档。
        if layout.side_by_side
            && !layout.short_landscape
            && layout.info.height() > 185.
            && self.font_scale <= 1.3
        {
            let meta = if !book.category.is_empty() {
                Some(book.category.clone())
            } else {
                let count = self.chapters_for(&book.id).len();
                (count > 1).then(|| format!("共 {count} 话"))
            };
            if let Some(meta) = meta {
                let galley = ui.painter().layout(
                    meta,
                    FontId::proportional(11.),
                    palette.muted,
                    title_width,
                );
                let pill = Rect::from_min_size(
                    pos2(info_cx - (galley.size().x + 18.) / 2., progress_y - 1.),
                    vec2(galley.size().x + 18., galley.size().y + 8.),
                );
                info_painter.rect_filled(pill, pill.height() / 2., palette.soft);
                info_painter.galley(pill.min + vec2(9., 4.), galley, palette.muted);
                progress_y += pill.height() + 6.;
            }
        }
        let has_progress = book.last_read_at.is_some();
        let pages = if has_progress {
            format!("第 {} / {} 页", book.current_page + 1, book.page_count)
        } else {
            format!("共 {} 页", book.page_count)
        };
        let progress = if let Some(chapter) = self.chapter(&book.id) {
            format!("{} · {pages}", chapter.chapter_title)
        } else {
            pages
        };
        let mut progress = egui::text::LayoutJob::simple(
            progress,
            FontId::proportional(14. * self.font_scale),
            muted,
            (info_right - info.x).max(80.),
        );
        progress.wrap.max_rows = 1;
        let progress = ui.painter().layout_job(progress);
        // Keep the action with the title and progress when the column scrolls.
        // 展签式布局：按钮必须留在舞台内，不允许被挤到屏幕外。
        // +34：进度与按钮行之间多留呼吸位，同时给下方 3pt 分隔线留 12pt 空档。
        // 底夹 -58 保证 52pt 按钮底缘距展签底 ≥6pt。
        let action_y = (progress_y + progress.size().y + 34.)
            .min(layout.info.bottom() - 58.)
            .max(layout.info.top() + 40.);
        let target_fraction = if has_progress {
            (book.current_page + 1) as f32 / book.page_count.max(1) as f32
        } else {
            0.
        };
        if focus_new {
            let from = self
                .progress_tween
                .map(|t| t.value(now))
                .unwrap_or(target_fraction);
            self.progress_tween = Some(motion::TimedTween::new(
                now,
                if self.reduced { 0. } else { 0.2 },
                from,
                target_fraction,
            ));
        }
        let fill_fraction = self
            .progress_tween
            .map(|t| t.value(now))
            .unwrap_or(target_fraction);
        if self.progress_tween.is_some_and(|t| t.done(now)) {
            self.progress_tween = None;
        }
        // 分隔线锚到按钮行上方 15pt：action_y 已被夹进展签底 56pt 内，
        // 短展签（iPad-11 横屏）里锚文本块会让线横切按钮中线。
        // 若线会压到进度文本（极短展签），则整条线不画。
        let track_y = action_y - 15.;
        if track_y > progress_y + progress.size().y {
            let track = Rect::from_min_size(
                pos2(
                    info_cx - (info_right - info.x).min(360.) / 2.,
                    track_y,
                ),
                vec2((info_right - info.x).min(360.), 3.),
            );
            info_painter.rect_filled(track, 2., palette.line);
            if has_progress {
                info_painter.rect_filled(
                    Rect::from_min_size(track.min, vec2(track.width() * fill_fraction, 3.)),
                    2.,
                    palette.accent,
                );
            }
        }
        info_painter.galley(
            pos2(info_cx - progress.size().x / 2., progress_y),
            progress,
            muted,
        );
        let detail_width = 30. * self.font_scale + 24.;
        // 展签内边距：按钮组左右都不许越过 info±8pt（320pt 分屏左缘出血修复）。
        let inner_left = info.x + 8.;
        let inner_right = info_right - 8.;
        // 触控热区不得小于 44pt；右侧信息栏收窄后按钮也要保底 88pt 才不会压线。
        let action_width = (84. * self.font_scale + 68.)
            .min(inner_right - inner_left - detail_width - 12.)
            .max(88.);
        // 两个按钮按「整组」居中并夹进展签内边距——旧代码按组间缝隙居中，
        // 不等宽按钮在 320pt 分屏会把「开始阅读」左缘推到屏幕边。
        let group_w = action_width + 12. + detail_width;
        let group_left = (info_cx - group_w / 2.)
            .clamp(inner_left, (inner_right - group_w).max(inner_left));
        let open_rect = Rect::from_min_size(
            pos2(group_left + action_width + 12., action_y),
            vec2(detail_width, 52.),
        );
        if self.selected.is_empty() && self.button(ui, "open-book", open_rect, "详情", false) {
            if let Some(volume) = volumes
                .iter()
                .find(|v| v.lane == self.motion.focus.round() as i32)
            {
                self.open_detail(book, volume, &volumes, area, now);
            }
        }
        let action = Rect::from_min_size(
            pos2(group_left, action_y),
            vec2(action_width, 52.),
        );
        if self.selected.is_empty()
            && self.button(
                ui,
                "shelf-read",
                action,
                if has_progress {
                    "继续阅读"
                } else {
                    "开始阅读"
                },
                true,
            )
        {
            self.open_reader(book);
        }
        // 分页器只留一种语义：所有档位都锚在舞台右上角（P1-5）。
        // 此前窄屏把它挪到卡片右下，切档时同一控件满屏漂移。
        let tally = format!("{:02} / {:02}", index + 1, books.len());
        let tally = ui
            .painter()
            .layout_no_wrap(tally, FontId::monospace(11.), muted);
        let tally_gap = 8.;
        let pager_right = layout.stage.right() - 16.;
        let pager_left = pager_right - 88. - tally.size().x - tally_gap * 2.;
        let y = layout.stage.top() + 12.;
        if books.len() > 1
            && self.icon(
                ui,
                "shelf-prev",
                Rect::from_min_size(pos2(pager_left, y), vec2(44., 44.)),
                "prev",
                "上一本",
            )
        {
            self.motion
                .select(self.motion.focus.round() - 1., now, self.reduced);
        }
        if books.len() > 1 {
            ui.painter().galley(
                pos2(pager_left + 44. + tally_gap, y + (44. - tally.size().y) / 2.),
                tally,
                muted,
            );
        }
        if books.len() > 1
            && self.icon(
                ui,
                "shelf-next",
                Rect::from_min_size(pos2(pager_right - 44., y), vec2(44., 44.)),
                "next",
                "下一本",
            )
        {
            self.motion
                .select(self.motion.focus.round() + 1., now, self.reduced);
        }
        // 藏书一览网格已下线：书架即主体，不再在下方重复同一批书。
        if !self.selected.is_empty() {
            let rect = Rect::from_min_size(
                pos2(area.left() + 24., area.bottom() - 122.),
                vec2(area.width() - 48., 48.),
            );
            ui.painter().rect_filled(
                rect,
                8.,
                if self.dark {
                    Color32::from_gray(46)
                } else {
                    Color32::WHITE
                },
            );
            self.label(
                ui.painter(),
                rect.min + vec2(10., 14.),
                &format!("已选 {}", self.selected.len()),
                12.,
                ink,
            );
            ui.put(
                Rect::from_min_size(rect.min + vec2(80., 6.), vec2(120., 34.)),
                TextEdit::singleline(&mut self.category_edit).hint_text("分类名称"),
            );
            if self.button(
                ui,
                "set-category",
                Rect::from_min_size(rect.min + vec2(205., 6.), vec2(58., 34.)),
                "分类",
                false,
            ) {
                self.send(Command::Category(
                    self.selected_books(),
                    self.category_edit.clone(),
                ));
                self.selected.clear();
            }
            if self.button(
                ui,
                "remove-selected",
                Rect::from_min_size(pos2(rect.right() - 66., rect.top() + 6.), vec2(60., 34.)),
                "移出",
                false,
            ) {
                self.confirm_remove = true;
            }
        }
    }
    #[allow(dead_code)] // 藏书一览已下线，保留实现备回滚
fn library_thumbnails(
        &mut self,
        ui: &mut Ui,
        area: Rect,
        books: &[Book],
        selected: usize,
        now: f64,
    ) {
        let palette = crate::design::palette(self.dark);
        ui.painter().text(
            area.min,
            Align2::LEFT_TOP,
            "藏书一览",
            crate::typography::display(19. * self.font_scale),
            palette.ink,
        );
        ui.painter().text(
            pos2(area.right(), area.top() + 4.),
            Align2::RIGHT_TOP,
            format!("{} 本", books.len()),
            FontId::proportional(12. * self.font_scale),
            palette.muted,
        );
        let top = area.top() + 34. * self.font_scale;
        let canvas = Rect::from_min_max(pos2(area.left(), top), area.max);
        let cover_height = if area.width() >= 660. {
            area.height() - 98. - 28. * (self.font_scale - 1.)
        } else {
            92.
        };
        let cover_width = cover_height * 118. / 166.;
        let cell_width = cover_width + if area.width() >= 660. { 38. } else { 20. };
        let cell_height = cover_height + 38. * self.font_scale;
        ui.scope_builder(UiBuilder::new().max_rect(canvas), |ui| {
            ui.set_clip_rect(ui.clip_rect().intersect(canvas));
            ScrollArea::horizontal()
                .id_salt("library-thumbnails")
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    let total = vec2(
                        // Symmetric tail padding keeps the last cover off the
                        // clip edge instead of hard-cropping at the canvas.
                        (books.len() as f32 * cell_width + 8.).max(canvas.width()),
                        cell_height,
                    );
                    let (row, _) = ui.allocate_exact_size(total, Sense::hover());
                    for (index, book) in books.iter().enumerate() {
                        let rect = Rect::from_min_size(
                            row.min + vec2(index as f32 * cell_width, 0.),
                            vec2(cell_width - 8., cell_height),
                        );
                        if !rect.intersects(ui.clip_rect()) {
                            continue;
                        }
                        let response = ui.interact(
                            rect,
                            Id::unique(("library-thumbnail", &book.id)),
                            Sense::click(),
                        );
                        response
                            .widget_info(|| WidgetInfo::labeled(Role::Button, true, &book.title));
                        self.hitboxes
                            .insert(format!("shelf-thumbnail-{}", book.id), rect);
                        // Long shelves follow the active book once per change.
                        if index == selected && self.last_thumb_selected != Some(selected) {
                            ui.scroll_to_rect(rect, Some(Align::Center));
                        }
                        let cover = Rect::from_min_size(
                            rect.min + vec2(4., 3.),
                            vec2(cover_width, cover_height),
                        );
                        let lifted_cover = if index == selected {
                            cover.translate(vec2(0., -4.))
                        } else {
                            cover
                        };
                        crate::design::soft_shadow(
                            ui.painter(),
                            lifted_cover,
                            6.,
                            if index == selected { 1.3 } else { 0.6 },
                            self.dark,
                        );
                        let cover = crate::design::press_rect(
                            lifted_cover,
                            response.is_pointer_button_down_on() && !self.reduced,
                        );
                        if index == selected || response.hovered() {
                            ui.painter().rect_stroke(
                                cover,
                                6.,
                                Stroke::new(1.5, palette.accent),
                                StrokeKind::Inside,
                            );
                        }
                        let cover_id = self.cover_id(&book.id);
                        self.ask_cover(&cover_id);
                        if let Some(texture) = shelf::cover_pick(
                            self.cover_mips.get(&cover_id),
                            self.covers.get(&cover_id),
                            Plane::rect(cover),
                            cover.size(),
                            ui.ctx().pixels_per_point(),
                        ) {
                            shelf::paint_art(
                                ui.painter(),
                                Plane::rect(cover),
                                cover.size(),
                                texture,
                                palette.soft,
                            );
                        } else {
                            ui.painter().rect_filled(cover, 5., palette.soft);
                        }
                        // Partially read books get a quiet progress tick along
                        // the inside of the cover's bottom edge.
                        let pages = book.page_count;
                        let read = book.current_page;
                        if pages > 0 && read > 0 && read < pages {
                            let rail = Rect::from_min_max(
                                pos2(cover.left() + 5., cover.bottom() - 5.),
                                pos2(cover.right() - 5., cover.bottom() - 3.),
                            );
                            ui.painter().rect_filled(
                                rail,
                                1.,
                                palette.muted.gamma_multiply(0.30),
                            );
                            let fill = Rect::from_min_max(
                                rail.min,
                                pos2(
                                    rail.left()
                                        + rail.width() * (read as f32 / pages as f32),
                                    rail.max.y,
                                ),
                            );
                            ui.painter().rect_filled(fill, 1., palette.accent);
                        }
                        let mut title = egui::text::LayoutJob::simple(
                            book.title.clone(),
                            FontId::proportional(12. * self.font_scale),
                            if index == selected {
                                palette.accent
                            } else {
                                palette.ink
                            },
                            cover_width,
                        );
                        title.wrap.max_rows = 1;
                        ui.painter().galley(
                            pos2(cover.left(), cover.bottom() + 9.),
                            ui.painter().layout_job(title),
                            palette.ink,
                        );
                        if response.clicked() {
                            let count = books.len() as i32;
                            let current = self.motion.focus.round() as i32;
                            let delta = (index as i32 - current).rem_euclid(count);
                            let delta = if delta > count / 2 {
                                delta - count
                            } else {
                                delta
                            };
                            self.motion
                                .select((current + delta) as f32, now, self.reduced);
                        }
                    }
                });
        });
        self.last_thumb_selected = Some(selected);
    }
    fn open_detail(
        &mut self,
        book: &Book,
        volume: &shelf::Volume,
        volumes: &[shelf::Volume],
        area: Rect,
        now: f64,
    ) {
        self.screen = Screen::Detail(book.id.clone());
        let cover = self.cover_id(&book.id);
        if !self.reduced && self.covers.contains_key(&cover) {
            self.flight = Some(Flight {
                id: cover,
                book_id: book.id.clone(),
                started: now,
                returning: false,
                from: volume.cover,
                to: Plane::rect(self.detail_cover_rect(self.shelf_viewport)),
                foreground: shelf::foreground(volumes, volume.lane, volume.cover),
                origin_lane: volume.lane,
                clip: self.shelf_area(area),
            });
        }
    }
    fn detail_ui(&mut self, ui: &mut Ui, area: Rect, id: &str) {
        let Some(book) = self.books.iter().find(|b| b.id == id).cloned() else {
            return;
        };
        let cover_id = self.cover_id(id);
        self.ask_cover(&cover_id);
        let cover = self.detail_cover_rect(area);
        let now = ui.ctx().input(|i| i.time);
        let reveal = self.flight.as_ref().map(|flight| {
            let duration = if flight.returning {
                motion::close_travel_seconds()
            } else {
                motion::OPEN
            };
            let elapsed = ((now - flight.started) / duration).clamp(0., 1.) as f32;
            if flight.returning {
                1. - elapsed
            } else {
                elapsed
            }
        });
        // The text column's reveal opacity; it also lifts 8pt into place.
        // R2 (spec §7): text stays unclipped (ScrollArea cannot be
        // re-emitted per clip band), so `motion::detail_text_alpha` ramps
        // behind the diagonal front instead of the travel fade — otherwise
        // the column is already opaque while the right half still sits in
        // the unrevealed region (穿帮, visible in spread-mid before this).
        // Anchors: unit test `text_column_stays_invisible_until_sweep_
        // mostly_done` + the acceptance text-row strip in spread-mid.
        let detail_alpha = reveal.map(motion::detail_text_alpha).unwrap_or(1.);
        // While the reader peels away over this page, the text column
        // waits under the bands and fades in past `under_alpha_start`.
        let exit_alpha = self.exit_under_alpha(now);
        // 摊开 (R2 分工): the slanted staircase clip owns the cover
        // geometry — soft shadow, hairline border, and the cover art itself
        // are the `incoming` plane (archive_motion.dart incoming: slant clip
        // + whole-plane offset). R1: egui has no arbitrary-path clip, so
        // `shelf::spread_clip` stair-steps the trapezoid. The text column
        // keeps `detail_alpha` because ScrollArea widgets cannot be
        // re-emitted per band; the alpha fade also keeps text crisp per the
        // reference's "opaque text stays crisp" design note.
        // Once `flight` is None, `spread` is 1. and the bands cover `area`
        // wholesale — the resting detail frame is exactly the p=1 frame.
        let spread = reveal.map_or(1., motion::spread_progress);
        if spread > 0.0005 {
            let dir = crate::params::params().spread.dir;
            let texture = self.covers.get(&cover_id);
            let mips = self.cover_mips.get(&cover_id);
            let ppp = ui.ctx().pixels_per_point();
            let dark = self.dark;
            // P1-6: the detail cover is the same solid book as on the shelf
            // — shelf::detail_volume interpolates Volume geometry from the
            // flat landing card (morph 0) to a ~12 deg oblique book (morph 1)
            // along the same spread_progress, so thickness, spine and page
            // cap grow during the sweep instead of popping in at p=1.
            // Shelf furniture (lane index / NO.xxx / selection dot) is off,
            // the spine title stays. morph smooths the endpoints so the last
            // stretch eases into place rather than snapping.
            let morph = motion::smooth(spread);
            let title_str = book.title.clone();
            shelf::paint_spread(ui.painter(), area, spread, dir, |p, shift| {
                let card = cover.translate(shift);
                let volume = shelf::detail_volume(card, morph);
                crate::shelf::paint_floor_one(p, &volume, dark);
                let texture = shelf::cover_pick(
                    mips,
                    texture,
                    volume.cover,
                    shelf::detail_face(volume.cover),
                    ppp,
                );
                shelf::paint_detail_volume(p, &volume, &title_str, texture, dark);
            });
        }
        let (_, ink, muted) = self.colors();
        let title = self
            .chapter(id)
            .map(|m| m.manga_title.clone())
            .unwrap_or_else(|| book.title.clone());
        let text = if let Some(chapter) = self.chapter(id) {
            format!("{} · {} 页", chapter.chapter_title, book.page_count)
        } else if book.author.is_empty() {
            format!(
                "{} 页 · {}",
                book.page_count,
                if book.category.is_empty() {
                    "未分类"
                } else {
                    &book.category
                }
            )
        } else {
            format!("{} · {} 页", book.author, book.page_count)
        };
        let wide = Self::detail_wide(area);
        let details_start = if Self::detail_side_by_side(area) {
            pos2(area.center().x, area.top() + 68.)
        } else if wide {
            pos2(cover.right() + 72., cover.top() + cover.height() * 0.15)
        } else {
            pos2(area.left() + 24., cover.bottom() + 16.)
        } - vec2(0., 8. * (1. - detail_alpha));
        let body = if wide {
            Rect::from_min_size(
                details_start,
                vec2(392., (area.bottom() - 24. - details_start.y).max(160.)),
            )
        } else {
            Rect::from_min_max(details_start, area.max - vec2(24., 6.))
        };
        let column_alpha = detail_alpha * exit_alpha;
        // 同系列已下载章节：≥2 章渲染 Apple Books 分组章节卡（S3），本地单卷
        // 书退化为进度/卷宗概览卡；章节名取链接元数据，过滤同 chapters_for。
        let mut siblings: Vec<(Book, String)> = self
            .chapters
            .iter()
            .filter(|link| self.same_series(id, &link.book_id))
            .filter_map(|link| {
                self.books
                    .iter()
                    .find(|b| b.id == link.book_id)
                    .cloned()
                    .map(|b| (b, link.metadata.chapter_title.clone()))
            })
            .collect();
        let mut app = self;
        ui.scope_builder(UiBuilder::new().max_rect(body), |ui| {
            ScrollArea::vertical()
                .id_salt(("detail-body", id))
                .show(ui, |ui| {
                    if column_alpha < 0.999 {
                        ui.set_opacity(column_alpha);
                    }
                    Self::detail_column(ui, &mut app, &book, id, &title, &text, ink, muted, &mut siblings, wide);
                });
        });
    }
    /// Apple Books 工具条：次按钮按文字量取宽、≥44pt、自动折行的紧凑胶囊。
    /// 主按钮 read 仍是唯一视觉焦点（accent 胶囊 + 轻落影）。
    fn detail_actions(&mut self, ui: &mut Ui, book: &Book, id: &str, width: f32, primary_height: f32) {
        let can_restart = book.current_page > 0 || book.page_fraction > 0.;
        let width = width.min(ui.available_width());
        let (rect, _) = ui.allocate_exact_size(vec2(width, primary_height), Sense::hover());
        let action = if book.last_read_at.is_some() {
            format!("继续阅读 · 第 {} 页", book.current_page + 1)
        } else {
            "开始阅读".to_string()
        };
        if self.button(ui, "read", rect, &action, true) {
            self.flight = None;
            self.open_reader(book);
        }
        let mut row: Vec<(&str, String)> = Vec::new();
        if can_restart {
            row.push(("read-start", "从第一页开始".into()));
        }
        if self.chapter(id).is_some() {
            row.push(("detail-chapters", "章节目录".into()));
        }
        let key = if let Some(chapter) = self.chapter(id) {
            crate::trackers::binding_key(Some((&chapter.source_id, &chapter.manga_id)), None)
        } else {
            crate::trackers::binding_key(None, Some(id))
        };
        if !key.is_empty() {
            row.push(("detail-tracking", "进度跟踪".into()));
        }
        if row.is_empty() {
            return;
        }
        ui.add_space(10.);
        let font = FontId::proportional(15. * self.font_scale);
        let gap = 8.;
        let widths: Vec<f32> = row
            .iter()
            .map(|(_, label)| (ui.painter().layout_no_wrap(label.clone(), font.clone(), Color32::WHITE).size().x + 36.).clamp(76., width.max(76.)))
            .collect();
        let total = widths.iter().sum::<f32>() + gap * (widths.len() - 1) as f32;
        let click = |app: &mut Self, hid: &str, book: &Book, id: &str, key: &str| {
            match hid {
                "read-start" => {
                    let mut first = book.clone();
                    first.current_page = 0;
                    first.page_fraction = 0.;
                    app.flight = None;
                    app.open_reader(&first);
                }
                "detail-chapters" => app.chapter_dialog = Some(id.to_string()),
                _ => {
                    app.track_panel = Some(key.to_string());
                    app.track_query.clear();
                }
            }
        };
        if total <= ui.available_width() {
            // 一行放得下：整块手动居中（horizontal_wrapped 会左漂）。
            ui.horizontal(|ui| {
                ui.add_space(((ui.available_width() - total) / 2.).max(0.));
                for ((hid, label), w) in row.iter().zip(&widths) {
                    let (rect, _) = ui.allocate_exact_size(vec2(*w, 44.), Sense::hover());
                    if self.button(ui, hid, rect, label, false) {
                        click(self, hid, book, id, &key);
                    }
                    ui.add_space(gap);
                }
            });
            return;
        }
        // 200% 大字号等超宽场景：每个次按钮竖排居中，热区仍 ≥44pt。
        for ((hid, label), w) in row.iter().zip(&widths) {
            let (rect, _) = ui.allocate_exact_size(vec2(*w, 44.), Sense::hover());
            if self.button(ui, hid, rect, label, false) {
                click(self, hid, book, id, &key);
            }
            ui.add_space(gap);
        }
    }
/// 详情页文字列：hero（书名/元信息/进度徽章）→ 操作区 → 内联分组卡；
/// 全程在 ScrollArea 里，保持扫边 alpha 契约与静止帧逐字节稳定（close-p1==close-pre）。
fn detail_column(ui: &mut Ui, app: &mut ReaderApp, book: &Book, id: &str, title: &str, text: &str,
                 ink: Color32, muted: Color32, siblings: &mut Vec<(Book, String)>, wide: bool) {
    let fs = app.font_scale;
    let dark = app.dark;
    let (t, s, g) = if wide { (36., 15., 8.) } else if ui.available_width() >= 560. { (30., 13., 6.) } else { (24., 13., 6.) };
    let hero = |ui: &mut Ui| {
        ui.add(Label::new(RichText::new(title).font(crate::typography::display(t * fs)).color(ink)).wrap());
        ui.add_space(g);
        ui.add(Label::new(RichText::new(text).size(s * fs).color(muted)).wrap());
        if book.last_read_at.is_some() || book.current_page > 0 {
            ui.add_space(8.);
            // 「已读 X / Y 页 · XX%」柔和胶囊徽章。
            let p = crate::design::palette(dark);
            let read = (book.current_page + 1).min(book.page_count);
            let pct = if book.page_count > 0 { ((book.current_page as f32 + book.page_fraction.clamp(0., 1.)) / book.page_count as f32 * 100.).round() as u32 } else { 0 };
            let label = format!("已读 {read} / {} 页 · {pct}%", book.page_count);
            let font = FontId::proportional(12.5 * fs);
            let size = ui.painter().layout_no_wrap(label.clone(), font.clone(), p.ink).size();
            let (rect, _) = ui.allocate_exact_size(size + vec2(26., 14.), Sense::hover());
            let pt = ui.painter();
            pt.rect_filled(rect, rect.height() / 2., p.soft);
            pt.circle_filled(rect.left_center() + vec2(15., 0.), 3., p.accent);
            pt.text(rect.left_center() + vec2(24., 0.), Align2::LEFT_CENTER, label, font, p.muted);
        }
    };
    if wide {
        hero(ui);
        ui.add_space(24.);
        app.detail_actions(ui, book, id, 280., 52.);
        ui.add_space(16.);
        Self::detail_card(ui, app, book, id, siblings);
        return;
    }
    // 单列：hero、按钮、内联卡整体收进居中 ≤560pt 栏宽（原 1040pt 通栏是
    // 拦截项）。scope_builder 显式钉栏位——vertical_centered 只在自己的
    // max_rect 内居中，套在 horizontal 余量里会以列右半宽为轴偏右。
    let avail = ui.available_width();
    let col_w = avail.min(560.);
    let left = ui.cursor().left() + ((avail - col_w) / 2.).max(0.);
    let col = Rect::from_min_max(
        pos2(left, ui.cursor().top()),
        pos2(left + col_w, ui.max_rect().bottom()),
    );
    ui.scope_builder(UiBuilder::new().max_rect(col), |ui| {
        ui.vertical_centered(|ui| {
            ui.set_width(col_w);
            hero(ui);
            ui.add_space(10.);
            app.detail_actions(ui, book, id, (252. * fs.sqrt()).min(col_w), 48.);
            ui.add_space(8.);
            Self::detail_card(ui, app, book, id, siblings);
        });
    });
}
/// 内联分组卡：≥2 章渲染章节列表（正/倒序胶囊 + 前 6 章 + 查看全部）；
/// 单卷本地书退化为阅读进度与卷宗概览（栏宽由 detail_column 钉死）。
fn detail_card(ui: &mut Ui, app: &mut ReaderApp, book: &Book, id: &str, siblings: &mut Vec<(Book, String)>) {
    let p = crate::design::palette(app.dark);
    crate::design::grouped_card(ui, p, |ui| {
        ui.set_width(ui.available_width());
        if siblings.len() >= 2 {
            if app.detail_desc { siblings.reverse(); }
            let fs = app.font_scale;
            let title = format!("章节 · 共 {} 话", siblings.len());
            let pills = |ui: &mut Ui, app: &mut ReaderApp| {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    for (desc, label) in [(true, "最新在前"), (false, "从头开始")] {
                        let r = ui.selectable_label(app.detail_desc == desc, label);
                        app.hitboxes.insert(format!("detail-sort-{desc}"), r.rect);
                        if r.clicked() { app.detail_desc = desc; }
                    }
                });
            };
            // 头行放不下时胶囊下移到独立一行（320pt×200% 大字号防挤压）。
            let pill_font = TextStyle::Body.resolve(ui.style());
            let pill_w = ["最新在前", "从头开始"].iter()
                .map(|l| ui.painter().layout_no_wrap((*l).into(), pill_font.clone(), p.ink).size().x + 30.)
                .sum::<f32>() + 12.;
            let title_w = ui.painter().layout_no_wrap(title.clone(), FontId::proportional(15. * fs), p.ink).size().x;
            if title_w + pill_w + 16. > ui.available_width() {
                ui.add(Label::new(RichText::new(title).size(15. * fs).strong().color(p.ink)).truncate());
                ui.add_space(4.);
                pills(ui, app);
            } else {
                ui.horizontal(|ui| {
                    ui.add(Label::new(RichText::new(title).size(15. * fs).strong().color(p.ink)).truncate());
                    pills(ui, app);
                });
            }
            ui.add_space(6.);
            for (i, (entry, chapter)) in siblings.iter().take(6).enumerate() {
                Self::detail_chapter_row(ui, app, entry, id, chapter, p, i > 0);
            }
            if siblings.len() > 6 {
                let y = ui.cursor().top();
                ui.painter().line_segment([pos2(ui.min_rect().left(), y), pos2(ui.min_rect().right(), y)], Stroke::new(0.5, p.line));
                let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 44.), Sense::click());
                app.hitboxes.insert("detail-chapters-all".into(), rect);
                ui.painter().text(rect.center(), Align2::CENTER_CENTER, format!("查看全部 {} 话", siblings.len()),
                    FontId::proportional(14. * app.font_scale), p.accent);
                if resp.clicked() { app.chapter_dialog = Some(id.to_string()); }
            }
            return;
        }
        ui.add(Label::new(RichText::new("阅读进度 · 卷宗").size(15. * app.font_scale).strong().color(p.ink)).wrap());
        ui.add_space(10.);
        let track = Rect::from_min_size(ui.cursor().min, vec2(ui.available_width(), 6.));
        ui.advance_cursor_after_rect(track);
        ui.painter().rect_filled(track, 3., p.soft);
        let frac = if book.page_count > 0 { ((book.current_page as f32 + book.page_fraction.clamp(0., 1.)) / book.page_count as f32).clamp(0., 1.) } else { 0. };
        if frac > 0.01 {
            ui.painter().rect_filled(Rect::from_min_size(track.min, vec2((track.width() * frac).max(6.), 6.)), 3., p.accent);
        }
        ui.add_space(10.);
        let ext = book.archive_path.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_uppercase()).unwrap_or_else(|| "CBZ".into());
        ui.label(RichText::new(format!("已读 {} / {} 页 · {}% · {ext} · {}",
            (book.current_page + 1).min(book.page_count), book.page_count, (frac * 100.).round() as u32,
            if book.category.is_empty() { "未分类" } else { book.category.as_str() }))
            .size(13. * app.font_scale).color(p.muted));
    });
}
/// 内联章节行：当前章琥珀圆点、0.5pt 发丝分割、44pt 热区直达阅读位置。
/// 章节名扣除右侧页码带后单行截断（…），320pt×200% 大字号也不越界。
fn detail_chapter_row(ui: &mut Ui, app: &mut ReaderApp, entry: &Book, id: &str, chapter: &str,
                      p: crate::design::Palette, hairline: bool) {
    if hairline {
        let y = ui.cursor().top();
        ui.painter().line_segment([pos2(ui.min_rect().left(), y), pos2(ui.min_rect().right(), y)], Stroke::new(0.5, p.line));
    }
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 44.), Sense::click());
    if resp.hovered() { ui.painter().rect_filled(rect, 6., p.soft.gamma_multiply(0.55)); }
    let current = entry.id == id;
    let pt = ui.painter();
    if current { pt.circle_filled(rect.left_center() + vec2(6., 0.), 3.5, p.accent); }
    let read_badge = (entry.last_read_at.is_some() || entry.current_page > 0)
        .then(|| format!("第 {} / {} 页", (entry.current_page + 1).min(entry.page_count), entry.page_count));
    let page_w = read_badge.as_ref()
        .map(|s| pt.layout_no_wrap(s.clone(), FontId::proportional(12. * app.font_scale), p.muted).size().x + 16.)
        .unwrap_or(16.);
    let name_x = rect.left_center() + vec2(if current { 20. } else { 12. }, 0.);
    let mut job = egui::epaint::text::LayoutJob::single_section(
        chapter.to_string(),
        egui::epaint::text::TextFormat::simple(FontId::proportional(14. * app.font_scale), p.ink),
    );
    job.wrap.max_width = (rect.right() - page_w - name_x.x).max(24.);
    job.wrap.max_rows = 1;
    job.wrap.overflow_character = Some('…');
    let galley = pt.layout_job(job);
    pt.galley(pos2(name_x.x, rect.center().y - galley.size().y / 2.), galley, p.ink);
    if let Some(s) = read_badge {
        pt.text(rect.right_center() + vec2(-8., 0.), Align2::RIGHT_CENTER, s,
            FontId::proportional(12. * app.font_scale), p.muted);
    }
    app.hitboxes.insert(format!("dchapter-{}", entry.id), rect);
    if resp.clicked() {
        app.flight = None;
        app.open_reader(entry);
    }
}


    fn reader_input_blocked(&self) -> bool {
        self.reader_options
            || self.chapter_dialog.is_some()
            || self.track_panel.is_some()
            || self.error.is_some()
    }
    fn set_reading_mode(&mut self, mode: &str) {
        if self.vertical == (mode == "vertical") && self.spread == (mode == "spread") {
            return;
        }
        self.flush_position();
        self.vertical = mode == "vertical";
        self.spread = mode == "spread";
        self.vertical_reader.reset(self.page, 0.);
        self.vertical_prev = None;
        self.vertical_next = None;
        self.next_stream = None;
        self.zoom = 1.;
        self.pan = Vec2::ZERO;
        self.page_slide.cancel();
        self.zoom_anim.cancel();
        self.pending_reader_tap = None;
        if matches!(self.screen, Screen::Reader(_)) {
            self.save_reader_prefs();
        } else {
            self.reader_globals.mode = if self.vertical {
                crate::reader_prefs::Mode::Vertical
            } else if self.spread {
                crate::reader_prefs::Mode::Spread
            } else {
                crate::reader_prefs::Mode::Paged
            };
            self.setting("reading-mode", mode.into());
        }
    }
    /// Chrome rects (bar + capsule <=680pt); shifts = show-hide slide.
    fn reader_chrome(&self, painter: &Painter, area: Rect, book: &Book, id: &str, top_shift: f32, bottom_shift: f32) -> ReaderChrome {
        let top = Rect::from_min_max(area.min + vec2(12., 10. + top_shift), pos2(area.right() - 12., area.top() + 66. + top_shift));
        let dock = Rect::from_center_size(pos2(area.center().x, area.bottom() - 42. + bottom_shift), vec2((area.width() - 24.).min(680.), 60.));
        let back = Rect::from_min_size(top.min + vec2(6., 6.), vec2(44., 44.));
        let settings = Rect::from_min_size(pos2(top.right() - 50., top.top() + 6.), vec2(44., 44.));
        let chapters = Rect::from_min_size(pos2(settings.left() - 48., top.top() + 6.), vec2(44., 44.));
        let title_r = (if self.chapter(id).is_some() { chapters.left() } else { settings.left() }) - 8.;
        let title_clip = Rect::from_min_max(pos2(back.right() + 8., top.top()), pos2(title_r, top.bottom()));
        let prev = Rect::from_min_size(dock.min + vec2(8., 8.), vec2(44., 44.));
        let next = Rect::from_min_size(pos2(dock.right() - 52., dock.top() + 8.), vec2(44., 44.));
        let galley = painter.layout_no_wrap(page_label(book, self.page, self.private), FontId::monospace(12.), Color32::WHITE);
        let badge_x = (prev.right() + 22. + galley.size().x / 2.).max(dock.left() + 96.);
        let badge = Rect::from_center_size(pos2(badge_x, dock.center().y), vec2(galley.size().x + 32., 34.));
        let slider = Rect::from_min_max(pos2((badge.right() + 8.).min(next.left() - 88.), dock.top() + 8.), pos2(next.left() - 8., dock.bottom() - 8.));
        ReaderChrome { top, dock, back, settings, chapters, title_clip, prev, next, badge, slider }
    }
    /// Static chrome for live frame and ghosts (icon() skips glyphs live).
    fn paint_reader_chrome(&self, painter: &Painter, c: &ReaderChrome, book: &Book, id: &str) {
        let p = crate::design::palette(true);
        crate::design::translucent_bar(painter, c.top, p, true, 16);
        crate::design::translucent_bar(painter, c.dock, p, true, 30);
        let icon = Color32::from_gray(232);
        crate::design::draw_icon(painter, c.back.center(), "back", icon, false);
        if self.chapter(id).is_some() { crate::design::draw_icon(painter, c.chapters.center(), "chapters", icon, false); }
        crate::design::draw_icon(painter, c.settings.center(), "settings", icon, false);
        painter.with_clip_rect(c.title_clip).text(pos2(c.title_clip.left(), c.top.center().y), Align2::LEFT_CENTER, self.reading_title(book), FontId::proportional(15.), p.ink);
        crate::design::draw_icon(painter, c.prev.center(), "prev", icon, false);
        crate::design::draw_icon(painter, c.next.center(), "next", icon, false);
        painter.rect_filled(c.badge, 17., p.soft);
        painter.text(c.badge.center(), Align2::CENTER_CENTER, page_label(book, self.page, self.private), FontId::monospace(12.), p.ink);
    }
    fn reader_tap(&mut self, id: &str, relative: f32, step: i32, now: f64, viewport_w: f32) {
        if self.vertical || self.zoom > 1. || (0.27..=0.73).contains(&relative) {
            let current = self
                .controls_anim
                .map(|t| t.value(now))
                .unwrap_or(if self.controls { 1. } else { 0. });
            self.controls = !self.controls;
            self.controls_anim = Some(motion::TimedTween::new(
                now,
                if self.reduced { 0. } else { 0.18 },
                current,
                if self.controls { 1. } else { 0. },
            ));
        } else if relative < 0.27 {
            self.next_page(id, if self.rtl { step } else { -step }, now, viewport_w);
        } else {
            self.next_page(id, if self.rtl { -step } else { step }, now, viewport_w);
        }
    }
    /// Reader -> target peel-exit overlay. The screen field is already the
    /// return target (painted by the normal branch above); this repaints
    /// the outgoing reader plane inside spread bands shrinking
    /// `close_p` 1 -> 0, so the book looks closed rather than cut. The
    /// `INFINITY` sentinel discharges on the first exit frame, exactly
    /// like the entry clock.
    fn reader_exit_overlay(&mut self, ui: &mut Ui, area: Rect, now: f64) {
        let Some((armed, id)) = self.reader_exit.clone() else {
            return;
        };
        let close = crate::params::params().close.clone();
        let started = if armed.is_infinite() {
            self.reader_exit = Some((now, id.clone()));
            now
        } else {
            armed
        };
        let raw = ((now - started) / motion::close_seconds().max(0.001)).clamp(0., 1.) as f32;
        let p = motion::close_p(raw);
        if p <= 0.001 {
            self.reader_exit = None;
            return;
        }
        let Some(book) = self.reader_book(&id) else {
            self.reader_exit = None;
            return;
        };
        let page_viewport = area.shrink2(vec2(8., 0.));
        let page_fit = area.shrink2(vec2(8., 64.));
        let step: i32 = if self.spread && !self.vertical && area.width() > 700. {
            if self.spread_offset && self.page == 0 { 1 } else { 2 }
        } else { 1 };
        let (base, partner) =
            if step == 2 { spread_pair(self.page, self.spread_offset) } else { (self.page, None) };
        let pair = step == 2;
        let first_on_right = pair && self.rtl;
        let available = if pair {
            Rect::from_min_size(
                page_fit.min + vec2(if first_on_right { page_fit.width() / 2. } else { 0. }, 0.),
                vec2(page_fit.width() / 2., page_fit.height()),
            )
        } else { page_fit };
        let (crop, pan, zoom) = (self.crop, self.pan, self.zoom);
        let fit_single = self.fit_width && step == 1;
        // Same slide state reader_ui paints with — live so a mid-turn back
        // keeps continuity; tick only retires finished tweens.
        let slide_x = if zoom > 1. { 0. } else { self.page_slide.current_offset(now) };
        self.page_slide.tick(now);
        let outgoing = (step == 1).then(|| self.page_slide.outgoing(now)).flatten();
        let mut queue: Vec<(usize, Rect, bool)> = Vec::new();
        for (page, offset_x, fit_w) in [
            (base, slide_x, fit_single),
            (partner.unwrap_or(usize::MAX),
             slide_x + if first_on_right { -available.width() } else { available.width() }, false),
            (outgoing.map(|(p, _)| p).unwrap_or(usize::MAX), slide_x - self.slide_turn_gap, fit_single),
        ] {
            if page > book.page_count.saturating_sub(1) { continue; }
            if let Some(tex) = self.pages.get(&(id.clone(), page)) {
                let size = tex.shown_size(crop);
                let scale = if fit_w {
                    available.width() / size.x
                } else {
                    (available.width() / size.x).min(available.height() / size.y)
                };
                let target =
                    Rect::from_center_size(available.center() + vec2(offset_x, 0.) + pan, size * scale * zoom);
                queue.push((page, target, (target.center().x - available.center().x) > 0.));
            }
        }
        if queue.is_empty() {
            if let Some(tex) = self.pages.get(&(id.clone(), self.page)) {
                let size = tex.shown_size(crop);
                let scale = (available.width() / size.x).min(available.height() / size.y);
                queue.push((self.page,
                            Rect::from_center_size(available.center() + pan, size * scale * zoom),
                            false));
            }
        }
        // Chrome leads out on its own fast fade while the bands close.
        let chrome_t = self.controls_anim.map(|t| t.value(now))
            .unwrap_or(if self.controls { 1. } else { 0. })
            * (1. - ((now - started) / (close.chrome_ms as f64 / 1000.).max(0.001)).clamp(0., 1.) as f32);
        let dir = motion::close_dir(self.rtl);
        let (dx, dy) = motion::spread_offset(p);
        let shift = vec2(dx * dir, dy);
        // The peel ridge rides the retreating edge (1-p) and never drops
        // below the live drag/turn phase, mirroring the entry's max().
        let phase = (1. - p).max((slide_x.abs() / page_viewport.width().max(1.)).clamp(0., 1.));
        let canvas = self.reader_canvas_color();
        for band in shelf::spread_clip(area, p, dir) {
            let clipped = ui.painter().with_clip_rect(band);
            clipped.rect_filled(band, 0., canvas);
            let pages_painter = clipped.with_clip_rect(page_viewport);
            for (page, target, mirror) in &queue {
                if let Some(tex) = self.pages.get(&(id.clone(), *page)) {
                    tex.paint_flip(&pages_painter, target.translate(shift), crop, phase, *mirror);
                }
            }
            if chrome_t <= 0.001 { continue; }
            let mut ghost = clipped.clone(); // chrome ghost, fading out
            ghost.multiply_opacity(chrome_t);
            let chrome = self.reader_chrome(&ghost, area, &book, &id, -76. * (1. - chrome_t), 72. * (1. - chrome_t));
            self.paint_reader_chrome(&ghost, &chrome, &book, &id);
        }
    }
    /// Under-screen content stays hidden until the band coverage drops
    /// below `close.under_alpha_start`, then settles in smoothly.
    fn exit_under_alpha(&self, now: f64) -> f32 {
        let Some((armed, _)) = self.reader_exit else {
            return 1.;
        };
        let close = crate::params::params().close.clone();
        let started = if armed.is_infinite() { now } else { armed };
        let raw = ((now - started) / motion::close_seconds().max(0.001)).clamp(0., 1.) as f32;
        let p = motion::close_p(raw);
        let uas = close.under_alpha_start.clamp(0.01, 1.);
        motion::smooth(((uas - p) / uas).clamp(0., 1.))
    }
    fn reader_ui(&mut self, ui: &mut Ui, area: Rect, id: &str, now: f64) {
        let Some(book) = self.reader_book(id) else {
            return;
        };
        let ctx = ui.ctx().clone();
        ui.painter().rect_filled(area, 0., self.reader_canvas_color());
        self.ask_page(id, self.page);
        let step: i32 = if self.spread && !self.vertical && area.width() > 700. {
            if self.spread_offset && self.page == 0 {
                1
            } else {
                2
            }
        } else {
            1
        };
        let (pair_base, _) = spread_pair(self.page, self.spread_offset && step != 1);
        if pair_base + 1 < book.page_count {
            self.ask_page(id, pair_base + 1);
        }
        if pair_base != self.page {
            self.ask_page(id, pair_base);
        }
        // `page_viewport` never moves when the chrome toggles — all page
        // scaling, centering, panning and the dim overlay use it, so showing
        // or hiding the bars never resizes the comic. `canvas_hit` is the
        // gesture surface only, and still yields to the visible chrome.
        let page_viewport = area.shrink2(vec2(8., 0.));
        // The page always fits inside the band between the chrome bars, so it
        // can never slide underneath them and toggling chrome never resizes
        // the comic. `canvas_hit` is the gesture surface only, and still
        // yields to the visible chrome.
        let page_fit = area.shrink2(vec2(8., 64.));
        let canvas_hit = area.shrink2(vec2(8., if self.controls { 64. } else { 0. }));
        let input_blocked = self.reader_input_blocked();
        let gesture = ui.interact(
            canvas_hit,
            Id::unique("reader-canvas"),
            if input_blocked {
                Sense::hover()
            } else {
                Sense::click_and_drag()
            },
        );
        self.hitboxes.insert("reader-canvas".into(), canvas_hit);
        let fit_single = self.fit_width && !self.vertical && step == 1;
        let page_swipe = !self.vertical && self.zoom <= 1. && !fit_single;
        let zoom = if input_blocked {
            1.
        } else {
            ui.input(|i| i.zoom_delta())
        };
        if zoom != 1. {
            self.zoom_anim.cancel();
            self.page_slide.cancel();
            self.zoom = (self.zoom * zoom).clamp(1., 5.);
            self.pending_reader_tap = None;
            if self.zoom == 1. {
                self.pan = Vec2::ZERO;
            }
        }
        if let Some((zoom, [px, py])) = self.zoom_anim.sample(now) {
            self.zoom = zoom;
            self.pan = vec2(px, py);
        }
        if gesture.drag_started() {
            self.reader_drag = 0.;
            self.reader_drag_y = 0.;
            self.vertical_reader.stop();
            self.pending_reader_tap = None;
            if page_swipe {
                self.page_slide.begin_drag();
                // Warm the neighbours so a committed swipe lands on a
                // decoded page instead of a placeholder.
                if self.page + (step as usize) < book.page_count {
                    self.ask_page(id, self.page + step as usize);
                }
                if self.page >= step as usize {
                    self.ask_page(id, self.page - step as usize);
                }
            }
        }
        if gesture.dragged() {
            let delta = ui.input(|i| i.pointer.delta());
            self.reader_drag += delta.x;
            self.reader_drag_y += delta.y;
            if page_swipe {
                let can_prev = self.page > 0 || self.has_neighbor(id, -1);
                let can_next = self.page + (step as usize) < book.page_count
                    || self.has_neighbor(id, 1);
                self.page_slide.drag(
                    delta.x,
                    can_prev,
                    can_next,
                    self.rtl,
                    page_viewport.width(),
                );
            }
        }
        if gesture.dragged() && (self.zoom > 1. || fit_single) {
            let delta = ui.input(|i| i.pointer.delta());
            self.zoom_anim.cancel();
            self.pan += if self.vertical {
                vec2(delta.x, 0.)
            } else if self.zoom > 1. {
                delta
            } else {
                vec2(0., delta.y)
            };
        }
        if gesture.drag_stopped() && self.zoom <= 1. && !self.vertical {
            let distance = self.reader_drag;
            let distance_y = self.reader_drag_y;
            self.reader_drag = 0.;
            self.reader_drag_y = 0.;
            if page_swipe {
                let flip = crate::params::params().flip.clone();
                let release_vx = ui.input(|i| i.pointer.velocity().x);
                let committed = distance.abs() > flip.commit_px
                    || (release_vx.abs() >= flip.commit_vx
                        && release_vx.signum() == distance.signum());
                let distance = if distance.abs() > flip.commit_px {
                    distance
                } else {
                    release_vx
                };
                if committed {
                    let old_page = self.page;
                    let old_book = id.to_owned();
                    let drag_off = self.page_slide.current_offset(now);
                    self.next_page(
                        id,
                        if (distance < 0.) ^ self.rtl {
                            step
                        } else {
                            -step
                        },
                        now,
                        page_viewport.width(),
                    );
                    if self.page != old_page
                        && matches!(&self.screen, Screen::Reader(current) if current == &old_book)
                    {
                        // The neighbour followed the finger one strip width
                        // away; it now settles from where the drag left it.
                        let sign = if (distance < 0.) ^ self.rtl { 1. } else { -1. };
                        let start_offset = drag_off
                            + if drag_off < 0. {
                                page_viewport.width()
                            } else {
                                -page_viewport.width()
                            };
                        self.page_slide.page = self.page;
                        self.slide_turn_gap = sign * page_viewport.width();
                        self.page_slide
                            .animate_turn(old_page, start_offset, now, self.reduced);
                    } else {
                        self.page_slide.rebound(now, self.reduced);
                    }
                } else {
                    self.page_slide.rebound(now, self.reduced);
                }
            } else if distance.abs() > 45. {
                self.next_page(
                    id,
                    if (distance < 0.) ^ self.rtl {
                        step
                    } else {
                        -step
                    },
                    now,
                    page_viewport.width(),
                );
            } else if fit_single && distance_y.abs() > 45. {
                // Once the fitted page cannot pan any further, a vertical
                // swipe turns like the usual next/previous controls.
                if distance_y < 0. && self.pan.y <= -self.reader_fit_limit + 1. {
                    self.next_page(id, step, now, page_viewport.width());
                } else if distance_y > 0. && self.pan.y >= self.reader_fit_limit - 1. {
                    self.next_page(id, -step, now, page_viewport.width());
                }
            }
        }
        if input_blocked {
            self.pending_reader_tap = None;
            self.vertical_reader.stop();
        } else {
            // Wait for the same double-click interval as egui. Otherwise the
            // first tap turns a page before the second can request zoom.
            let tapped = gesture
                .clicked()
                .then(|| gesture.interact_pointer_pos())
                .flatten();
            let double_tap = tapped.is_some_and(|pos| {
                self.pending_reader_tap.is_some_and(|(deadline, first)| {
                    now <= deadline
                        && first.distance(pos)
                            <= ctx.options(|options| options.input_options.max_click_dist)
                })
            });
            if double_tap {
                self.pending_reader_tap = None;
                if let Some(pos) = tapped {
                    if self.zoom <= 1.01 {
                        // Focal zoom: the tapped point stays put as the page
                        // doubles, then the target pan is clamped to the 2x
                        // travel limits so nothing lands off-canvas.
                        let target_zoom = 2.0_f32;
                        let mut target_pan =
                            (page_viewport.center() - pos) * (target_zoom - 1.);
                        if let Some(texture) = self.pages.get(&(id.into(), self.page)) {
                            let size = texture.shown_size(self.crop);
                            let scale = (page_viewport.width() / size.x)
                                .min(page_viewport.height() / size.y);
                            let limit_2x = ((size * scale * target_zoom
                                - page_viewport.size())
                                * 0.5)
                                .max(Vec2::ZERO);
                            target_pan.x = target_pan.x.clamp(-limit_2x.x, limit_2x.x);
                            target_pan.y = target_pan.y.clamp(-limit_2x.y, limit_2x.y);
                        }
                        self.zoom_anim.start(
                            self.zoom,
                            target_zoom,
                            [self.pan.x, self.pan.y],
                            [target_pan.x, target_pan.y],
                            now,
                            self.reduced,
                        );
                    } else {
                        self.zoom_anim.start(
                            self.zoom,
                            1.0,
                            [self.pan.x, self.pan.y],
                            [0., 0.],
                            now,
                            self.reduced,
                        );
                    }
                }
            }
            if let Some((deadline, pos)) = self.pending_reader_tap {
                if now >= deadline && !ctx.input(|i| i.pointer.primary_down()) {
                    self.pending_reader_tap = None;
                    self.reader_tap(
                        id,
                        (pos.x - canvas_hit.left()) / canvas_hit.width(),
                        step,
                        now,
                        page_viewport.width(),
                    );
                } else {
                    ctx.request_repaint_after(std::time::Duration::from_secs_f64(
                        (deadline - now).max(0.01),
                    ));
                }
            }
            if !double_tap && let Some(pos) = tapped {
                if let Some((_, first)) = self.pending_reader_tap.take() {
                    self.reader_tap(
                        id,
                        (first.x - canvas_hit.left()) / canvas_hit.width(),
                        step,
                        now,
                        page_viewport.width(),
                    );
                }
                let delay = ctx.options(|options| options.input_options.max_double_click_delay);
                self.pending_reader_tap = Some((now + delay, pos));
                ctx.request_repaint_after(std::time::Duration::from_secs_f64(delay));
            }
        }
        if !input_blocked && !ctx.egui_wants_keyboard_input() {
            let delta = ctx.input(|i| {
                if !self.vertical && (i.key_pressed(Key::PageDown) || i.key_pressed(Key::Space)) {
                    step
                } else if !self.vertical && i.key_pressed(Key::PageUp) {
                    -step
                } else if i.key_pressed(Key::ArrowRight) {
                    if self.rtl { -step } else { step }
                } else if i.key_pressed(Key::ArrowLeft) {
                    if self.rtl { step } else { -step }
                } else {
                    0
                }
            });
            if delta != 0 {
                self.next_page(id, delta, now, page_viewport.width());
            }
        }
        if !matches!(&self.screen,Screen::Reader(current) if current==id) {
            return;
        }
        self.rendered_page_nodes = 0;
        let slide_x = if self.vertical || self.zoom > 1. {
            0.
        } else {
            self.page_slide.current_offset(now)
        };
        self.page_slide.tick(now);
        // Detail -> reader peel-entry progress. The armed sentinel discharges
        // here — outside the textured-page branch — so vertical or
        // not-yet-decoded readers still finish the entry and stop painting.
        let flip = crate::params::params().flip.clone();
        let entry_p = if let Some(start) = self.reader_entry {
            let started = if start.is_infinite() {
                self.reader_entry = Some(now);
                now
            } else {
                start
            };
            let p = motion::flip_entry_ease(
                ((now - started) / (flip.entry_ms as f64 / 1000.).max(0.001)).clamp(0., 1.)
                    as f32,
            );
            if p >= 1. {
                self.reader_entry = None;
            }
            p
        } else {
            1.
        };
        if self.vertical {
            self.vertical_ui(ui, page_viewport, id, book.page_count, now, &gesture);
        } else if let Some(error) = self.page_errors.get(&(id.into(), self.page)).cloned() {
            ui.put(
                Rect::from_center_size(
                    page_viewport.center(),
                    vec2(page_viewport.width() - 40., 120.),
                ),
                Label::new(
                    RichText::new(format!("这一页暂时无法打开\n{error}")).color(Color32::WHITE),
                )
                .wrap(),
            );
            if self.button(
                ui,
                "retry-page",
                Rect::from_center_size(page_viewport.center() + vec2(0., 90.), vec2(144., 44.)),
                "重试这一页",
                false,
            ) {
                self.page_errors.remove(&(id.into(), self.page));
                self.ask_page(id, self.page);
            }
        } else if let Some(texture) = self.pages.get(&(id.into(), self.page)) {
            let pair = step == 2;
            let (base, partner) = if pair {
                spread_pair(self.page, self.spread_offset)
            } else {
                (self.page, None)
            };
            let first_on_right = pair && self.rtl;
            let available = if pair {
                Rect::from_min_size(
                    page_fit.min
                        + vec2(
                            if first_on_right {
                                page_fit.width() / 2.
                            } else {
                                0.
                            },
                            0.,
                        ),
                    vec2(page_fit.width() / 2., page_fit.height()),
                )
            } else {
                page_fit
            };
            let painter = ui.painter().with_clip_rect(page_viewport);
            let crop = self.crop;
            // Page paints are queued so the detail->reader entry can replay
            // them behind peel clip bands; `phase` drives the flip-depth
            // ridge (|offset|/page width), `mirror` puts it on the left.
            let w = page_viewport.width().max(1.);
            let phase = (slide_x.abs() / w).clamp(0., 1.);
            // Page paints are queued as page indices (re-resolved at flush)
            // so nothing here borrows `self.pages` across the mutable
            // `ask_page`/button calls below.
            let mut queue: Vec<(usize, Rect, bool)> = Vec::new();
            fn slot(
                page: usize,
                texture: &PageTexture,
                available: Rect,
                crop: bool,
                center: Pos2,
                pan: Vec2,
                zoom: f32,
                queue: &mut Vec<(usize, Rect, bool)>,
            ) -> Rect {
                let size = texture.shown_size(crop);
                let scale = (available.width() / size.x).min(available.height() / size.y);
                let target = Rect::from_center_size(center + pan, size * scale * zoom);
                let mirror = (target.center().x - available.center().x) > 0.;
                queue.push((page, target, mirror));
                target
            }
            if base == self.page {
                let size = texture.shown_size(self.crop);
                let scale = if fit_single {
                    available.width() / size.x
                } else {
                    (available.width() / size.x).min(available.height() / size.y)
                };
                let size = size * scale * self.zoom;
                let limit = ((size - available.size()) * 0.5).max(Vec2::ZERO);
                self.reader_fit_limit = if pair { 0. } else { limit.y };
                self.pan.x = self.pan.x.clamp(-limit.x, limit.x);
                self.pan.y = self.pan.y.clamp(-limit.y, limit.y);
                let image_rect = Rect::from_center_size(
                    available.center() + self.pan + vec2(slide_x, 0.),
                    size,
                );
                self.hitboxes.insert("reader-image".into(), image_rect);
                let mirror = (image_rect.center().x - available.center().x) > 0.;
                queue.push((self.page, image_rect, mirror));
                self.rendered_page_nodes += 1;
            } else {
                self.reader_fit_limit = 0.;
                if let Some(first) = self.pages.get(&(id.into(), base)) {
                    slot(
                        base,
                        first,
                        available,
                        crop,
                        available.center() + vec2(slide_x, 0.),
                        self.pan,
                        self.zoom,
                        &mut queue,
                    );
                    self.rendered_page_nodes += 1;
                } else {
                    painter.text(
                        available.center(),
                        Align2::CENTER_CENTER,
                        "正在打开这一页…",
                        FontId::proportional(15.),
                        Color32::LIGHT_GRAY,
                    );
                }
            }
            if let Some(partner) = partner {
                let second_center = available.center()
                    + vec2(
                        if first_on_right {
                            -available.width()
                        } else {
                            available.width()
                        },
                        0.,
                    )
                    + vec2(slide_x, 0.);
                if let Some(texture) = self.pages.get(&(id.into(), partner)) {
                    let target = slot(
                        partner,
                        texture,
                        available,
                        crop,
                        second_center,
                        self.pan,
                        self.zoom,
                        &mut queue,
                    );
                    if partner == self.page {
                        self.hitboxes.insert("reader-image".into(), target);
                    }
                    self.rendered_page_nodes += 1;
                } else if partner < book.page_count {
                    if let Some(error) = self
                        .page_errors
                        .get(&(id.to_string(), partner))
                        .cloned()
                    {
                        ui.put(
                            Rect::from_center_size(
                                second_center,
                                vec2(available.width() - 28., 130.),
                            ),
                            Label::new(
                                RichText::new(format!("第 {} 页无法打开\n{error}", partner + 1))
                                    .color(Color32::WHITE),
                            )
                            .wrap(),
                        );
                        if self.button(
                            ui,
                            "retry-spread-page",
                            Rect::from_center_size(second_center + vec2(0., 94.), vec2(144., 44.)),
                            "重试这一页",
                            false,
                        ) {
                            self.page_errors.remove(&(id.to_string(), partner));
                            self.prefetch_blocked.remove(&(id.to_string(), partner));
                            self.ask_page(id, partner);
                        }
                    } else {
                        ui.painter().text(
                            second_center,
                            Align2::CENTER_CENTER,
                            "正在打开下一页…",
                            FontId::proportional(15.),
                            Color32::LIGHT_GRAY,
                        );
                    }
                }
            }
            // A committed turn slides the outgoing page away one strip gap
            // behind the incoming one (single-page turns only).
            if step == 1
                && let Some((from_page, _)) = self.page_slide.outgoing(now)
                && let Some(outgoing) = self.pages.get(&(id.into(), from_page))
            {
                slot(
                    from_page,
                    outgoing,
                    available,
                    crop,
                    available.center() + vec2(slide_x - self.slide_turn_gap, 0.),
                    self.pan,
                    self.zoom,
                    &mut queue,
                );
                self.rendered_page_nodes += 1;
            }
            // While the finger holds, the page the drag reveals follows at
            // exactly one strip width away.
            if step == 1 && gesture.dragged() && slide_x.abs() > 2. {
                let reveals_prev = if self.rtl { slide_x < 0. } else { slide_x > 0. };
                let neighbor = if reveals_prev {
                    self.page.checked_sub(1)
                } else {
                    (self.page + 1 < book.page_count).then_some(self.page + 1)
                };
                let neighbor_side = if slide_x > 0. { -1. } else { 1. };
                if let Some(neighbor) = neighbor
                    && let Some(texture) = self.pages.get(&(id.into(), neighbor))
                {
                    slot(
                        neighbor,
                        texture,
                        available,
                        crop,
                        available.center()
                            + vec2(slide_x + neighbor_side * page_viewport.width(), 0.),
                        self.pan,
                        self.zoom,
                        &mut queue,
                    );
                    self.rendered_page_nodes += 1;
                }
            }
            // Flush the queued pages. During the detail->reader entry the
            // reader is treated as one incoming plane: the eased peel front
            // reveals it through clip bands (the R3 widget-tree fallback),
            // while the depth ridge hugs the sweeping reveal edge. After
            // the entry the same paints run flat and unbanded.
            if entry_p >= 1. {
                for (page, target, mirror) in &queue {
                    if let Some(tex) = self.pages.get(&(id.into(), *page)) {
                        tex.paint_flip(&painter, *target, crop, phase, *mirror);
                    }
                }
            } else {
                let dir = if self.rtl { -1. } else { 1. };
                let (dx, dy) = motion::spread_offset(entry_p);
                let shift = vec2(dx * dir, dy);
                for band in shelf::spread_clip(page_viewport, entry_p, dir) {
                    let clipped = painter.with_clip_rect(band);
                    for (page, target, mirror) in &queue {
                        if let Some(tex) = self.pages.get(&(id.into(), *page)) {
                            tex.paint_flip(
                                &clipped,
                                target.translate(shift),
                                crop,
                                (1. - entry_p).max(phase),
                                *mirror,
                            );
                        }
                    }
                }
            }
        } else {
            let text = self
                .stream_waiting_text(id, self.page)
                .unwrap_or_else(|| "正在打开这一页…".into());
            ui.put(
                Rect::from_center_size(
                    page_viewport.center(),
                    vec2(page_viewport.width() - 40., 120.),
                ),
                Label::new(RichText::new(text).size(15.).color(Color32::LIGHT_GRAY)).wrap(),
            );
        }
        // Dragging past the chapter's first or last page surfaces a quiet
        // boundary hint at the edge being pulled open.
        if !self.vertical
            && step == 1
            && gesture.dragged()
            && slide_x.abs() > 28.
        {
            let reveals_prev = if self.rtl { slide_x < 0. } else { slide_x > 0. };
            let past_edge = if reveals_prev {
                self.page == 0
            } else {
                self.page + 1 >= book.page_count
            };
            if past_edge {
                let hint = if self.has_neighbor(id, if reveals_prev { -1 } else { 1 }) {
                    if reveals_prev {
                        "松手进入上一章"
                    } else {
                        "松手进入下一章"
                    }
                } else if reveals_prev {
                    "已是第一页"
                } else {
                    "已是最后一页"
                };
                let galley = ui.painter().layout(
                    hint.into(),
                    FontId::proportional(13.),
                    Color32::WHITE,
                    page_viewport.width() - 40.,
                );
                let pill_size = vec2(galley.size().x + 30., galley.size().y + 18.);
                let pill = Rect::from_center_size(
                    pos2(
                        if slide_x > 0. {
                            page_viewport.left() + pill_size.x / 2. + 10.
                        } else {
                            page_viewport.right() - pill_size.x / 2. - 10.
                        },
                        page_viewport.center().y,
                    ),
                    pill_size,
                );
                crate::design::soft_shadow(ui.painter(), pill, pill_size.y / 2., 1., true);
                ui.painter()
                    .rect_filled(pill, pill_size.y / 2., Color32::from_black_alpha(215));
                ui.painter().galley(
                    pos2(pill.left() + 15., pill.center().y - galley.size().y / 2.),
                    galley,
                    Color32::WHITE,
                );
            }
        }
        if self.brightness < 1. {
            ui.painter().rect_filled(
                page_viewport,
                0.,
                Color32::from_black_alpha(((1. - self.brightness) * 255.).round() as u8),
            );
        }
        if !matches!(&self.screen, Screen::Reader(current) if current == id) {
            return;
        }
        if let Some((task, reason)) = self
            .stream
            .as_ref()
            .filter(|stream| stream.book.id == id)
            .and_then(|stream| stream.stopped.clone().map(|reason| (stream.task.clone(), reason)))
        {
            let banner = Rect::from_center_size(
                pos2(page_viewport.center().x, page_viewport.bottom() - 60.),
                vec2((page_viewport.width() - 24.).min(420.), 96.),
            );
            ui.painter().rect_filled(banner, 12., Color32::from_black_alpha(200));
            ui.put(
                Rect::from_min_size(banner.min + vec2(12., 8.), vec2(banner.width() - 24., 36.)),
                Label::new(RichText::new(format!("下载中断：{reason}")).color(Color32::WHITE)).truncate(),
            );
            if self.button(
                ui,
                "stream-resume",
                Rect::from_center_size(pos2(banner.center().x, banner.bottom() - 30.), vec2(160., 44.)),
                "继续下载",
                true,
            ) {
                self.sources.resume_stream(&task);
            }
        }
        // The chrome fades and slides over a fixed page viewport, so it can
        // never make the comic jump. Interactive controls only register
        // while `controls` is on; a fading ghost carries them out.
        let chrome_t = self
            .controls_anim
            .map(|t| t.value(now))
            .unwrap_or(if self.controls { 1. } else { 0. });
        if self.controls_anim.is_some_and(|t| t.done(now)) {
            self.controls_anim = None;
        }
        if chrome_t > 0.001 {
            let palette = crate::design::palette(true);
            ui.style_mut().visuals = crate::design::visuals(true);
            let chrome = self.reader_chrome(ui.painter(), area, &book, id, -76. * (1. - chrome_t), 72. * (1. - chrome_t));
            if self.controls {
                self.paint_reader_chrome(ui.painter(), &chrome, &book, id);
                // back() defers: it flips is_reader mid-paint (chip glyphs).
                let go_back = self.icon(
                    ui,
                    "reader-back",
                    chrome.back,
                    "back",
                    match (&self.reader_return, self.tab) {
                        (Screen::Library, Tab::History) => "返回阅读足迹",
                        (Screen::Library, Tab::Sources) => "返回书源",
                        (Screen::Library, _) => "返回书架",
                        _ => "返回详情",
                    },
                );
                if self.chapter(id).is_some()
                    && self.icon(ui, "reader-chapters", chrome.chapters, "chapters", "已下载章节") {
                    self.chapter_dialog = Some(id.to_string());
                }
                if self.icon(ui, "reader-settings", chrome.settings, "settings", "阅读设置") {
                    self.reader_options = true;
                    self.vertical_reader.stop();
                    self.pending_reader_tap = None;
                }
                if self.icon(ui, "reader-prev", chrome.prev, "prev", "上一页") {
                    self.next_page(id, -step, now, page_viewport.width());
                }
                if self.icon(ui, "reader-next", chrome.next, "next", "下一页") {
                    self.next_page(id, step, now, page_viewport.width());
                }
                let mut page_f32 = self.page as f32 + 1.;
                let slider_rect = chrome.slider;
                let out = ui
                    .scope_builder(UiBuilder::new().max_rect(slider_rect), |ui| {
                        crate::design::slider(
                            ui,
                            "reader-page-slider",
                            &mut page_f32,
                            1.0..=(book.page_count.max(1) as f32),
                            slider_rect.width(),
                            self.rtl,
                            true,
                        )
                    })
                    .inner;
                self.hitboxes.insert("reader-page-slider".into(), out.rect);
                if out.changed {
                    let target = (page_f32.round() as usize).saturating_sub(1);
                    self.next_page(
                        id,
                        target as i32 - self.page as i32,
                        now,
                        page_viewport.width(),
                    );
                    self.page_slide.cancel();
                }
                if out.dragging {
                    let text =
                        format!("第 {} / {} 页", page_f32.round() as usize, book.page_count);
                    let galley =
                        ui.painter().layout_no_wrap(text, FontId::proportional(13.), palette.ink);
                    let bubble_size = vec2(galley.size().x + 26., galley.size().y + 14.);
                    let thumb_x = out.thumb_x.clamp(
                        area.left() + bubble_size.x / 2. + 6.,
                        area.right() - bubble_size.x / 2. - 6.,
                    );
                    let bubble = Rect::from_center_size(
                        pos2(thumb_x, chrome.dock.top() - 22.),
                        bubble_size,
                    );
                    crate::design::soft_shadow(ui.painter(), bubble, bubble_size.y / 2., 1.4, true);
                    ui.painter()
                        .rect_filled(bubble, bubble_size.y / 2., palette.surface);
                    ui.painter().rect_stroke(
                        bubble,
                        bubble_size.y / 2.,
                        Stroke::new(1., palette.line),
                        StrokeKind::Inside,
                    );
                    ui.painter()
                        .galley(bubble.center() - galley.size() / 2., galley, palette.ink);
                }
                if go_back { self.back(now, area); }
            } else {
                let mut ghost = ui.painter().clone(); // fading chrome ghost
                ghost.multiply_opacity(chrome_t);
                self.paint_reader_chrome(&ghost, &chrome, &book, id);
            }
        }
    }
    fn vertical_ui(
        &mut self,
        ui: &mut Ui,
        content: Rect,
        id: &str,
        count: usize,
        now: f64,
        gesture: &Response,
    ) {
        let input_blocked = self.reader_input_blocked();
        let scroll = if input_blocked {
            0.
        } else {
            ui.input(|i| i.smooth_scroll_delta.y)
        };
        self.vertical_reader.scroll(-scroll);
        if gesture.dragged() {
            self.vertical_reader
                .scroll(-ui.input(|i| i.pointer.delta().y));
        }
        if gesture.drag_stopped() {
            self.vertical_reader
                .release(-ui.input(|i| i.pointer.velocity().y), now, self.reduced);
        }
        if !input_blocked
            && !ui.ctx().egui_wants_keyboard_input()
            && ui.input(|i| i.key_pressed(Key::PageDown) || i.key_pressed(Key::Space))
        {
            self.vertical_reader.scroll(content.height() * 0.85);
        }
        if !input_blocked
            && !ui.ctx().egui_wants_keyboard_input()
            && ui.input(|i| i.key_pressed(Key::PageUp))
        {
            self.vertical_reader.scroll(-content.height() * 0.85);
        }
        if self.vertical_reader.tick(now) {
            ui.ctx().request_repaint();
        }
        let width = content.width() * self.zoom;
        self.pan.x = self.pan.x.clamp(
            -(width - content.width()) / 2.,
            (width - content.width()) / 2.,
        );
        // The strip is the current chapter plus at most one following
        // segment; global indices run across both. The next chapter is
        // queued once the anchor reaches its last pages, and joins the
        // strip as soon as its page count is known — until then a thin
        // band below the last page stands in for it.
        let prev_count = self.vertical_prev.as_ref().map(|(_, c)| *c).unwrap_or(0);
        let near_end = self.vertical_reader.page.saturating_sub(prev_count) + 3 >= count;
        let peek = self.peek_next(id, near_end);
        let loading_next = matches!(peek, Peek::Loading);
        match peek {
            Peek::Ready(seg_id, seg_count, seg_meta) => {
                if self
                    .vertical_next
                    .as_ref()
                    .is_none_or(|(old, _)| old != &seg_id)
                {
                    self.next_stream =
                        seg_meta.and_then(|meta| self.stream_segment_book(&seg_id, &meta));
                }
                self.vertical_next = Some((seg_id, seg_count));
            }
            Peek::Loading => {}
            Peek::Missing => {
                self.vertical_next = None;
                self.next_stream = None;
            }
        }
        let footer = if loading_next { 56. } else { 0. };
        let mut segments = self.strip_segments();
        let mut total: usize = segments.iter().map(|(_, c)| *c).sum();
        let mut heights = self.strip_heights(&segments, width);
        self.vertical_reader.settle(total, content.height() - footer, |g| {
            heights.get(&g).copied()
        });
        // The anchor entered the following segment: that chapter becomes
        // the reader's book, keeping the exact scroll offset.
        let cur_id = if self.vertical_reader.page >= prev_count + count
            && self.vertical_next.is_some()
        {
            let (new_id, _) = self.cross_vertical(id, count);
            segments = self.strip_segments();
            total = segments.iter().map(|(_, c)| *c).sum();
            heights = self.strip_heights(&segments, width);
            new_id
        } else {
            id.to_string()
        };
        let g = self.vertical_reader.page;
        if let Some((seg, local)) = Self::locate(&segments, g)
            && seg == cur_id
            && self.page != local
        {
            self.page = local;
            self.prefetch_blocked.clear();
        }
        self.trim_pages();
        for global in g.saturating_sub(1)..=(g + 2).min(total.saturating_sub(1)) {
            if let Some((seg, local)) = Self::locate(&segments, global) {
                self.ask_page(seg, local);
            }
        }
        let height = heights.get(&g).copied().unwrap_or(content.height());
        let mut top = content.top() - self.vertical_reader.top_offset(height);
        let start = g.saturating_sub(1);
        if start < g {
            top -= heights.get(&start).copied().unwrap_or(content.height());
        }
        let painter = ui.painter().with_clip_rect(content);
        for global in start..=(g + 2).min(total.saturating_sub(1)) {
            let Some((seg, local)) = Self::locate(&segments, global) else {
                continue;
            };
            let height = heights.get(&global).copied().unwrap_or(content.height());
            let target = Rect::from_min_size(
                pos2(content.center().x - width / 2. + self.pan.x, top),
                vec2(width, height),
            );
            top += height;
            if !target.intersects(content) {
                continue;
            }
            self.hitboxes
                .insert(format!("vertical-page-{global}"), target);
            if let Some(texture) = self.pages.get(&(seg.to_string(), local)) {
                texture.paint(&painter, target, self.crop);
                self.rendered_page_nodes += 1;
            } else if let Some(error) = self
                .page_errors
                .get(&(seg.to_string(), local))
                .cloned()
            {
                let center = target.intersect(content).center();
                ui.put(
                    Rect::from_center_size(center, vec2(content.width() - 40., 120.)),
                    Label::new(
                        RichText::new(format!("第 {} 页无法打开\n{error}", local + 1))
                            .color(Color32::WHITE),
                    )
                    .wrap(),
                );
                if self.button(
                    ui,
                    &format!("retry-page-{global}"),
                    Rect::from_center_size(center + vec2(0., 82.), vec2(144., 44.)),
                    "重试这一页",
                    false,
                ) {
                    self.page_errors.remove(&(seg.to_string(), local));
                    self.ask_page(seg, local);
                }
            } else {
                let text = self
                    .stream_waiting_text(seg, local)
                    .unwrap_or_else(|| "正在打开这一页…".into());
                painter.text(
                    target.intersect(content).center(),
                    Align2::CENTER_CENTER,
                    text,
                    FontId::proportional(15.),
                    Color32::LIGHT_GRAY,
                );
            }
        }
        if let Some((seg, local)) = Self::locate(&segments, g)
            && self.pages.contains_key(&(seg.to_string(), local))
            && (now - self.last_position_save >= 0.25
                || (!gesture.dragged() && !self.vertical_reader.active() && scroll.abs() < 0.1))
        {
            self.record_anchor(seg, local);
            self.last_position_save = now;
        }
        if loading_next
            && let Some(last) = self
                .hitboxes
                .get(&format!("vertical-page-{}", total.saturating_sub(1)))
                .copied()
        {
            let band = Rect::from_min_size(
                pos2(
                    content.center().x - (content.width() - 24.).min(420.) / 2.,
                    last.bottom() + 8.,
                ),
                vec2((content.width() - 24.).min(420.), 40.),
            );
            if band.intersects(content) {
                ui.painter().rect_filled(band, 10., Color32::from_black_alpha(150));
                ui.painter().text(
                    band.center(),
                    Align2::CENTER_CENTER,
                    "正在加载下一章…",
                    FontId::proportional(14.),
                    Color32::LIGHT_GRAY,
                );
            }
        }
        let first_id = segments
            .first()
            .map(|(seg, _)| seg.as_str())
            .unwrap_or(cur_id.as_str());
        if g == 0 && self.has_neighbor(first_id, -1) {
            let button = Rect::from_center_size(
                pos2(content.center().x, content.top() + 30.),
                vec2((content.width() - 24.).min(300.), 44.),
            );
            if self.button(ui, "reader-prev-chapter", button, "上一章", true) {
                self.open_neighbor(first_id, -1);
            }
        }
    }
    fn history_ui(&mut self, ui: &mut Ui, area: Rect) {
        let palette = crate::design::palette(self.dark);
        let mut column = Self::page_column(area, 112. + 20. * (self.font_scale - 1.), 88.);
        let mut books: Vec<_> = self
            .books
            .iter()
            .filter(|b| b.last_read_at.is_some())
            .cloned()
            .collect();
        books.sort_by_key(|b| std::cmp::Reverse(b.last_read_at));
        let introduction = ui.painter().layout(
            if books.is_empty() {
                "下一次打开，从离开的那一页继续。"
            } else {
                "最近读过的故事，都在这里。"
            }
            .into(),
            FontId::proportional(14. * self.font_scale),
            palette.muted,
            column.width(),
        );
        column.min.y = area.top() + 82. + introduction.size().y + 20.;
        ui.painter().galley(
            pos2(column.left(), area.top() + 82.),
            introduction,
            palette.muted,
        );
        ui.scope_builder(UiBuilder::new().max_rect(column), |ui| {
            ScrollArea::vertical()
                .id_salt("reading-history")
                .show(ui, |ui| {
                    if books.is_empty() {
                        // P2-1：空卡放到可视区约 40% 处——视觉居中略偏上，
                        // 不再贴顶、下面也不留整屏死白。卡高约 160pt。
                        ui.add_space(((ui.available_height() - 190.) * 0.4).max(0.));
                        crate::design::surface(ui).show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.add_space(24.);
                            crate::design::section(ui, "还没有阅读记录");
                            ui.label("打开书架上的一本漫画，读到哪里就会记到哪里。");
                            ui.add_space(16.);
                            if ui
                                .add(crate::design::primary("去书架看看", self.dark))
                                .clicked()
                            {
                                self.tab = Tab::Library;
                            }
                            ui.add_space(16.);
                        });
                    }
                    for book in books {
                        let narrow = ui.available_width() < 380. || self.font_scale > 1.4;
                        let height = if narrow {
                            156. * self.font_scale.sqrt()
                        } else {
                            116.
                        };
                        let (rect, _) = ui.allocate_exact_size(
                            vec2(ui.available_width(), height),
                            Sense::hover(),
                        );
                        if !ui.is_rect_visible(rect) {
                            continue;
                        }
                        let cover_id = self.cover_id(&book.id);
                        self.ask_cover(&cover_id);
                        crate::design::soft_shadow(ui.painter(), rect, 18., 0.8, self.dark);
                        ui.painter().rect_filled(rect, 18., palette.surface);
                        ui.painter().rect_stroke(
                            rect,
                            18.,
                            Stroke::new(1., palette.line.gamma_multiply(0.5)),
                            StrokeKind::Inside,
                        );
                        let cover = Rect::from_min_size(rect.min + vec2(14., 14.), vec2(58., 82.));
                        if let Some(texture) = shelf::cover_pick(
                            self.cover_mips.get(&cover_id),
                            self.covers.get(&cover_id),
                            Plane::rect(cover),
                            cover.size(),
                            ui.ctx().pixels_per_point(),
                        ) {
                            shelf::paint_art(
                                ui.painter(),
                                Plane::rect(cover),
                                cover.size(),
                                texture,
                                palette.soft,
                            );
                        } else {
                            ui.painter().rect_filled(cover, 7., palette.soft);
                        }
                        let text_pos = rect.min + vec2(88., 16.);
                        let mut title = egui::text::LayoutJob::simple(
                            self.reading_title(&book),
                            FontId::proportional(17. * self.font_scale),
                            palette.ink,
                            (rect.width() - if narrow { 104. } else { 210. }).max(60.),
                        );
                        title.wrap.max_rows = 1;
                        ui.painter()
                            .galley(text_pos, ui.painter().layout_job(title), palette.ink);
                        // 副标/相对时间都收进卡内带（L-S5c-r2）：320pt×200% 下
                        // Painter::text 无宽度约束，「第 x / y 页 · 刚刚」曾
                        // 溢出卡右缘 18pt。改用限宽 LayoutJob（…省略）+ clip；
                        // 相对时间改挂卡片底角，不与「继续」按钮同一行。
                        let card_inner = rect.shrink2(vec2(14., 8.));
                        let clip = ui.painter().with_clip_rect(card_inner);
                        let button_left = rect.right() - 104. - 12.;
                        let sub_width = if narrow {
                            // 窄档按钮在底行右下，副标在左上不同行——限到
                            // 卡内整宽即可，否则会误按按钮左缘截到「第 …」。
                            card_inner.right() - text_pos.x
                        } else {
                            // 宽档按钮与副标同行右侧，必须让位。
                            (button_left - text_pos.x).max(60.)
                        };
                        let mut sub = egui::text::LayoutJob::simple(
                            format!("第 {} / {} 页", book.current_page + 1, book.page_count),
                            FontId::proportional(13. * self.font_scale),
                            palette.muted,
                            sub_width,
                        );
                        sub.wrap.max_rows = 1;
                        sub.wrap.overflow_character = Some('…');
                        clip.galley(
                            text_pos + vec2(0., 24. * self.font_scale + 4.),
                            clip.layout_job(sub),
                            palette.muted,
                        );
                        if let Some(at) = book.last_read_at {
                            let (ago_left, ago_right) = if narrow {
                                (rect.left() + 14., button_left)
                            } else {
                                (card_inner.left(), rect.right() - 14.)
                            };
                            let mut ago_job = egui::text::LayoutJob::simple(
                                Self::reading_ago(at),
                                FontId::proportional(12. * self.font_scale),
                                palette.muted,
                                (ago_right - ago_left).max(60.),
                            );
                            ago_job.wrap.max_rows = 1;
                            ago_job.wrap.overflow_character = Some('…');
                            let ago_galley = clip.layout_job(ago_job);
                            let ago_pos = if narrow {
                                pos2(ago_left, rect.bottom() - 10. - ago_galley.size().y)
                            } else {
                                pos2(
                                    ago_right - ago_galley.size().x,
                                    rect.bottom() - 6. - ago_galley.size().y,
                                )
                            };
                            clip.galley(ago_pos, ago_galley, palette.muted);
                        }
                        let track = Rect::from_min_size(
                            pos2(text_pos.x, cover.bottom() - 5.),
                            vec2(
                                (rect.right() - text_pos.x - if narrow { 16. } else { 120. })
                                    .max(20.),
                                3.,
                            ),
                        );
                        if self.font_scale <= 1.4 {
                            ui.painter().rect_filled(track, 2., palette.line);
                            ui.painter().rect_filled(
                                Rect::from_min_size(
                                    track.min,
                                    vec2(
                                        track.width() * (book.current_page + 1) as f32
                                            / book.page_count.max(1) as f32,
                                        3.,
                                    ),
                                ),
                                2.,
                                palette.accent,
                            );
                        }
                        if self.button(
                            ui,
                            &format!("history-{}", book.id),
                            Rect::from_min_size(
                                pos2(
                                    rect.right() - 104.,
                                    if narrow {
                                        rect.bottom() - 58.
                                    } else {
                                        rect.center().y - 24.
                                    },
                                ),
                                vec2(88., 48.),
                            ),
                            "继续",
                            true,
                        ) {
                            self.open_reader(&book);
                        }
                    }
                });
        });
    }
    fn settings_ui(&mut self, ui: &mut Ui, area: Rect) {
        let palette = crate::design::palette(self.dark);
        // P2-2：竖屏底部标签栏盖内容区——滚动视口下缘收到栏顶 6pt 之上，
        // 内容再上移 8pt，让 AniList token 输入框首屏完整露出。
        let bottom_bar = if Self::side_navigation(area) {
            88.
        } else {
            Self::TAB_BAR_HEIGHT + 8.
        };
        let column = Self::page_column(area, if bottom_bar == 88. { 92. } else { 84. }, bottom_bar);
        ui.scope_builder(UiBuilder::new().max_rect(column), |ui| {
            ScrollArea::vertical()
                .id_salt("reader-settings")
                .show(ui, |ui| {
                    ui.style_mut().wrap_mode = Some(TextWrapMode::Wrap);
                    crate::design::grouped_card(ui, palette, |ui| {
                            ui.set_width(ui.available_width());
                            crate::design::section(ui, "外观");
                            ui.label(RichText::new("主题").color(palette.muted).small());
                            let choice_width = if ui.available_width() < 330. {
                                ui.available_width()
                            } else {
                                ((ui.available_width() - 20.) / 3.).floor() - 1.
                            };
                            ui.horizontal_wrapped(|ui| {
                                for (key, name) in
                                    [("system", "跟随系统"), ("light", "浅色"), ("dark", "深色")]
                                {
                                    if crate::design::theme_choice(
                                        ui,
                                        name,
                                        key,
                                        self.theme == key,
                                        choice_width,
                                    ) {
                                        self.theme = key.into();
                                        self.setting("theme", self.theme.clone());
                                    }
                                }
                            });
                            ui.add_space(22.);
                            let mut percent = (self.font_scale * 100.).round();
                            ui.label(format!("文字大小 · {percent:.0}%"));
                            let width = (ui.available_width() - 12.).max(80.);
                            let out = crate::design::slider(
                                ui,
                                "font-scale-slider",
                                &mut percent,
                                100.0..=200.,
                                width,
                                false,
                                self.dark,
                            );
                            self.hitboxes
                                .insert("font-scale-slider".into(), out.rect);
                            if out.changed {
                                percent = (percent / 5.).round() * 5.;
                                self.font_scale = percent / 100.;
                                self.setting("font-scale", self.font_scale.to_string());
                            }
                            ui.add_space(12.);
                            let reduced_now = self.reduced;
                            if crate::design::toggle_animated(
                                ui,
                                &mut self.reduced,
                                "减少动态效果",
                                "切书和打开封面时，使用更轻的过渡",
                                reduced_now,
                            ) {
                                self.setting("reduce-motion", self.reduced.to_string());
                            }
                        });
                    ui.add_space(14.);
                    crate::design::grouped_card(ui, palette, |ui| {
                            ui.set_width(ui.available_width());
                            crate::design::section(ui, "阅读");
                            let mut mode = if self.vertical {
                                "vertical"
                            } else if self.spread {
                                "spread"
                            } else {
                                "paged"
                            };
                            let mode_width = if ui.available_width() < 370. {
                                ((ui.available_width() - 10.) / 2.).floor() - 1.
                            } else {
                                ((ui.available_width() - 20.) / 3.).floor() - 1.
                            };
                            ui.horizontal_wrapped(|ui| {
                                for (key, name) in [
                                    ("paged", "单页"),
                                    ("spread", "平板双页"),
                                    ("vertical", "上下滚动"),
                                ] {
                                    if ui
                                        .add(
                                            Button::new(name)
                                                .selected(mode == key)
                                                .min_size(vec2(mode_width, 48.))
                                                .wrap(),
                                        )
                                        .clicked()
                                    {
                                        mode = key;
                                    }
                                }
                            });
                            if self.vertical != (mode == "vertical")
                                || self.spread != (mode == "spread")
                            {
                                self.set_reading_mode(mode);
                            }
                            ui.add_space(14.);
                            if crate::design::toggle_animated(
                                ui,
                                &mut self.rtl,
                                "从右向左翻页",
                                "适合日式漫画的阅读顺序",
                                self.reduced,
                            ) {
                                self.reader_globals.rtl = self.rtl;
                                self.setting(
                                    "reading-direction",
                                    if self.rtl { "rtl" } else { "ltr" }.into(),
                                );
                            }
                            ui.separator();
                            ui.add_space(6.);
                            if crate::design::toggle_animated(
                                ui,
                                &mut self.private,
                                "私密阅读",
                                "不保存新的阅读进度和历史",
                                self.reduced,
                            ) {
                                self.setting("private", self.private.to_string());
                            }
                        });
                    ui.add_space(14.);
                    crate::design::grouped_card(ui, palette, |ui| {
                            ui.set_width(ui.available_width());
                            crate::design::section(ui, "追更");
                            ui.label(
                                RichText::new("自动检查新章节").color(palette.muted).small(),
                            );
                            let interval = self.sources.follow_interval();
                            let mut chosen = interval;
                            let width = if ui.available_width() < 340. {
                                ((ui.available_width() - 10.) / 2.).floor() - 1.
                            } else {
                                ((ui.available_width() - 40.) / 5.).floor() - 1.
                            };
                            ui.horizontal_wrapped(|ui| {
                                for (minutes, name) in crate::sources::FOLLOW_INTERVAL_CHOICES {
                                    if ui
                                        .add(
                                            Button::new(*name)
                                                .selected(chosen == *minutes)
                                                .min_size(vec2(width, 44.))
                                                .wrap(),
                                        )
                                        .clicked()
                                    {
                                        chosen = *minutes;
                                    }
                                }
                            });
                            if chosen != interval {
                                self.sources.set_follow_interval(chosen);
                                self.setting("follow-interval", chosen.to_string());
                            }
                        });
                    ui.add_space(14.);
                    crate::design::grouped_card(ui, palette, |ui| {
                            ui.set_width(ui.available_width());
                            crate::design::section(ui, "进度跟踪（AniList / Kitsu）");
                            let mut sync = self.trackers.auto_sync();
                            if crate::design::toggle_animated(
                                ui,
                                &mut sync,
                                "自动同步章节进度",
                                "读完一章就把远端进度推进到已读（私密阅读时不上传）",
                                self.reduced,
                            ) {
                                self.trackers
                                    .submit(crate::trackers::TrackCommand::SetAutoSync(sync));
                            }
                            if let Some(error) = self.trackers.error.clone() {
                                ui.add_space(6.);
                                ui.label(RichText::new(error).color(palette.muted).small());
                            }
                            for tracker in [
                                crate::trackers::TrackerKind::Anilist,
                                crate::trackers::TrackerKind::Kitsu,
                            ] {
                                ui.add_space(10.);
                                let name = tracker.label();
                                let account = self.trackers.account(tracker).cloned();
                                if let Some(account) = account {
                                    ui.horizontal(|ui| {
                                        ui.label(format!("{name} · {}", account.name));
                                        if ui
                                            .add_enabled(
                                                self.trackers.idle(),
                                                Button::new("退出登录").min_size(vec2(88., 40.)),
                                            )
                                            .clicked()
                                        {
                                            self.trackers.submit(
                                                crate::trackers::TrackCommand::Logout(tracker),
                                            );
                                        }
                                    });
                                } else {
                                    ui.label(
                                        RichText::new(format!("{name} · 未登录"))
                                            .color(palette.muted)
                                            .small(),
                                    );
                                    self.tracker_login_row(ui, tracker);
                                }
                            }
                        });
                    ui.add_space(14.);
                    crate::design::grouped_card(ui, palette, |ui| {
                            ui.set_width(ui.available_width());
                            crate::design::section(ui, "书库与备份");
                            ui.horizontal_wrapped(|ui| {
                                if ui.button("选择漫画文件").clicked() {
                                    self.pick_import();
                                }
                                #[cfg(not(any(target_os = "android", target_os = "ios")))]
                                if ui.button("选择图片文件夹").clicked() {
                                    if let Some(path) = rfd::FileDialog::new().pick_folder() {
                                        self.import_files(vec![path]);
                                    }
                                }
                            });
                            ui.add_space(6.);
                            ui.label(
                                RichText::new("支持 CBZ / ZIP、CBT / TAR、CBR / RAR")
                                    .small()
                                    .color(palette.muted),
                            );
                            ui.add_space(12.);
                            ui.horizontal_wrapped(|ui| {
                                if ui.button("创建完整备份").clicked() {
                                    self.send(Command::Backup(self.backup_path.clone().into()));
                                }
                                if ui.button("恢复备份").clicked() {
                                    self.pick_restore();
                                }
                            });
                            #[cfg(not(any(target_os = "android", target_os = "ios")))]
                            {
                                ui.add_space(8.);
                                CollapsingHeader::new("文件位置").show(ui, |ui| {
                                    ui.add(
                                        TextEdit::singleline(&mut self.import_text)
                                            .desired_width(ui.available_width())
                                            .hint_text("漫画文件或文件夹路径"),
                                    );
                                    if ui.button("导入上述路径").clicked() {
                                        self.import_files(vec![PathBuf::from(
                                            self.import_text.trim(),
                                        )]);
                                    }
                                    ui.label("备份保存到");
                                    ui.add(
                                        TextEdit::singleline(&mut self.backup_path)
                                            .desired_width(ui.available_width()),
                                    );
                                });
                            }
                        });
                    ui.add_space(14.);
                    crate::design::grouped_card(ui, palette, |ui| {
                            ui.set_width(ui.available_width());
                            CollapsingHeader::new(RichText::new("设备间同步").strong()).show(
                                ui,
                                |ui| {
                                    self.sync.ui(ui);
                                },
                            );
                        });
                    ui.add_space(24.);
                    ui.vertical_centered(|ui| {
                        ui.label(
                            RichText::new(concat!("页间  ", env!("CARGO_PKG_VERSION")))
                                .color(palette.muted),
                        );
                        ui.small("书架设计源自 Wenku8");
                        if ui.button("查看开源许可").clicked() {
                            self.error = Some(format!(
                                "Wenku8 Flutter v0.5.6\n\n{}\n\nGPL-3.0\n\n{}\n\n阅读字体\n\n{}\n\nNoto Sans SC · SIL OFL 1.1\n\n{}\n\nNoto Serif SC · SIL OFL 1.1\n\n{}",
                                include_str!("../licenses/Wenku8-NOTICE"),
                                include_str!("../licenses/Wenku8-GPL-3.0.txt"),
                                include_str!("../licenses/ReadingFonts.md"),
                                include_str!("../licenses/ReadingSans-OFL.txt"),
                                include_str!("../licenses/ReadingSerif-OFL.txt")
                            ));
                        }
                    });
                    // 末行也要能滚出底栏覆盖区（P2-2 安全区内边距）。
                    ui.add_space(Self::TAB_BAR_HEIGHT + 8.);
                });
        });
    }
    fn tracker_login_row(&mut self, ui: &mut Ui, tracker: crate::trackers::TrackerKind) {
        let idle = self.trackers.idle();
        match tracker {
            crate::trackers::TrackerKind::Anilist => {
                ui.add(
                    TextEdit::singleline(&mut self.tracker_anilist_token)
                        .desired_width(ui.available_width())
                        .hint_text("粘贴 AniList Access Token"),
                );
                #[cfg(not(any(target_os = "android", target_os = "ios")))]
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(idle, Button::new("浏览器授权登录").min_size(vec2(0., 44.)))
                        .clicked()
                    {
                        self.trackers
                            .submit(crate::trackers::TrackCommand::AnilistOAuth);
                    }
                    let token = self.tracker_anilist_token.trim().to_string();
                    if ui
                        .add_enabled(
                            idle && !token.is_empty(),
                            Button::new("用粘贴的令牌登录").min_size(vec2(0., 44.)),
                        )
                        .clicked()
                    {
                        self.trackers
                            .submit(crate::trackers::TrackCommand::AnilistToken(token));
                        self.tracker_anilist_token.clear();
                    }
                });
                #[cfg(any(target_os = "android", target_os = "ios"))]
                {
                    let token = self.tracker_anilist_token.trim().to_string();
                    if ui
                        .add_enabled(
                            idle && !token.is_empty(),
                            Button::new("用粘贴的令牌登录").min_size(vec2(0., 44.)),
                        )
                        .clicked()
                    {
                        self.trackers
                            .submit(crate::trackers::TrackCommand::AnilistToken(token));
                        self.tracker_anilist_token.clear();
                    }
                }
            }
            crate::trackers::TrackerKind::Kitsu => {
                ui.add(
                    TextEdit::singleline(&mut self.tracker_kitsu_user)
                        .desired_width(ui.available_width())
                        .hint_text("Kitsu 邮箱或用户名"),
                );
                ui.add(
                    TextEdit::singleline(&mut self.tracker_kitsu_pass)
                        .desired_width(ui.available_width())
                        .password(true)
                        .hint_text("Kitsu 密码"),
                );
                let user = self.tracker_kitsu_user.trim().to_string();
                let pass = self.tracker_kitsu_pass.clone();
                if ui
                    .add_enabled(
                        idle && !user.is_empty() && !pass.is_empty(),
                        Button::new("登录 Kitsu").min_size(vec2(0., 44.)),
                    )
                    .clicked()
                {
                    self.trackers
                        .submit(crate::trackers::TrackCommand::KitsuLogin(user, pass));
                    self.tracker_kitsu_pass.clear();
                }
            }
        }
    }
    fn track_panel_ui(&mut self, ui: &mut Ui, key: &str) {
        use crate::trackers::{TrackCommand, TrackStatus, TrackerKind};
        ui.heading("进度跟踪");
        let palette = crate::design::palette(self.dark);
        let bound = self.trackers.binding(key).cloned();
        if let Some(binding) = &bound {
            let tracker = if binding.tracker == "kitsu" {
                TrackerKind::Kitsu
            } else {
                TrackerKind::Anilist
            };
            ui.label(format!(
                "已绑定 {} · {}",
                tracker.label(),
                binding.title
            ));
            ui.label(
                RichText::new(format!(
                    "状态 {} · 进度 {}",
                    binding.status.label(),
                    binding.progress
                ))
                .color(palette.muted)
                .small(),
            );
            if let Some(entry) = self.trackers.entries.get(key).and_then(|e| e.as_ref()) {
                ui.label(
                    RichText::new(format!(
                        "远端：{} · 第 {} 章{}",
                        entry.status.label(),
                        entry.progress,
                        if entry.total_chapters > 0 {
                            format!("/{}", entry.total_chapters)
                        } else {
                            String::new()
                        }
                    ))
                    .color(palette.muted)
                    .small(),
                );
            }
            let mut status = binding.status;
            ui.horizontal_wrapped(|ui| {
                for candidate in [
                    TrackStatus::Reading,
                    TrackStatus::Completed,
                    TrackStatus::Planned,
                    TrackStatus::OnHold,
                    TrackStatus::Dropped,
                    TrackStatus::ReReading,
                ] {
                    if ui
                        .selectable_label(status == candidate, candidate.label())
                        .clicked()
                    {
                        status = candidate;
                    }
                }
            });
            let mut progress = binding.progress.max(0);
            ui.horizontal(|ui| {
                ui.label("已读章节");
                if ui.button("−").clicked() {
                    progress = (progress - 1).max(0);
                }
                ui.label(format!("{progress}"));
                if ui.button("+").clicked() {
                    progress += 1;
                }
            });
            let idle = self.trackers.idle();
            if ui
                .add_enabled(
                    idle && (status != binding.status || progress != binding.progress),
                    Button::new("保存并同步").min_size(vec2(0., 44.)),
                )
                .clicked()
            {
                self.trackers.submit(TrackCommand::Update {
                    key: key.to_string(),
                    status,
                    progress,
                });
            }
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(idle, Button::new("刷新远端").min_size(vec2(0., 44.)))
                    .clicked()
                {
                    self.trackers.submit(TrackCommand::Refresh(key.to_string()));
                }
                if ui
                    .add_enabled(idle, Button::new("解除绑定").min_size(vec2(0., 44.)))
                    .clicked()
                {
                    self.trackers.submit(TrackCommand::Unbind(key.to_string()));
                }
            });
        } else {
            ui.label(RichText::new("未绑定远端条目").color(palette.muted).small());
            ui.add(
                TextEdit::singleline(&mut self.track_query)
                    .desired_width(ui.available_width())
                    .hint_text("搜索要绑定的作品"),
            );
            let idle = self.trackers.idle();
            ui.horizontal(|ui| {
                for tracker in [TrackerKind::Anilist, TrackerKind::Kitsu] {
                    let logged = self.trackers.account(tracker).is_some();
                    if ui
                        .add_enabled(
                            idle && logged && !self.track_query.trim().is_empty(),
                            Button::new(format!("搜 {}", tracker.label()))
                                .min_size(vec2(0., 44.)),
                        )
                        .clicked()
                    {
                        self.trackers.submit(TrackCommand::Search {
                            tracker,
                            key: key.to_string(),
                            query: self.track_query.trim().to_string(),
                        });
                    }
                }
            });
            if self.trackers.account(TrackerKind::Anilist).is_none()
                && self.trackers.account(TrackerKind::Kitsu).is_none()
            {
                ui.label(
                    RichText::new("先到 设置 → 进度跟踪 登录 AniList 或 Kitsu")
                        .color(palette.muted)
                        .small(),
                );
            }
            if let Some(list) = self.trackers.searches.get(key).cloned() {
                ScrollArea::vertical().max_height(240.).show(ui, |ui| {
                    for item in list {
                        let text = format!(
                            "{}{}{}",
                            item.title,
                            if item.total_chapters > 0 {
                                format!(" · {} 章", item.total_chapters)
                            } else {
                                String::new()
                            },
                            if item.status.is_empty() {
                                String::new()
                            } else {
                                format!(" · {}", item.status)
                            }
                        );
                        if ui
                            .add(Button::new(text).min_size(vec2(ui.available_width(), 44.)).wrap())
                            .clicked()
                        {
                            self.trackers.submit(TrackCommand::Bind {
                                tracker: if self
                                    .trackers
                                    .account(TrackerKind::Anilist)
                                    .is_some()
                                {
                                    TrackerKind::Anilist
                                } else {
                                    TrackerKind::Kitsu
                                },
                                key: key.to_string(),
                                pick: item,
                                progress: 0,
                            });
                        }
                    }
                });
            }
        }
        ui.add_space(8.);
        if ui.button("关闭").clicked() {
            self.track_panel = None;
        }
    }
    fn sources_ui(&mut self, ui: &mut Ui, area: Rect) {
        // 与设置/历史同一条列基线（P1-3）：横屏切 tab 时内容柱左缘不再横跳。
        let column = Self::page_column(area, 88., 88.);
        // Feed library reading progress into the source detail page so
        // 继续阅读 picks the freshest chapter and rows show 已读/读到状态.
        let mut progress = std::collections::HashMap::new();
        for link in &self.chapters {
            let Some(book) = self.books.iter().find(|book| book.id == link.book_id)
            else {
                continue;
            };
            progress.insert(
                crate::sources::identity_id(
                    &link.metadata.source_id,
                    &link.metadata.manga_id,
                    &link.metadata.chapter_id,
                ),
                (book.current_page, book.page_count, book.last_read_at),
            );
        }
        self.sources.set_chapter_progress(progress);
        ui.scope_builder(UiBuilder::new().max_rect(column), |ui| {
            ui.style_mut().wrap_mode = Some(TextWrapMode::Wrap);
            self.sources.ui(ui);
        });
        self.hitboxes
            .extend(self.sources.hitboxes.iter().map(|(id, rect)| (id.clone(), *rect)));
    }
    fn pick_import(&mut self) {
        #[cfg(not(any(target_os = "android", target_os = "ios")))]
        if let Some(paths) = rfd::FileDialog::new()
            .add_filter("漫画文件", &["cbz", "zip", "cbt", "tar", "cbr", "rar"])
            .pick_files()
        {
            self.import_files(paths);
        }
        #[cfg(any(target_os = "android", target_os = "ios"))]
        {
            if let Err(error) = yejian_platform::pick_comics() {
                self.error = Some(format!("无法打开文件选择器：{error}"));
            }
        }
    }
    fn pick_restore(&mut self) {
        #[cfg(not(any(target_os = "android", target_os = "ios")))]
        if let Some(path) = rfd::FileDialog::new()
            .add_filter("书库备份", &["zip", "json"])
            .pick_file()
        {
            self.send(Command::Restore(path));
        }
        #[cfg(any(target_os = "android", target_os = "ios"))]
        {
            if let Err(error) = yejian_platform::pick_backup() {
                self.error = Some(format!("无法打开备份选择器：{error}"));
            }
        }
    }
    fn dialogs(&mut self, ctx: &Context) {
        if self.reader_options {
            if self.reader_options_at.is_none() {
                self.reader_options_at = (!self.reduced).then(|| ctx.input(|i| i.time));
            }
            let now = ctx.input(|i| i.time);
            let open_t = self
                .reader_options_at
                .map(|t0| motion::smooth(((now - t0) / 0.16).clamp(0., 1.) as f32))
                .unwrap_or(1.);
            let response = Modal::new(Id::unique("reader-options"))
                .frame(
                    Frame::popup(&ctx.style_of(Theme::Dark))
                        .fill(crate::design::palette(true).surface)
                        .inner_margin(20)
                        .corner_radius(20)
                        .stroke(Stroke::new(
                            1.,
                            crate::design::palette(true).line.gamma_multiply(0.6),
                        )),
                )
                .show(ctx, |ui| {
                    ui.set_width((ctx.content_rect().width() - 56.).clamp(200., 440.));
                    ui.set_max_width((ctx.content_rect().width() - 56.).clamp(200., 440.));
                    ui.style_mut().visuals = crate::design::visuals(true);
                    ui.scope(|ui| {
                        ui.set_opacity(open_t);
                        ui.add_space(8. * (1. - open_t));
                        ui.horizontal(|ui| {
                            ui.heading("阅读设置");
                        });
                        let mut safe = ctx.content_rect();
                        if let Ok(insets) = yejian_platform::safe_insets() {
                            let scale = ctx.pixels_per_point();
                            safe = safe.intersect(Rect::from_min_max(
                                safe.min
                                    + vec2(
                                        insets.left as f32 / scale,
                                        insets.top as f32 / scale,
                                    ),
                                safe.max
                                    - vec2(
                                        insets.right as f32 / scale,
                                        insets.bottom as f32 / scale,
                                    ),
                            ));
                        }
                        let height = (safe.height() - 180.).max(120.);
                        let p = crate::design::palette(true);
                        let size = 14. * self.font_scale.min(1.5);
                        ScrollArea::vertical().max_height(height).show(ui, |ui| {
                            let avail_w = ui.available_width();
                            ui.label(RichText::new("阅读方式").color(p.muted).small());
                            let mode = if self.vertical {
                                "vertical"
                            } else if self.spread {
                                "spread"
                            } else {
                                "paged"
                            };
                            // 3-up segmented control when labels fit; the 2+1
                            // wrap keeps every cell unclipped at 200% type.
                            let cols = if avail_w >= 330. && self.font_scale <= 1.35 {
                                3.
                            } else {
                                2.
                            };
                            let col_w = ((avail_w - (cols - 1.) * 8.) / cols).floor();
                            let item_spacing = ui.spacing().item_spacing;
                            ui.spacing_mut().item_spacing = vec2(8., 8.);
                            ui.horizontal_wrapped(|ui| {
                                for (key, name, icon) in [
                                    ("paged", "单页", "paged"),
                                    ("spread", "平板双页", "spread"),
                                    ("vertical", "上下滚动", "vertical"),
                                ] {
                                    let (rect, response) = ui.allocate_exact_size(
                                        vec2(col_w, 48.),
                                        Sense::click(),
                                    );
                                    response.widget_info(|| {
                                        WidgetInfo::labeled(Role::Button, true, name)
                                    });
                                    let selected = mode == key;
                                    let draw = crate::design::press_rect(
                                        rect,
                                        response.is_pointer_button_down_on()
                                            && !self.reduced,
                                    );
                                    let fill = if selected || response.hovered() {
                                        p.accent_soft
                                    } else {
                                        p.soft
                                    };
                                    let stroke = if selected {
                                        Stroke::new(1.5, p.accent)
                                    } else {
                                        Stroke::new(1., p.line)
                                    };
                                    ui.painter().rect_filled(draw, 12., fill);
                                    ui.painter().rect_stroke(
                                        draw,
                                        12.,
                                        stroke,
                                        StrokeKind::Inside,
                                    );
                                    let label_color =
                                        if selected { p.accent } else { p.ink };
                                    let galley = ui.painter().layout_no_wrap(
                                        name.to_owned(),
                                        FontId::proportional(size),
                                        label_color,
                                    );
                                    let show_icon =
                                        col_w >= 112. && self.font_scale <= 1.2;
                                    let icon_w = if show_icon { 24. } else { 0. };
                                    let text_x = rect.center().x + icon_w / 2.
                                        - galley.size().x / 2.;
                                    if show_icon {
                                        crate::design::draw_icon(
                                            ui.painter(),
                                            pos2(
                                                text_x - 12.,
                                                rect.center().y,
                                            ),
                                            icon,
                                            label_color,
                                            selected,
                                        );
                                    }
                                    ui.painter().galley(
                                        pos2(
                                            text_x,
                                            rect.center().y - galley.size().y / 2.,
                                        ),
                                        galley,
                                        label_color,
                                    );
                                    self.hitboxes
                                        .insert(format!("reader-mode-{key}"), rect);
                                    if response.clicked() {
                                        self.set_reading_mode(key);
                                    }
                                }
                            });
                            ui.spacing_mut().item_spacing = item_spacing;
                            ui.add_space(10.);
                            // Full-width direction row: label left, RTL/LTR
                            // badge right. The painted "翻页方向" stays inside
                            // the registered hitbox for the acceptance text
                            // bounds check.
                            let (dir_rect, direction) =
                                ui.allocate_exact_size(vec2(avail_w, 46.), Sense::click());
                            direction.widget_info(|| {
                                WidgetInfo::labeled(Role::Button, true, "翻页方向")
                            });
                            let draw = crate::design::press_rect(
                                dir_rect,
                                direction.is_pointer_button_down_on() && !self.reduced,
                            );
                            ui.painter().rect_filled(
                                draw,
                                12.,
                                if direction.hovered() {
                                    p.accent_soft
                                } else {
                                    p.soft
                                },
                            );
                            ui.painter().rect_stroke(
                                draw,
                                12.,
                                Stroke::new(1., p.line),
                                StrokeKind::Inside,
                            );
                            ui.painter().text(
                                pos2(draw.left() + 14., draw.center().y),
                                Align2::LEFT_CENTER,
                                "翻页方向",
                                FontId::proportional(size),
                                p.ink,
                            );
                            let value = if self.rtl { "从右向左" } else { "从左向右" };
                            let badge_text = ui.painter().layout_no_wrap(
                                value.to_owned(),
                                FontId::proportional((size - 1.).max(11.)),
                                p.accent,
                            );
                            let badge = Rect::from_center_size(
                                pos2(
                                    draw.right() - 12. - (badge_text.size().x + 22.) / 2.,
                                    draw.center().y,
                                ),
                                vec2(badge_text.size().x + 22., 26.),
                            );
                            ui.painter().rect_filled(badge, 13., p.accent_soft);
                            ui.painter().rect_stroke(
                                badge,
                                13.,
                                Stroke::new(1., p.accent.gamma_multiply(0.55)),
                                StrokeKind::Inside,
                            );
                            ui.painter().galley(
                                pos2(
                                    badge.left() + 11.,
                                    badge.center().y - badge_text.size().y / 2.,
                                ),
                                badge_text,
                                p.accent,
                            );
                            self.hitboxes.insert("reader-direction".into(), dir_rect);
                            if direction.clicked() {
                                self.rtl = !self.rtl;
                                self.save_reader_prefs();
                            }
                            ui.add_space(14.);
                            ui.label(
                                RichText::new("画面").color(p.muted).small(),
                            );
                            ui.add_space(4.);
                            // Grouped toggle rows with animated pill switches.
                            let toggles = [
                                ("裁剪白边", "reader-crop", self.crop),
                                ("适应宽度", "reader-fit", self.fit_width),
                                ("双页首页单独", "reader-spread-offset", self.spread_offset),
                            ];
                            let (group, _) = ui.allocate_exact_size(
                                vec2(avail_w, 46. * toggles.len() as f32),
                                Sense::hover(),
                            );
                            ui.painter().rect_filled(group, 12., p.soft);
                            ui.painter().rect_stroke(
                                group,
                                12.,
                                Stroke::new(1., p.line),
                                StrokeKind::Inside,
                            );
                            let mut toggled = None;
                            for (row, (name, hitbox, on)) in toggles.into_iter().enumerate()
                            {
                                let rect = Rect::from_min_size(
                                    group.min + vec2(0., 46. * row as f32),
                                    vec2(avail_w, 46.),
                                );
                                if row > 0 {
                                    ui.painter().line_segment(
                                        [
                                            pos2(rect.left() + 12., rect.top()),
                                            pos2(rect.right() - 12., rect.top()),
                                        ],
                                        Stroke::new(1., p.line),
                                    );
                                }
                                let response = ui.interact(
                                    rect,
                                    ui.make_persistent_id(hitbox),
                                    Sense::click(),
                                );
                                response.widget_info(|| {
                                    WidgetInfo::labeled(Role::Button, true, name)
                                });
                                if response.hovered()
                                    || response.is_pointer_button_down_on()
                                {
                                    ui.painter().rect_filled(
                                        rect.shrink2(vec2(0., 2.)),
                                        8.,
                                        p.accent_soft.linear_multiply(0.5),
                                    );
                                }
                                ui.painter().text(
                                    pos2(rect.left() + 14., rect.center().y),
                                    Align2::LEFT_CENTER,
                                    name,
                                    FontId::proportional(size),
                                    p.ink,
                                );
                                let t = ctx.animate_bool_with_time(
                                    Id::unique(hitbox),
                                    on,
                                    if self.reduced { 0. } else { 0.14 },
                                );
                                let track = Rect::from_center_size(
                                    pos2(rect.right() - 14. - 22., rect.center().y),
                                    vec2(44., 26.),
                                );
                                let track_color = egui::lerp(
                                    Rgba::from(p.muted.gamma_multiply(0.45))
                                        ..=Rgba::from(p.accent),
                                    t,
                                );
                                ui.painter()
                                    .rect_filled(track, 13., track_color);
                                let knob = pos2(
                                    track.left() + 13. + (track.width() - 26.) * t,
                                    track.center().y,
                                );
                                ui.painter()
                                    .circle_filled(knob + vec2(0., 1.), 10., p.line);
                                ui.painter().circle_filled(knob, 10., p.surface);
                                self.hitboxes.insert(hitbox.into(), rect);
                                if response.clicked() {
                                    toggled = Some(hitbox);
                                }
                            }
                            match toggled {
                                Some("reader-crop") => self.crop = !self.crop,
                                Some("reader-fit") => self.fit_width = !self.fit_width,
                                Some(_) => self.spread_offset = !self.spread_offset,
                                None => {}
                            }
                            if toggled.is_some() {
                                self.save_reader_prefs();
                            }
                            ui.add_space(10.);
                            // Brightness row: sun icon, label, custom slider,
                            // percent readout. The row rect is the hitbox; the
                            // slider's own id stays separate.
                            let (bright_rect, _) = ui.allocate_exact_size(
                                vec2(avail_w, 46.),
                                Sense::hover(),
                            );
                            ui.painter().rect_filled(bright_rect, 12., p.soft);
                            ui.painter().rect_stroke(
                                bright_rect,
                                12.,
                                Stroke::new(1., p.line),
                                StrokeKind::Inside,
                            );
                            crate::design::draw_icon(
                                ui.painter(),
                                pos2(
                                    bright_rect.left() + 20.,
                                    bright_rect.center().y,
                                ),
                                "sun",
                                p.muted,
                                false,
                            );
                            ui.painter().text(
                                pos2(bright_rect.left() + 36., bright_rect.center().y),
                                Align2::LEFT_CENTER,
                                "亮度",
                                FontId::proportional(size),
                                p.ink,
                            );
                            let readout = format!("{:.0}%", self.brightness * 100.);
                            let readout_text = ui.painter().layout_no_wrap(
                                readout,
                                FontId::proportional(13. * self.font_scale.min(1.3)),
                                p.muted,
                            );
                            ui.painter().galley(
                                pos2(
                                    bright_rect.right()
                                        - 14.
                                        - readout_text.size().x,
                                    bright_rect.center().y - readout_text.size().y / 2.,
                                ),
                                readout_text.clone(),
                                p.muted,
                            );
                            let slider_zone = Rect::from_min_max(
                                pos2(
                                    bright_rect.left() + 78.,
                                    bright_rect.top() + 1.,
                                ),
                                pos2(
                                    (bright_rect.right()
                                        - 22.
                                        - readout_text.size().x)
                                        .max(bright_rect.left() + 160.),
                                    bright_rect.bottom() - 1.,
                                ),
                            );
                            let out = ui
                                .scope_builder(
                                    UiBuilder::new().max_rect(slider_zone).layout(
                                        Layout::left_to_right(Align::Center),
                                    ),
                                    |ui| {
                                        crate::design::slider(
                                            ui,
                                            "reader-brightness-slider",
                                            &mut self.brightness,
                                            0.3..=1.0,
                                            slider_zone.width(),
                                            false,
                                            true,
                                        )
                                    },
                                )
                                .inner;
                            self.hitboxes
                                .insert("reader-brightness".into(), bright_rect);
                            if out.changed {
                                self.brightness = self.brightness.clamp(0.3, 1.);
                                self.save_reader_prefs();
                            }
                            ui.add_space(10.);
                            // Secondary actions share a row when they fit.
                            let two_col = avail_w >= 360. && self.font_scale <= 1.3;
                            let act_w = if two_col {
                                ((avail_w - 8.) / 2.).floor()
                            } else {
                                avail_w
                            };
                            let mut secondary =
                                |ui: &mut Ui, rect: Rect, name: &str, hitbox: &str| {
                                    let response = ui.interact(
                                        rect,
                                        ui.make_persistent_id(hitbox),
                                        Sense::click(),
                                    );
                                    response.widget_info(|| {
                                        WidgetInfo::labeled(Role::Button, true, name)
                                    });
                                    let draw = crate::design::press_rect(
                                        rect,
                                        response.is_pointer_button_down_on()
                                            && !self.reduced,
                                    );
                                    ui.painter().rect_filled(
                                        draw,
                                        12.,
                                        if response.hovered()
                                            || response.is_pointer_button_down_on()
                                        {
                                            p.accent_soft
                                        } else {
                                            p.soft
                                        },
                                    );
                                    ui.painter().rect_stroke(
                                        draw,
                                        12.,
                                        Stroke::new(1., p.line),
                                        StrokeKind::Inside,
                                    );
                                    let text = ui.painter().layout(
                                        name.to_owned(),
                                        FontId::proportional(size),
                                        p.ink,
                                        (rect.width() - 16.).max(40.),
                                    );
                                    ui.painter().galley(
                                        rect.center() - text.size() / 2.,
                                        text,
                                        p.ink,
                                    );
                                    self.hitboxes.insert(hitbox.into(), rect);
                                    response.clicked()
                                };
                            let [save_rect, reset_rect] = if two_col {
                                let (row, _) = ui.allocate_exact_size(
                                    vec2(avail_w, 46.),
                                    Sense::hover(),
                                );
                                [
                                    Rect::from_min_size(row.min, vec2(act_w, 46.)),
                                    Rect::from_min_size(
                                        pos2(row.right() - act_w, row.top()),
                                        vec2(act_w, 46.),
                                    ),
                                ]
                            } else {
                                let (r1, _) = ui.allocate_exact_size(
                                    vec2(avail_w, 46.),
                                    Sense::hover(),
                                );
                                ui.add_space(8.);
                                let (r2, _) = ui.allocate_exact_size(
                                    vec2(avail_w, 46.),
                                    Sense::hover(),
                                );
                                [r1, r2]
                            };
                            let save = secondary(
                                ui,
                                save_rect,
                                "设为所有作品的默认",
                                "reader-save-default",
                            );
                            let reset = secondary(
                                ui,
                                reset_rect,
                                "恢复原始大小",
                                "reader-reset-zoom",
                            );
                            if save {
                                self.save_reader_globals();
                            }
                            if reset {
                                self.zoom = 1.;
                                self.pan = Vec2::ZERO;
                                self.zoom_anim.cancel();
                            }
                        });
                        ui.add_space(8.);
                        let close = ui.add_sized(
                            [ui.available_width(), 48.],
                            crate::design::primary("继续阅读", true),
                        );
                        self.hitboxes
                            .insert("reader-options-close".into(), close.rect);
                        if close.clicked() {
                            self.reader_options = false;
                        }
                    });
                });
            if response.should_close() {
                self.reader_options = false;
            }
        }
        if let Some(id) = self.chapter_dialog.clone() {
            let title = self
                .chapter(&id)
                .map(|m| m.manga_title.clone())
                .unwrap_or_else(|| "已下载章节".into());
            let chapters = self.chapters_for(&id);
            let response = Modal::new(Id::unique("chapter-picker")).show(ctx, |ui| {
                ui.set_width((ctx.content_rect().width() - 56.).clamp(200., 460.));
                ui.heading(title);
                ScrollArea::vertical().max_height(380.).show(ui, |ui| {
                    for book in chapters {
                        let name = self
                            .chapter(&book.id)
                            .map(|m| m.chapter_title.clone())
                            .unwrap_or_else(|| book.title.clone());
                        let label = format!(
                            "{}{}\n第 {} / {} 页",
                            if book.id == id {
                                "正在阅读 · "
                            } else {
                                ""
                            },
                            name,
                            book.current_page + 1,
                            book.page_count
                        );
                        let response = ui.add_sized(
                            [ui.available_width(), 58. * self.font_scale.sqrt()],
                            Button::new(label).wrap(),
                        );
                        self.hitboxes
                            .insert(format!("chapter-{}", book.id), response.rect);
                        if response.clicked() {
                            self.open_reader(&book);
                        }
                    }
                });
                if ui.button("关闭").clicked() {
                    self.chapter_dialog = None;
                }
            });
            if response.should_close() {
                self.chapter_dialog = None;
            }
        }
        if let Some(text) = self.error.clone() {
            let response = Modal::new(Id::unique("reader-notice")).show(ctx, |ui| {
                ui.set_width((ctx.content_rect().width() - 56.).clamp(200., 520.));
                ui.heading("提示");
                ScrollArea::vertical().max_height(300.).show(ui, |ui| {
                    ui.label(&text);
                });
                ui.horizontal_wrapped(|ui| {
                    if let Some((id, page, fraction, private)) = self.failed_position.clone() {
                        if ui.button("重试保存阅读位置").clicked() {
                            self.send(Command::Progress {
                                id,
                                page,
                                fraction,
                                private,
                            });
                        }
                    }
                    if ui.button("复制内容").clicked() {
                        ctx.copy_text(text.clone());
                    }
                    if ui.button("关闭").clicked() {
                        self.error = None;
                    }
                });
            });
            if response.should_close() {
                self.error = None;
            }
        }
        if self.confirm_remove {
            Window::new("移出书架")
                .collapsible(false)
                .resizable(false)
                .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
                .show(ctx, |ui| {
                    ui.label(format!(
                        "将 {} 本书移出书架。原漫画文件保留。",
                        self.selected.len()
                    ));
                    ui.horizontal(|ui| {
                        if ui.button("取消").clicked() {
                            self.confirm_remove = false;
                        }
                        if ui.button("移出").clicked() {
                            self.send(Command::Remove(self.selected_books()));
                            self.selected.clear();
                            self.confirm_remove = false;
                        }
                    });
                });
        }
        if let Some(key) = self.track_panel.clone() {
            let response = Modal::new(Id::unique("track-panel")).show(ctx, |ui| {
                ui.set_width((ctx.content_rect().width() - 48.).clamp(220., 460.));
                self.track_panel_ui(ui, &key);
            });
            if response.should_close() {
                self.track_panel = None;
            }
        }
        self.sources.web_dialog(ctx);
    }
    pub fn ready(&self) -> bool {
        self.ready
    }
    pub fn book_count(&self) -> usize {
        self.books.len()
    }
    pub fn current_page(&self) -> usize {
        self.page
    }
    pub fn current_fraction(&self) -> f32 {
        if self.vertical {
            self.vertical_reader.fraction
        } else {
            0.
        }
    }
    pub fn position_saved(&self) -> bool {
        match (&self.saved_position, &self.persisted_position) {
            (None, _) => true,
            (Some((id, page, fraction)), Some((stored, stored_page, stored_fraction))) => {
                id == stored && page == stored_page && (*fraction - *stored_fraction).abs() < 0.0001
            }
            _ => false,
        }
    }
    /// 书架焦点书的 id。藏书一览网格下线后，验收用它在「书架 → 详情 → 阅读」链路里核对身份。
    pub fn shelf_focus_book_id(&self) -> Option<String> {
        let books = self.visible_books();
        if books.is_empty() {
            return None;
        }
        let index = (self.motion.focus.round() as i32).rem_euclid(books.len() as i32) as usize;
        books.get(index).map(|book| book.id.clone())
    }
    /// 焦点书标题。验收用它核对详情页标题，不再靠"第一行大字"猜。
    pub fn shelf_focus_book_title(&self) -> Option<String> {
        let books = self.visible_books();
        if books.is_empty() {
            return None;
        }
        let index = (self.motion.focus.round() as i32).rem_euclid(books.len() as i32) as usize;
        books.get(index).map(|book| book.title.clone())
    }

    pub fn current_book_id(&self) -> Option<&str> {
        match &self.screen {
            Screen::Reader(id) => Some(id),
            _ => None,
        }
    }
    /// 详情页当前书 id。验收用它核对「点哪本封面开哪本详情」——
    /// current_book_id 只覆盖 Reader，不能用于 Detail 屏幕。
    pub fn detail_book_id(&self) -> Option<&str> {
        match &self.screen {
            Screen::Detail(id) => Some(id),
            _ => None,
        }
    }
    pub fn shelf_book_count(&self) -> usize {
        self.visible_books().len()
    }
    pub fn screen_name(&self) -> &'static str {
        match self.screen {
            Screen::Library => "library",
            Screen::Detail(_) => "detail",
            Screen::Reader(_) => "reader",
        }
    }
    pub fn page_loaded(&self) -> bool {
        match &self.screen {
            Screen::Reader(id) => self.pages.contains_key(&(id.clone(), self.page)),
            _ => false,
        }
    }
    pub fn covers_loaded(&self) -> bool {
        self.cover_pending.is_empty()
    }
    /// 合上动画进行中（outgoing 阅读器平面仍在收缩 bands 里绘制）。
    pub fn reader_exit_active(&self) -> bool {
        self.reader_exit.is_some()
    }
}

impl eframe::App for ReaderApp {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        self.draw(ui);
        #[cfg(debug_assertions)]
        self.report_snapshot_ready(ui.ctx());
    }
    fn on_exit(&mut self) {
        self.flush_position();
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
        for index in 0..80 {
            let id = format!("book-{index}");
            app.covers.insert(id.clone(), ctx.load_texture(
                id.clone(), ColorImage::filled([2, 2], Color32::WHITE), TextureOptions::LINEAR,
            ));
            app.cover_usage.insert(id, index);
        }
        app.visible_covers.insert("book-0".into());
        app.trim_covers();
        assert_eq!(app.covers.len(), 48);
        assert!(app.covers.contains_key("book-0"));
        assert!(app.covers.contains_key("book-79"));
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
    fn nav_pill_keeps_edge_gap_in_320pt_split() {
        // 320pt split view: four 80pt slots in the bottom bar. The pill must
        // stay at least 8pt off the rail edge even for the edge slots and
        // mid-way tween positions.
        let slots = [0usize, 1, 2, 3].map(|i| {
            Rect::from_min_size(pos2(i as f32 * 80., 0.), vec2(80., 54.))
        });
        for tenth in 0..=30 {
            let pill = ReaderApp::nav_pill_rect(&slots, tenth as f32 / 10.);
            assert!(pill.left() >= 8. - 0.01, "pill left {pill:?}");
            assert!(pill.right() <= 320. - 8. + 0.01, "pill right {pill:?}");
        }
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
    fn back_from_reader_arms_peel_exit_and_commits_to_the_target() {
        let root = tempfile::tempdir().unwrap();
        let ctx = Context::default();
        crate::configure_fonts(&ctx);
        let mut app = ReaderApp::new(root.path().into(), ctx.clone());
        let area = Rect::from_min_size(Pos2::ZERO, vec2(390., 844.));
        app.reader_return = Screen::Detail("book".into());
        app.screen = Screen::Reader("book".into());
        app.back(1., area);
        // The screen field is already the return target; the exit clock
        // overlays the closing reader plane until raw reaches 1.
        assert!(matches!(app.screen, Screen::Detail(_)));
        let (armed, id) = app.reader_exit.clone().unwrap();
        assert!(armed.is_infinite() && id == "book");
        // A repeated back acts on the now-visible target (no re-arm).
        app.back(2., area);
        assert!(matches!(app.screen, Screen::Library));
        assert!(app.reader_exit.clone().unwrap().0.is_infinite(), "exit clock must not rebuild");
        // Frames run the clock; p hits 0 inside exit_ms and clears.
        for i in 0..40 {
            ctx.run_ui(
                RawInput {
                    screen_rect: Some(area),
                    time: Some(3. + i as f64 / 60.),
                    ..Default::default()
                },
                |ui| app.draw(ui),
            )
            .drop_without_applying_deltas();
        }
        assert!(app.reader_exit.is_none());
        // Reduced motion skips the peel entirely: plain screen swap.
        let mut reduced = ReaderApp::new(root.path().into(), Context::default());
        reduced.reduced = true;
        reduced.reader_return = Screen::Detail("book".into());
        reduced.screen = Screen::Reader("book".into());
        reduced.back(1., area);
        assert!(reduced.reader_exit.is_none() && matches!(reduced.screen, Screen::Detail(_)));
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
    fn detail_chapter_card_persists_fold_and_sort_contract() {
        let root = tempfile::tempdir().unwrap();
        let ctx = Context::default();
        crate::configure_fonts(&ctx);
        let mut app = ReaderApp::new(root.path().into(), ctx.clone());
        app.books = (1..=7).map(|n| book(&format!("c{n}"))).collect();
        app.replace_links(
            app.books
                .iter()
                .enumerate()
                .map(|(i, b)| ChapterLink {
                    book_id: b.id.clone(),
                    metadata: ChapterMetadata {
                        source_id: "s".into(),
                        manga_id: "m".into(),
                        manga_title: "系列".into(),
                        chapter_id: format!("ch{i}"),
                        chapter_title: format!("第 {} 话", i + 1),
                        chapter_number: Some(i as f64 + 1.),
                        volume_number: None,
                    },
                })
                .collect(),
        );
        app.books[0].current_page = 2;
        let frame = |app: &mut ReaderApp, w: f32, h: f32| {
            ctx.run_ui(
                RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(w, h))),
                    ..Default::default()
                },
                |ui| app.detail_ui(ui, ui.max_rect(), "c1"),
            )
            .drop_without_applying_deltas();
        };
        frame(&mut app, 820., 1180.);
        app.hitboxes.clear();
        frame(&mut app, 820., 1180.);
        for n in 1..=6 {
            let rect = app
                .hitboxes
                .get(&format!("dchapter-c{n}"))
                .unwrap_or_else(|| panic!("missing dchapter-c{n}"));
            assert!(rect.height() >= 40., "dchapter-c{n} too small: {rect:?}");
        }
        assert!(!app.hitboxes.contains_key("dchapter-c7"), "7th chapter must fold into the footer");
        let all = app.hitboxes.get("detail-chapters-all").expect("missing 查看全部 row");
        assert!(all.height() >= 40., "查看全部 row too small: {all:?}");
        for hid in ["detail-sort-true", "detail-sort-false", "detail-chapters", "read", "detail-tracking"] {
            assert!(app.hitboxes.contains_key(hid), "missing hitbox {hid}");
        }
        app.detail_desc = true;
        app.hitboxes.clear();
        frame(&mut app, 820., 1180.); // 倒序：最上面的行是最后一话
        let row_top = |app: &ReaderApp, id: &str| app.hitboxes.get(&format!("dchapter-{id}")).map(|r| r.top());
        assert!(row_top(&app, "c7").is_some_and(|top| top < row_top(&app, "c2").unwrap_or(f32::MAX)),
            "descending sort did not put the newest chapter first: {:?}", app.hitboxes.keys().collect::<Vec<_>>());
        assert!(!app.hitboxes.contains_key("dchapter-c1"), "ascending c1 must fold in descending view");
        app.detail_desc = false;
        // 320pt×200% 大字号：行热区仍不越屏、仍可点。
        app.font_scale = 2.;
        app.hitboxes.clear();
        frame(&mut app, 320., 1024.);
        for n in 1..=6 {
            let rect = app.hitboxes.get(&format!("dchapter-c{n}")).expect("missing row");
            assert!(rect.left() >= 0. && rect.right() <= 320.5, "row overflows narrow screen: {rect:?}");
        }
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
}
