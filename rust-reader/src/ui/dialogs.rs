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
use super::history_page::*;
use super::categories::*;
use super::test_support::*;


impl ReaderApp {
    pub(super) fn dialogs(&mut self, ctx: &Context) {
        if self.reader_options {
            let response = Modal::new(Id::unique("reader-options"))
                .frame(
                    Frame::popup(&ctx.style_of(Theme::Dark))
                        .fill(crate::design::palette(true).surface)
                        .inner_margin(18)
                        .corner_radius(20),
                )
                .show(ctx, |ui| {
                    ui.set_width((ctx.content_rect().width() - 56.).clamp(200., 440.));
                    ui.set_max_width((ctx.content_rect().width() - 56.).clamp(200., 440.));
                    ui.style_mut().visuals = crate::design::visuals(true);
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
                            self.save_reader_prefs();
                        }
                        for (name, hitbox, on) in [
                            ("裁剪白边", "reader-crop", self.crop),
                            ("适应宽度", "reader-fit", self.fit_width),
                            ("双页首页单独", "reader-spread-offset", self.spread_offset),
                        ] {
                            let toggle = ui.add_sized(
                                [ui.available_width(), 48.],
                                Button::new(format!(
                                    "{name}：{}",
                                    if on { "开" } else { "关" }
                                ))
                                .wrap(),
                            );
                            self.hitboxes.insert(hitbox.into(), toggle.rect);
                            if toggle.clicked() {
                                match hitbox {
                                    "reader-crop" => self.crop = !self.crop,
                                    "reader-fit" => self.fit_width = !self.fit_width,
                                    _ => self.spread_offset = !self.spread_offset,
                                }
                                self.save_reader_prefs();
                            }
                        }
                        // `Slider` places its rail, drag value and text in a
                        // horizontal row that is laid out at its desired width
                        // even inside `add_sized`, so the rail must be shrunk
                        // to the space that actually remains or the whole modal
                        // grows past the viewport at large font scales.
                        let brightness = ui.scope_builder(
                            UiBuilder::new()
                                .max_rect(Rect::from_min_size(
                                    ui.cursor().min,
                                    vec2(ui.available_width(), 48.),
                                ))
                                .layout(Layout::left_to_right(Align::Center)),
                            |ui| {
                                ui.label("亮度");
                                // The default rail is transparent on the dark
                                // dialog surface; give it a visible neutral
                                // and accent the filled portion.
                                let palette = crate::design::palette(true);
                                let visuals = ui.visuals_mut();
                                visuals.widgets.inactive.weak_bg_fill = palette.soft;
                                visuals.widgets.inactive.bg_fill = palette.accent;
                                ui.spacing_mut().slider_width =
                                    (ui.available_width() - 120.).max(40.);
                                ui.add(
                                    Slider::new(&mut self.brightness, 0.3..=1.)
                                        .fixed_decimals(2)
                                        .trailing_fill(true),
                                )
                            },
                        );
                        self.hitboxes
                            .insert("reader-brightness".into(), brightness.response.rect);
                        if brightness.inner.changed() {
                            self.brightness = self.brightness.clamp(0.3, 1.);
                            self.save_reader_prefs();
                        }
                        let defaults = ui.add_sized(
                            [ui.available_width(), 48.],
                            Button::new("设为所有作品的默认").wrap(),
                        );
                        self.hitboxes
                            .insert("reader-save-default".into(), defaults.rect);
                        if defaults.clicked() {
                            self.save_reader_globals();
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
            // Keep only selections that still resolve to downloaded chapters.
            let ids: std::collections::HashSet<String> =
                chapters.iter().map(|book| book.id.clone()).collect();
            self.chapter_selected.retain(|id| ids.contains(id));
            if chapters.is_empty() {
                self.chapter_manage = false;
            }
            let response = Modal::new(Id::unique("chapter-picker")).show(ctx, |ui| {
                ui.set_width((ctx.content_rect().width() - 56.).clamp(200., 460.));
                ui.horizontal(|ui| {
                    ui.heading(title);
                    ui.with_layout(
                        egui::Layout::right_to_left(egui::Align::Center),
                        |ui| {
                            let manage =
                                ui.button(if self.chapter_manage { "完成" } else { "管理" });
                            self.hitboxes
                                .insert("chapter-manage".into(), manage.rect);
                            if manage.clicked() {
                                self.chapter_manage = !self.chapter_manage;
                                if !self.chapter_manage {
                                    self.chapter_selected.clear();
                                }
                            }
                        },
                    );
                });
                let palette = crate::design::palette(self.dark);
                ui.horizontal(|ui| {
                    for (label, flag) in [("章节", false), ("书签", true)] {
                        let selected = self.dialog_bookmarks == flag;
                        let tab = Button::new(
                            RichText::new(label).color(if selected {
                                palette.ink
                            } else {
                                palette.muted
                            }),
                        )
                        .fill(if selected {
                            palette.soft
                        } else {
                            palette.surface
                        });
                        let tab = ui.add(tab);
                        self.hitboxes
                            .insert(format!("dialog-tab-{}", label), tab.rect);
                        if tab.clicked() {
                            self.dialog_bookmarks = flag;
                        }
                    }
                });
                if self.dialog_bookmarks {
                    let meta = self.reading_metadata(&id);
                    let work = crate::bookmarks::work_key(&id, meta.as_ref());
                    let marks: Vec<crate::bookmarks::Bookmark> = self
                        .bookmarks
                        .for_work(&work)
                        .into_iter()
                        .cloned()
                        .collect();
                    if self.private {
                        ui.label("私密阅读时书签不可用。");
                    } else if marks.is_empty() {
                        ui.label(RichText::new("还没有书签。顶栏的书签按钮收藏当前页。").small());
                    }
                    ScrollArea::vertical().max_height(300.).show(ui, |ui| {
                        for (index, mark) in marks.iter().enumerate() {
                            ui.horizontal(|ui| {
                                ui.add(
                                    Label::new(format!(
                                        "{} · 第 {} 页",
                                        mark.title,
                                        mark.page + 1
                                    ))
                                    .wrap(),
                                );
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        if ui.button("删除").clicked() {
                                            self.bookmarks.remove(&work, index);
                                            if let Err(error) =
                                                self.bookmarks.save(&self.root)
                                            {
                                                self.error =
                                                    Some(format!("书签保存失败：{error}"));
                                            }
                                        }
                                        let jump = ui.button("跳转");
                                        self.hitboxes.insert(
                                            format!("bookmark-jump-{index}"),
                                            jump.rect,
                                        );
                                        if jump.clicked() {
                                            self.bookmark_jump(mark.clone(), &id);
                                        }
                                    },
                                );
                            });
                            ui.add_space(4.);
                        }
                    });
                }
                if !self.dialog_bookmarks {
                ScrollArea::vertical().max_height(380.).show(ui, |ui| {
                    for book in &chapters {
                        let name = self
                            .chapter(&book.id)
                            .map(|m| m.chapter_title.clone())
                            .unwrap_or_else(|| book.title.clone());
                        let done = book.last_read_at.is_some()
                            && book.current_page + 1 >= book.page_count;
                        if self.chapter_manage {
                            let mut checked = self.chapter_selected.contains(&book.id);
                            let label = format!("{}{}", name, if done { " · 已读完" } else { "" });
                            let response = ui.checkbox(&mut checked, label);
                            self.hitboxes
                                .insert(format!("chapter-check-{}", book.id), response.rect);
                            if response.changed() {
                                if checked {
                                    self.chapter_selected.insert(book.id.clone());
                                } else {
                                    self.chapter_selected.remove(&book.id);
                                }
                            }
                        } else {
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
                                self.open_reader(book);
                            }
                        }
                    }
                });
                if self.chapter_manage {
                    let finished: Vec<String> = chapters
                        .iter()
                        .filter(|book| {
                            book.last_read_at.is_some()
                                && book.current_page + 1 >= book.page_count
                        })
                        .map(|book| book.id.clone())
                        .collect();
                    ui.horizontal(|ui| {
                        let delete_selected = ui.add_enabled(
                            !self.chapter_selected.is_empty(),
                            Button::new("删除所选"),
                        );
                        self.hitboxes
                            .insert("chapter-delete".into(), delete_selected.rect);
                        if delete_selected.clicked() {
                            self.storage_confirm = Some(StorageConfirm::Chapters(
                                self.chapter_selected.iter().cloned().collect(),
                            ));
                        }
                        let delete_done = ui.add_enabled(
                            !finished.is_empty(),
                            Button::new("删除已读完的章节"),
                        );
                        self.hitboxes
                            .insert("chapter-delete-done".into(), delete_done.rect);
                        if delete_done.clicked() {
                            self.storage_confirm =
                                Some(StorageConfirm::Chapters(finished));
                        }
                    });
                    ui.add_space(6.);
                }
                }
                if ui.button("关闭").clicked() {
                    self.chapter_dialog = None;
                    self.chapter_manage = false;
                    self.chapter_selected.clear();
                    self.dialog_bookmarks = false;
                }
            });
            if response.should_close() {
                self.chapter_dialog = None;
                self.chapter_manage = false;
                self.chapter_selected.clear();
                self.dialog_bookmarks = false;
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
                    let online: Vec<String> = self
                        .selected
                        .iter()
                        .filter(|id| Self::online_parts(id).is_some())
                        .cloned()
                        .collect();
                    let local = self.selected.len() - online.len();
                    ui.label(if online.is_empty() {
                        if self.remove_files {
                            format!("将 {} 本书移出书架并删除文件。阅读进度保留。", self.selected.len())
                        } else {
                            format!("将 {} 本书移出书架。原漫画文件保留。", self.selected.len())
                        }
                    } else if local == 0 {
                        format!(
                            "将 {} 部作品移出书架。会取消追更，已下载的章节不受影响。",
                            online.len()
                        )
                    } else {
                        "在线作品会取消追更；本地章节的文件处理见下方选项。".into()
                    });
                    if local > 0 {
                        let checkbox = ui.checkbox(&mut self.remove_files, "同时删除文件");
                        self.hitboxes
                            .insert("remove-files".into(), checkbox.rect);
                    }
                    ui.horizontal(|ui| {
                        if ui.button("取消").clicked() {
                            self.confirm_remove = false;
                        }
                        if ui.button("移出").clicked() {
                            let local_ids = self.selected_books();
                            if self.remove_files {
                                self.send(Command::PurgeBooks(local_ids));
                            } else {
                                self.send(Command::Remove(local_ids));
                            }
                            for id in online {
                                if let Some((source, manga)) = Self::online_parts(&id) {
                                    self.sources.unfollow(source, manga);
                                }
                            }
                            self.selected.clear();
                            self.confirm_remove = false;
                            self.remove_files = true;
                        }
                    });
                });
        }
        self.storage_confirm_ui(ctx);
        self.history_clear_ui(ctx);
        self.category_panels_ui(ctx);
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
}
/// A destructive storage cleanup waiting on the 存储 confirm dialog.
#[derive(Clone)]
pub(super) enum StorageConfirm {
    /// Delete files of all removed books; rows + progress stay.
    RemovedBooks,
    /// Delete finished/failed/orphaned download staging.
    Staging,
    /// Delete the source cover cache (re-downloads on demand).
    Covers,
    /// Delete these downloaded chapter books (files + rows kept progress).
    Chapters(Vec<String>),
    /// Delete a category; its books move back to 未分类.
    Category(String),
}
