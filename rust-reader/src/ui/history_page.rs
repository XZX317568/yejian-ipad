use crate::{
    follows,
    motion::{self, ShelfMotion},
    shelf::{self, Plane},
    sources::{STREAM_PREFIX, StreamChapter},
    worker::{Command, Event, PAGE_NOT_READY, Worker},
};
use chrono::Datelike;
use egui::*;
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    path::PathBuf,
    sync::atomic::Ordering,
};
use yejian_core::{Book, ChapterLink, ChapterMetadata};
use super::*;
use super::shelf_page::*;
use super::reader_page::*;
use super::settings_page::*;
use super::categories::*;
use super::dialogs::*;
use super::test_support::*;


impl ReaderApp {
    pub(super) fn history_ui(&mut self, ui: &mut Ui, area: Rect) {
        let palette = crate::design::palette(self.dark);
        let column = Self::page_column(area, 112. + 20. * (self.font_scale - 1.), 88.);
        let now = crate::follows::now() as i64;

        // Unified rows: local books carry last_read_at (millis), online
        // works carry the download task's read_at (secs).
        struct Row {
            key: String,
            title: String,
            detail: String,
            progress: f32,
            at: i64,
            book: Option<Book>,
            online: Option<(String, String)>,
        }
        let mut rows: Vec<Row> = Vec::new();
        for book in &self.books {
            let Some(at) = book.last_read_at else {
                continue;
            };
            rows.push(Row {
                key: book.id.clone(),
                title: self.reading_title(book),
                detail: format!("第 {} / {} 页", book.current_page + 1, book.page_count),
                progress: (book.current_page + 1) as f32 / book.page_count.max(1) as f32,
                at: at / 1000,
                book: Some(book.clone()),
                online: None,
            });
        }
        for read in self.sources.reading_history() {
            let chapter = read
                .chapter_number
                .filter(|n| n.is_finite() && *n >= 1.)
                .map(|n| format!("第 {n} 话"))
                .unwrap_or_else(|| read.chapter_title.clone());
            rows.push(Row {
                key: format!("{}\u{1f}{}", read.source_id, read.manga_id),
                title: read.manga_title.clone(),
                detail: format!("在线 · {chapter} · 第 {} 页", read.read_page + 1),
                progress: if read.pages_total > 0 {
                    (read.read_page + 1) as f32 / read.pages_total as f32
                } else {
                    0.
                },
                at: read.read_at,
                book: None,
                online: Some((read.source_id, read.manga_id)),
            });
        }
        // Hidden rows reappear when the work is read again after hidden_at.
        rows.retain(|row| Self::history_row_visible(&self.history_hidden, &row.key, row.at));
        let needle = self.history_query.trim().to_lowercase();
        if !needle.is_empty() {
            rows.retain(|row| row.title.to_lowercase().contains(&needle));
        }
        rows.sort_by_key(|row| std::cmp::Reverse(row.at));

        // 今天 / 昨天 / 本周 / 更早 by local date (same ISO week = 本周).
        let today = chrono::Local::now().date_naive();
        let mut grouped: [Vec<Row>; 4] = [Vec::new(), Vec::new(), Vec::new(), Vec::new()];
        for row in rows {
            grouped[Self::history_group(row.at, today)].push(row);
        }
        let any_rows = grouped.iter().any(|rows| !rows.is_empty());

        let introduction = ui.painter().layout(
            if any_rows {
                "本地和网络，最近读过的故事都在这里。"
            } else {
                "下一次打开，从离开的那一页继续。"
            }
            .into(),
            FontId::proportional(14. * self.font_scale),
            palette.muted,
            column.width(),
        );
        let mut body = column;
        body.min.y = area.top() + 82. + introduction.size().y + 20.;
        ui.painter()
            .galley(pos2(column.left(), area.top() + 82.), introduction, palette.muted);

        ui.scope_builder(UiBuilder::new().max_rect(body), |ui| {
            ScrollArea::vertical()
                .id_salt("reading-history")
                .show(ui, |ui| {
                    // egui hands the inner content an unconstrained max
                    // width here; clamp every child to the column so 320pt
                    // splits never paint off-screen. The floating scroll
                    // bar overlays the content, so reserve a right gutter.
                    ui.set_max_width(
                        column.width() - ui.spacing().scroll.bar_width - 8.,
                    );
                    // Stats card: 今天/本周 minutes, streak, month chapters,
                    // painted 7-day bar chart.
                    crate::design::surface(ui).show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.add_space(12.);
                        crate::design::section(ui, "阅读统计");
                        let epoch = crate::follows::now() as f64;
                        let cells = [
                            ("今天", format!("{} 分钟", self.stats.today_seconds(epoch) / 60)),
                            ("本周", format!("{} 分钟", self.stats.week_seconds(epoch) / 60)),
                            ("连续阅读", format!("{} 天", self.stats.streak(epoch))),
                            ("本月读完", format!("{} 章", self.stats.month_chapters(epoch))),
                        ];
                        // Four equal columns: they never overflow the card,
                        // even at 200 percent on a 320pt split.
                        ui.columns(cells.len(), |cols| {
                            for (col, (label, value)) in
                                cols.iter_mut().zip(cells.into_iter())
                            {
                                col.vertical(|ui| {
                                    ui.label(
                                        RichText::new(value)
                                            .strong()
                                            .size(17. * self.font_scale),
                                    );
                                    ui.label(
                                        RichText::new(label).small().color(palette.muted),
                                    );
                                });
                            }
                        });
                        ui.add_space(10.);
                        // 7-day bars, oldest left, accent fill.
                        let seven = self.stats.last_seven(epoch);
                        let max = seven.iter().map(|(_, s)| *s).max().unwrap_or(0).max(1) as f32;
                        let width = ui.available_width();
                        let gap = 8.;
                        let bar_w = ((width - gap * 6.) / 7.).max(6.);
                        let chart_h = 56. * self.font_scale.min(1.4);
                        let (rect, _) = ui.allocate_exact_size(
                            vec2(width, chart_h + 18. * self.font_scale),
                            Sense::hover(),
                        );
                        for (index, (day, secs)) in seven.iter().enumerate() {
                            let x = rect.left() + index as f32 * (bar_w + gap);
                            let h =
                                (chart_h * (*secs as f32 / max)).max(if *secs > 0 { 3. } else { 0. });
                            let bar = Rect::from_min_size(
                                pos2(x, rect.top() + chart_h - h),
                                vec2(bar_w, h.max(2.)),
                            );
                            ui.painter().rect_filled(
                                bar,
                                4.,
                                if h > 0. { palette.accent } else { palette.line },
                            );
                            let weekday = chrono::NaiveDate::parse_from_str(day, "%Y-%m-%d")
                                .map(|d| d.weekday().num_days_from_monday() as usize)
                                .unwrap_or(0);
                            let ch = ['一', '二', '三', '四', '五', '六', '日'][weekday];
                            ui.painter().text(
                                pos2(x + bar_w / 2., rect.bottom() - 6. * self.font_scale),
                                Align2::CENTER_CENTER,
                                ch.to_string(),
                                FontId::proportional(10. * self.font_scale),
                                palette.muted,
                            );
                        }
                        ui.add_space(8.);
                    });
                    ui.add_space(14.);

                    // Button first in right-to-left layout so the search
                    // field gets exactly what is left — no overflow at 200%.
                    // The outer horizontal row pins the height to the
                    // content; a bare with_layout would allocate the whole
                    // remaining scroll height.
                    ui.horizontal(|ui| {
                        ui.with_layout(
                            Layout::right_to_left(Align::Center),
                            |ui| {
                                let clear =
                                    ui.add_enabled(any_rows, Button::new("清空足迹"));
                                self.hitboxes.insert("history-clear".into(), clear.rect);
                                if clear.clicked() {
                                    self.history_clear = true;
                                }
                                let search = ui.add(
                                    TextEdit::singleline(&mut self.history_query)
                                        .hint_text("搜索标题")
                                        .desired_width(ui.available_width().max(60.)),
                                );
                                self.hitboxes.insert("history-search".into(), search.rect);
                            },
                        );
                    });
                    ui.add_space(10.);

                    if !any_rows {
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

                    for (index, rows) in grouped.into_iter().enumerate() {
                        if rows.is_empty() {
                            continue;
                        }
                        crate::design::section(ui, ["今天", "昨天", "本周", "更早"][index]);
                        for row in rows {
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
                            // The row continues reading; the action zone
                            // (移除/继续) is left to its widgets. Narrow rows
                            // stack the buttons at the bottom, so the open
                            // area is everything above them.
                            let button_y = if narrow {
                                rect.bottom() - 58.
                            } else {
                                rect.center().y - 24.
                            };
                            let open_rect = if narrow {
                                Rect::from_min_max(
                                    rect.min,
                                    pos2(rect.right(), button_y - 4.),
                                )
                            } else {
                                Rect::from_min_max(
                                    rect.min,
                                    pos2(rect.right() - 180., rect.max.y),
                                )
                            };
                            let open = ui.interact(
                                open_rect,
                                ui.scope_id().with(("history-row", &row.key)),
                                Sense::click(),
                            );
                            self.hitboxes
                                .insert(format!("history-{}", row.key), rect);
                            ui.painter().rect_filled(rect, 18., palette.surface);
                            let cover_rect =
                                Rect::from_min_size(rect.min + vec2(14., 14.), vec2(58., 82.));
                            let mut covered = false;
                            if let Some(book) = &row.book {
                                let cover_id = self.cover_id(&book.id);
                                self.ask_cover(ui.ctx(), &cover_id);
                                if let Some(texture) = self.covers.get(&cover_id) {
                                    covered = true;
                                    shelf::paint_art(
                                        ui.painter(),
                                        Plane::rect(cover_rect),
                                        cover_rect.size(),
                                        texture,
                                        palette.soft,
                                    );
                                }
                            }
                            if !covered {
                                ui.painter().rect_filled(cover_rect, 7., palette.soft);
                                if row.online.is_some() {
                                    ui.painter().text(
                                        cover_rect.center(),
                                        Align2::CENTER_CENTER,
                                        "在线",
                                        FontId::proportional(12. * self.font_scale),
                                        palette.muted,
                                    );
                                }
                            }
                            let text_pos = rect.min + vec2(88., 16.);
                            let mut title = egui::text::LayoutJob::simple(
                                row.title.clone(),
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
                                &row.detail,
                                13.,
                                palette.muted,
                            );
                            let track = Rect::from_min_size(
                                pos2(text_pos.x, cover_rect.bottom() - 5.),
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
                                        vec2(track.width() * row.progress.clamp(0., 1.), 3.),
                                    ),
                                    2.,
                                    palette.accent,
                                );
                            }
                            let continue_clicked = self.button(
                                ui,
                                &format!("history-go-{}", row.key),
                                Rect::from_min_size(
                                    pos2(rect.right() - 104., button_y),
                                    vec2(88., 48.),
                                ),
                                "继续",
                                true,
                            ) || open.clicked();
                            if continue_clicked {
                                if let Some(book) = &row.book {
                                    let book = book.clone();
                                    self.open_reader(&book);
                                } else if let Some((source, manga)) = &row.online {
                                    let (source, manga) = (source.clone(), manga.clone());
                                    if self.sources.open_history_work(&source, &manga) {
                                        self.selected.clear();
                                        self.tab = Tab::Sources;
                                        self.screen = Screen::Library;
                                    }
                                }
                            }
                            // 从足迹移除 hides the row; progress is untouched
                            // and a later read reappears it. A quiet button
                            // in a fixed slot left of 继续.
                            let hide_rect = Rect::from_min_size(
                                pos2(rect.right() - 172., button_y),
                                vec2(60., 48.),
                            );
                            if self.button(
                                ui,
                                &format!("history-hide-{}", row.key),
                                hide_rect,
                                "移除",
                                false,
                            ) {
                                self.history_hidden.insert(row.key.clone(), now);
                                self.save_history_hidden();
                            }
                        }
                    }
                });
        });
    }

    /// A hidden footprint row reappears once it is read again after
    /// `hidden_at` (epoch seconds).
    pub(super) fn history_row_visible(
        hidden: &BTreeMap<String, i64>,
        key: &str,
        at: i64,
    ) -> bool {
        hidden.get(key).is_none_or(|hidden_at| at > *hidden_at)
    }

    /// 0=今天 1=昨天 2=本周(same ISO week) 3=更早 for a local-day group.
    pub(super) fn history_group(at: i64, today: chrono::NaiveDate) -> usize {
        let Some(date) = chrono::DateTime::from_timestamp(at, 0)
            .map(|stamp| stamp.with_timezone(&chrono::Local).date_naive())
        else {
            return 3;
        };
        if date == today {
            0
        } else if date == today - chrono::Duration::days(1) {
            1
        } else if date.iso_week() == today.iso_week() {
            2
        } else {
            3
        }
    }

    /// The snapshot sent to the cloud worker — progress is filled there
    /// (only the worker thread holds the library store).
    pub(super) fn save_history_hidden(&mut self) {
        let entries: Vec<serde_json::Value> = self
            .history_hidden
            .iter()
            .map(|(key, at)| serde_json::json!({"key": key, "hidden_at": at}))
            .collect();
        self.setting(
            "history-hidden",
            serde_json::to_string(&entries).unwrap_or_else(|_| "[]".into()),
        );
    }

    /// Opaque 清空足迹 confirm, same recipe as the storage modals.
    pub(super) fn history_clear_ui(&mut self, ctx: &Context) {
        if !self.history_clear {
            return;
        }
        let screen = ctx.content_rect();
        let scrim = Area::new(Id::unique("footprint-clear-scrim"))
            .order(Order::Foreground)
            .fade_in(false)
            .interactable(true)
            .fixed_pos(screen.min)
            .show(ctx, |ui| {
                ui.painter()
                    .rect_filled(screen, 0., Color32::from_black_alpha(96));
                ui.allocate_rect(screen, Sense::click())
            });
        if scrim.inner.clicked() {
            self.history_clear = false;
            return;
        }
        let mut card_rect = Rect::NOTHING;
        Area::new(Id::unique("footprint-clear"))
            .order(Order::Foreground)
            .fade_in(false)
            .interactable(true)
            .movable(false)
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .show(ctx, |ui| {
                let frame = crate::design::surface(ui).show(ui, |ui| {
                    ui.set_width((screen.width() - 56.).clamp(220., 420.));
                    ui.heading("清空阅读足迹");
                    ui.label("从足迹移除全部记录，阅读进度不受影响，再次阅读会重新出现。");
                    ui.add_space(6.);
                    ui.horizontal(|ui| {
                        if ui.button("取消").clicked() {
                            self.history_clear = false;
                        }
                        let confirm = ui.add(crate::design::danger("清空", self.dark));
                        self.hitboxes
                            .insert("footprint-clear-go".into(), confirm.rect);
                        if confirm.clicked() {
                            // Hide everything currently visible at this
                            // moment; re-reading surfaces the work again.
                            let now = crate::follows::now() as i64;
                            for book in &self.books {
                                if book.last_read_at.is_some() {
                                    self.history_hidden.insert(book.id.clone(), now);
                                }
                            }
                            for read in self.sources.reading_history() {
                                self.history_hidden.insert(
                                    format!("{}\u{1f}{}", read.source_id, read.manga_id),
                                    now,
                                );
                            }
                            self.save_history_hidden();
                            self.history_clear = false;
                        }
                    });
                });
                card_rect = frame.response.rect;
            });
        self.hitboxes
            .insert("footprint-clear-card".into(), card_rect);
    }
}
