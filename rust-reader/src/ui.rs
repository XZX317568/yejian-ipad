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
    reader_from_shelf: bool,
    motion: ShelfMotion,
    first_frame: Option<f64>,
    shelf_viewport: Rect,
    shelf_shift: f32,
    flight: Option<Flight>,
    covers: HashMap<String, TextureHandle>,
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
            reader_from_shelf: false,
            motion: ShelfMotion::default(),
            first_frame: None,
            shelf_viewport: Rect::NOTHING,
            shelf_shift: 0.,
            flight: None,
            covers: HashMap::new(),
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
                || (!query.is_empty() && !book.title.to_lowercase().contains(&query)
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
        representatives.into_iter().map(|book| {
            let mut entry = book.clone();
            if let Some(chapter) = self.chapter(&book.id) {
                entry.title.clone_from(&chapter.manga_title);
            }
            entry
        }).collect()
    }
    fn chapter(&self, id: &str) -> Option<&ChapterMetadata> {
        self.chapter_indices.get(id).map(|&index| &self.chapters[index].metadata)
    }
    fn same_series(&self, a: &str, b: &str) -> bool {
        match (self.chapter(a), self.chapter(b)) {
            (Some(a), Some(b)) => a.source_id == b.source_id && a.manga_id == b.manga_id,
            _ => a == b,
        }
    }
    fn replace_links(&mut self, links: Vec<ChapterLink>) {
        self.chapter_indices = links.iter().enumerate()
            .map(|(i, link)| (link.book_id.clone(), i))
            .collect();
        self.chapters = links;
    }
    fn chapters_for(&self, id: &str) -> Vec<Book> {
        self.chapters
            .iter()
            .filter(|link| self.same_series(id, &link.book_id))
            .filter_map(|link| self.books.iter().find(|book| book.id == link.book_id).cloned())
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
        if self.chapter(id).is_none() { return id.to_string(); }
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
                            Err(_) => {
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
            self.reader_from_shelf = matches!(self.screen, Screen::Library) && self.tab == Tab::Library;
        }
        self.page = book.current_page.min(book.page_count.saturating_sub(1));
        self.screen = Screen::Reader(book.id.clone());
        self.vertical_reader.reset(self.page, book.page_fraction);
        self.saved_position = None;
        self.chapter_dialog = None;
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
            Screen::Reader(id) => {
                self.flush_position();
                self.vertical_reader.stop();
                self.page_generation += 1;
                self.worker
                    .generation
                    .store(self.page_generation, Ordering::Relaxed);
                self.page_pending.clear();
                self.screen = if self.reader_from_shelf { Screen::Library } else { Screen::Detail(id) };
                self.send(Command::Refresh);
            }
            Screen::Detail(id) => {
                let books = self.visible_books();
                let shelf_area = self.shelf_area(area.translate(vec2(0., self.shelf_shift)));
                if let Some(index) = books.iter().position(|b| self.same_series(&b.id, &id)) {
                    self.motion.select(index as f32, now, true);
                }
                let volumes = shelf::geometry(shelf_area, books.len(), self.motion.focus, 1.);
                if self.tab == Tab::Library && let Some(volume) = volumes
                    .iter()
                    .find(|v| v.lane == self.motion.focus.round() as i32 && self.same_series(&books[v.index].id, &id))
                {
                    if !self.reduced && self.covers.contains_key(&self.cover_id(&id)) {
                        let (started, source) = if let Some(out) = self.flight.as_ref().filter(|f| !f.returning && f.book_id == id) {
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
        if self.dark {
            (
                Color32::from_rgb(24, 27, 29),
                Color32::from_rgb(237, 239, 238),
                Color32::from_rgb(158, 168, 175),
            )
        } else {
            (
                Color32::from_rgb(245, 248, 250),
                Color32::from_rgb(33, 44, 50),
                Color32::from_rgb(91, 108, 120),
            )
        }
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
        let (_, ink, muted) = self.colors();
        let fill = if primary {
            ink
        } else if response.hovered() {
            muted.gamma_multiply(0.12)
        } else {
            Color32::TRANSPARENT
        };
        ui.painter().rect_filled(rect, 8., fill);
        ui.painter().text(
            rect.center(),
            Align2::CENTER_CENTER,
            label,
            FontId::proportional(13. * self.font_scale),
            if primary { self.colors().0 } else { ink },
        );
        response.clicked()
    }
    fn icon(&mut self, ui: &mut Ui, id: &str, rect: Rect, kind: &str, label: &str) -> bool {
        self.hitboxes.insert(id.into(), rect);
        let response = ui.interact(rect, Id::unique(id), Sense::click());
        response.widget_info(|| WidgetInfo::labeled(Role::Button, true, label));
        let color = self.colors().2;
        let p = ui.painter();
        let c = rect.center();
        let stroke = Stroke::new(1.4, color);
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
        if Self::detail_side_by_side(area) {
            let height = (area.height() - 173.).clamp(88., 416.);
            return Rect::from_min_size(
                area.min + vec2(24., 92.),
                vec2((area.width() * 0.55 - 32.).min(520.), height),
            );
        }
        Rect::from_min_size(
            pos2(area.left() + 24., area.top() + 114.),
            vec2((area.width() - 48.).clamp(1., 520.), 416.),
        )
    }
    fn detail_cover_rect(&self, area: Rect) -> Rect {
        if Self::detail_side_by_side(area) {
            let height = (area.height() - 86.).clamp(60., 290.);
            return Rect::from_center_size(
                pos2(area.left() + area.width() * 0.25, area.center().y + 24.),
                vec2(height * 118. / 166., height),
            );
        }
        let width = if area.width() > 700. { 224. } else { 164. };
        let top = if area.height() < 640. { 66. } else { 105. };
        Rect::from_min_size(
            pos2(area.center().x - width / 2., area.top() + top),
            vec2(width, width * 166. / 118.),
        )
    }
    fn detail_side_by_side(area: Rect) -> bool {
        area.height() < 600. && area.width() >= 480. && area.width() > area.height()
    }
    pub fn draw(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        self.receive(&ctx);
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
        ctx.set_visuals(if self.dark {
            Visuals::dark()
        } else {
            Visuals::light()
        });
        if self.widget_font_scale != self.font_scale {
            let mut fonts = Style::default().text_styles;
            for font in fonts.values_mut() {
                font.size *= self.font_scale;
            }
            for theme in [Theme::Light, Theme::Dark] {
                ctx.style_mut_of(theme, |style| style.text_styles = fonts.clone());
            }
            ui.style_mut().text_styles = fonts;
            self.widget_font_scale = self.font_scale;
        }
        self.hitboxes.clear();
        let full = ui.max_rect();
        #[cfg(any(target_os = "android", target_os = "ios"))]
        let full = match yejian_platform::safe_insets() {
            Ok(insets) => {
                let scale = ctx.pixels_per_point();
                Rect::from_min_max(
                    full.min + vec2(insets.left as f32 / scale, insets.top as f32 / scale),
                    full.max - vec2(insets.right as f32 / scale, insets.bottom as f32 / scale),
                )
            }
            Err(_) => full,
        };
        let (background, ink, _) = self.colors();
        ui.painter().rect_filled(full, 0., background);
        let tablet = full.width() >= 880.;
        let content = if tablet {
            Rect::from_min_max(full.min + vec2(72., 0.), full.max)
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
                    Rect::from_min_size(content.min + vec2(12., 10.), vec2(40., 40.)),
                    "back",
                    "返回书架",
                ) {
                    self.back(now, content);
                }
            } else {
                ui.painter().text(
                    content.min + vec2(20., 29.),
                    Align2::LEFT_CENTER,
                    "页间",
                    FontId::proportional(23.),
                    ink,
                );
                if self.icon(
                    ui,
                    "search",
                    Rect::from_min_size(
                        pos2(content.right() - 49., content.top() + 8.),
                        vec2(40., 40.),
                    ),
                    "search",
                    "搜索书库",
                ) {
                    self.search = !self.search;
                }
                if self.icon(
                    ui,
                    "import",
                    Rect::from_min_size(
                        pos2(content.right() - 94., content.top() + 8.),
                        vec2(40., 40.),
                    ),
                    "plus",
                    "导入漫画",
                ) {
                    self.pick_import();
                }
            }
            let transition = self.flight.as_ref().map(|flight| {
                let duration = if flight.returning { motion::CLOSE } else { motion::OPEN };
                let elapsed = ((now - flight.started) / duration).clamp(0., 1.) as f32;
                let progress = if flight.returning { 1. - elapsed } else { elapsed };
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
            } else { match self.screen.clone() {
                Screen::Library => match self.tab {
                    Tab::Library => self.library_viewport(ui, content, now, entrance),
                    Tab::History => self.history_ui(ui, content),
                    Tab::Settings => self.settings_ui(ui, content),
                    Tab::Sources => self.sources_ui(ui, content),
                },
                Screen::Detail(id) => self.detail_ui(ui, content, &id),
                _ => {}
            } }
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
                        mask.multiply_opacity(1. - motion::cubic(progress / 0.12, 0.42, 0., 0.58, 1.));
                        let books = self.visible_books();
                        for volume in &flight.foreground {
                            if let Some(book) = books.get(volume.index) {
                                let cover = self.cover_id(&book.id);
                                shelf::paint_volume(&mask, volume, &book.title, self.covers.get(&cover),
                                    self.dark, false, self.selected.contains(&book.id));
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
        let (_, ink, muted) = self.colors();
        let tabs = [
            (Tab::Library, "书架", "library"),
            (Tab::Sources, "书源", "search"),
            (Tab::History, "历史", "history"),
            (Tab::Settings, "设置", "settings"),
        ];
        let rail = if tablet {
            Rect::from_min_size(full.min, vec2(72., full.height()))
        } else {
            Rect::from_min_size(
                pos2(full.left() + 28., full.bottom() - 62.),
                vec2(full.width() - 56., 54.),
            )
        };
        ui.painter().rect_filled(
            rail,
            if tablet { 0. } else { 27. },
            if self.dark {
                Color32::from_rgb(34, 38, 40)
            } else {
                Color32::from_rgb(250, 252, 253)
            },
        );
        if !tablet {
            ui.painter().rect_stroke(
                rail,
                27.,
                Stroke::new(0.6, muted.gamma_multiply(0.2)),
                StrokeKind::Inside,
            );
        }
        for (i, (tab, label, kind)) in tabs.into_iter().enumerate() {
            let rect = if tablet {
                Rect::from_min_size(
                    pos2(rail.left() + 10., rail.top() + 96. + i as f32 * 78.),
                    vec2(52., 60.),
                )
            } else {
                Rect::from_min_size(
                    pos2(rail.left() + i as f32 * rail.width() / 4., rail.top()),
                    vec2(rail.width() / 4., 54.),
                )
            };
            let icon_rect = Rect::from_center_size(rect.center() - vec2(0., 8.), vec2(32., 29.));
            let clicked = self.icon(ui, &format!("nav-{kind}"), icon_rect, kind, label);
            ui.painter().text(
                rect.center() + vec2(0., 8.),
                Align2::CENTER_TOP,
                label,
                FontId::proportional(10.),
                if tab == self.tab { ink } else { muted },
            );
            let text_clicked = ui
                .interact(
                    Rect::from_min_max(pos2(rect.left(), rect.center().y + 5.), rect.max),
                    Id::unique(("nav-text", i)),
                    Sense::click(),
                )
                .clicked();
            if tab == self.tab {
                ui.painter().line_segment(
                    [
                        rect.center() + vec2(-5., 19.),
                        rect.center() + vec2(5., 19.),
                    ],
                    Stroke::new(1.2, ink),
                );
            }
            if clicked || text_clicked {
                self.tab = tab;
                self.screen = Screen::Library;
                self.flight = None;
            }
        }
    }
    fn library_viewport(&mut self, ui: &mut Ui, area: Rect, now: f64, entrance: f32) {
        self.shelf_viewport = area;
        let height = (680. + (self.font_scale - 1.) * 110.).max(area.height() - 76.);
        if area.height() >= height + 62. || Self::detail_side_by_side(area) {
            self.shelf_shift = 0.;
            self.library_ui(ui, area, now, entrance);
            return;
        }
        let rect = Rect::from_min_max(area.min + vec2(0., 60.), area.max - vec2(0., 68.));
        ui.scope_builder(UiBuilder::new().max_rect(rect), |ui| {
            ScrollArea::vertical().id_salt("library-scroll").show(ui, |ui| {
                let size = vec2(area.width(), height);
                let (inner, _) = ui.allocate_exact_size(size - vec2(0., 60.), Sense::hover());
                self.shelf_shift = inner.top() - 60. - area.top();
                self.library_ui(ui, Rect::from_min_size(inner.min - vec2(0., 60.), size), now, entrance);
            });
        });
    }
    fn library_ui(&mut self, ui: &mut Ui, area: Rect, now: f64, entrance: f32) {
        let (_, ink, muted) = self.colors();
        if self.search {
            let rect =
                Rect::from_min_size(area.min + vec2(24., 66.), vec2(area.width() - 48., 36.));
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
            ui.painter().line_segment(
                [
                    area.min + vec2(24., 78.),
                    pos2(area.right() - 24., area.top() + 78.),
                ],
                Stroke::new(0.6, muted.gamma_multiply(0.25)),
            );
            self.label(ui.painter(), area.min + vec2(24., 93.), "书架", 17., ink);
            if self.button(
                ui,
                "categories",
                Rect::from_min_size(pos2(area.right() - 130., area.top() + 88.), vec2(106., 32.)),
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
        let shelf_area = self.shelf_area(area);
        // Faint drafting grid matches the reference's spatial cue.
        let grid = muted.gamma_multiply(0.035);
        for x in (0..=(area.width() / 32.) as usize).map(|i| area.left() + 24. + i as f32 * 32.) {
            ui.painter().line_segment(
                [pos2(x, area.top() + 60.), pos2(x, shelf_area.bottom())],
                Stroke::new(0.5, grid),
            );
        }
        for y in (0..).map(|i| area.top() + 60. + i as f32 * 32.).take_while(|&y| y < shelf_area.bottom()) {
            ui.painter().line_segment(
                [pos2(area.left() + 24., y), pos2(area.right() - 24., y)],
                Stroke::new(0.5, grid),
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
                self.hitboxes.insert(
                    "shelf-center".into(),
                    Rect::from_min_max(volume.cover.point(0., 0.), volume.cover.point(1., 1.)),
                );
            }
        }
        let gesture = ui.interact(
            shelf_area,
            Id::unique("shelf-gesture"),
            Sense::click_and_drag(),
        );
        let pitch = shelf::lane_pitch(shelf_area);
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
        let short_landscape = Self::detail_side_by_side(area);
        let wide = short_landscape || (area.width() >= 880. && self.font_scale < 1.5);
        let info = if short_landscape {
            pos2(shelf_area.right() + 24., area.top() + (area.height() * 0.34).max(112.))
        } else if wide {
            pos2(shelf_area.right() + 48., area.top() + 300.)
        } else {
            pos2(area.left() + 24., area.top() + 522.)
        };
        let title_width = (area.right() - 70. - info.x).max(80.);
        let mut title = egui::text::LayoutJob::simple(
            book.title.clone(),
            FontId::proportional(if wide {
                26. * self.font_scale
            } else {
                20. * self.font_scale
            }),
            ink,
            title_width,
        );
        title.wrap.max_rows = 1;
        let title = ui.painter().layout_job(title);
        let progress_pos = info + vec2(0., title.size().y + 6.);
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
            FontId::proportional(12. * self.font_scale),
            muted,
            (area.right() - 24. - info.x).max(80.),
        );
        progress.wrap.max_rows = 1;
        let progress = ui.painter().layout_job(progress);
        let action_y = progress_pos.y + progress.size().y + 16.;
        ui.painter().galley(progress_pos, progress, muted);
        let open_rect = Rect::from_min_size(pos2(area.right() - 68., info.y - 5.), vec2(44., 44.));
        if self.selected.is_empty() && self.icon(ui, "open-book", open_rect, "open", "查看漫画详情") {
            if let Some(volume) = volumes
                .iter()
                .find(|v| v.lane == self.motion.focus.round() as i32)
            {
                self.open_detail(book, volume, &volumes, area, now);
            }
        }
        let action_width = (52. * self.font_scale + 36.).max(112.);
        let action = Rect::from_min_size(pos2(info.x, action_y), vec2(action_width, 44.));
        if self.selected.is_empty()
            && self.button(ui, "shelf-read", action, if has_progress { "继续阅读" } else { "开始阅读" }, true)
        {
            self.open_reader(book);
        }
        let pager_right = shelf_area.right();
        let tally = format!("{:02} / {:02}", index + 1, books.len());
        let tally = ui.painter().layout_no_wrap(tally, FontId::monospace(11.), muted);
        let pager_left = pager_right - 88. - tally.size().x - 4.;
        let y = if short_landscape {
            shelf_area.bottom() - 28.
        } else if wide {
            shelf_area.bottom() + 12.
        } else if action.right() + 8. > pager_left {
            action.bottom() + 4.
        } else {
            action_y
        };
        if books.len() > 1 && self.icon(
            ui,
            "shelf-prev",
            Rect::from_min_size(pos2(pager_left, y), vec2(44., 44.)),
            "prev",
            "上一本",
        ) {
            self.motion
                .select(self.motion.focus.round() - 1., now, self.reduced);
        }
        if books.len() > 1 {
            ui.painter().galley(pos2(pager_left + 46., y + (44. - tally.size().y) / 2.), tally, muted);
        }
        if books.len() > 1 && self.icon(
            ui,
            "shelf-next",
            Rect::from_min_size(pos2(pager_right - 44., y), vec2(44., 44.)),
            "next",
            "下一本",
        ) {
            self.motion
                .select(self.motion.focus.round() + 1., now, self.reduced);
        }
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
    fn open_detail(&mut self, book: &Book, volume: &shelf::Volume, volumes: &[shelf::Volume], area: Rect, now: f64) {
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
        if self.flight.is_none() && let Some(texture) = self.covers.get(&cover_id) {
            shelf::paint_art(ui.painter(), Plane::rect(cover), cover.size(), texture, self.colors().0);
        }
        let (_, ink, muted) = self.colors();
        let title = self.chapter(id)
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
        } else {
            pos2(area.left() + 24., cover.bottom() + 30.)
        };
        let body = Rect::from_min_max(details_start, area.max - vec2(24., 12.));
        let can_restart = book.current_page > 0 || book.page_fraction > 0.;
        ui.scope_builder(UiBuilder::new().max_rect(body), |ui| {
            ScrollArea::vertical().id_salt(("detail-body", id)).show(ui, |ui| {
                ui.vertical_centered(|ui| {
                    ui.add(Label::new(RichText::new(title).size(24. * self.font_scale).color(ink)).wrap());
                    ui.add_space(6.);
                    ui.add(Label::new(RichText::new(text).size(13. * self.font_scale).color(muted)).wrap());
                    ui.add_space(18.);
                    let width = (218. * self.font_scale.sqrt()).min(ui.available_width());
                    let (rect, _) = ui.allocate_exact_size(vec2(width, 48.), Sense::hover());
                    let action = if book.last_read_at.is_some() {
                        format!("继续阅读 · 第 {} 页", book.current_page + 1)
                    } else {
                        "开始阅读".to_string()
                    };
                    if self.button(ui, "read", rect, &action, true) {
                        self.flight = None;
                        self.open_reader(&book);
                    }
                    if can_restart {
                        let (rect, _) = ui.allocate_exact_size(vec2(width, 44.), Sense::hover());
                        if self.button(ui, "read-start", rect, "从第一页开始", false) {
                            let mut first = book.clone();
                            first.current_page = 0;
                            first.page_fraction = 0.;
                            self.flight = None;
                            self.open_reader(&first);
                        }
                    }
                    if self.chapter(id).is_some() {
                        let (rect, _) = ui.allocate_exact_size(vec2(width, 44.), Sense::hover());
                        if self.button(ui, "detail-chapters", rect, "已下载章节", false) {
                            self.chapter_dialog = Some(id.to_string());
                        }
                    }
                });
            });
        });
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
        let gesture = ui.interact(
            content,
            Id::unique("reader-canvas"),
            Sense::click_and_drag(),
        );
        self.hitboxes.insert("reader-canvas".into(), content);
        let zoom = ui.input(|i| i.zoom_delta());
        if zoom != 1. {
            self.zoom = (self.zoom * zoom).clamp(1., 5.);
        }
        if gesture.double_clicked() {
            self.zoom = if self.zoom > 1. { 1. } else { 2. };
            self.pan = Vec2::ZERO;
        }
        if gesture.drag_started() {
            self.reader_drag = 0.;
            self.vertical_reader.stop();
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
        if gesture.clicked() {
            if self.vertical {
                self.controls = !self.controls;
            } else if let Some(pos) = gesture.interact_pointer_pos() {
                let relative = (pos.x - content.left()) / content.width();
                if relative < 0.27 {
                    self.next_page(id, if self.rtl { step } else { -step });
                } else if relative > 0.73 {
                    self.next_page(id, if self.rtl { -step } else { step });
                } else {
                    self.controls = !self.controls;
                }
            }
        }
        let next = ctx.input(|i| {
            i.key_pressed(Key::ArrowRight)
                || (!self.vertical && (i.key_pressed(Key::PageDown) || i.key_pressed(Key::Space)))
        });
        let prev = ctx.input(|i| {
            i.key_pressed(Key::ArrowLeft) || (!self.vertical && i.key_pressed(Key::PageUp))
        });
        if next {
            self.next_page(id, if self.rtl { -step } else { step });
        }
        if prev {
            self.next_page(id, if self.rtl { step } else { -step });
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
            let center = available.center();
            let image_rect = Rect::from_center_size(center + self.pan, size);
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
        if self.controls {
            let (bar, ink, muted) = self.colors();
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
                Rect::from_min_size(area.min + vec2(8., 8.), vec2(40., 40.)),
                "back",
                if self.reader_from_shelf { "返回书架" } else { "返回详情" },
            ) {
                self.back(now, area);
            }
            ui.painter()
                .with_clip_rect(Rect::from_min_max(
                    area.min + vec2(56., 0.),
                    pos2(area.right() - 58., area.top() + 56.),
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
                    Rect::from_min_size(pos2(area.right() - 50., area.top() + 8.), vec2(40., 40.)),
                    "chapters",
                    "已下载章节",
                )
            {
                self.chapter_dialog = Some(id.to_string());
            }
            if self.icon(
                ui,
                "reader-prev",
                Rect::from_min_size(pos2(area.left() + 10., area.bottom() - 55.), vec2(40., 40.)),
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
                    vec2(40., 40.),
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
        let scroll = ui.input(|i| i.smooth_scroll_delta.y);
        self.vertical_reader.scroll(-scroll);
        if gesture.dragged() {
            self.vertical_reader
                .scroll(-ui.input(|i| i.pointer.delta().y));
        }
        if gesture.drag_stopped() {
            self.vertical_reader
                .release(-ui.input(|i| i.pointer.velocity().y), now, self.reduced);
        }
        if ui.input(|i| i.key_pressed(Key::PageDown) || i.key_pressed(Key::Space)) {
            self.vertical_reader.scroll(content.height() * 0.85);
        }
        if ui.input(|i| i.key_pressed(Key::PageUp)) {
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
        self.vertical_reader
            .settle(count, content.height(), |page| heights.get(&page).copied());
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
    }
    fn history_ui(&mut self, ui: &mut Ui, area: Rect) {
        let (_, ink, muted) = self.colors();
        self.label(
            ui.painter(),
            area.min + vec2(24., 80.),
            "阅读历史",
            23.,
            ink,
        );
        let mut books: Vec<_> = self
            .books
            .iter()
            .filter(|b| b.last_read_at.is_some())
            .cloned()
            .collect();
        books.sort_by_key(|b| std::cmp::Reverse(b.last_read_at));
        let rect = Rect::from_min_max(area.min + vec2(24., 128.), area.max - vec2(24., 88.));
        ui.scope_builder(UiBuilder::new().max_rect(rect), |ui| {
            ScrollArea::vertical().show(ui, |ui| {
                if books.is_empty() {
                    ui.label("还没有阅读记录。");
                }
                for book in books {
                    let (rect, _) =
                        ui.allocate_exact_size(vec2(ui.available_width(), 72.), Sense::hover());
                    let mut title = egui::text::LayoutJob::simple(
                        self.reading_title(&book),
                        FontId::proportional(16. * self.font_scale),
                        ink,
                        (rect.width() - 94.).max(80.),
                    );
                    title.wrap.max_rows = 1;
                    ui.painter().galley(
                        rect.min + vec2(4., 8.),
                        ui.painter().layout_job(title),
                        ink,
                    );
                    self.label(
                        ui.painter(),
                        rect.min + vec2(4., 35.),
                        &format!("第 {} / {} 页", book.current_page + 1, book.page_count),
                        12.,
                        muted,
                    );
                    if self.button(
                        ui,
                        &format!("history-{}", book.id),
                        Rect::from_min_size(
                            pos2(rect.right() - 80., rect.top() + 12.),
                            vec2(76., 40.),
                        ),
                        "继续",
                        false,
                    ) {
                        self.open_reader(&book);
                    }
                }
            });
        });
    }
    fn settings_ui(&mut self, ui: &mut Ui, area: Rect) {
        let (_, ink, _) = self.colors();
        self.label(ui.painter(), area.min + vec2(24., 80.), "设置", 23., ink);
        let rect = Rect::from_min_max(area.min + vec2(24., 130.), area.max - vec2(24., 88.));
        ui.scope_builder(UiBuilder::new().max_rect(rect), |ui| {
            ScrollArea::vertical().show(ui, |ui| {
                ui.heading("外观与阅读");
                ui.horizontal(|ui| {
                    ui.label("主题");
                    for (key, name) in [("system", "跟随系统"), ("light", "浅色"), ("dark", "深色")]
                    {
                        if ui
                            .selectable_value(&mut self.theme, key.into(), name)
                            .changed()
                        {
                            self.setting("theme", self.theme.clone());
                        }
                    }
                });
                if ui.checkbox(&mut self.reduced, "减少动态效果").changed() {
                    self.setting("reduce-motion", self.reduced.to_string());
                }
                if ui
                    .checkbox(&mut self.private, "私密阅读：不保存新进度和历史")
                    .changed()
                {
                    self.setting("private", self.private.to_string());
                }
                if ui.checkbox(&mut self.rtl, "从右向左翻页").changed() {
                    self.setting(
                        "reading-direction",
                        if self.rtl { "rtl" } else { "ltr" }.into(),
                    );
                }
                let mut mode = if self.vertical {
                    "vertical"
                } else if self.spread {
                    "spread"
                } else {
                    "paged"
                };
                ui.horizontal(|ui| {
                    ui.label("阅读方式");
                    ui.selectable_value(&mut mode, "paged", "单页");
                    ui.selectable_value(&mut mode, "spread", "平板双页");
                    ui.selectable_value(&mut mode, "vertical", "上下滚动");
                });
                if self.vertical != (mode == "vertical") || self.spread != (mode == "spread") {
                    self.vertical = mode == "vertical";
                    self.spread = mode == "spread";
                    self.setting("reading-mode", mode.into());
                }
                if ui
                    .add(Slider::new(&mut self.font_scale, 1. ..=2.).text("文字大小"))
                    .changed()
                {
                    self.setting("font-scale", self.font_scale.to_string());
                }
                ui.add_space(24.);
                ui.heading("导入与备份");
                ui.label("支持 CBZ / ZIP、CBT / TAR、CBR / RAR 和图片文件夹。");
                if ui.button("选择漫画文件").clicked() {
                    self.pick_import();
                }
                #[cfg(not(any(target_os = "android", target_os = "ios")))]
                if ui.button("选择图片文件夹").clicked() {
                    if let Some(path) = rfd::FileDialog::new().pick_folder() {
                        self.import_files(vec![path]);
                    }
                }
                ui.add_space(8.);
                ui.text_edit_singleline(&mut self.import_text);
                if ui.button("导入上述路径").clicked() {
                    self.import_files(vec![PathBuf::from(self.import_text.trim())]);
                }
                ui.add_space(16.);
                ui.label("备份文件");
                ui.text_edit_singleline(&mut self.backup_path);
                ui.horizontal(|ui| {
                    if ui.button("创建完整备份").clicked() {
                        self.send(Command::Backup(self.backup_path.clone().into()));
                    }
                    if ui.button("恢复备份").clicked() {
                        self.pick_restore();
                    }
                });
                ui.add_space(24.);
                self.sync.ui(ui);
                ui.add_space(24.);
                ui.heading("关于页间");
                ui.label("书架设计改编自 Wenku8 Flutter v0.5.6");
                ui.label(concat!(
                    "页间 ",
                    env!("CARGO_PKG_VERSION"),
                    " · Rust · egui / wgpu · GPL-3.0"
                ));
                if ui.button("查看开源许可").clicked() {
                    self.error = Some(format!(
                        "Wenku8 Flutter v0.5.6\n\n{}\n\nGPL-3.0\n\n{}",
                        include_str!("../licenses/Wenku8-NOTICE"),
                        include_str!("../licenses/Wenku8-GPL-3.0.txt")
                    ));
                }
            });
        });
    }
    fn sources_ui(&mut self, ui: &mut Ui, area: Rect) {
        let (_, ink, _) = self.colors();
        self.label(ui.painter(), area.min + vec2(24., 80.), "书源", 23., ink);
        let rect = Rect::from_min_max(area.min + vec2(24., 132.), area.max - vec2(24., 88.));
        ui.scope_builder(UiBuilder::new().max_rect(rect), |ui| {
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
        if let Some(id) = self.chapter_dialog.clone() {
            let title = self.chapter(&id)
                .map(|m| m.manga_title.clone())
                .unwrap_or_else(|| "已下载章节".into());
            let chapters = self.chapters_for(&id);
            Window::new(title)
                .id(Id::unique("chapter-picker"))
                .collapsible(false)
                .resizable(false)
                .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
                .max_width(460.)
                .show(ctx, |ui| {
                    ScrollArea::vertical().max_height(380.).show(ui, |ui| {
                        for book in chapters {
                            let name = self.chapter(&book.id)
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
                                [ui.available_width().min(420.).max(250.), 58.],
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
        }
        if let Some(text) = self.error.clone() {
            Window::new("提示")
                .collapsible(false)
                .resizable(false)
                .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
                .max_width(520.)
                .show(ctx, |ui| {
                    ScrollArea::vertical().max_height(300.).show(ui, |ui| {
                        ui.label(&text);
                    });
                    ui.horizontal(|ui| {
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
    fn returning_closes_dialog_before_leaving_the_reader() {
        let root = tempfile::tempdir().unwrap();
        let mut app = ReaderApp::new(root.path().into(), Context::default());
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
        app.request_back(3., area, true);
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
        app.books.iter_mut().find(|b| b.id == "extra").unwrap().last_read_at = Some(123);
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
