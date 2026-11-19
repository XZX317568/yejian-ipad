use crate::{
    motion::{self, ShelfMotion},
    shelf::{self, Plane},
    worker::{Command, Event, Worker},
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
    clip: Rect,
}
struct PageTexture {
    tiles: Vec<TextureHandle>,
    size: Vec2,
}
impl PageTexture {
    fn paint(&self, painter: &Painter, rect: Rect) {
        let mut top = rect.top();
        for tile in &self.tiles {
            let height = tile.size()[1] as f32 / self.size.y * rect.height();
            let target = Rect::from_min_size(pos2(rect.left(), top), vec2(rect.width(), height));
            if target.intersects(painter.clip_rect()) {
                painter.image(
                    tile.id(),
                    target,
                    Rect::from_min_max(Pos2::ZERO, pos2(1., 1.)),
                    Color32::WHITE,
                );
            }
            top += height;
        }
    }
}

#[derive(Clone, Copy)]
struct ShelfLayout {
    stage: Rect,
    books: Rect,
    info: Rect,
    browse: Rect,
    side_by_side: bool,
    short_landscape: bool,
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
    saved_position: Option<(String, usize, f32)>,
    persisted_position: Option<(String, usize, f32)>,
    failed_position: Option<(String, usize, f32, bool)>,
    position_save_error: Option<String>,
    last_position_save: f64,
    zoom: f32,
    pan: Vec2,
    controls: bool,
    reader_drag: f32,
    private: bool,
    rtl: bool,
    vertical: bool,
    spread: bool,
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
    sync: crate::sync::ProgressSync,
    pub hitboxes: BTreeMap<String, Rect>,
    pub rendered_book_nodes: usize,
    pub frame_is_animating: bool,
    pub rendered_page_nodes: usize,
}

impl ReaderApp {
    pub fn new(root: PathBuf, ctx: Context) -> Self {
        let backup_path = root.join("backup.yejian.zip").to_string_lossy().to_string();
        Self {
            worker: Worker::start(root.clone(), ctx.clone()),
            sync: crate::sync::ProgressSync::new(root.clone(), ctx.clone()),
            sources: crate::sources::Sources::new(root.clone(), ctx),
            root,
            books: vec![],
            chapters: Vec::new(),
            chapter_indices: HashMap::new(),
            chapter_dialog: None,
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
            saved_position: None,
            persisted_position: None,
            failed_position: None,
            position_save_error: None,
            last_position_save: 0.,
            zoom: 1.,
            pan: Vec2::ZERO,
            controls: true,
            reader_drag: 0.,
            private: false,
            rtl: false,
            vertical: false,
            spread: false,
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
                            "reading-direction" => self.rtl = value == "rtl",
                            "private" => self.private = value == "true",
                            "reading-mode" => {
                                self.vertical = value == "vertical";
                                self.spread = value == "spread";
                            }
                            "font-scale" => {
                                self.font_scale = value.parse::<f32>().unwrap_or(1.).clamp(1., 2.)
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
                        if !matches!(&self.screen,Screen::Reader(current) if current==&id)
                            || page.abs_diff(self.page) > 2
                        {
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
                                self.pages
                                    .insert((id.clone(), page), PageTexture { tiles, size });
                                self.trim_pages(&id);
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
        let mut bytes: usize = self.covers.values().map(|texture| texture.size()[0] * texture.size()[1] * 4).sum();
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
            self.cover_usage.remove(&id);
        }
    }
    fn ask_page(&mut self, id: &str, page: usize) {
        let key = (id.to_string(), page);
        if page != self.page && self.prefetch_blocked.contains(&key) {
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
    fn trim_pages(&mut self, id: &str) {
        self.pages
            .retain(|(key, page), _| key == id && page.abs_diff(self.page) <= 2);
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
                .filter(|(_, page)| {
                    *page != self.page && !(self.spread && !self.vertical && *page == self.page + 1)
                })
                .max_by_key(|(_, page)| page.abs_diff(self.page))
                .cloned()
            else {
                break;
            };
            self.pages.remove(&key);
            self.prefetch_blocked.insert(key);
        }
    }
    fn open_reader(&mut self, book: &Book) {
        self.flush_position();
        if !matches!(self.screen, Screen::Reader(_)) {
            self.reader_return = self.screen.clone();
        }
        self.page = book.current_page.min(book.page_count.saturating_sub(1));
        self.screen = Screen::Reader(book.id.clone());
        self.vertical_reader.reset(self.page, book.page_fraction);
        self.saved_position = None;
        self.chapter_dialog = None;
        self.reader_options = false;
        self.pending_reader_tap = None;
        self.pages
            .retain(|(id, page), _| id == &book.id && page.abs_diff(self.page) <= 2);
        self.zoom = 1.;
        self.pan = Vec2::ZERO;
        self.controls = true;
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
    fn next_page(&mut self, id: &str, delta: i32) {
        self.pending_reader_tap = None;
        let Some(book) = self.books.iter().find(|b| b.id == id).cloned() else {
            return;
        };
        let target = self.page as i64 + delta as i64;
        if target < 0 || target >= book.page_count as i64 {
            if let Some(mut adjacent) = self.adjacent_chapter(id, if target < 0 { -1 } else { 1 }) {
                adjacent.current_page = if target < 0 {
                    adjacent.page_count.saturating_sub(1)
                } else {
                    0
                };
                adjacent.page_fraction = 0.;
                self.open_reader(&adjacent);
                return;
            }
        }
        let page = (self.page as i64 + delta as i64)
            .clamp(0, book.page_count.saturating_sub(1) as i64) as usize;
        if page == self.page {
            return;
        }
        self.page = page;
        self.vertical_reader.reset(page, 0.);
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
    }
    fn back(&mut self, now: f64, area: Rect) {
        match self.screen.clone() {
            Screen::Reader(_) => {
                self.flush_position();
                self.vertical_reader.stop();
                self.page_generation += 1;
                self.worker
                    .generation
                    .store(self.page_generation, Ordering::Relaxed);
                self.page_pending.clear();
                self.screen = self.reader_return.clone();
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
        let fill = if primary {
            if response.hovered() {
                palette.accent.gamma_multiply(0.88)
            } else {
                palette.accent
            }
        } else if response.hovered() {
            palette.accent_soft
        } else {
            palette.soft
        };
        ui.painter().rect_filled(rect, 10., fill);
        if !primary {
            ui.painter()
                .rect_stroke(rect, 10., Stroke::new(1., palette.line), StrokeKind::Inside);
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
        let p = ui.painter();
        if !id.starts_with("nav-") && !is_reader {
            p.rect_filled(
                rect,
                12.,
                if response.hovered() {
                    palette.accent_soft
                } else {
                    palette.surface
                },
            );
        }
        let c = rect.center();
        let stroke = Stroke::new(1.7, color);
        match kind {
            "search" => {
                p.circle_stroke(c - vec2(2., 2.), 5.6, stroke);
                p.line_segment([c + vec2(2., 2.), c + vec2(9., 9.)], stroke);
            }
            "plus" => {
                p.line_segment([c - vec2(7., 0.), c + vec2(7., 0.)], stroke);
                p.line_segment([c - vec2(0., 7.), c + vec2(0., 7.)], stroke);
            }
            "back" => {
                p.line_segment([c + vec2(4., -7.), c + vec2(-4., 0.)], stroke);
                p.line_segment([c + vec2(-4., 0.), c + vec2(4., 7.)], stroke);
            }
            "next" => {
                p.line_segment([c + vec2(-3., -4.), c + vec2(2., 0.)], stroke);
                p.line_segment([c + vec2(2., 0.), c + vec2(-3., 4.)], stroke);
            }
            "prev" => {
                p.line_segment([c + vec2(3., -4.), c + vec2(-2., 0.)], stroke);
                p.line_segment([c + vec2(-2., 0.), c + vec2(3., 4.)], stroke);
            }
            "open" => {
                p.line_segment([c + vec2(-6., 6.), c + vec2(6., -6.)], stroke);
                p.line_segment([c + vec2(-4., -6.), c + vec2(6., -6.)], stroke);
                p.line_segment([c + vec2(6., -6.), c + vec2(6., 4.)], stroke);
            }
            "library" => {
                p.rect_stroke(
                    Rect::from_center_size(c, vec2(12., 16.)),
                    1.,
                    stroke,
                    StrokeKind::Inside,
                );
                p.line_segment([c + vec2(1., -7.), c + vec2(1., -1.)], stroke);
            }
            "history" => {
                p.circle_stroke(c, 7., stroke);
                p.line_segment([c, c - vec2(0., 4.)], stroke);
                p.line_segment([c, c + vec2(4., 2.)], stroke);
            }
            "settings" => {
                p.circle_stroke(c, 7., stroke);
                p.circle_stroke(c, 2., stroke);
            }
            "chapters" => {
                for y in [-5., 0., 5.] {
                    p.line_segment([c + vec2(-7., y), c + vec2(7., y)], stroke);
                }
            }
            _ => {
                p.circle_stroke(c, 7., stroke);
            }
        }
        response.clicked()
    }
    fn shelf_area(&self, area: Rect) -> Rect {
        self.shelf_layout(area).books
    }
    fn shelf_layout(&self, area: Rect) -> ShelfLayout {
        let margin = if area.width() >= 700. { 32. } else { 16. };
        let width = (area.width() - margin * 2.).min(1200.);
        let side_by_side = width >= 660.;
        let short_landscape = side_by_side && self.shelf_viewport.height() < 600.;
        let stacked_header = self.font_scale > 1.4 && area.width() < 520.;
        let top = 144.
            + if stacked_header {
                84.
            } else {
                14. * (self.font_scale - 1.)
            };
        let tall_extra = if side_by_side {
            ((self.shelf_viewport.height() - 900.) * 0.30).clamp(0., 100.)
        } else {
            0.
        };
        let height = if short_landscape {
            224. + (self.font_scale - 1.) * 16.
        } else if side_by_side {
            374. + tall_extra + (self.font_scale - 1.) * 40.
        } else {
            384. + (self.font_scale - 1.) * 112.
        };
        let stage = Rect::from_min_size(
            pos2(area.center().x - width / 2., area.top() + top),
            vec2(width, height),
        );
        let books = if side_by_side {
            Rect::from_min_size(
                stage.min + vec2(12., 12.),
                vec2(width * 0.55 - 12., height - 24.),
            )
        } else {
            Rect::from_min_size(stage.min + vec2(8., 6.), vec2(width - 16., 212.))
        };
        let info = if side_by_side {
            Rect::from_min_max(
                pos2(
                    books.right() + 28.,
                    stage.top() + if short_landscape { 14. } else { 76. },
                ),
                stage.max - vec2(28., 26.),
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
        while let Some(chapter) = self.sources.take_downloaded() {
            let id = chapter.id.clone();
            if let Err(error) = self.worker.send(Command::ImportChapter {
                task_id: chapter.id,
                path: chapter.path,
                metadata: chapter.metadata,
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
        let rail_width = if full.width() >= 1280. { 176. } else { 88. };
        let content = if tablet {
            Rect::from_min_max(full.min + vec2(rail_width, 0.), full.max)
        } else {
            full
        };
        let system_back = yejian_platform::take_back_requested()
            || ctx.input(|i| i.key_pressed(Key::BrowserBack));
        if system_back || ctx.input(|i| i.key_pressed(Key::Escape)) {
            self.request_back(now, content, system_back);
        }
        let is_reader = matches!(self.screen, Screen::Reader(_));
        let animating = self.motion.tick(now);
        self.frame_is_animating = animating || entrance < 1. || self.flight.is_some();
        if self.frame_is_animating {
            ctx.request_repaint();
        }
        if is_reader {
            if let Screen::Reader(id) = self.screen.clone() {
                self.reader_ui(ui, full, &id, now);
            }
        } else {
            if !matches!(self.screen, Screen::Library) {
                if self.icon(
                    ui,
                    "back",
                    Rect::from_min_size(content.min + vec2(24., 14.), vec2(44., 44.)),
                    "back",
                    "返回书架",
                ) {
                    self.back(now, content);
                }
            } else {
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
                    self.search = !self.search;
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
                    self.pick_import();
                }
            }
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
                    Screen::Library => match self.tab {
                        Tab::Library => self.library_viewport(ui, content, now, entrance),
                        Tab::History => self.history_ui(ui, content),
                        Tab::Settings => self.settings_ui(ui, content),
                        Tab::Sources => self.sources_ui(ui, content),
                    },
                    Screen::Detail(id) => self.detail_ui(ui, content, &id),
                    _ => {}
                }
            }
            if tablet || matches!(self.screen, Screen::Library) {
                self.navigation(ui, full, tablet);
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
        if let Some((text, at)) = &self.notice {
            if now - *at < 5. {
                let rect = Rect::from_center_size(
                    pos2(full.center().x, full.bottom() - 100.),
                    vec2((full.width() - 32.).min(560.), 40.),
                );
                ui.painter().rect_filled(rect, 10., ink);
                ui.painter().text(
                    rect.center(),
                    Align2::CENTER_CENTER,
                    text,
                    FontId::proportional(12.),
                    self.colors().0,
                );
            }
        }
        self.dialogs(&ctx);
        self.trim_covers();
        self.frame_is_animating = self.motion.active()
            || entrance < 1.
            || self.flight.is_some()
            || (matches!(self.screen, Screen::Reader(_))
                && self.vertical
                && self.vertical_reader.active());
        if self.frame_is_animating {
            ctx.request_repaint();
        }
    }
    fn navigation(&mut self, ui: &mut Ui, full: Rect, tablet: bool) {
        let palette = crate::design::palette(self.dark);
        let tabs = [
            (Tab::Library, "书架", "library"),
            (Tab::Sources, "书源", "search"),
            (Tab::History, "历史", "history"),
            (Tab::Settings, "设置", "settings"),
        ];
        let expanded = tablet && full.width() >= 1280.;
        let rail = if tablet {
            Rect::from_min_size(
                full.min,
                vec2(if expanded { 176. } else { 88. }, full.height()),
            )
        } else {
            Rect::from_min_size(
                pos2(
                    full.center().x - (full.width() - 32.).min(480.) / 2.,
                    full.bottom() - 72.,
                ),
                vec2((full.width() - 32.).min(480.), 64.),
            )
        };
        ui.painter()
            .rect_filled(rail, if tablet { 0. } else { 23. }, palette.surface);
        if !tablet {
            ui.painter()
                .rect_stroke(rail, 23., Stroke::new(1., palette.line), StrokeKind::Inside);
        } else {
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
        for (i, (tab, label, kind)) in tabs.into_iter().enumerate() {
            let rect = if tablet {
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
                Rect::from_min_size(
                    pos2(rail.left() + i as f32 * rail.width() / 4., rail.top()),
                    vec2(rail.width() / 4., 64.),
                )
            };
            if tab == self.tab {
                ui.painter()
                    .rect_filled(rect.shrink2(vec2(5., 5.)), 16., palette.accent_soft);
            }
            // The whole item is the tap target: the glyph alone is only
            // 32x29 pt, below the 44 pt Apple asks for.
            let row_clicked = ui
                .interact(rect, Id::unique(("nav-row", i)), Sense::click())
                .clicked();
            let icon_rect = Rect::from_center_size(
                if expanded {
                    pos2(rect.left() + 28., rect.center().y)
                } else {
                    rect.center() - vec2(0., 9.)
                },
                vec2(32., 29.),
            );
            let clicked = self.icon(ui, &format!("nav-{kind}"), icon_rect, kind, label);
            self.hitboxes.insert(format!("nav-{kind}"), rect);
            ui.painter().text(
                if expanded {
                    pos2(rect.left() + 55., rect.center().y)
                } else {
                    rect.center() + vec2(0., 8.)
                },
                if expanded {
                    Align2::LEFT_CENTER
                } else {
                    Align2::CENTER_TOP
                },
                label,
                FontId::proportional(if expanded { 16. } else { 12. }),
                if tab == self.tab {
                    palette.accent
                } else {
                    palette.muted
                },
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
    fn side_navigation(area: Rect) -> bool {
        area.width() >= 1000. || (area.width() >= 700. && area.height() < 600.)
    }
    fn library_viewport(&mut self, ui: &mut Ui, area: Rect, now: f64, entrance: f32) {
        self.shelf_viewport = area;
        let height = self.shelf_layout(area).browse.bottom() - area.top() + 24.;
        let bottom = if Self::side_navigation(ui.ctx().content_rect()) {
            16.
        } else {
            80.
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
            let y = area.top() + area.height() * 0.32;
            let empty = Rect::from_min_max(pos2(area.left() + 32., y), area.max - vec2(32., 76.));
            ui.scope_builder(UiBuilder::new().max_rect(empty), |ui| {
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
        let shelf_area = layout.books;
        let palette = crate::design::palette(self.dark);
        let stage_color = if self.dark {
            Color32::from_rgb(40, 43, 39)
        } else {
            Color32::from_rgb(235, 233, 223)
        };
        ui.painter().rect_filled(layout.stage, 22., stage_color);
        if layout.side_by_side {
            ui.painter().line_segment(
                [
                    pos2(layout.info.left() - 20., layout.stage.top() + 40.),
                    pos2(layout.info.left() - 20., layout.stage.bottom() - 40.),
                ],
                Stroke::new(1., palette.ink.gamma_multiply(0.08)),
            );
        }
        let volumes = shelf::geometry(shelf_area, books.len(), self.motion.focus, entrance);
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
        }
        let p = ui.painter().with_clip_rect(shelf_area);
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
                self.covers.get(&cover),
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
            let background = stage_color;
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
        let pitch = shelf::lane_pitch_for(shelf_area, books.len());
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
        let progress_pos = info + vec2(0., title.size().y + 10.);
        ui.painter().galley(info, title, ink);
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
        let action_y = progress_pos.y + progress.size().y + 25.;
        let palette = crate::design::palette(self.dark);
        let track = Rect::from_min_size(
            pos2(info.x, progress_pos.y + progress.size().y + 12.),
            vec2((info_right - info.x).min(360.), 3.),
        );
        ui.painter().rect_filled(track, 2., palette.line);
        if has_progress {
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
        ui.painter().galley(progress_pos, progress, muted);
        let detail_width = 30. * self.font_scale + 24.;
        let action_width =
            (84. * self.font_scale + 68.).min(layout.info.width() - detail_width - 12.);
        let open_rect = Rect::from_min_size(
            pos2(info.x + action_width + 12., action_y),
            vec2(detail_width, 50.),
        );
        if self.selected.is_empty() && self.button(ui, "open-book", open_rect, "详情", false) {
            if let Some(volume) = volumes
                .iter()
                .find(|v| v.lane == self.motion.focus.round() as i32)
            {
                self.open_detail(book, volume, &volumes, area, now);
            }
        }
        let action = Rect::from_min_size(pos2(info.x, action_y), vec2(action_width, 50.));
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
        let pager_right = if layout.short_landscape {
            layout.books.right() - 8.
        } else {
            layout.stage.right() - 16.
        };
        let tally = format!("{:02} / {:02}", index + 1, books.len());
        let tally = ui
            .painter()
            .layout_no_wrap(tally, FontId::monospace(11.), muted);
        let pager_left = pager_right - 88. - tally.size().x - 4.;
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
                pos2(pager_left + 46., y + (44. - tally.size().y) / 2.),
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
        self.library_thumbnails(ui, layout.browse, &books, index, now);
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
                        (books.len() as f32 * cell_width).max(canvas.width()),
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
                        if ui.clip_rect().contains_rect(rect) {
                            self.hitboxes
                                .insert(format!("shelf-thumbnail-{}", book.id), rect);
                        }
                        let cover = Rect::from_min_size(
                            rect.min + vec2(4., 3.),
                            vec2(cover_width, cover_height),
                        );
                        if index == selected || response.hovered() {
                            ui.painter()
                                .rect_filled(cover.expand(4.), 9., palette.accent_soft);
                            ui.painter().rect_stroke(
                                cover.expand(4.),
                                9.,
                                Stroke::new(1., palette.accent),
                                StrokeKind::Inside,
                            );
                        }
                        let cover_id = self.cover_id(&book.id);
                        self.ask_cover(&cover_id);
                        if let Some(texture) = self.covers.get(&cover_id) {
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
        let palette = crate::design::palette(self.dark);
        ui.painter()
            .rect_filled(cover.expand2(vec2(24., 24.)), 24., palette.soft);
        ui.painter().rect_filled(
            cover.translate(vec2(7., 9.)).expand(2.),
            9.,
            palette.ink.gamma_multiply(0.08),
        );
        if self.flight.is_none()
            && let Some(texture) = self.covers.get(&cover_id)
        {
            shelf::paint_art(
                ui.painter(),
                Plane::rect(cover),
                cover.size(),
                texture,
                self.colors().0,
            );
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
        let details_start = if Self::detail_side_by_side(area) {
            pos2(area.center().x, area.top() + 68.)
        } else if Self::detail_wide(area) {
            pos2(cover.right() + 72., cover.top() + cover.height() * 0.15)
        } else {
            pos2(area.left() + 24., cover.bottom() + 44.)
        };
        if Self::detail_wide(area) {
            let body = Rect::from_min_size(
                details_start,
                vec2(360., (area.bottom() - 24. - details_start.y).max(160.)),
            );
            ui.scope_builder(UiBuilder::new().max_rect(body), |ui| {
                ScrollArea::vertical()
                    .id_salt(("detail-body", id))
                    .show(ui, |ui| {
                        ui.add(
                            Label::new(
                                RichText::new(title)
                                    .font(crate::typography::display(36. * self.font_scale))
                                    .color(ink),
                            )
                            .wrap(),
                        );
                        ui.add_space(8.);
                        ui.add(
                            Label::new(
                                RichText::new(text).size(15. * self.font_scale).color(muted),
                            )
                            .wrap(),
                        );
                        ui.add_space(28.);
                        self.detail_actions(ui, &book, id, 280., 52.);
                    });
            });
            return;
        }
        let body = Rect::from_min_max(details_start, area.max - vec2(24., 12.));
        ui.scope_builder(UiBuilder::new().max_rect(body), |ui| {
            ScrollArea::vertical()
                .id_salt(("detail-body", id))
                .show(ui, |ui| {
                    ui.vertical_centered(|ui| {
                        let title_size = if area.width() >= 700. { 30. } else { 24. };
                        ui.add(
                            Label::new(
                                RichText::new(title)
                                    .font(crate::typography::display(title_size * self.font_scale))
                                    .color(ink),
                            )
                            .wrap(),
                        );
                        ui.add_space(6.);
                        ui.add(
                            Label::new(
                                RichText::new(text).size(13. * self.font_scale).color(muted),
                            )
                            .wrap(),
                        );
                        ui.add_space(18.);
                        let width = (218. * self.font_scale.sqrt()).min(ui.available_width());
                        self.detail_actions(ui, &book, id, width, 48.);
                    });
                });
        });
    }
    fn detail_actions(
        &mut self,
        ui: &mut Ui,
        book: &yejian_core::Book,
        id: &str,
        width: f32,
        primary_height: f32,
    ) {
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
        if can_restart {
            let (rect, _) = ui.allocate_exact_size(vec2(width, 46.), Sense::hover());
            if self.button(ui, "read-start", rect, "从第一页开始", false) {
                let mut first = book.clone();
                first.current_page = 0;
                first.page_fraction = 0.;
                self.flight = None;
                self.open_reader(&first);
            }
        }
        if self.chapter(id).is_some() {
            let (rect, _) = ui.allocate_exact_size(vec2(width, 46.), Sense::hover());
            if self.button(ui, "detail-chapters", rect, "已下载章节", false) {
                self.chapter_dialog = Some(id.to_string());
            }
        }
    }
    fn reader_input_blocked(&self) -> bool {
        self.reader_options || self.chapter_dialog.is_some() || self.error.is_some()
    }
    fn set_reading_mode(&mut self, mode: &str) {
        if self.vertical == (mode == "vertical") && self.spread == (mode == "spread") {
            return;
        }
        self.flush_position();
        self.vertical = mode == "vertical";
        self.spread = mode == "spread";
        self.vertical_reader.reset(self.page, 0.);
        self.zoom = 1.;
        self.pan = Vec2::ZERO;
        self.pending_reader_tap = None;
        self.setting("reading-mode", mode.into());
    }
    fn reader_tap(&mut self, id: &str, relative: f32, step: i32) {
        if self.vertical || self.zoom > 1. || (0.27..=0.73).contains(&relative) {
            self.controls = !self.controls;
        } else if relative < 0.27 {
            self.next_page(id, if self.rtl { step } else { -step });
        } else {
            self.next_page(id, if self.rtl { -step } else { step });
        }
    }
    fn reader_ui(&mut self, ui: &mut Ui, area: Rect, id: &str, now: f64) {
        let Some(book) = self.books.iter().find(|b| b.id == id).cloned() else {
            return;
        };
        let ctx = ui.ctx().clone();
        ui.painter()
            .rect_filled(area, 0., Color32::from_rgb(18, 20, 22));
        self.ask_page(id, self.page);
        let step = if self.spread && !self.vertical && area.width() > 700. {
            2
        } else {
            1
        };
        if self.page + 1 < book.page_count {
            self.ask_page(id, self.page + 1);
        }
        let content = area.shrink2(vec2(8., if self.controls { 64. } else { 0. }));
        let input_blocked = self.reader_input_blocked();
        let gesture = ui.interact(
            content,
            Id::unique("reader-canvas"),
            if input_blocked {
                Sense::hover()
            } else {
                Sense::click_and_drag()
            },
        );
        self.hitboxes.insert("reader-canvas".into(), content);
        let zoom = if input_blocked {
            1.
        } else {
            ui.input(|i| i.zoom_delta())
        };
        if zoom != 1. {
            self.zoom = (self.zoom * zoom).clamp(1., 5.);
            self.pending_reader_tap = None;
            if self.zoom == 1. {
                self.pan = Vec2::ZERO;
            }
        }
        if gesture.drag_started() {
            self.reader_drag = 0.;
            self.vertical_reader.stop();
            self.pending_reader_tap = None;
        }
        if gesture.dragged() {
            self.reader_drag += ui.input(|i| i.pointer.delta().x);
        }
        if gesture.dragged() && self.zoom > 1. {
            let delta = ui.input(|i| i.pointer.delta());
            self.pan += if self.vertical {
                vec2(delta.x, 0.)
            } else {
                delta
            };
        }
        if gesture.drag_stopped() && self.zoom <= 1. && !self.vertical {
            let distance = self.reader_drag;
            self.reader_drag = 0.;
            if distance.abs() > 45. {
                self.next_page(
                    id,
                    if (distance < 0.) ^ self.rtl {
                        step
                    } else {
                        -step
                    },
                );
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
                self.zoom = if self.zoom > 1. { 1. } else { 2. };
                self.pan = Vec2::ZERO;
                self.pending_reader_tap = None;
            }
            if let Some((deadline, pos)) = self.pending_reader_tap {
                if now >= deadline && !ctx.input(|i| i.pointer.primary_down()) {
                    self.pending_reader_tap = None;
                    self.reader_tap(id, (pos.x - content.left()) / content.width(), step);
                } else {
                    ctx.request_repaint_after(std::time::Duration::from_secs_f64(
                        (deadline - now).max(0.01),
                    ));
                }
            }
            if !double_tap && let Some(pos) = tapped {
                if let Some((_, first)) = self.pending_reader_tap.take() {
                    self.reader_tap(id, (first.x - content.left()) / content.width(), step);
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
                self.next_page(id, delta);
            }
        }
        if !matches!(&self.screen,Screen::Reader(current) if current==id) {
            return;
        }
        self.rendered_page_nodes = 0;
        if self.vertical {
            self.vertical_ui(ui, content, id, book.page_count, now, &gesture);
        } else if let Some(error) = self.page_errors.get(&(id.into(), self.page)).cloned() {
            ui.put(
                Rect::from_center_size(content.center(), vec2(content.width() - 40., 120.)),
                Label::new(
                    RichText::new(format!("这一页暂时无法打开\n{error}")).color(Color32::WHITE),
                )
                .wrap(),
            );
            if self.button(
                ui,
                "retry-page",
                Rect::from_center_size(content.center() + vec2(0., 90.), vec2(144., 44.)),
                "重试这一页",
                false,
            ) {
                self.page_errors.remove(&(id.into(), self.page));
                self.ask_page(id, self.page);
            }
        } else if let Some(texture) = self.pages.get(&(id.into(), self.page)) {
            let first_on_right = step == 2 && self.rtl;
            let available = if step == 2 {
                Rect::from_min_size(
                    content.min
                        + vec2(
                            if first_on_right {
                                content.width() / 2.
                            } else {
                                0.
                            },
                            0.,
                        ),
                    vec2(content.width() / 2., content.height()),
                )
            } else {
                content
            };
            let size = texture.size;
            let scale = (available.width() / size.x).min(available.height() / size.y);
            let size = size * scale * self.zoom;
            let limit = ((size - available.size()) * 0.5).max(Vec2::ZERO);
            self.pan.x = self.pan.x.clamp(-limit.x, limit.x);
            self.pan.y = self.pan.y.clamp(-limit.y, limit.y);
            let center = available.center();
            let image_rect = Rect::from_center_size(center + self.pan, size);
            self.hitboxes.insert("reader-image".into(), image_rect);
            texture.paint(&ui.painter().with_clip_rect(content), image_rect);
            self.rendered_page_nodes = 1;
            if step == 2 {
                let second_center = available.center()
                    + vec2(
                        if first_on_right {
                            -available.width()
                        } else {
                            available.width()
                        },
                        0.,
                    );
                if let Some(second) = self.pages.get(&(id.into(), self.page + 1)) {
                    let size = second.size;
                    let scale = (available.width() / size.x).min(available.height() / size.y);
                    let target = Rect::from_center_size(
                        available.center()
                            + vec2(
                                if first_on_right {
                                    -available.width()
                                } else {
                                    available.width()
                                },
                                0.,
                            )
                            + self.pan,
                        size * scale * self.zoom,
                    );
                    second.paint(&ui.painter().with_clip_rect(content), target);
                    self.rendered_page_nodes += 1;
                } else if self.page + 1 < book.page_count {
                    if let Some(error) = self
                        .page_errors
                        .get(&(id.to_string(), self.page + 1))
                        .cloned()
                    {
                        let page = self.page + 1;
                        ui.put(
                            Rect::from_center_size(
                                second_center,
                                vec2(available.width() - 28., 130.),
                            ),
                            Label::new(
                                RichText::new(format!("第 {} 页无法打开\n{error}", page + 1))
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
                            self.page_errors.remove(&(id.to_string(), page));
                            self.prefetch_blocked.remove(&(id.to_string(), page));
                            self.ask_page(id, page);
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
        } else {
            ui.painter().text(
                content.center(),
                Align2::CENTER_CENTER,
                "正在打开这一页…",
                FontId::proportional(15.),
                Color32::LIGHT_GRAY,
            );
        }
        if !matches!(&self.screen, Screen::Reader(current) if current == id) {
            return;
        }
        if self.controls {
            let palette = crate::design::palette(true);
            let (bar, ink, muted) = (palette.surface, palette.ink, palette.muted);
            ui.style_mut().visuals = crate::design::visuals(true);
            ui.painter().rect_filled(
                Rect::from_min_size(area.min, vec2(area.width(), 56.)),
                0.,
                bar,
            );
            ui.painter().rect_filled(
                Rect::from_min_size(
                    pos2(area.left(), area.bottom() - 62.),
                    vec2(area.width(), 62.),
                ),
                0.,
                bar,
            );
            if self.icon(
                ui,
                "reader-back",
                Rect::from_min_size(area.min + vec2(8., 8.), vec2(44., 44.)),
                "back",
                match (&self.reader_return, self.tab) {
                    (Screen::Library, Tab::History) => "返回阅读足迹",
                    (Screen::Library, Tab::Sources) => "返回书源",
                    (Screen::Library, _) => "返回书架",
                    _ => "返回详情",
                },
            ) {
                self.back(now, area);
            }
            ui.painter()
                .with_clip_rect(Rect::from_min_max(
                    area.min + vec2(56., 0.),
                    pos2(
                        area.right()
                            - if self.chapter(id).is_some() {
                                110.
                            } else {
                                58.
                            },
                        area.top() + 56.,
                    ),
                ))
                .text(
                    area.min + vec2(56., 20.),
                    Align2::LEFT_TOP,
                    self.reading_title(&book),
                    FontId::proportional(15.),
                    ink,
                );
            if self.chapter(id).is_some()
                && self.icon(
                    ui,
                    "reader-chapters",
                    Rect::from_min_size(pos2(area.right() - 104., area.top() + 8.), vec2(44., 44.)),
                    "chapters",
                    "已下载章节",
                )
            {
                self.chapter_dialog = Some(id.to_string());
            }
            if self.icon(
                ui,
                "reader-settings",
                Rect::from_min_size(pos2(area.right() - 52., area.top() + 8.), vec2(44., 44.)),
                "settings",
                "阅读设置",
            ) {
                self.reader_options = true;
                self.vertical_reader.stop();
                self.pending_reader_tap = None;
            }
            if self.icon(
                ui,
                "reader-prev",
                Rect::from_min_size(pos2(area.left() + 10., area.bottom() - 55.), vec2(44., 44.)),
                "prev",
                "上一页",
            ) {
                self.next_page(id, -step);
            }
            if self.icon(
                ui,
                "reader-next",
                Rect::from_min_size(
                    pos2(area.right() - 50., area.bottom() - 55.),
                    vec2(44., 44.),
                ),
                "next",
                "下一页",
            ) {
                self.next_page(id, step);
            }
            let mut page = self.page + 1;
            let rect = Rect::from_min_size(
                pos2(area.left() + 62., area.bottom() - 49.),
                vec2(area.width() - 124., 24.),
            );
            let old_width = ui.spacing().slider_width;
            ui.spacing_mut().slider_width = rect.width();
            if ui
                .put(
                    rect,
                    Slider::new(&mut page, 1..=book.page_count.max(1)).show_value(false),
                )
                .changed()
            {
                self.next_page(id, page as i32 - 1 - self.page as i32);
            }
            ui.spacing_mut().slider_width = old_width;
            ui.painter().text(
                pos2(area.center().x, area.bottom() - 15.),
                Align2::CENTER_CENTER,
                format!(
                    "{} / {}{}",
                    self.page + 1,
                    book.page_count,
                    if self.private { " · 私密阅读" } else { "" }
                ),
                FontId::monospace(10.),
                muted,
            );
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
        let heights: HashMap<usize, f32> = self
            .pages
            .iter()
            .filter(|((key, _), _)| key == id)
            .map(|((_, page), image)| (*page, image.size.y / image.size.x * width))
            .collect();
        let next_chapter = self.adjacent_chapter(id, 1);
        let footer = if next_chapter.is_some() { 68. } else { 0. };
        self.vertical_reader
            .settle(count, content.height() - footer, |page| {
                heights.get(&page).copied()
            });
        if self.page != self.vertical_reader.page {
            self.page = self.vertical_reader.page;
            self.prefetch_blocked.clear();
            self.trim_pages(id);
        }
        for page in self.page.saturating_sub(1)..=(self.page + 2).min(count.saturating_sub(1)) {
            self.ask_page(id, page);
        }
        let height = heights.get(&self.page).copied().unwrap_or(content.height());
        let mut top = content.top() - self.vertical_reader.top_offset(height);
        let start = self.page.saturating_sub(1);
        if start < self.page {
            top -= heights.get(&start).copied().unwrap_or(content.height());
        }
        let painter = ui.painter().with_clip_rect(content);
        for page in start..=(self.page + 2).min(count.saturating_sub(1)) {
            let height = heights.get(&page).copied().unwrap_or(content.height());
            let target = Rect::from_min_size(
                pos2(content.center().x - width / 2. + self.pan.x, top),
                vec2(width, height),
            );
            top += height;
            if !target.intersects(content) {
                continue;
            }
            self.hitboxes
                .insert(format!("vertical-page-{page}"), target);
            if let Some(texture) = self.pages.get(&(id.to_string(), page)) {
                texture.paint(&painter, target);
                self.rendered_page_nodes += 1;
            } else if let Some(error) = self.page_errors.get(&(id.to_string(), page)).cloned() {
                let center = target.intersect(content).center();
                ui.put(
                    Rect::from_center_size(center, vec2(content.width() - 40., 120.)),
                    Label::new(
                        RichText::new(format!("第 {} 页无法打开\n{error}", page + 1))
                            .color(Color32::WHITE),
                    )
                    .wrap(),
                );
                if self.button(
                    ui,
                    &format!("retry-page-{page}"),
                    Rect::from_center_size(center + vec2(0., 82.), vec2(144., 44.)),
                    "重试这一页",
                    false,
                ) {
                    self.page_errors.remove(&(id.to_string(), page));
                    self.ask_page(id, page);
                }
            } else {
                painter.text(
                    target.intersect(content).center(),
                    Align2::CENTER_CENTER,
                    "正在打开这一页…",
                    FontId::proportional(15.),
                    Color32::LIGHT_GRAY,
                );
            }
        }
        if self.pages.contains_key(&(id.to_string(), self.page))
            && (now - self.last_position_save >= 0.25
                || (!gesture.dragged() && !self.vertical_reader.active() && scroll.abs() < 0.1))
        {
            self.record_page(id, self.page);
            self.last_position_save = now;
        }
        if let Some(mut next) = next_chapter
            && let Some(last) = self
                .hitboxes
                .get(&format!("vertical-page-{}", count.saturating_sub(1)))
                .copied()
        {
            let button = Rect::from_min_size(
                pos2(
                    content.center().x - (content.width() - 24.).min(420.) / 2.,
                    last.bottom() + 12.,
                ),
                vec2((content.width() - 24.).min(420.), 48.),
            );
            if content.contains_rect(button)
                && self.button(ui, "reader-next-chapter", button, "下一章", true)
            {
                next.current_page = 0;
                next.page_fraction = 0.;
                self.open_reader(&next);
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
                        ui.painter().rect_filled(rect, 18., palette.surface);
                        let cover = Rect::from_min_size(rect.min + vec2(14., 14.), vec2(58., 82.));
                        if let Some(texture) = self.covers.get(&cover_id) {
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
                        self.label(
                            ui.painter(),
                            text_pos + vec2(0., 24. * self.font_scale + 4.),
                            &format!("第 {} / {} 页", book.current_page + 1, book.page_count),
                            13.,
                            palette.muted,
                        );
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
        let column = Self::page_column(area, 92., 88.);
        ui.scope_builder(UiBuilder::new().max_rect(column), |ui| {
            ScrollArea::vertical()
                .id_salt("reader-settings")
                .show(ui, |ui| {
                    ui.style_mut().wrap_mode = Some(TextWrapMode::Wrap);
                    Frame::new()
                        .inner_margin(Margin::symmetric(2, 12))
                        .show(ui, |ui| {
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
                            ui.spacing_mut().slider_width = (ui.available_width() - 12.).max(80.);
                            if ui
                                .add(
                                    Slider::new(&mut percent, 100. ..=200.)
                                        .show_value(false)
                                        .step_by(5.),
                                )
                                .changed()
                            {
                                self.font_scale = percent / 100.;
                                self.setting("font-scale", self.font_scale.to_string());
                            }
                            ui.add_space(12.);
                            if crate::design::toggle(
                                ui,
                                &mut self.reduced,
                                "减少动态效果",
                                "切书和打开封面时，使用更轻的过渡",
                            ) {
                                self.setting("reduce-motion", self.reduced.to_string());
                            }
                        });
                    ui.add_space(10.);
                    ui.separator();
                    Frame::new()
                        .inner_margin(Margin::symmetric(2, 16))
                        .show(ui, |ui| {
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
                            if crate::design::toggle(
                                ui,
                                &mut self.rtl,
                                "从右向左翻页",
                                "适合日式漫画的阅读顺序",
                            ) {
                                self.setting(
                                    "reading-direction",
                                    if self.rtl { "rtl" } else { "ltr" }.into(),
                                );
                            }
                            ui.separator();
                            ui.add_space(6.);
                            if crate::design::toggle(
                                ui,
                                &mut self.private,
                                "私密阅读",
                                "不保存新的阅读进度和历史",
                            ) {
                                self.setting("private", self.private.to_string());
                            }
                        });
                    ui.add_space(10.);
                    ui.separator();
                    Frame::new()
                        .inner_margin(Margin::symmetric(2, 16))
                        .show(ui, |ui| {
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
                    ui.add_space(10.);
                    ui.separator();
                    Frame::new()
                        .inner_margin(Margin::symmetric(2, 16))
                        .show(ui, |ui| {
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
                });
        });
    }
    fn sources_ui(&mut self, ui: &mut Ui, area: Rect) {
        let mut column = Self::page_column(area, 88., 88.);
        let width = (area.width() - 48.).min(1040.);
        column.min.x = area.center().x - width / 2.;
        column.max.x = area.center().x + width / 2.;
        ui.scope_builder(UiBuilder::new().max_rect(column), |ui| {
            ui.style_mut().wrap_mode = Some(TextWrapMode::Wrap);
            self.sources.ui(ui);
        });
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
            let response = Modal::new(Id::unique("reader-options"))
                .frame(
                    Frame::popup(&ctx.style_of(Theme::Dark))
                        .fill(crate::design::palette(true).surface)
                        .inner_margin(18)
                        .corner_radius(16),
                )
                .show(ctx, |ui| {
                    ui.set_width((ctx.content_rect().width() - 56.).clamp(200., 440.));
                    ui.set_max_width((ctx.content_rect().width() - 56.).clamp(200., 440.));
                    ui.style_mut().visuals = crate::design::visuals(true);
                    ui.horizontal(|ui| {
                        ui.heading("阅读设置");
                    });
                    let height = (ctx.content_rect().height() - 180.).max(120.);
                    ScrollArea::vertical().max_height(height).show(ui, |ui| {
                        ui.label("阅读方式");
                        let mode = if self.vertical {
                            "vertical"
                        } else if self.spread {
                            "spread"
                        } else {
                            "paged"
                        };
                        for (key, name) in [
                            ("paged", "单页"),
                            ("spread", "平板双页"),
                            ("vertical", "上下滚动"),
                        ] {
                            let response = ui.add_sized(
                                [ui.available_width(), 48.],
                                Button::new(name).selected(mode == key).wrap(),
                            );
                            self.hitboxes
                                .insert(format!("reader-mode-{key}"), response.rect);
                            if response.clicked() {
                                self.set_reading_mode(key);
                            }
                        }
                        ui.add_space(8.);
                        let direction = ui.add_sized(
                            [ui.available_width(), 48.],
                            Button::new(if self.rtl {
                                "翻页方向：从右向左"
                            } else {
                                "翻页方向：从左向右"
                            })
                            .wrap(),
                        );
                        self.hitboxes
                            .insert("reader-direction".into(), direction.rect);
                        if direction.clicked() {
                            self.rtl = !self.rtl;
                            self.setting(
                                "reading-direction",
                                if self.rtl { "rtl" } else { "ltr" }.into(),
                            );
                        }
                        let reset = ui.add_sized(
                            [ui.available_width(), 48.],
                            Button::new("恢复原始大小").wrap(),
                        );
                        self.hitboxes.insert("reader-reset-zoom".into(), reset.rect);
                        if reset.clicked() {
                            self.zoom = 1.;
                            self.pan = Vec2::ZERO;
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
    pub fn current_book_id(&self) -> Option<&str> {
        match &self.screen {
            Screen::Reader(id) => Some(id),
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
        app.next_page("new", 10);
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
    fn page_cache_never_evicts_the_visible_spread_partner() {
        let root = tempfile::tempdir().unwrap();
        let ctx = Context::default();
        let mut app = ReaderApp::new(root.path().into(), ctx.clone());
        app.spread = true;
        for index in 0..3 {
            let mut texture = page(&ctx);
            texture.size = vec2(3000., 9000.);
            app.pages.insert(("book".into(), index), texture);
        }
        app.trim_pages("book");
        assert!(app.pages.contains_key(&("book".into(), 0)));
        assert!(app.pages.contains_key(&("book".into(), 1)));
        assert!(!app.pages.contains_key(&("book".into(), 2)));
        assert!(app.prefetch_blocked.contains(&("book".into(), 2)));
    }
}
