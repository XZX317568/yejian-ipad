use crate::follows;
use crate::motion::self;

use crate::shelf::self;
use crate::shelf::Plane;





use crate::worker::Worker;

use egui::*;

use std::collections::HashMap;
use std::collections::HashSet;
use std::path::PathBuf;

use yejian_core::Book;
use yejian_core::ChapterLink;

use super::*;


impl ReaderApp {
    pub(super) fn visible_books(&self) -> Vec<Book> {
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
        let mut entries: Vec<Book> = representatives
            .into_iter()
            .map(|book| {
                let mut entry = book.clone();
                if let Some(chapter) = self.chapter(&book.id) {
                    entry.title.clone_from(&chapter.manga_title);
                }
                entry
            })
            .collect();
        // Followed works with no local chapters still belong on the shelf:
        // synthesise one entry per follow so sorting, animation and hit
        // testing all apply unchanged. Dedupe against every linked work —
        // filtering must not resurrect a hidden local work's online twin.
        let linked_works: HashSet<(&str, &str)> = self
            .chapters
            .iter()
            .map(|link| {
                (
                    link.metadata.source_id.as_str(),
                    link.metadata.manga_id.as_str(),
                )
            })
            .collect();
        for follow in self.sources.follows() {
            let key = (follow.source_id.as_str(), follow.manga_id.as_str());
            if linked_works.contains(&key) {
                continue;
            }
            if (!self.category.is_empty() && self.category != "在线追更")
                || (!query.is_empty() && !follow.title.to_lowercase().contains(&query))
            {
                continue;
            }
            let (mut current_page, mut last_read_at) = (0, None);
            let mut progress_known = true;
            if let Some((chapter_key, _name, at)) =
                self.sources.work_last_read(&follow.source_id, &follow.manga_id)
            {
                last_read_at = Some(at);
                match follow
                    .reading_order
                    .iter()
                    .position(|key| key == &chapter_key)
                {
                    Some(index) => current_page = index,
                    // Records saved before the reading order was kept can't be
                    // placed; marking progress unknown keeps them behind works
                    // whose progress is real rather than showing a flipped
                    // index for newest-first sources.
                    None => progress_known = false,
                }
            }
            let added_at = follow.followed() as i64;
            entries.push(Book {
                id: format!("online:{}\u{1f}{}", follow.source_id, follow.manga_id),
                title: follow.title.clone(),
                author: String::new(),
                archive_path: PathBuf::new(),
                cover_path: None,
                page_count: follow.total.max(1),
                current_page,
                page_fraction: if progress_known { 0. } else { -1. },
                updated_at: last_read_at.unwrap_or(added_at),
                added_at,
                last_read_at,
                category: "在线追更".into(),
            });
        }
        self.sort_visible(&mut entries);
        entries
    }
    /// A representative's position inside its series: `(ordinal, count)` in
    /// `self.chapters` order, or `(0, 1)` for an unlinked book.
    pub(super) fn series_ordinals(&self) -> HashMap<&str, (usize, usize)> {
        let mut groups: HashMap<(&str, &str), Vec<&str>> = HashMap::new();
        for link in &self.chapters {
            let m = &link.metadata;
            groups
                .entry((m.source_id.as_str(), m.manga_id.as_str()))
                .or_default()
                .push(link.book_id.as_str());
        }
        let mut ordinals = HashMap::new();
        for ids in groups.values() {
            for (ordinal, id) in ids.iter().enumerate() {
                ordinals.insert(*id, (ordinal, ids.len()));
            }
        }
        ordinals
    }
    /// `(group, fraction)`: 0 in progress, 1 finished, 2 not started. The
    /// fraction blends the chapter ordinal with the page position inside it.
    pub(super) fn work_progress(&self, book: &Book, ordinals: &HashMap<&str, (usize, usize)>) -> (u8, f32) {
        let page_progress = if book.page_fraction < 0. {
            // Online entries whose reading order is unknown sort behind every
            // work with a real in-progress fraction.
            -1.
        } else if book.last_read_at.is_none() {
            0.
        } else if book.current_page + 1 >= book.page_count {
            1.
        } else {
            (book.current_page + 1) as f32 / book.page_count.max(1) as f32
        };
        let (ordinal, count) = ordinals.get(book.id.as_str()).copied().unwrap_or((0, 1));
        let fraction = (ordinal as f32 + page_progress) / count as f32;
        if book.last_read_at.is_none() {
            (2, fraction)
        } else if ordinal + 1 == count && page_progress >= 1. {
            (1, fraction)
        } else {
            (0, fraction)
        }
    }
    pub(super) fn sort_visible(&self, books: &mut Vec<Book>) {
        match self.shelf_sort {
            ShelfSort::Recent => {}
            ShelfSort::Added => {
                // One pass over books and links: the earliest chapter's join
                // date stands in for the work's.
                let added: HashMap<&str, i64> = self
                    .books
                    .iter()
                    .map(|book| (book.id.as_str(), book.added_at))
                    .collect();
                let mut series_added: HashMap<(&str, &str), i64> = HashMap::new();
                for link in &self.chapters {
                    let m = &link.metadata;
                    let at = added.get(link.book_id.as_str()).copied().unwrap_or(0);
                    series_added
                        .entry((m.source_id.as_str(), m.manga_id.as_str()))
                        .and_modify(|value| *value = (*value).min(at))
                        .or_insert(at);
                }
                let work_added = |book: &Book| {
                    self.chapter(&book.id)
                        .and_then(|m| {
                            series_added
                                .get(&(m.source_id.as_str(), m.manga_id.as_str()))
                                .copied()
                        })
                        .unwrap_or(book.added_at)
                };
                books.sort_by(|a, b| {
                    work_added(b)
                        .cmp(&work_added(a))
                        .then_with(|| a.title.cmp(&b.title))
                });
            }
            ShelfSort::Progress => {
                let ordinals = self.series_ordinals();
                books.sort_by(|a, b| {
                    let (group_a, fraction_a) = self.work_progress(a, &ordinals);
                    let (group_b, fraction_b) = self.work_progress(b, &ordinals);
                    group_a
                        .cmp(&group_b)
                        .then_with(|| {
                            if group_a == 0 {
                                fraction_b
                                    .partial_cmp(&fraction_a)
                                    .unwrap_or(std::cmp::Ordering::Equal)
                            } else {
                                std::cmp::Ordering::Equal
                            }
                        })
                        .then_with(|| b.last_read_at.cmp(&a.last_read_at))
                        .then_with(|| a.title.cmp(&b.title))
                });
            }
        }
    }
    /// Changes the shelf order; the books animate from their old slots and
    /// the focus lands on the first book of the new order.
    pub(super) fn set_shelf_sort(&mut self, sort: ShelfSort, now: f64) {
        self.sort_menu = false;
        if self.shelf_sort == sort {
            return;
        }
        if !self.reduced {
            self.shelf_reorder = Some(Reorder {
                from_ids: self
                    .visible_books()
                    .iter()
                    .map(|book| book.id.clone())
                    .collect(),
                from_focus: self.motion.focus,
                started: now,
            });
        }
        self.shelf_sort = sort;
        self.motion.jump(0.);
        self.setting("shelf-sort", sort.key().to_string());
    }
    pub(super) fn same_series(&self, a: &str, b: &str) -> bool {
        match (self.chapter(a), self.chapter(b)) {
            (Some(a), Some(b)) => a.source_id == b.source_id && a.manga_id == b.manga_id,
            _ => a == b,
        }
    }
    pub(super) fn replace_links(&mut self, links: Vec<ChapterLink>) {
        self.sources.set_library_chapters(&links);
        self.chapter_indices = links
            .iter()
            .enumerate()
            .map(|(i, link)| (link.book_id.clone(), i))
            .collect();
        self.chapters = links;
    }
    pub(super) fn chapters_for(&self, id: &str) -> Vec<Book> {
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
    pub(super) fn adjacent_chapter(&self, id: &str, delta: i32) -> Option<Book> {
        let chapters = self.chapters_for(id);
        let index = chapters.iter().position(|book| book.id == id)? as i32 + delta;
        if index < 0 {
            return None;
        }
        chapters.get(index as usize).cloned()
    }
    /// `online:{source}\u{1f}{manga}` entries synthesised for the shelf.
    pub(super) fn online_parts(id: &str) -> Option<(&str, &str)> {
        id.strip_prefix("online:")?.split_once('\u{1f}')
    }
    pub(super) fn online_follow(&self, id: &str) -> Option<&follows::Follow> {
        let (source, manga) = Self::online_parts(id)?;
        self.sources
            .follows()
            .iter()
            .find(|follow| follow.source_id == source && follow.manga_id == manga)
    }
    /// Fresh-chapter count shown on the spine badge: online entries use their
    /// own follow; local books borrow the follow of their work.
    pub(super) fn online_badge(&self, book: &Book) -> Option<usize> {
        let follows = self.sources.follows();
        let follow = if let Some((source, manga)) = Self::online_parts(&book.id) {
            follows
                .iter()
                .find(|f| f.source_id == source && f.manga_id == manga)
        } else {
            let meta = self.chapter(&book.id)?;
            follows
                .iter()
                .find(|f| f.source_id == meta.source_id && f.manga_id == meta.manga_id)
        }?;
        (follow.fresh.len() > 0).then(|| follow.fresh.len())
    }
    /// Info line under a followed work: fresh count, else the last-read
    /// chapter's name, else just the source.
    pub(super) fn online_progress_label(&self, follow: &follows::Follow) -> String {
        let name = self.sources.source_display_name(&follow.source_id);
        if !follow.fresh.is_empty() {
            return format!("在线 · {} · {} 话更新", name, follow.fresh.len());
        }
        match self
            .sources
            .work_last_read(&follow.source_id, &follow.manga_id)
        {
            Some((_, chapter, _)) => format!("在线 · 读到 {chapter}"),
            None => format!("在线 · {name}"),
        }
    }
    pub(super) fn open_online(&mut self, id: &str, resume: bool) {
        let Some((source, manga)) = Self::online_parts(id).map(|(s, m)| (s.to_string(), m.to_string()))
        else {
            return;
        };
        if self.sources.open_work(&source, &manga, resume) {
            self.flight = None;
            self.selected.clear();
            self.tab = Tab::Sources;
            self.screen = Screen::Library;
        }
    }
    pub(super) fn cover_id(&self, id: &str) -> String {
        if self.chapter(id).is_none() {
            return id.to_string();
        }
        self.chapters
            .iter()
            .find(|link| self.same_series(id, &link.book_id))
            .map(|link| link.book_id.clone())
            .unwrap_or_else(|| id.to_string())
    }
    pub(super) fn reading_title(&self, book: &Book) -> String {
        self.chapter(&book.id)
            .map(|m| format!("{} · {}", m.manga_title, m.chapter_title))
            .unwrap_or_else(|| book.title.clone())
    }
    pub(super) fn selected_books(&self) -> Vec<String> {
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
    pub(super) fn replace_books(&mut self, books: Vec<Book>, now: f64) {
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
        let from_focus = self.motion.focus;
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
        // Same shelf in a new order (a finished book resorting itself):
        // animate the swap instead of teleporting.
        let from_ids: Vec<String> = previous.iter().map(|book| book.id.clone()).collect();
        let to_ids: Vec<String> = visible.iter().map(|book| book.id.clone()).collect();
        if !self.reduced && from_ids.len() == to_ids.len() && from_ids != to_ids {
            let mut before = from_ids.clone();
            let mut after = to_ids;
            before.sort();
            after.sort();
            if before == after {
                self.shelf_reorder = Some(Reorder {
                    from_ids,
                    from_focus,
                    started: now,
                });
            }
        }
        self.selected
            .retain(|id| Self::online_parts(id).is_some() || self.books.iter().any(|b| &b.id == id));
    }
    pub(super) fn category_tabs(&self) -> Vec<(String, Option<String>)> {
        let mut tabs = vec![
            ("全部".to_string(), Some(String::new())),
            ("未分类".to_string(), Some("未分类".to_string())),
            ("在线追更".to_string(), Some("在线追更".to_string())),
        ];
        let mut seen = std::collections::HashSet::from(["未分类", "在线追更"]);
        for name in &self.user_categories {
            if seen.insert(name.as_str()) {
                tabs.push((name.clone(), Some(name.clone())));
            }
        }
        let mut derived: Vec<String> = self
            .books
            .iter()
            .map(|book| book.category.clone())
            .filter(|name| !name.is_empty() && !seen.contains(name.as_str()))
            .collect();
        derived.sort();
        derived.dedup();
        for name in derived {
            tabs.push((name.clone(), Some(name)));
        }
        tabs.push(("管理".to_string(), None));
        tabs
    }
    /// Saves the user-category list under `categories`.
    pub(super) fn save_categories(&mut self) {
        if let Ok(json) = serde_json::to_string(&self.user_categories) {
            self.setting("categories", json);
        }
    }
    /// Horizontally scrolling category tabs; the selected tab gets ink text
    /// and a vermillion underline. Last tab opens the manage panel.
    pub(super) fn category_strip(&mut self, ui: &mut Ui, rect: Rect) {
        let palette = crate::design::palette(self.dark);
        let tabs = self.category_tabs();
        let font_scale = self.font_scale;
        ui.scope_builder(
            UiBuilder::new()
                .max_rect(rect)
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
            |ui| {
                ScrollArea::horizontal()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        for (index, (label, value)) in tabs.iter().enumerate() {
                            let selected = value.as_deref() == Some(self.category.as_str());
                            let galley = ui.painter().layout_no_wrap(
                                label.clone(),
                                FontId::proportional(15. * font_scale),
                                if selected { palette.ink } else { palette.muted },
                            );
                            let width =
                                (galley.size().x + 20. * font_scale).max(44.);
                            let (tab_rect, response) = ui.allocate_exact_size(
                                vec2(width, rect.height()),
                                Sense::click(),
                            );
                            let id = value
                                .as_ref()
                                .map(|value| format!("category-tab-{index}-{value}"))
                                .unwrap_or_else(|| "category-tabs-manage".into());
                            self.hitboxes.insert(id, tab_rect);
                            response.widget_info(|| {
                                WidgetInfo::labeled(Role::Button, true, label.clone())
                            });
                            let text_color = if response.hovered() || selected {
                                palette.ink
                            } else {
                                palette.muted
                            };
                            ui.painter().galley(
                                pos2(
                                    tab_rect.center().x - galley.size().x / 2.,
                                    tab_rect.center().y - galley.size().y / 2.,
                                ),
                                galley.clone(),
                                text_color,
                            );
                            if selected {
                                ui.painter().rect_filled(
                                    Rect::from_center_size(
                                        pos2(
                                            tab_rect.center().x,
                                            tab_rect.bottom() - 3.,
                                        ),
                                        vec2(galley.size().x + 8., 3.),
                                    ),
                                    1.5,
                                    palette.accent,
                                );
                            }
                            if response.clicked() {
                                match value {
                                    Some(value) => {
                                        self.category = value.clone();
                                        self.motion.focus = 0.;
                                    }
                                    None => {
                                        self.category_manage = true;
                                        self.category_renaming = None;
                                    }
                                }
                            }
                        }
                    });
            },
        );
    }
    pub(super) fn icon(&mut self, ui: &mut Ui, id: &str, rect: Rect, kind: &str, label: &str) -> bool {
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
            palette.ink
        } else {
            palette.muted
        };
        let p = ui.painter();
        if !id.starts_with("nav-") && !is_reader {
            p.rect_filled(
                rect,
                12.,
                if response.hovered() {
                    palette.soft
                } else {
                    // `soft` sits one step above the shelf stage so the pager
                    // buttons stay legible in dark mode without a heavy frame.
                    palette.soft
                },
            );
            p.rect_stroke(
                rect,
                12.,
                Stroke::new(1., palette.line),
                StrokeKind::Inside,
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
            "bookmark" => {
                p.line_segment([c + vec2(-5., -7.), c + vec2(-5., 7.)], stroke);
                p.line_segment([c + vec2(5., -7.), c + vec2(5., 7.)], stroke);
                p.line_segment([c + vec2(-5., -7.), c + vec2(5., -7.)], stroke);
                p.line_segment([c + vec2(-5., 7.), c + vec2(0., 3.)], stroke);
                p.line_segment([c + vec2(5., 7.), c + vec2(0., 3.)], stroke);
            }
            "bookmark-on" => {
                let pts = vec![
                    c + vec2(-5., -7.),
                    c + vec2(5., -7.),
                    c + vec2(5., 7.),
                    c + vec2(0., 3.),
                    c + vec2(-5., 7.),
                ];
                p.add(Shape::convex_polygon(pts, color, stroke));
            }
            "share" => {
                // Open box with an arrow rising out of it.
                p.line_segment([c + vec2(-7., -1.), c + vec2(-7., 7.)], stroke);
                p.line_segment([c + vec2(-7., 7.), c + vec2(7., 7.)], stroke);
                p.line_segment([c + vec2(7., 7.), c + vec2(7., -1.)], stroke);
                p.line_segment([c + vec2(0., 4.), c + vec2(0., -7.)], stroke);
                p.line_segment([c + vec2(0., -7.), c + vec2(-4., -3.)], stroke);
                p.line_segment([c + vec2(0., -7.), c + vec2(4., -3.)], stroke);
            }
            _ => {
                p.circle_stroke(c, 7., stroke);
            }
        }
        response.clicked()
    }
    pub(super) fn shelf_area(&self, area: Rect) -> Rect {
        self.shelf_layout(area).books
    }
    pub(super) fn shelf_layout(&self, area: Rect) -> ShelfLayout {
        let margin = if area.width() >= 700. { 32. } else { 16. };
        let width = (area.width() - margin * 2.).min(1200.);
        let side_by_side = width >= 660.;
        let short_landscape = side_by_side && self.shelf_viewport.height() < 600.;
        let stacked_header = self.font_scale > 1.4 && area.width() < 520.;
        let row_h = (24. * self.font_scale + 20.).max(44.);
        let category_width = (80. * self.font_scale + 26.).min(area.width() - 48.);
        let sort_w = 150. * self.font_scale + 40.;
        // The sort button drops to a second row when it would cover the
        // collection label, and to a third when it cannot share the
        // stacked second row with the category button.
        let sort_second_row = !stacked_header
            && !short_landscape
            && 24. + 140. * self.font_scale + 12. + sort_w + 10. + category_width + 24.
                > area.width();
        let sort_third_row = stacked_header
            && 24. + category_width + 10. + sort_w + 24. > area.width();
        let top = 144.
            + if stacked_header {
                84. + if sort_third_row { row_h + 10. } else { 0. }
            } else {
                14. * (self.font_scale - 1.) + if sort_second_row { row_h + 10. } else { 0. }
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
        let (stage, books, browse, arranged_side_by_side) = if self.library_grid {
            (
                stage,
                books,
                Rect::from_min_size(
                    pos2(stage.left(), stage.bottom() + 28.),
                    vec2(
                        width,
                        if side_by_side {
                            236. + tall_extra * 0.5
                        } else {
                            170.
                        } + 28. * (self.font_scale - 1.),
                    ),
                ),
                side_by_side,
            )
        } else {
            // Collapsed: a single toggle row replaces the thumbnail strip and
            // the freed height grows the 3D shelf stage. Portrait-ish widths
            // switch to the stacked arrangement so the books get the height.
            let row = (24. * self.font_scale + 20.).max(44.);
            let avail =
                self.shelf_viewport.height() - top - 20. - row - self.shelf_reserve - 12.;
            let collapsed_side_by_side =
                side_by_side && (short_landscape || avail < width * 0.78);
            let (stage, books) = if collapsed_side_by_side {
                let stage_h = avail.clamp(height, 760.);
                let stage = Rect::from_min_size(stage.min, vec2(width, stage_h));
                let books = Rect::from_min_size(
                    stage.min + vec2(12., 12.),
                    vec2(width * 0.55 - 12., stage_h - 24.),
                );
                (stage, books)
            } else {
                // The info column keeps its content height so the action row
                // never paints over the toggle; the books shrink to make room
                // and the page scrolls for the toggle if it still overflows.
                let info_h = 172. + 112. * (self.font_scale - 1.);
                let books_h = (avail - info_h).clamp(80., 600.);
                let stage_h = books_h + info_h;
                let stage = Rect::from_min_size(stage.min, vec2(width, stage_h));
                let books = Rect::from_min_size(
                    stage.min + vec2(8., 6.),
                    vec2(width - 16., books_h),
                );
                (stage, books)
            };
            let browse = Rect::from_min_size(
                pos2(stage.left(), stage.bottom() + 20.),
                vec2(width, row),
            );
            (stage, books, browse, collapsed_side_by_side)
        };
        let info = if arranged_side_by_side {
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
        ShelfLayout {
            stage,
            books,
            info,
            browse,
            side_by_side: arranged_side_by_side,
            short_landscape: short_landscape && arranged_side_by_side,
        }
    }
    pub(super) fn detail_cover_rect(&self, area: Rect) -> Rect {
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
    pub(super) fn detail_side_by_side(area: Rect) -> bool {
        area.height() < 600. && area.width() >= 480. && area.width() > area.height()
    }
    /// iPad-sized detail page with a separate column beside the cover.
    pub(super) fn detail_wide(area: Rect) -> bool {
        area.width() >= 1100. && area.height() >= 620.
    }
    /// A readable column: full width on phones, centred and capped on iPad.
    pub(super) fn page_column(area: Rect, top: f32, bottom: f32) -> Rect {
        let width = (area.width() - 48.).min(760.);
        Rect::from_min_max(
            pos2(area.center().x - width / 2., area.top() + top),
            pos2(area.center().x + width / 2., area.bottom() - bottom),
        )
    }
    pub(super) fn navigation(&mut self, ui: &mut Ui, full: Rect, tablet: bool) {
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
                palette.ink,
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
                // Selected item keeps ink ink + a small accent indicator
                // instead of a tinted pill.
                ui.painter().rect_filled(
                    Rect::from_center_size(
                        pos2(rect.center().x, rect.bottom() - 4.),
                        vec2(18., 2.5),
                    ),
                    1.2,
                    palette.accent,
                );
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
            let fresh = if tab == Tab::Sources {
                self.sources.fresh_follows() + self.sources.updatable_count()
            } else {
                0
            };
            if fresh > 0 {
                let badge = Rect::from_center_size(icon_rect.right_top() + vec2(2., 2.), vec2(20., 18.));
                ui.painter().rect_filled(badge, 9., palette.accent);
                ui.painter().text(
                    badge.center(),
                    Align2::CENTER_CENTER,
                    fresh.min(99).to_string(),
                    FontId::proportional(11.),
                    palette.on_accent,
                );
            }
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
                    palette.ink
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
                if tab != Tab::Sources {
                    self.sources.clear_transient_status();
                }
                self.tab = tab;
                self.screen = Screen::Library;
                self.flight = None;
            }
        }
    }
    pub(super) fn side_navigation(area: Rect) -> bool {
        area.width() >= 1000. || (area.width() >= 700. && area.height() < 600.)
    }
    pub(super) fn library_viewport(&mut self, ui: &mut Ui, area: Rect, now: f64, entrance: f32) {
        self.shelf_viewport = area;
        let bottom = if Self::side_navigation(ui.ctx().content_rect()) {
            16.
        } else {
            80.
        };
        self.shelf_reserve = bottom;
        let height = self.shelf_layout(area).browse.bottom() - area.top() + 24.;
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
    pub(super) fn library_ui(&mut self, ui: &mut Ui, area: Rect, now: f64, entrance: f32) {
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
            let palette = crate::design::palette(self.dark);
            let row_h = (24. * self.font_scale + 20.).max(44.);
            let sort_w = 150. * self.font_scale + 40.;
            let margin = if area.width() >= 700. { 32. } else { 16. };
            let short = (area.width() - margin * 2.).min(1200.) >= 660.
                && self.shelf_viewport.height() < 600.;
            // Layout: [我的收藏 label][category tab strip][排序]. The strip
            // scrolls horizontally, so a modest footprint decides when the
            // sort button wraps to its own row.
            let strip_min = 120. * self.font_scale + 40.;
            let sort_second_row = !stacked
                && !short
                && 24. + 140. * self.font_scale + 12. + strip_min + 10. + sort_w
                    + 24.
                    > area.width();
            let sort_third_row = stacked
                && 24. + strip_min + 10. + sort_w + 24. > area.width();
            let strip_top = area.top()
                + if stacked {
                    142.
                } else {
                    82.
                };
            let strip_left = area.left() + 24.
                + if !stacked {
                    140. * self.font_scale + 12.
                } else {
                    0.
                };
            let strip_right = if sort_third_row || !stacked && sort_second_row {
                area.right() - 24.
            } else {
                area.right() - 24. - sort_w - 10.
            };
            let strip_rect = Rect::from_min_max(
                pos2(strip_left.min(strip_right - 44.), strip_top),
                pos2(strip_right.max(strip_left + 44.), strip_top + row_h),
            );
            let sort_rect = if !stacked && !sort_second_row || stacked && !sort_third_row {
                Rect::from_min_size(
                    pos2(strip_rect.right() + 10., strip_top),
                    vec2(sort_w, row_h),
                )
            } else {
                Rect::from_min_size(
                    pos2(area.left() + 24., strip_rect.bottom() + 10.),
                    vec2(sort_w, row_h),
                )
            };
            self.hitboxes.insert("categories".into(), strip_rect);
            self.category_strip(ui, strip_rect);
            self.hitboxes.insert("shelf-sort".into(), sort_rect);
            let sort_response =
                ui.interact(sort_rect, Id::unique("shelf-sort"), Sense::click());
            sort_response.widget_info(|| {
                WidgetInfo::labeled(Role::Button, true, "排序")
            });
            ui.painter().rect_filled(
                sort_rect,
                10.,
                if sort_response.hovered() || self.sort_menu {
                    palette.soft
                } else {
                    palette.surface
                },
            );
            ui.painter().rect_stroke(
                sort_rect,
                10.,
                Stroke::new(1., palette.line),
                StrokeKind::Inside,
            );
            let sort_label =
                ui.painter().layout_no_wrap(
                    format!("排序 · {}", self.shelf_sort.label()),
                    FontId::proportional(15. * self.font_scale),
                    palette.ink,
                );
            let tri_w = 8. * self.font_scale;
            let content = sort_label.size().x + 4. * self.font_scale + tri_w;
            let text_x = (sort_rect.center().x - content / 2.).max(sort_rect.left() + 8.);
            ui.painter().galley(
                pos2(text_x, sort_rect.center().y - sort_label.size().y / 2.),
                sort_label.clone(),
                palette.ink,
            );
            let cx = text_x + sort_label.size().x + 4. * self.font_scale + tri_w / 2.;
            let cy = sort_rect.center().y;
            let s = self.font_scale;
            ui.painter().add(Shape::convex_polygon(
                vec![
                    pos2(cx - 4. * s, cy - 2.5 * s),
                    pos2(cx + 4. * s, cy - 2.5 * s),
                    pos2(cx, cy + 3. * s),
                ],
                palette.muted,
                Stroke::NONE,
            ));
            if sort_response.clicked() {
                self.sort_menu = !self.sort_menu;
            }
            if self.sort_menu {
                let menu_row = (28. * self.font_scale + 16.).max(44.);
                let labels: Vec<_> = SHELF_SORTS
                    .iter()
                    .map(|sort| {
                        (
                            *sort,
                            ui.painter().layout_no_wrap(
                                sort.label().to_owned(),
                                FontId::proportional(15. * self.font_scale),
                                palette.ink,
                            ),
                        )
                    })
                    .collect();
                let inner_w = labels
                    .iter()
                    .map(|(_, galley)| galley.size().x)
                    .fold(0., f32::max)
                    + 34.;
                let panel_size = vec2(inner_w + 40., menu_row * 3. + 40.);
                let left = (sort_rect.right() - panel_size.x)
                    .clamp(area.left() + 8., (area.right() - panel_size.x - 8.).max(area.left() + 8.));
                let panel = Rect::from_min_size(
                    pos2(left, (sort_rect.bottom() + 8.).min(area.bottom() - panel_size.y - 8.)),
                    panel_size,
                );
                let mut picked = None;
                egui::Area::new(Id::unique("shelf-sort-menu"))
                    .order(Order::Foreground)
                    .fade_in(false)
                    .movable(false)
                    .fixed_pos(panel.min)
                    .show(ui.ctx(), |ui| {
                        crate::design::surface(ui).show(ui, |ui| {
                            ui.set_width(inner_w);
                            for (sort, galley) in &labels {
                                let (rect, response) = ui.allocate_exact_size(
                                    vec2(inner_w, menu_row),
                                    Sense::click(),
                                );
                                response.widget_info(|| {
                                    WidgetInfo::selected(
                                        Role::RadioButton,
                                        true,
                                        self.shelf_sort == *sort,
                                        sort.label(),
                                    )
                                });
                                if response.hovered() {
                                    ui.painter().rect_filled(rect, 8., palette.soft);
                                }
                                if self.shelf_sort == *sort {
                                    ui.painter().rect_filled(
                                        Rect::from_min_size(
                                            rect.min + vec2(0., 10.),
                                            vec2(3., menu_row - 20.),
                                        ),
                                        1.5,
                                        palette.accent,
                                    );
                                }
                                ui.painter().galley(
                                    pos2(
                                        rect.left() + 16.,
                                        rect.center().y - galley.size().y / 2.,
                                    ),
                                    galley.clone(),
                                    if self.shelf_sort == *sort {
                                        palette.ink
                                    } else {
                                        palette.muted
                                    },
                                );
                                self.hitboxes.insert(sort.hitbox().into(), rect);
                                if response.clicked() {
                                    picked = Some(*sort);
                                }
                            }
                        });
                    });
                if ui.ctx().input(|i| i.pointer.primary_clicked())
                    && let Some(pos) = ui.ctx().pointer_interact_pos()
                    && !panel.contains(pos)
                    && !sort_rect.contains(pos)
                {
                    self.sort_menu = false;
                }
                if let Some(sort) = picked {
                    self.set_shelf_sort(sort, now);
                }
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
        self.shelf_stage = layout.stage;
        self.shelf_books = layout.books;
        let shelf_area = layout.books;
        let palette = crate::design::palette(self.dark);
        ui.painter().rect_filled(layout.stage, 22., palette.soft);
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
        // During a reorder each book slides from its old slot to the new one
        // with a small lift; shelf input is suspended until it lands.
        let reordering = self.shelf_reorder.is_some();
        let draw_volumes: Vec<shelf::Volume> = if let Some(reorder) = &self.shelf_reorder {
            let t = ((now - reorder.started) / REORDER_SECONDS).clamp(0., 1.) as f32;
            if t >= 1. {
                volumes.clone()
            } else {
                ui.ctx().request_repaint();
                let eased = motion::cubic(t, 0.215, 0.61, 0.355, 1.);
                let old = shelf::geometry(shelf_area, books.len(), reorder.from_focus, 1.);
                let mut moved = Vec::with_capacity(volumes.len());
                for volume in &volumes {
                    let id = books[volume.index].id.as_str();
                    let origin = old
                        .iter()
                        .filter(|v| reorder.from_ids[v.index] == id)
                        .min_by(|a, b| {
                            (a.lane - volume.lane)
                                .abs()
                                .cmp(&(b.lane - volume.lane).abs())
                        });
                    moved.push(match origin {
                        Some(from) => {
                            let lift = ((volume.lane - from.lane).abs() as f32 * 6.)
                                .clamp(0., 22.);
                            shelf::lerp_volume(from, volume, eased, lift)
                        }
                        None => {
                            let dx = if (volume.lane as f32) < reorder.from_focus {
                                -shelf_area.width()
                            } else {
                                shelf_area.width()
                            };
                            shelf::lerp_volume(&shelf::offstage(volume, dx), volume, eased, 0.)
                        }
                    });
                }
                moved.sort_by_key(|v| v.lane);
                moved
            }
        } else {
            volumes.clone()
        };
        if self
            .shelf_reorder
            .as_ref()
            .is_some_and(|r| now - r.started >= REORDER_SECONDS)
        {
            self.shelf_reorder = None;
        }
        let mut keep = HashSet::new();
        for volume in &volumes {
            if (volume.lane as f32 - self.motion.focus).abs() <= 3. {
                let cover = self.cover_id(&books[volume.index].id);
                keep.insert(cover.clone());
                self.ask_cover(ui.ctx(), &cover);
            }
        }
        if self.covers.len() > 40 {
            self.covers.retain(|id, _| keep.contains(id));
        }
        let p = ui.painter().with_clip_rect(shelf_area);
        shelf::paint_floor(&p, &draw_volumes, self.dark);
        for volume in &draw_volumes {
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
                self.online_badge(book),
            );
            if is_central {
                self.hitboxes.insert("shelf-center".into(), volume.bounds());
            }
        }
        // The row is an endless shelf, so spines must dissolve into the page
        // instead of ending on a hard clip line.
        {
            let background = palette.soft;
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
        if gesture.dragged() && books.len() > 1 && !reordering {
            self.motion.drag(-ui.input(|i| i.pointer.delta().x) / pitch);
        }
        if gesture.drag_stopped() && books.len() > 1 && !reordering {
            self.motion.release(
                -ui.input(|i| i.pointer.velocity().x) / pitch,
                pitch,
                now,
                self.reduced,
            );
        }
        if gesture.hovered() && books.len() > 1 && !reordering {
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
            && !reordering
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
                    } else if Self::online_parts(&book.id).is_some() {
                        self.open_online(&book.id, false);
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
        let progress = if let Some(follow) = self.online_follow(&book.id) {
            self.online_progress_label(follow)
        } else {
            let pages = if has_progress {
                format!("第 {} / {} 页", book.current_page + 1, book.page_count)
            } else {
                format!("共 {} 页", book.page_count)
            };
            if let Some(chapter) = self.chapter(&book.id) {
                format!("{} · {pages}", chapter.chapter_title)
            } else {
                pages
            }
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
        let online = Self::online_parts(&book.id).is_some();
        // Controls stay painted (but inert) while books glide to their new
        // order so the stage does not flicker mid-reorder.
        let detail_clicked =
            self.selected.is_empty() && self.button(ui, "open-book", open_rect, "详情", false);
        if detail_clicked && !reordering {
            if online {
                self.open_online(&book.id, false);
            } else if let Some(volume) = volumes
                .iter()
                .find(|v| v.lane == self.motion.focus.round() as i32)
            {
                self.open_detail(book, volume, &volumes, area, now);
            }
        }
        let action = Rect::from_min_size(pos2(info.x, action_y), vec2(action_width, 50.));
        let read_clicked = self.selected.is_empty()
            && self.button(
                ui,
                "shelf-read",
                action,
                if online && self.sources.resume_pending() {
                    "正在打开…"
                } else if has_progress {
                    "继续阅读"
                } else {
                    "开始阅读"
                },
                true,
            );
        if read_clicked && !reordering {
            if online {
                self.open_online(&book.id, true);
            } else {
                self.open_reader(book);
            }
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
        let prev_clicked = books.len() > 1
            && self.icon(
                ui,
                "shelf-prev",
                Rect::from_min_size(pos2(pager_left, y), vec2(44., 44.)),
                "prev",
                "上一本",
            );
        if prev_clicked && !reordering {
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
        let next_clicked = books.len() > 1
            && self.icon(
                ui,
                "shelf-next",
                Rect::from_min_size(pos2(pager_right - 44., y), vec2(44., 44.)),
                "next",
                "下一本",
            );
        if next_clicked && !reordering {
            self.motion
                .select(self.motion.focus.round() + 1., now, self.reduced);
        }
        self.library_thumbnails(ui, layout.browse, &books, index, now);
        if !self.selected.is_empty() {
            let rect = Rect::from_min_size(
                pos2(area.left() + 24., area.bottom() - 122.),
                vec2(area.width() - 48., 48.),
            );
            ui.painter()
                .rect_filled(rect, 8., crate::design::palette(self.dark).surface);
            self.label(
                ui.painter(),
                rect.min + vec2(10., 14.),
                &format!("已选 {}", self.selected.len()),
                12.,
                ink,
            );
            // Online-only selections have no library row to categorize.
            let has_local = self
                .selected
                .iter()
                .any(|id| Self::online_parts(id).is_none());
            if has_local
                && self.button(
                    ui,
                    "set-category",
                    Rect::from_min_size(rect.min + vec2(80., 6.), vec2(120., 34.)),
                    "分类",
                    false,
                )
            {
                self.category_pick = true;
            }
            if self.button(
                ui,
                "remove-selected",
                Rect::from_min_size(pos2(rect.right() - 66., rect.top() + 6.), vec2(60., 34.)),
                "移出",
                false,
            ) {
                self.confirm_remove = true;
                self.remove_files = true;
            }
        }
    }
    pub(super) fn library_thumbnails(
        &mut self,
        ui: &mut Ui,
        area: Rect,
        books: &[Book],
        selected: usize,
        now: f64,
    ) {
        let palette = crate::design::palette(self.dark);
        let title = ui.painter().layout_no_wrap(
            "藏书一览".into(),
            crate::typography::display(19. * self.font_scale),
            palette.ink,
        );
        let count = ui.painter().layout_no_wrap(
            format!("· {} 本", books.len()),
            FontId::proportional(12. * self.font_scale),
            palette.muted,
        );
        let label = if self.library_grid { "收起" } else { "展开" };
        let galley = ui.painter().layout_no_wrap(
            label.to_owned(),
            FontId::proportional(12. * self.font_scale),
            palette.muted,
        );
        // A painted triangle is font-independent where a chevron glyph would
        // depend on the bundled faces covering it.
        let s = self.font_scale;
        let tri_w = 8. * s;
        let right_block = galley.size().x + 4. * s + tri_w + 6.;
        let show_count =
            title.size().x + 6. + count.size().x + 24. + right_block <= area.width();
        ui.painter().galley(area.min, title.clone(), palette.ink);
        if show_count {
            ui.painter().galley(
                pos2(area.left() + title.size().x + 6., area.top() + 5.),
                count,
                palette.muted,
            );
        }
        ui.painter().galley(
            pos2(area.right() - right_block, area.top() + 5.),
            galley.clone(),
            palette.muted,
        );
        let cy = area.top() + 5. + galley.size().y / 2.;
        let cx = area.right() - 6. - tri_w / 2.;
        let tri = if self.library_grid {
            [
                pos2(cx - 4. * s, cy + 2.5 * s),
                pos2(cx + 4. * s, cy + 2.5 * s),
                pos2(cx, cy - 3. * s),
            ]
        } else {
            [
                pos2(cx - 4. * s, cy - 2.5 * s),
                pos2(cx + 4. * s, cy - 2.5 * s),
                pos2(cx, cy + 3. * s),
            ]
        };
        ui.painter().add(Shape::convex_polygon(
            tri.to_vec(),
            palette.muted,
            Stroke::NONE,
        ));
        let row = Rect::from_min_size(
            area.min,
            vec2(area.width(), (34. * self.font_scale).max(44.)),
        );
        self.hitboxes.insert("shelf-grid-toggle".into(), row);
        let response = ui.interact(row, Id::unique("shelf-grid-toggle"), Sense::click());
        response.widget_info(|| {
            WidgetInfo::labeled(
                Role::Button,
                true,
                if show_count {
                    "藏书一览".to_owned()
                } else {
                    format!("藏书一览，{} 本", books.len())
                },
            )
        });
        if response.clicked() {
            self.library_grid = !self.library_grid;
            self.setting(
                "shelf-grid",
                if self.library_grid { "open" } else { "closed" }.to_owned(),
            );
        }
        if !self.library_grid {
            return;
        }
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
                                .rect_filled(cover.expand(5.), 10., palette.accent_soft);
                            // Two hairlines — one flush with the art, one at
                            // the halo — read as a single calm selection ring.
                            ui.painter().rect_stroke(
                                cover.expand(5.),
                                10.,
                                Stroke::new(1., palette.accent),
                                StrokeKind::Inside,
                            );
                            ui.painter().rect_stroke(
                                cover.expand(1.5),
                                6.,
                                Stroke::new(1., palette.accent.gamma_multiply(0.7)),
                                StrokeKind::Inside,
                            );
                        }
                        let cover_id = self.cover_id(&book.id);
                        self.ask_cover(ui.ctx(), &cover_id);
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
    }
    pub(super) fn open_detail(
        &mut self,
        book: &Book,
        volume: &shelf::Volume,
        volumes: &[shelf::Volume],
        area: Rect,
        now: f64,
    ) {
        if Self::online_parts(&book.id).is_some() {
            self.open_online(&book.id, false);
            return;
        }
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
    pub(super) fn detail_ui(&mut self, ui: &mut Ui, area: Rect, id: &str) {
        let Some(book) = self.books.iter().find(|b| b.id == id).cloned() else {
            return;
        };
        let cover_id = self.cover_id(id);
        self.ask_cover(ui.ctx(), &cover_id);
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
    pub(super) fn detail_actions(
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
        let key = if let Some(chapter) = self.chapter(id) {
            crate::trackers::binding_key(
                Some((&chapter.source_id, &chapter.manga_id)),
                None,
            )
        } else {
            crate::trackers::binding_key(None, Some(id))
        };
        if !key.is_empty() {
            let (rect, _) = ui.allocate_exact_size(vec2(width, 46.), Sense::hover());
            if self.button(ui, "detail-tracking", rect, "进度跟踪", false) {
                self.track_panel = Some(key);
                self.track_query.clear();
                self.track_draft = None;
            }
        }
    }
}
pub(super) struct Flight {
    pub(super) id: String,
    pub(super) book_id: String,
    pub(super) started: f64,
    pub(super) returning: bool,
    pub(super) from: Plane,
    pub(super) to: Plane,
    pub(super) foreground: Vec<shelf::Volume>,
    pub(super) clip: Rect,
}
/// The library shelf's book ordering; "recent" is the library order.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum ShelfSort {
    Recent,
    Progress,
    Added,
}
impl ShelfSort {
    pub(super) fn key(self) -> &'static str {
        match self {
            ShelfSort::Recent => "recent",
            ShelfSort::Progress => "progress",
            ShelfSort::Added => "added",
        }
    }
    pub(super) fn label(self) -> &'static str {
        match self {
            ShelfSort::Recent => "最近阅读",
            ShelfSort::Progress => "阅读进度",
            ShelfSort::Added => "加入时间",
        }
    }
    pub(super) fn hitbox(self) -> &'static str {
        match self {
            ShelfSort::Recent => "shelf-sort-recent",
            ShelfSort::Progress => "shelf-sort-progress",
            ShelfSort::Added => "shelf-sort-added",
        }
    }
    pub(super) fn from_key(key: &str) -> Self {
        match key {
            "progress" => ShelfSort::Progress,
            "added" => ShelfSort::Added,
            _ => ShelfSort::Recent,
        }
    }
}
pub(super) const SHELF_SORTS: [ShelfSort; 3] = [ShelfSort::Recent, ShelfSort::Progress, ShelfSort::Added];
/// A mid-flight book swap on the 3D shelf: `from_ids` is the visible order
/// the animation started from and `from_focus` the lane it was centred on.
pub(super) struct Reorder {
    pub(super) from_ids: Vec<String>,
    pub(super) from_focus: f32,
    pub(super) started: f64,
}
pub(super) const REORDER_SECONDS: f64 = 0.560;
#[derive(Clone, Copy)]
pub(super) struct ShelfLayout {
    pub(super) stage: Rect,
    pub(super) books: Rect,
    pub(super) info: Rect,
    pub(super) browse: Rect,
    pub(super) side_by_side: bool,
    pub(super) short_landscape: bool,
}
