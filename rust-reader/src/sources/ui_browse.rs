use crate::design;


use crate::source_filters::FilterEditor;



use egui::*;



use std::collections::HashSet;










use std::sync::atomic::Ordering;




use yejian_sources::*;
use super::*;
use super::engine::BrowseSession;
use super::covers::CoverReq;
use super::downloads::DownloadStatus;
use super::tasks::DownloadTask;
use super::engine::Task;
use super::covers::cover_key;

pub(super) fn quiet_action(label: &str) -> Button<'_> {
    Button::new(label)
        .min_size(vec2(44., 44.))
        .wrap_mode(egui::TextWrapMode::Extend)
}

pub(super) fn disclosure(ui: &mut Ui, label: &str, default_open: bool, body: impl FnOnce(&mut Ui)) {
    let mut state = collapsing_header::CollapsingState::load_with_default_open(
        ui.ctx(),
        ui.make_persistent_id(label),
        default_open,
    );
    let mut response =
        ui.add(quiet_action(label).right_text(if state.is_open() { "−" } else { "+" }));
    if response.clicked() {
        state.toggle(ui);
        response.mark_changed();
    }
    response.widget_info(|| WidgetInfo::labeled(Role::DisclosureTriangle, ui.is_enabled(), label));
    ui.ctx().accesskit_node_builder(response.id, |node| {
        node.set_expanded(state.is_open());
    });
    state.show_body_unindented(ui, body);
}

pub(super) fn source_item(
    ui: &mut Ui,
    id: impl std::hash::Hash + std::fmt::Debug,
    title: &str,
    metadata: &str,
    initial: bool,
    enabled: bool,
) -> bool {
    let palette = design::palette(ui.visuals().dark_mode);
    let width = ui.available_width();
    let leading = if initial { 50. } else { 0. };
    let text_width = (width - leading - 28.).max(40.);
    let mut title_job = egui::text::LayoutJob::simple(
        title.to_owned(),
        TextStyle::Body.resolve(ui.style()),
        palette.ink,
        text_width,
    );
    title_job.wrap.max_rows = 2;
    let title_galley = ui.painter().layout_job(title_job);
    let metadata_galley = ui.painter().layout(
        metadata.to_owned(),
        TextStyle::Small.resolve(ui.style()),
        palette.muted,
        text_width,
    );
    let text_height = title_galley.size().y
        + if metadata.is_empty() {
            0.
        } else {
            metadata_galley.size().y + 4.
        };
    let height = (text_height + 24.).max(68.);
    let (rect, _) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
    let response = ui.interact(
        rect,
        ui.scope_id().with(id),
        if enabled {
            Sense::click()
        } else {
            Sense::hover()
        },
    );
    response.widget_info(|| WidgetInfo::labeled(Role::Button, enabled, title));
    let painter = ui.painter().with_clip_rect(ui.clip_rect().intersect(rect));
    if response.hovered() && enabled {
        painter.rect_filled(rect, 8., palette.soft);
    }
    if initial {
        let badge =
            Rect::from_center_size(pos2(rect.left() + 18., rect.center().y), vec2(36., 36.));
        painter.rect_filled(badge, 9., palette.soft);
        let letter = title
            .chars()
            .next()
            .map(|c| c.to_string())
            .unwrap_or_default();
        painter.text(
            badge.center(),
            Align2::CENTER_CENTER,
            letter,
            FontId::proportional(20.),
            palette.accent,
        );
    }
    let origin = pos2(rect.left() + leading, rect.center().y - text_height / 2.);
    painter.galley(origin, title_galley.clone(), palette.ink);
    if !metadata.is_empty() {
        painter.galley(
            origin + vec2(0., title_galley.size().y + 4.),
            metadata_galley,
            palette.muted,
        );
    }
    let c = pos2(rect.right() - 10., rect.center().y);
    painter.line_segment(
        [c + vec2(-3., -5.), c + vec2(2., 0.)],
        Stroke::new(1.5, palette.muted),
    );
    painter.line_segment(
        [c + vec2(2., 0.), c + vec2(-3., 5.)],
        Stroke::new(1.5, palette.muted),
    );
    painter.line_segment(
        [
            pos2(rect.left() + leading, rect.bottom()),
            pos2(rect.right(), rect.bottom()),
        ],
        Stroke::new(1., palette.line),
    );
    enabled && response.clicked()
}

pub(super) fn grid_columns(width: f32) -> usize {
    let min_cell = if width >= 600. { 150. } else { 112. };
    (((width + 16.) / (min_cell + 16.)).floor() as usize).max(1)
}

pub(super) fn paint_cover(
    painter: &Painter,
    rect: Rect,
    texture: Option<&TextureHandle>,
    title: &str,
    font: FontId,
    palette: &crate::design::Palette,
) {
    match texture {
        Some(texture) => {
            painter.image(
                texture.id(),
                rect,
                Rect::from_min_max(pos2(0., 0.), pos2(1., 1.)),
                Color32::WHITE,
            );
        }
        None => {
            painter.rect_filled(rect, 8., palette.soft);
            if let Some(initial) = title.chars().next() {
                painter.text(
                    rect.center(),
                    Align2::CENTER_CENTER,
                    initial.to_string(),
                    font,
                    palette.muted,
                );
            }
        }
    }
    painter.rect_stroke(rect, 8., Stroke::new(1., palette.line), StrokeKind::Inside);
}

/// abandoned search can never be shown under another source.
#[derive(Default)]
pub(super) struct ResultsState {
    pub(super) source: String,
    pub(super) query: String,
    pub(super) page: i32,
    pub(super) has_more: bool,
    pub(super) entries: Vec<Manga>,
    pub(super) revision: u64,
}

impl ResultsState {
    pub(super)fn clear(&mut self) {
        let revision = self.revision.wrapping_add(1);
        *self = Self {
            revision,
            ..Self::default()
        };
    }
    pub(super)fn begin(&mut self, source: &str, query: &str) {
        self.revision = self.revision.wrapping_add(1);
        self.entries.clear();
        self.source = source.to_string();
        self.query = query.to_string();
        self.page = 0;
        self.has_more = false;
    }
    /// Source, query and page for the next request, or `None` at the end.

    pub(super)fn next_page(&self) -> Option<(String, String, i32)> {
        if !self.has_more || self.source.is_empty() {
            None
        } else {
            Some((self.source.clone(), self.query.clone(), self.page + 1))
        }
    }
    /// Accepts exactly the reply we asked for. Returns `false` for stale,

    /// foreign-source or duplicate replies, leaving state untouched so the

    /// page counter only moves on real success and a failed page stays

    /// retryable.

    pub(super)fn accept(
        &mut self,
        source: &str,
        query: &str,
        page: i32,
        entries: Vec<Manga>,
        has_next: bool,
    ) -> bool {
        if source != self.source.as_str() || query != self.query.as_str() || page != self.page + 1 {
            return false;
        }
        let mut seen: HashSet<(String, String)> = self
            .entries
            .iter()
            .map(|m| (m.source_key.clone(), m.key.clone()))
            .collect();
        for mut manga in entries {
            if manga.source_key.is_empty() {
                manga.source_key = source.to_string();
            }
            if seen.insert((manga.source_key.clone(), manga.key.clone())) {
                self.entries.push(manga);
            }
        }
        self.page = page;
        self.has_more = has_next;
        true
    }
}

pub(super) fn apply_source_change(
    selected: &mut String,
    results: &mut ResultsState,
    selected_manga: &mut Option<Manga>,
    chapters: &mut Vec<Chapter>,
    id: &str,
) -> bool {
    if selected.as_str() == id {
        return false;
    }
    *selected = id.to_string();
    *selected_manga = None;
    chapters.clear();
    results.clear();
    true
}

impl Sources {
    pub(super)fn select_source(&mut self, id: &str) {
        if self.selected_source == id {
            return;
        }
        self.pending_open = None;
        if !self.selected_source.is_empty() {
            self.sessions.insert(
                self.selected_source.clone(),
                BrowseSession {
                    query: self.query.clone(),
                    filters: self.applied_filters.clone(),
                    listing: self.listing.clone(),
                },
            );
        }
        apply_source_change(
            &mut self.selected_source,
            &mut self.results,
            &mut self.selected_manga,
            &mut self.chapters,
            id,
        );
        let session = self.sessions.get(id).cloned().unwrap_or_default();
        self.query = session.query;
        self.applied_filters = session.filters;
        self.listing = session.listing;
        self.filters.clear();
        self.listings.clear();
        self.filter_editor = None;
        self.chapter_query.clear();
        self.filter_error = None;
        self.listing_error = None;
        self.metadata_loaded = false;
        self.request_id = self.request_id.wrapping_add(1);
        self.current_request
            .store(self.request_id, Ordering::Relaxed);
        self.busy = false;
        self.pending = None;
        self.failed = None;
        self.status.clear();
        if !id.is_empty() {
            self.submit(Task::Metadata(id.into()));
        }
    }
    /// Restored from the `search-scope` setting at startup.
    pub(crate) fn set_search_scope(&mut self, scope: &str) {
        self.search_scope = if scope == "global" {
            "global"
        } else {
            "current"
        }
        .into();
    }
    /// UI toggle; the change is queued for the app to persist.
    pub(super)fn choose_scope(&mut self, scope: &str) {
        if self.search_scope == scope {
            return;
        }
        self.set_search_scope(scope);
        self.scope_dirty = Some(self.search_scope.clone());
        if !self.query.trim().is_empty() {
            self.start_search();
        }
    }
    /// `ReaderApp` persists the pending scope under `search-scope`.
    pub(crate) fn take_scope_change(&mut self) -> Option<String> {
        self.scope_dirty.take()
    }
    /// Restored from the `search-history` setting at startup.
    pub(crate) fn set_search_history(&mut self, history: Vec<String>) {
        self.search_history = history;
    }
    #[cfg(debug_assertions)]
    /// Empties the search field so history chips show (acceptance).
    pub(crate) fn clear_query(&mut self) {
        self.query.clear();
    }
    #[allow(dead_code)] // read by the acceptance bin and unit tests
    pub(crate) fn search_history(&self) -> &[String] {
        &self.search_history
    }
    /// History changed (record/clear/remove): `ReaderApp` persists it.
    pub(crate) fn take_history_change(&mut self) -> Option<Vec<String>> {
        if self.history_dirty {
            self.history_dirty = false;
            Some(self.search_history.clone())
        } else {
            None
        }
    }
    pub(crate) fn set_private(&mut self, private: bool) {
        self.private = private;
    }
    /// Newest first, deduplicated, at most 20 entries; private mode records
    /// nothing.
    pub(super)fn record_history(&mut self) {
        let query = self.query.trim();
        if self.private || query.is_empty() {
            return;
        }
        let query = query.to_string();
        self.search_history.retain(|item| item != &query);
        self.search_history.insert(0, query);
        self.search_history.truncate(20);
        self.history_dirty = true;
    }
    /// Width of the two-segment scope control, so the search row can budget.
    pub(super)fn scope_segment_width(&self, ui: &Ui) -> f32 {
        let font = TextStyle::Body.resolve(ui.style());
        let mut widest = 0f32;
        for label in ["当前书源", "全部书源"] {
            let galley = ui.painter().layout_no_wrap(
                label.into(),
                font.clone(),
                Color32::BLACK,
            );
            widest = widest.max(galley.size().x);
        }
        (widest + 28.) * 2.
    }
    /// Joined two-segment toggle: selected segment is soft-filled and stroked.
    /// `width` is the natural measured width, or the full row on narrow
    /// layouts so the control never overflows its row.
    pub(super)fn scope_segment(&mut self, ui: &mut Ui, width: f32) {
        let palette = design::palette(ui.visuals().dark_mode);
        let font = TextStyle::Body.resolve(ui.style());
        let (rect, _) = ui.allocate_exact_size(vec2(width, 44.), Sense::hover());
        let seg_w = width / 2.;
        let mut choose = None;
        for (index, (scope, label)) in
            [("current", "当前书源"), ("global", "全部书源")].iter().enumerate()
        {
            let seg = Rect::from_min_size(
                rect.min + vec2(index as f32 * seg_w, 0.),
                vec2(seg_w, 44.),
            );
            let hit = ui.interact(
                seg,
                ui.scope_id().with(("search-scope", index)),
                Sense::click(),
            );
            hit.widget_info(|| WidgetInfo::labeled(Role::Button, true, *label));
            self.hitboxes.insert(format!("scope-{scope}"), seg);
            let selected = self.search_scope == *scope;
            let painter = ui.painter();
            if selected {
                painter.rect_filled(seg, 10., palette.soft);
                painter.rect_stroke(
                    seg,
                    10.,
                    Stroke::new(1.5, palette.ink),
                    StrokeKind::Inside,
                );
            } else if hit.hovered() {
                painter.rect_filled(seg, 10., palette.soft);
            }
            let color = if selected { palette.ink } else { palette.muted };
            painter.text(
                seg.center(),
                Align2::CENTER_CENTER,
                *label,
                font.clone(),
                color,
            );
            if hit.clicked() {
                choose = Some(*scope);
            }
        }
        ui.painter().vline(
            rect.center().x,
            rect.shrink(10.).y_range(),
            Stroke::new(1., palette.line),
        );
        ui.painter().rect_stroke(
            rect,
            10.,
            Stroke::new(1., palette.line),
            StrokeKind::Inside,
        );
        if let Some(scope) = choose {
            self.choose_scope(scope);
        }
    }
    pub(super)fn start_search(&mut self) {
        self.record_history();
        if self.global_mode() {
            self.global_search();
            return;
        }
        let source = self.selected_source.clone();
        if source.is_empty() {
            return;
        }
        let query = self.query.trim().to_string();
        self.query = query.clone();
        self.listing = None;
        self.results.begin(&source, &query);
        self.selected_manga = None;
        self.chapters.clear();
        let filters = if self.filters_hidden() {
            Vec::new()
        } else {
            self.applied_filters.clone()
        };
        self.submit(Task::Search {
            source,
            query,
            page: 1,
            filters,
        });
    }
    pub(super)fn filters_hidden(&self) -> bool {
        !self.query.trim().is_empty()
            && self.installed.iter().any(|source| {
                source.manifest.id == self.selected_source
                    && source.manifest.hides_filters_while_searching
            })
    }
    pub(super)fn start_listing(&mut self, id: &str) {
        self.query.clear();
        self.listing = Some(id.into());
        self.selected_manga = None;
        self.chapters.clear();
        self.results.begin(&self.selected_source, "");
        self.submit(Task::Browse {
            source: self.selected_source.clone(),
            listing: id.into(),
            page: 1,
        });
    }
    pub(super)fn load_next_page(&mut self) {
        if let Some((source, query, page)) = self.results.next_page() {
            // Pagination uses the committed request, never the edited text field.

            let task = if let Some(listing) = &self.listing {
                Task::Browse {
                    source,
                    listing: listing.clone(),
                    page,
                }
            } else {
                Task::Search {
                    source,
                    query,
                    page,
                    filters: self.committed_filters(),
                }
            };
            self.submit(task);
        }
    }
    pub(super)fn committed_filters(&self) -> Vec<FilterValue> {
        let hidden = !self.results.query.is_empty()
            && self.installed.iter().any(|source| {
                source.manifest.id == self.selected_source
                    && source.manifest.hides_filters_while_searching
            });
        if hidden {
            Vec::new()
        } else {
            self.applied_filters.clone()
        }
    }
    pub(super)fn open_manga(&mut self, manga: Manga) {
        let source = if manga.source_key.is_empty() {
            self.results.source.clone()
        } else {
            manga.source_key.clone()
        };
        if source.is_empty() {
            self.status = "请先选择并搜索一个书源".into();
            return;
        }
        self.selected_manga = None;
        self.chapters.clear();
        self.chapter_query.clear();
        self.description_expanded = false;
        self.submit(Task::Chapters(source, manga));
    }
    pub(super)fn source_for(&self, manga: &Manga) -> String {
        if manga.source_key.is_empty() {
            self.selected_source.clone()
        } else {
            manga.source_key.clone()
        }
    }
    pub(super)fn filter_page(&mut self, ui: &mut Ui) {
        let Some(mut editor) = self.filter_editor.take() else {
            return;
        };
        let dark = ui.visuals().dark_mode;
        ui.add(
            Label::new(
                RichText::new("筛选与排序")
                    .size(TextStyle::Body.resolve(ui.style()).size * 1.1)
                    .strong(),
            )
            .wrap(),
        );
        ui.add_space(4.);
        let submitted = editor.submitted(&self.filters);
        let mut leave = false;
        ui.horizontal_wrapped(|ui| {
            if ui
                .add_enabled(submitted.is_ok(), design::primary("应用筛选", dark))
                .clicked()
            {
                self.applied_filters = submitted.clone().unwrap_or_default();
                leave = true;
                self.start_search();
            }
            if ui.add(quiet_action("重置")).clicked() {
                editor = FilterEditor::new(&self.filters, &[]);
            }
            if ui.add(quiet_action("取消")).clicked() {
                leave = true;
            }
        });
        if let Err(error) = submitted {
            ui.colored_label(ui.visuals().error_fg_color, error);
        }
        ui.add_space(14.);
        ui.separator();
        ui.add_space(14.);
        ScrollArea::vertical()
            .id_salt("source-filter-editor")
            .show(ui, |ui| {
                editor.ui(ui, &self.filters);
            });
        if !leave {
            self.filter_editor = Some(editor);
        }
    }
    pub(crate) fn back(&mut self) -> bool {
        if self.settings_open {
            self.close_settings();
            return true;
        }
        if self.filter_editor.take().is_some() {
            return true;
        }
        if self.selected_manga.take().is_some() {
            self.chapters.clear();
            if self.shelf_origin {
                self.shelf_origin = false;
                self.shelf_return = true;
            }
            return true;
        }
        if !self.selected_source.is_empty() {
            self.select_source("");
            return true;
        }
        false
    }
    pub(super)fn browser_header(&mut self, ui: &mut Ui) {
        let palette = design::palette(ui.visuals().dark_mode);
        ui.vertical(|ui| {
            ui.set_min_width(ui.available_width());
            ui.horizontal_wrapped(|ui| {
                if ui.add(quiet_action("‹ 我的书源")).clicked() {
                    self.select_source("");
                }
                let name = if self.global_mode() {
                    "全部书源".to_string()
                } else {
                    self.installed
                        .iter()
                        .find(|source| source.manifest.id == self.selected_source)
                        .map(|source| source.manifest.name.clone())
                        .unwrap_or_default()
                };
                ui.add(Label::new(RichText::new(name).strong().color(palette.ink)).wrap());
                if !self.global_mode()
                    && !self.selected_source.is_empty()
                    && !self.current_settings().is_empty()
                    && ui.add(quiet_action("书源设置")).clicked()
                {
                    self.settings_open = true;
                }
                let site = self
                    .installed
                    .iter()
                    .find(|source| source.manifest.id == self.selected_source)
                    .and_then(|source| source.manifest.primary_url().map(str::to_string));
                if let Some(site) = site.filter(|_| cfg!(target_os = "ios") && !self.global_mode())
                {
                    if ui.add(quiet_action("网页验证")).clicked() {
                        self.open_web(&site, None);
                    }
                }
            });
            if self.selected_source.is_empty() {
                return;
            }
            ui.add_space(2.);
            let narrow = ui.available_width() < 520.;
            let mut enter = false;
            if narrow {
                let input = ui.add(
                    TextEdit::singleline(&mut self.query)
                        .hint_text("搜索漫画")
                        .desired_width(ui.available_width()),
                );
                enter = input.lost_focus()
                    && ui.input(|input| input.key_pressed(Key::Enter));
                self.hitboxes.insert("source-search".into(), input.rect);
            }
            let gap = ui.spacing().item_spacing.x;
            ui.horizontal_wrapped(|ui| {
                if !narrow {
                    // Field takes whatever the segmented control and the
                    // 搜索 button (min 88pt) leave behind.
                    let rest = self.scope_segment_width(ui) + 88. + gap * 4.;
                    let input = ui.add(
                        TextEdit::singleline(&mut self.query)
                            .hint_text("搜索漫画")
                            .desired_width(
                                (ui.available_width() - rest).max(120.),
                            ),
                    );
                    enter = input.lost_focus()
                        && ui.input(|input| input.key_pressed(Key::Enter));
                    self.hitboxes.insert("source-search".into(), input.rect);
                }
                let seg_width = if narrow {
                    ui.available_width()
                } else {
                    self.scope_segment_width(ui)
                };
                self.scope_segment(ui, seg_width);
                let go = ui.add_enabled(
                    self.metadata_loaded,
                    design::primary("搜索", ui.visuals().dark_mode),
                );
                self.hitboxes.insert("source-search-go".into(), go.rect);
                if go.clicked() || enter && self.metadata_loaded {
                    self.start_search();
                }
            });
            if self.query.trim().is_empty() && !self.search_history.is_empty() {
                ui.horizontal_wrapped(|ui| {
                    let mut index = 0;
                    while index < self.search_history.len() {
                        let label = self.search_history[index].clone();
                        let chip = ui.add(quiet_action(&label));
                        self.hitboxes
                            .insert(format!("history-{index}"), chip.rect);
                        if chip.long_touched() {
                            self.search_history.remove(index);
                            self.history_dirty = true;
                            continue;
                        }
                        index += 1;
                        if chip.clicked() {
                            self.query = label;
                            self.start_search();
                            return;
                        }
                    }
                    let clear = ui.add(quiet_action("清空"));
                    self.hitboxes.insert("history-clear".into(), clear.rect);
                    if clear.clicked() {
                        self.history_confirm = true;
                    }
                });
            }
            ui.horizontal_wrapped(|ui| {
                if !self.global_mode() {
                    let hidden = self.filters_hidden();
                    let label = if self.applied_filters.is_empty() {
                        "筛选与排序".into()
                    } else {
                        format!("筛选（{}）", self.applied_filters.len())
                    };
                    if ui
                        .add_enabled(
                            self.metadata_loaded && !hidden && !self.filters.is_empty(),
                            quiet_action(&label),
                        )
                        .clicked()
                    {
                        self.filter_editor =
                            Some(FilterEditor::new(&self.filters, &self.applied_filters));
                    }
                    if !self.applied_filters.is_empty()
                        && !hidden
                        && ui.add(quiet_action("清除筛选")).clicked()
                    {
                        self.applied_filters.clear();
                        self.start_search();
                    }
                }
            });
            if self.global_mode() {
            } else if self.filters_hidden() {
                ui.label("此书源的关键词搜索不使用筛选；清空关键词后可筛选。");
            } else if self.metadata_loaded && self.filters.is_empty() && self.filter_error.is_none()
            {
                ui.label("此书源未提供筛选选项。");
            }
            if !self.global_mode()
                && let Some(error) = self.filter_error.clone()
            {
                disclosure(ui, "部分筛选未能加载 · 查看原因", false, |ui| {
                    ui.add(Label::new(error).wrap());
                });
            }
            if !self.global_mode()
                && let Some(error) = self.listing_error.clone()
            {
                disclosure(ui, "部分列表未能加载 · 查看原因", false, |ui| {
                    ui.add(Label::new(error).wrap());
                });
            }
            if !self.global_mode()
                && (self.filter_error.is_some() || self.listing_error.is_some())
                && ui
                    .add_enabled(!self.busy, quiet_action("重新加载书源选项"))
                    .clicked()
            {
                self.submit(Task::Metadata(self.selected_source.clone()));
            }
            if !self.global_mode() && self.listings.len() > 4 {
                let label = self
                    .listings
                    .iter()
                    .find(|listing| self.listing.as_ref() == Some(&listing.id))
                    .map(|listing| listing.name.as_str())
                    .unwrap_or("全部 / 搜索");
                let mut selection = self.listing.clone();
                ComboBox::from_id_salt("source-listing")
                    .selected_text(label)
                    .width(ui.available_width().min(400.))
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut selection, None, "全部 / 搜索");
                        for listing in &self.listings {
                            ui.selectable_value(
                                &mut selection,
                                Some(listing.id.clone()),
                                &listing.name,
                            );
                        }
                    });
                if selection != self.listing {
                    if let Some(id) = selection {
                        self.start_listing(&id);
                    } else {
                        self.start_search();
                    }
                }
            } else if !self.global_mode() {
                ui.horizontal_wrapped(|ui| {
                    if ui
                        .add_enabled(
                            self.metadata_loaded,
                            quiet_action("全部 / 搜索")
                                .selected(self.listing.is_none())
                                .frame_when_inactive(self.listing.is_none()),
                        )
                        .clicked()
                    {
                        self.start_search();
                    }
                    for listing in self.listings.clone() {
                        if ui
                            .add(
                                quiet_action(&listing.name)
                                    .selected(self.listing.as_ref() == Some(&listing.id))
                                    .frame_when_inactive(
                                        self.listing.as_ref() == Some(&listing.id),
                                    ),
                            )
                            .clicked()
                        {
                            self.start_listing(&listing.id);
                        }
                    }
                });
            }
            if !self.global_mode() && self.listing.is_some() && !self.applied_filters.is_empty() {
                ui.label("当前显示书源列表；应用筛选后切换到搜索结果。");
            }
        });
        ui.separator();
        ui.add_space(18.);
    }
    pub(super)fn results_grid(&mut self, ui: &mut Ui) {
        let palette = design::palette(ui.visuals().dark_mode);
        let width = ui.available_width();
        let gap = 16.;
        let cols = grid_columns(width);
        let cell_w = (width - gap * cols.saturating_sub(1) as f32) / cols as f32;
        let cover_h = cell_w * 1.5;
        let body = TextStyle::Body.resolve(ui.style());
        let body_h = ui.text_style_height(&TextStyle::Body);
        let small_h = ui.text_style_height(&TextStyle::Small);
        let cell_h = cover_h + 6. + body_h * 2. + 2. + small_h + 6.;
        let expanded = ui.clip_rect().expand(cell_h + gap);
        let entries = self.results.entries.clone();
        let mut want = Vec::new();
        let mut last_row = None;
        let mut open = None;
        for (row, chunk) in entries.chunks(cols).enumerate() {
            let (rect, _) = ui.allocate_exact_size(vec2(width, cell_h), Sense::hover());
            last_row = Some(rect);
            for (offset, manga) in chunk.iter().enumerate() {
                let index = row * cols + offset;
                let cell = Rect::from_min_size(
                    pos2(rect.left() + offset as f32 * (cell_w + gap), rect.top()),
                    vec2(cell_w, cell_h),
                );
                let response = ui.interact(
                    cell,
                    ui.scope_id().with(("source-result", index)),
                    if self.busy {
                        Sense::hover()
                    } else {
                        Sense::click()
                    },
                );
                response
                    .widget_info(|| WidgetInfo::labeled(Role::Button, !self.busy, &manga.title));
                self.hitboxes.insert(format!("source-result-{index}"), cell);
                if response.clicked() {
                    open = Some(manga.clone());
                }
                let source = self.source_for(manga);
                let url = manga.cover.clone().unwrap_or_default();
                let texture = if expanded.intersects(cell) {
                    let texture = self.cover_image(ui.ctx(), &source, &url);
                    if texture.is_none() && !url.is_empty() {
                        want.push(CoverReq {
                            key: cover_key(&source, &url),
                            source,
                            url,
                        });
                    }
                    texture
                } else {
                    None
                };
                if response.hovered() && !self.busy {
                    ui.painter().rect_filled(cell, 8., palette.soft);
                }
                let cover_rect = Rect::from_min_size(cell.min, vec2(cell_w, cover_h));
                let painter = ui.painter().with_clip_rect(ui.clip_rect().intersect(cell));
                paint_cover(
                    &painter,
                    cover_rect,
                    texture.as_ref(),
                    &manga.title,
                    TextStyle::Heading.resolve(ui.style()),
                    &palette,
                );
                let mut title_job = egui::text::LayoutJob::simple(
                    manga.title.clone(),
                    body.clone(),
                    palette.ink,
                    cell_w,
                );
                title_job.wrap.max_rows = 2;
                let title_galley = painter.layout_job(title_job);
                painter.galley(
                    pos2(cell.left(), cover_rect.bottom() + 6.),
                    title_galley,
                    palette.ink,
                );
                let meta = manga
                    .authors
                    .as_ref()
                    .filter(|values| !values.is_empty())
                    .map(|values| values.join(" / "))
                    .or_else(|| {
                        manga
                            .tags
                            .as_ref()
                            .and_then(|values| values.first().cloned())
                    })
                    .unwrap_or_default();
                let mut meta_job = egui::text::LayoutJob::simple(
                    meta,
                    TextStyle::Small.resolve(ui.style()),
                    palette.muted,
                    cell_w,
                );
                meta_job.wrap.max_rows = 1;
                let meta_galley = painter.layout_job(meta_job);
                painter.galley(
                    pos2(cell.left(), cover_rect.bottom() + 6. + body_h * 2. + 2.),
                    meta_galley,
                    palette.muted,
                );
            }
        }
        if self.results.has_more
            && !self.busy
            && self.auto_next_page != Some(self.results.page + 1)
            && last_row.is_some_and(|rect| rect.intersects(ui.clip_rect().expand(2.)))
        {
            self.auto_next_page = Some(self.results.page + 1);
            self.load_next_page();
        }
        self.plan_covers(want);
        if let Some(manga) = open {
            self.open_manga(manga);
        }
    }
    /// One global-search cover cell, sharing the result-grid card styling.
    pub(super)fn global_cell(
        &mut self,
        ui: &mut Ui,
        group: usize,
        index: usize,
        manga: &Manga,
        cell: Rect,
        cover_rect: Rect,
        want: &mut Vec<CoverReq>,
        open: &mut Option<Manga>,
    ) {
        let palette = design::palette(ui.visuals().dark_mode);
        let response = ui.interact(
            cell,
            ui.scope_id().with(("global-result", group, index)),
            if self.busy {
                Sense::hover()
            } else {
                Sense::click()
            },
        );
        response.widget_info(|| WidgetInfo::labeled(Role::Button, !self.busy, &manga.title));
        self.hitboxes
            .insert(format!("global-result-{group}-{index}"), cell);
        if response.clicked() {
            *open = Some(manga.clone());
        }
        let url = manga.cover.clone().unwrap_or_default();
        let texture = if ui.clip_rect().expand(cell.height() + 16.).intersects(cell) {
            let texture = self.cover_image(ui.ctx(), &manga.source_key, &url);
            if texture.is_none() && !url.is_empty() {
                want.push(CoverReq {
                    key: cover_key(&manga.source_key, &url),
                    source: manga.source_key.clone(),
                    url,
                });
            }
            texture
        } else {
            None
        };
        if response.hovered() && !self.busy {
            ui.painter().rect_filled(cell, 8., palette.soft);
        }
        let painter = ui.painter().with_clip_rect(ui.clip_rect().intersect(cell));
        paint_cover(
            &painter,
            cover_rect,
            texture.as_ref(),
            &manga.title,
            TextStyle::Heading.resolve(ui.style()),
            &palette,
        );
        let body_h = ui.text_style_height(&TextStyle::Body);
        let mut title_job = egui::text::LayoutJob::simple(
            manga.title.clone(),
            TextStyle::Body.resolve(ui.style()),
            palette.ink,
            cell.width(),
        );
        title_job.wrap.max_rows = 2;
        let title_galley = painter.layout_job(title_job);
        painter.galley(
            pos2(cell.left(), cover_rect.bottom() + 6.),
            title_galley,
            palette.ink,
        );
        let meta = manga
            .authors
            .as_ref()
            .filter(|values| !values.is_empty())
            .map(|values| values.join(" / "))
            .or_else(|| {
                manga
                    .tags
                    .as_ref()
                    .and_then(|values| values.first().cloned())
            })
            .unwrap_or_default();
        let mut meta_job = egui::text::LayoutJob::simple(
            meta,
            TextStyle::Small.resolve(ui.style()),
            palette.muted,
            cell.width(),
        );
        meta_job.wrap.max_rows = 1;
        let meta_galley = painter.layout_job(meta_job);
        painter.galley(
            pos2(cell.left(), cover_rect.bottom() + 6. + body_h * 2. + 2.),
            meta_galley,
            palette.muted,
        );
    }
    /// 全部书源 results: one group per installed source, covers in a row.
    pub(super)fn global_results_ui(&mut self, ui: &mut Ui) {
        let palette = design::palette(ui.visuals().dark_mode);
        let mut want = Vec::new();
        let mut open = None;
        let mut dive = None;
        let mut retry = None;
        // Results with entries first (installed order), then still-searching,
        // empty groups, and failures last — stable while events stream in.
        let mut order: Vec<usize> = (0..self.global_groups.len()).collect();
        let rank = |state: &GlobalState| match state {
            GlobalState::Done(entries) if !entries.is_empty() => 0,
            GlobalState::Searching => 1,
            GlobalState::Done(_) => 2,
            GlobalState::Failed(_) => 3,
        };
        order.sort_by_key(|index| rank(&self.global_groups[*index].state));
        for g in order {
            let group = self.global_groups[g].clone();
            let name = self
                .installed
                .iter()
                .find(|source| source.manifest.id == group.source)
                .map(|source| source.manifest.name.clone())
                .unwrap_or_else(|| group.source.clone());
            match &group.state {
                GlobalState::Done(entries) => {
                    if entries.is_empty() {
                        ui.add_space(6.);
                        ui.label(
                            RichText::new(format!("{name} · 无结果")).color(palette.muted),
                        );
                        continue;
                    }
                    // Section-size header; the chevron stays muted at the
                    // right edge and the label wraps before it ever clips.
                    let size = ui.text_style_height(&TextStyle::Body) * 17. / 15.;
                    let font = FontId::proportional(size);
                    let galley = ui.painter().layout(
                        format!("{name} · {} 条", entries.len()),
                        font.clone(),
                        palette.ink,
                        ui.available_width() - size - 16.,
                    );
                    let row_h = (galley.size().y + 14.).max(44.);
                    let (rect, _) = ui.allocate_exact_size(
                        vec2(ui.available_width(), row_h),
                        Sense::hover(),
                    );
                    let hit = ui.interact(
                        rect,
                        ui.scope_id().with(("global-group", g)),
                        Sense::click(),
                    );
                    hit.widget_info(|| {
                        WidgetInfo::labeled(
                            Role::Button,
                            true,
                            format!("{name} · {} 条", entries.len()),
                        )
                    });
                    self.hitboxes.insert(format!("global-group-{g}"), rect);
                    if hit.hovered() {
                        ui.painter().rect_filled(rect, 8., palette.soft);
                    }
                    if hit.clicked() {
                        dive = Some(group.source.clone());
                    }
                    ui.painter().galley(
                        rect.left_center() - vec2(0., galley.size().y / 2.),
                        galley,
                        palette.ink,
                    );
                    ui.painter().text(
                        pos2(rect.right() - size * 0.5, rect.center().y),
                        Align2::CENTER_CENTER,
                        "›",
                        font,
                        palette.muted,
                    );
                    let gap = 12.;
                    let cell_w = 120_f32.min(ui.available_width());
                    let cover_h = cell_w * 1.5;
                    // Same cell recipe as the results grid: 2-line title plus
                    // a one-line meta row under the cover.
                    let cell_h = cover_h
                        + 6.
                        + ui.text_style_height(&TextStyle::Body) * 2.
                        + 2.
                        + ui.text_style_height(&TextStyle::Small)
                        + 6.;
                    ScrollArea::horizontal()
                        .id_salt(("global-row", g))
                        .max_height(cell_h + 16.)
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                for (i, manga) in entries.iter().enumerate() {
                                    let (cell, _) = ui.allocate_exact_size(
                                        vec2(cell_w, cell_h),
                                        Sense::hover(),
                                    );
                                    self.global_cell(
                                        ui,
                                        g,
                                        i,
                                        manga,
                                        cell,
                                        Rect::from_min_size(
                                            cell.min,
                                            vec2(cell_w, cover_h),
                                        ),
                                        &mut want,
                                        &mut open,
                                    );
                                    ui.add_space(gap);
                                }
                            });
                        });
                    ui.add_space(6.);
                }
                GlobalState::Searching => {
                    ui.add_space(6.);
                    ui.label(format!("{name} · 搜索中…"));
                }
                GlobalState::Failed(reason) => {
                    let (label, verify) = Self::global_error_label(reason);
                    ui.add_space(6.);
                    ui.horizontal_wrapped(|ui| {
                        ui.label(RichText::new(format!("{name} · {label}")).color(palette.muted));
                        if verify && ui.add(quiet_action("网页验证")).clicked() {
                            self.open_web(&group.source.clone(), None);
                        }
                        if ui.add(quiet_action("重试")).clicked() {
                            retry = Some(group.source.clone());
                        }
                    });
                    // Raw error as a single muted, ellipsised line.
                    let mut job = egui::text::LayoutJob::simple(
                        reason.clone(),
                        TextStyle::Small.resolve(ui.style()),
                        palette.muted,
                        ui.available_width(),
                    );
                    job.wrap.max_rows = 1;
                    ui.add(Label::new(job).selectable(false));
                }
            }
        }
        self.plan_covers(want);
        if let Some(manga) = open {
            self.open_global_manga(manga);
        }
        if let Some(source) = dive {
            self.dive_into(&source);
        }
        if let Some(source) = retry {
            self.retry_global(&source);
        }
    }
    /// How long a success notice stays on the status line before it fades.
    pub(super) const STATUS_TTL: std::time::Duration = std::time::Duration::from_secs(4);
    /// Errors stay until the next action replaces them; everything else is
    /// a transient success notice that fades after `STATUS_TTL`.
    pub(super)fn status_is_error(text: &str) -> bool {
        [
            "失败", "错误", "无法", "未安装", "已丢失", "已停止", "损坏", "过期", "被拒绝",
            "超时", "请先", "暂不可", "不支持", "不正确",
        ]
        .iter()
        .any(|mark| text.contains(mark))
    }
    /// True while the status line should render. Tracks when the current
    /// string was first observed and schedules the repaint that fades it.
    pub(super)fn status_fresh(&mut self, ctx: &Context) -> bool {
        if self.status.is_empty() {
            self.status_seen = None;
            return false;
        }
        let now = std::time::Instant::now();
        if self.status_seen.as_ref().map(|(seen, _)| seen) != Some(&self.status) {
            self.status_seen = Some((self.status.clone(), now));
        }
        if Self::status_is_error(&self.status) {
            return true;
        }
        let elapsed = self
            .status_seen
            .as_ref()
            .map(|(_, seen)| now.duration_since(*seen))
            .unwrap_or_default();
        if elapsed < Self::STATUS_TTL {
            ctx.request_repaint_after(Self::STATUS_TTL - elapsed);
            true
        } else {
            false
        }
    }
    /// Leaving the sources tab drops transient success notices.
    pub(crate) fn clear_transient_status(&mut self) {
        if !Self::status_is_error(&self.status) {
            self.status.clear();
            self.status_seen = None;
        }
    }
    /// A status string set by acceptance previews must not leak into the
    /// next capture.
    #[cfg(debug_assertions)]
    pub(crate) fn clear_status(&mut self) {
        self.status.clear();
        self.status_seen = None;
    }
    pub fn ui(&mut self, ui: &mut Ui) {
        self.hitboxes.clear();
        self.poll();
        if let Some(origin) = self.challenge.clone() {
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("书源被 Cloudflare 拦截，需要网页验证").strong());
                if ui
                    .add(design::primary("网页验证", ui.visuals().dark_mode))
                    .clicked()
                {
                    self.open_web(&origin, None);
                }
                if ui.add(quiet_action("忽略")).clicked() {
                    self.challenge = None;
                }
            });
            ui.add_space(6.);
        }
        if self.settings_open {
            self.settings_page(ui);
            return;
        }
        if self.filter_editor.is_some() {
            self.filter_page(ui);
            return;
        }
        if !self.selected_source.is_empty() && self.selected_manga.is_none() {
            self.browser_header(ui);
            if self.filter_editor.is_some() {
                return;
            }
        }
        // The status footer is painted below the viewport — reserve its
        // height so the scroll area uses the full remaining height without
        // clipping the last content line mid-glyph.
        // "正在加载…" is only honest while a request is in flight; a stale
        // copy left by a finished reply must not render a phantom footer.
        let status_visible = self.status_fresh(ui.ctx())
            && (self.busy || self.status != "正在加载…");

        let footer_height =
            if self.busy || self.failed.is_some() || status_visible {
                40.
            } else {
                0.
            };
        let scroll_height = (ui.available_height() - footer_height).max(40.);
        ScrollArea::vertical()
            .auto_shrink([false, false])
            .max_height(scroll_height)
            .id_salt((
                "source-content",
                &self.selected_source,
                self.results.revision,
                self.selected_manga.as_ref().map(|manga| &manga.key),
            ))
            .show(ui, |ui| {
                // Never let one long row stretch the page beyond the viewport; rows

                // truncate or wrap inside this width instead. The floating
                // scroll bar overlays the content — reserve a right gutter.

                ui.set_max_width(
                    ui.available_width() - ui.spacing().scroll.bar_width - 8.,
                );
                if self.selected_manga.is_none() {
                    let unfinished: Vec<DownloadTask> = self
                        .tasks
                        .iter()
                        .filter(|task| task.status != DownloadStatus::Done)
                        .cloned()
                        .collect();
                    if !unfinished.is_empty() {
                        disclosure(
                            ui,
                            &format!("下载队列（{}）", unfinished.len()),
                            self.selected_source.is_empty(),
                            |ui| {
                                for task in &unfinished {
                                    self.download_row(ui, task);
                                }
                            },
                        );
                        ui.add_space(8.);
                    }
                    if self.selected_source.is_empty() {
                        self.follows_ui(ui);
                        self.repository_ui(ui);
                    }
                    if self.global_mode()
                        && !self.global_groups.is_empty()
                        && !self.query.trim().is_empty()
                    {
                        self.global_results_ui(ui);
                    } else {
                    if !self.results.entries.is_empty() {
                        let label = self
                            .listing
                            .as_ref()
                            .and_then(|id| self.listings.iter().find(|listing| listing.id == *id))
                            .map(|listing| listing.name.clone())
                            .unwrap_or_else(|| "搜索结果".into());
                        design::section(ui, &format!("{label} · {}", self.results.entries.len()));
                    }
                    self.results_grid(ui);
                    if self.results.has_more {
                        let next = self.results.page + 1;
                        ui.add_space(8.);
                        if ui
                            .add_enabled(
                                !self.busy,
                                Button::new(format!("加载下一页（第 {next} 页）"))
                                    .frame_when_inactive(false)
                                    .wrap()
                                    .min_size(vec2(ui.available_width(), 48.)),
                            )
                            .clicked()
                        {
                            self.load_next_page();
                        }
                    }
                    }
                } else {
                    self.detail_ui(ui);
                }
                if self.selected_manga.is_none() {
                    let pending: Vec<(String, String, String)> = self
                        .tasks
                        .iter()
                        .filter(|task| {
                            task.status == DownloadStatus::Done
                                && !task.linked
                                && task.cbz.as_ref().is_some_and(|path| path.exists())
                        })
                        .map(|task| {
                            (
                                task.id.clone(),
                                task.manga_title.clone(),
                                task.chapter_title.clone(),
                            )
                        })
                        .collect();
                    if !pending.is_empty() {
                        ui.add_space(12.);
                        ui.heading("待加入书架");
                        for (id, manga, chapter) in pending {
                            ui.add(Label::new(format!("{manga} - {chapter}")).wrap());
                            if self.import_attempts.contains(&id) {
                                ui.add(Label::new("正在加入书架…"));
                            } else if ui.add(quiet_action("加入书架 / 重试导入")).clicked()
                            {
                                self.requeue_import(&id);
                            }
                            ui.separator();
                        }
                    }
                }
            });
        if self.busy || self.failed.is_some() || status_visible {
            ui.add_space(6.);
            ui.horizontal(|ui| {
                if self.busy {
                    ui.spinner();
                }
                if status_visible {
                    ui.add(Label::new(&self.status).wrap());
                }
                if let Some(task) = self.failed.clone()
                    && ui.add(quiet_action("重试")).clicked()
                {
                    self.submit(task);
                }
            });
        }
        self.history_confirm_ui(ui.ctx());
        self.uninstall_confirm_ui(ui.ctx());
        self.migrate_ui(ui.ctx());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sources::test_support::*;
    use crate::sources::ui_detail::{format_epoch_date, chapter_state_lines};

    #[test]
    fn replies_from_another_source_are_rejected() {
        let mut results = ResultsState::default();
        results.begin("source.a", "dragon");
        assert!(results.accept("source.a", "dragon", 1, vec![manga("", "1")], true));
        assert_eq!(results.entries[0].source_key, "source.a");
        assert!(!results.accept("source.b", "dragon", 1, vec![manga("source.b", "9")], true));
        assert_eq!(results.entries.len(), 1);
        assert_eq!(results.entries[0].key, "1");
    }
    #[test]
    fn switching_source_drops_results_detail_and_chapters() {
        let mut results = ResultsState::default();
        results.begin("source.a", "dragon");
        assert!(results.accept("source.a", "dragon", 1, vec![manga("source.a", "1")], true));
        let mut selected = String::from("source.a");
        let mut detail = Some(manga("source.a", "1"));
        let mut chapters = vec![Chapter {
            key: "chapter-1".into(),
            ..Default::default()
        }];
        assert!(apply_source_change(
            &mut selected,
            &mut results,
            &mut detail,
            &mut chapters,
            "source.b"
        ));
        assert_eq!(selected, "source.b");
        assert!(results.entries.is_empty());
        assert!(results.next_page().is_none());
        assert!(detail.is_none());
        assert!(chapters.is_empty());
        results.begin("source.b", "one piece");
        assert!(results.accept(
            "source.b",
            "one piece",
            1,
            vec![manga("source.b", "7")],
            false
        ));
        assert!(!apply_source_change(
            &mut selected,
            &mut results,
            &mut detail,
            &mut chapters,
            "source.b"
        ));
        assert_eq!(results.entries.len(), 1);
    }
    #[test]
    fn failed_page_is_retried_without_advancing() {
        let mut results = ResultsState::default();
        results.begin("source.a", "dragon");
        assert!(results.accept("source.a", "dragon", 1, vec![manga("source.a", "1")], true));
        assert_eq!(results.next_page().map(|(_, _, page)| page), Some(2));
        assert_eq!(results.page, 1);
        assert_eq!(results.next_page().map(|(_, _, page)| page), Some(2));
        assert!(results.accept("source.a", "dragon", 2, vec![], false));
        assert_eq!(results.page, 2);
        assert!(results.next_page().is_none());
    }
    #[test]
    fn duplicate_keys_across_pages_are_merged_once() {
        let mut results = ResultsState::default();
        results.begin("source.a", "dragon");
        assert!(results.accept(
            "source.a",
            "dragon",
            1,
            vec![manga("source.a", "1"), manga("source.a", "2")],
            true
        ));
        assert!(results.accept(
            "source.a",
            "dragon",
            2,
            vec![manga("source.a", "2"), manga("source.a", "3")],
            false
        ));
        let keys: Vec<_> = results.entries.iter().map(|m| m.key.as_str()).collect();
        assert_eq!(keys, vec!["1", "2", "3"]);
        assert!(!results.accept("source.a", "dragon", 2, vec![manga("source.a", "4")], true));
        assert_eq!(results.page, 2);
    }
    #[test]
    fn changing_filters_rejects_old_success_and_error_even_with_identical_query() {
        let dir = tempfile::tempdir().unwrap();
        let mut sources = test_sources(dir.path());
        let (tx, rx) = mpsc::channel();
        sources.tx = tx;
        let (reply_tx, reply_rx) = mpsc::channel();
        sources.rx = reply_rx;
        sources.selected_source = "fixture".into();
        sources.query = "same".into();
        sources.start_search();
        let (old, _) = rx.recv().unwrap();
        sources.applied_filters = vec![FilterValue::Check {
            id: "completed".into(),
            value: 1,
        }];
        sources.start_search();
        let (new, task) = rx.recv().unwrap();
        assert!(
            matches!(task,Task::Search{filters,..} if matches!(&filters[..],[FilterValue::Check{value:1,..}]))
        );
        reply_tx
            .send((
                old,
                Ok(Reply::Results {
                    source: "fixture".into(),
                    query: "same".into(),
                    page: 1,
                    entries: vec![manga("fixture", "old")],
                    has_next: true,
                }),
            ))
            .unwrap();
        reply_tx.send((old, Err("old failed".into()))).unwrap();
        sources.poll();
        assert!(sources.busy && sources.results.entries.is_empty());
        assert!(!sources.status.contains("old failed"));
        reply_tx
            .send((
                new,
                Ok(Reply::Results {
                    source: "fixture".into(),
                    query: "same".into(),
                    page: 1,
                    entries: vec![manga("fixture", "new")],
                    has_next: true,
                }),
            ))
            .unwrap();
        sources.poll();
        assert!(!sources.busy);
        assert_eq!(sources.results.entries[0].key, "new");
    }
    #[test]
    fn next_page_keeps_committed_filters_and_query_then_listing_starts_over() {
        let dir = tempfile::tempdir().unwrap();
        let mut sources = test_sources(dir.path());
        let (tx, rx) = mpsc::channel();
        sources.tx = tx;
        sources.selected_source = "fixture".into();
        sources.query = "submitted".into();
        sources.applied_filters = vec![FilterValue::Select {
            id: "language".into(),
            value: "zh".into(),
        }];
        sources.start_search();
        rx.recv().unwrap();
        assert!(sources.results.accept(
            "fixture",
            "submitted",
            1,
            vec![manga("fixture", "1")],
            true
        ));
        sources.query = "not submitted".into();
        sources.load_next_page();
        let (
            _,
            Task::Search {
                query,
                page,
                filters,
                ..
            },
        ) = rx.recv().unwrap()
        else {
            panic!("expected search")
        };
        assert_eq!(query, "submitted");
        assert_eq!(page, 2);
        assert_eq!(filters, sources.applied_filters);
        sources.start_listing("latest");
        let (_, Task::Browse { listing, page, .. }) = rx.recv().unwrap() else {
            panic!("expected browse")
        };
        assert_eq!(listing, "latest");
        assert_eq!(page, 1);
        assert!(sources.results.entries.is_empty() && sources.query.is_empty());
    }
    #[test]
    fn source_that_hides_filters_uses_only_keyword_until_it_is_cleared() {
        let dir = tempfile::tempdir().unwrap();
        let mut sources = test_sources(dir.path());
        let (tx, rx) = mpsc::channel();
        sources.tx = tx;
        let mut manifest = source_manifest();
        manifest.id = "fixture".into();
        manifest.hides_filters_while_searching = true;
        sources.installed.push(InstalledSource {
            manifest,
            directory: dir.path().into(),
            wasm_path: dir.path().join("main.wasm"),
            settings_path: None,
            filters_path: None,
            icon_path: None,
        });
        sources.selected_source = "fixture".into();
        sources.applied_filters = vec![FilterValue::Select {
            id: "language".into(),
            value: "zh".into(),
        }];
        sources.query = "keyword".into();
        sources.start_search();
        assert!(matches!(rx.recv().unwrap().1,Task::Search{filters,..} if filters.is_empty()));
        assert_eq!(sources.applied_filters.len(), 1);
        sources.query.clear();
        sources.start_search();
        assert!(matches!(rx.recv().unwrap().1,Task::Search{filters,..} if filters.len()==1));
    }
    #[test]
    fn filter_drafts_cancel_without_mutating_search_and_source_sessions_are_separate() {
        let dir = tempfile::tempdir().unwrap();
        let mut sources = test_sources(dir.path());
        let (tx, _rx) = mpsc::channel();
        sources.tx = tx;
        sources.selected_source = "a".into();
        sources.query = "one".into();
        sources.applied_filters = vec![FilterValue::Check {
            id: "done".into(),
            value: 1,
        }];
        sources.filter_editor = Some(FilterEditor::new(&[], &[]));
        sources.filter_editor = None;
        assert_eq!(sources.applied_filters.len(), 1);
        sources.select_source("b");
        assert!(sources.applied_filters.is_empty() && sources.query.is_empty());
        sources.select_source("a");
        assert_eq!(sources.query, "one");
        assert!(matches!(
            &sources.applied_filters[..],
            [FilterValue::Check { value: 1, .. }]
        ));
    }
    #[test]
    fn failed_first_page_remains_retryable_and_late_detail_cannot_reopen_old_source() {
        let dir = tempfile::tempdir().unwrap();
        let mut sources = test_sources(dir.path());
        let (tx, rx) = mpsc::channel();
        sources.tx = tx;
        let (reply_tx, reply_rx) = mpsc::channel();
        sources.rx = reply_rx;
        sources.selected_source = "a".into();
        sources.start_search();
        let (id, _) = rx.recv().unwrap();
        reply_tx.send((id, Err("HTTP 503".into()))).unwrap();
        sources.poll();
        assert_eq!(sources.results.page, 0);
        assert!(sources.failed.is_some());
        sources.submit(sources.failed.clone().unwrap());
        let (retry, _) = rx.recv().unwrap();
        assert_ne!(id, retry);
        sources.select_source("b");
        reply_tx
            .send((
                retry,
                Ok(Reply::Chapters("a".into(), manga("a", "old"), vec![])),
            ))
            .unwrap();
        sources.poll();
        assert!(sources.selected_manga.is_none() && sources.busy);
    }
    #[test]
    fn chapter_search_and_order_keep_decimal_chapters_and_volume_groups() {
        let chapter = |key: &str, number: Option<f32>, volume: Option<f32>| Chapter {
            key: key.into(),
            title: Some(format!("第 {key} 话")),
            chapter_number: number,
            volume_number: volume,
            ..Default::default()
        };
        let chapters = vec![
            chapter("10", Some(10.), None),
            chapter("2.5", Some(2.5), None),
            chapter("2", Some(2.), Some(1.)),
            chapter("番外", None, None),
            chapter("第二卷", Some(1.), Some(2.)),
        ];
        let ordered = chapter_list(&chapters, "", false);
        assert_eq!(
            ordered
                .iter()
                .map(|chapter| chapter.key.as_str())
                .collect::<Vec<_>>(),
            ["2", "2.5", "10", "番外", "第二卷"],
        );
        assert_eq!(chapter_list(&chapters, "", true)[0].key, "第二卷");
        assert_eq!(chapter_list(&chapters, "2.5", false)[0].key, "2.5");
        assert_eq!(chapter_list(&chapters, "番外", false)[0].key, "番外");
        assert!(chapter_list(&chapters, "不存在", false).is_empty());
    }
    #[test]
    fn result_grid_columns_never_underflow_or_overflow() {
        let expect = [
            (320., 2),
            (507., 4),
            (744., 4),
            (834., 5),
            (1024., 6),
            (1366., 8),
        ];
        for (width, cols) in expect {
            assert_eq!(grid_columns(width), cols, "width {width}");
        }
        // Even at 200% type the math is font-independent: one column minimum

        // and every cell stays at least its minimum width inside the row.

        for width in [200., 320., 507.] {
            let cols = grid_columns(width);
            assert!(cols >= 1);
            let min_cell = if width >= 600. { 150. } else { 112. };
            let cell_w = (width - 16. * cols.saturating_sub(1) as f32) / cols as f32;
            assert!(cell_w >= min_cell, "width {width} cell {cell_w}");
        }
    }
    #[test]
    fn format_epoch_date_prints_civil_dates() {
        assert_eq!(format_epoch_date(1_690_000_000), "2023-07-22");
        assert_eq!(format_epoch_date(0), "1970-01-01");
        assert_eq!(format_epoch_date(-86_400), "1969-12-31");
    }
    #[test]
    fn chapter_state_lines_map_progress_and_queue() {
        // Locked wins outright; finished progress beats partial reads; download

        // state appears below the reading state.

        assert_eq!(
            chapter_state_lines(true, false, false, None, None),
            vec!["暂不可阅读"]
        );
        assert_eq!(
            chapter_state_lines(false, true, false, Some((9, 10)), None),
            vec!["已读", "已下载"]
        );
        assert_eq!(
            chapter_state_lines(false, false, false, Some((2, 10)), None),
            vec!["读到 3/10"]
        );
        let running = queue_state(DownloadStatus::Running, None, None);
        let mut partial = running.clone();
        partial.done = 4;
        partial.total = 30;
        assert_eq!(
            chapter_state_lines(false, false, false, None, Some(&partial)),
            vec!["下载中 4/30"]
        );
        let failed = queue_state(DownloadStatus::Failed, None, None);
        assert_eq!(
            chapter_state_lines(false, false, false, None, Some(&failed)),
            vec!["失败 · 重试"]
        );
        let done = queue_state(DownloadStatus::Done, Some(4), Some(9));
        assert_eq!(
            chapter_state_lines(false, false, true, None, Some(&done)),
            vec!["读到 5/10", "待加入书架"]
        );
    }
    #[test]
    fn resume_chapter_picks_the_freshest_and_steps_past_finished() {
        let root = tempfile::tempdir().unwrap();
        let mut sources = test_sources(root.path());
        let book = manga("source.a", "m");
        let ordered = vec![
            detail_chapter("c1", 1.),
            detail_chapter("c2", 2.),
            detail_chapter("c3", 3.),
        ];
        let mut states = std::collections::HashMap::new();
        states.insert(
            "c2".into(),
            queue_state(DownloadStatus::Done, Some(9), Some(200)),
        );
        sources
            .chapter_progress
            .insert(identity_id("source.a", "m", "c1"), (2, 10, Some(100)));
        // The stream read of c2 is newer but finished, so resume lands on c3.

        let (resume, started) = sources.resume_chapter("source.a", &book, &ordered, &states);
        assert!(started && resume.unwrap().key == "c3");
        // A newer partial library read wins over the finished stream.

        sources
            .chapter_progress
            .insert(identity_id("source.a", "m", "c1"), (2, 10, Some(300)));
        let (resume, started) = sources.resume_chapter("source.a", &book, &ordered, &states);
        assert!(started && resume.unwrap().key == "c1");
        // Finishing the last chapter keeps it as the resume target.

        sources.chapter_progress.clear();
        states.insert(
            "c3".into(),
            queue_state(DownloadStatus::Done, Some(9), Some(400)),
        );
        states.remove("c2");
        let (resume, started) = sources.resume_chapter("source.a", &book, &ordered, &states);
        assert!(started && resume.unwrap().key == "c3");
        // Nothing read starts at the first unlocked chapter.

        let (resume, started) =
            sources.resume_chapter("source.a", &book, &ordered, &Default::default());
        assert!(!started && resume.unwrap().key == "c1");
    }
    #[test]
    fn resume_chapter_skips_locked_when_starting_fresh() {
        let root = tempfile::tempdir().unwrap();
        let sources = test_sources(root.path());
        let book = manga("source.a", "m");
        let mut locked = detail_chapter("c0", 0.);
        locked.locked = true;
        let ordered = vec![locked, detail_chapter("c1", 1.)];
        let (resume, started) =
            sources.resume_chapter("source.a", &book, &ordered, &Default::default());
        assert!(!started && resume.unwrap().key == "c1");
    }
    #[test]
    fn search_scope_persists_and_reverts_to_current() {
        let root = tempfile::tempdir().unwrap();
        let mut sources = test_sources(root.path());
        assert_eq!(sources.search_scope, "current");
        sources.choose_scope("global");
        assert_eq!(sources.take_scope_change().as_deref(), Some("global"));
        assert!(sources.take_scope_change().is_none(), "change is consumed once");
        sources.choose_scope("current");
        assert_eq!(sources.take_scope_change().as_deref(), Some("current"));
        sources.set_search_scope("global");
        assert!(sources.global_mode());
        sources.set_search_scope("junk");
        assert!(!sources.global_mode(), "unknown values fall back to current");
    }
    #[test]
    fn search_history_dedupes_and_caps_at_twenty() {
        let root = tempfile::tempdir().unwrap();
        let mut sources = test_sources(root.path());
        sources.query = "  星海  ".into();
        sources.record_history();
        sources.query = "猎人".into();
        sources.record_history();
        sources.query = "星海".into();
        sources.record_history();
        assert_eq!(
            sources.search_history(),
            &["星海".to_string(), "猎人".to_string()]
        );
        for index in 0..25 {
            sources.query = format!("关键词{index}");
            sources.record_history();
        }
        assert_eq!(sources.search_history().len(), 20);
        assert_eq!(sources.search_history()[0], "关键词24");
        // The most recent submit marks history dirty exactly once per drain.
        assert!(sources.take_history_change().is_some());
        assert!(sources.take_history_change().is_none());
    }
    #[test]
    fn private_reading_records_no_search_history() {
        let root = tempfile::tempdir().unwrap();
        let mut sources = test_sources(root.path());
        sources.set_private(true);
        sources.query = "星海".into();
        sources.record_history();
        assert!(sources.search_history().is_empty());
        assert!(sources.take_history_change().is_none());
        sources.set_private(false);
        sources.record_history();
        assert_eq!(sources.search_history(), &["星海".to_string()]);
    }
}
