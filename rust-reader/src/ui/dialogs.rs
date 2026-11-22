






use crate::worker::Command;




use egui::*;








use super::*;


impl ReaderApp {
    pub(super) fn dialogs(&mut self, ctx: &Context) {
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
                        .inner_margin(18)
                        .corner_radius(20),
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
                                    let text_x =
                                        rect.center().x + icon_w / 2. - galley.size().x / 2.;
                                    if show_icon {
                                        crate::design::draw_icon(
                                            ui.painter(),
                                            pos2(text_x - 12., rect.center().y),
                                            icon,
                                            label_color,
                                            selected,
                                        );
                                    }
                                    ui.painter().galley(
                                        pos2(text_x, rect.center().y - galley.size().y / 2.),
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
                            ui.add_space(10.);
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
                                ui.painter().rect_filled(track, 13., track_color);
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
                            let (bright_rect, _) =
                                ui.allocate_exact_size(vec2(avail_w, 46.), Sense::hover());
                            ui.painter().rect_filled(bright_rect, 12., p.soft);
                            ui.painter().rect_stroke(
                                bright_rect,
                                12.,
                                Stroke::new(1., p.line),
                                StrokeKind::Inside,
                            );
                            crate::design::draw_icon(
                                ui.painter(),
                                pos2(bright_rect.left() + 20., bright_rect.center().y),
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
                                    bright_rect.right() - 14. - readout_text.size().x,
                                    bright_rect.center().y - readout_text.size().y / 2.,
                                ),
                                readout_text.clone(),
                                p.muted,
                            );
                            let slider_zone = Rect::from_min_max(
                                pos2(bright_rect.left() + 78., bright_rect.top() + 1.),
                                pos2(
                                    (bright_rect.right() - 22. - readout_text.size().x)
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
                                            self.bookmark_jump(
                                                mark.clone(),
                                                &id,
                                                ctx.input(|i| i.time),
                                                ctx.content_rect().width(),
                                            );
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
