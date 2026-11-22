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
use super::history_page::*;
use super::categories::*;
use super::dialogs::*;
use super::test_support::*;


impl ReaderApp {
    pub(super) fn cloud_snapshot(&self) -> crate::cloud::Snapshot {
        use crate::cloud::{Entry, Snapshot};
        let now = follows::now() as i64;
        let mut snapshot = Snapshot::default();
        snapshot.online = self
            .sources
            .online_read_state()
            .into_iter()
            .map(|state| {
                (
                    crate::cloud::online_entry_key(
                        &state.source_id,
                        &state.manga_id,
                        &state.chapter_id,
                    ),
                    Entry::new(
                        state.read_at,
                        serde_json::json!({
                            "read_page": state.read_page,
                            "total": state.total,
                            "read_at": state.read_at,
                        }),
                    ),
                )
            })
            .collect();
        snapshot.follows = self
            .sources
            .follows()
            .iter()
            .map(|follow| {
                (
                    crate::cloud::follow_entry_key(&follow.source_id, &follow.manga_id),
                    Entry::new(
                        follow.checked_at.unwrap_or(0) as i64,
                        serde_json::to_value(follow).unwrap_or_default(),
                    ),
                )
            })
            .collect();
        snapshot.bookmarks = self
            .bookmarks
            .entries()
            .iter()
            .map(|bookmark| {
                (
                    crate::cloud::bookmark_entry_key(
                        &bookmark.work,
                        &bookmark.chapter,
                        bookmark.page,
                    ),
                    Entry::new(
                        bookmark.created_at as i64,
                        serde_json::to_value(bookmark).unwrap_or_default(),
                    ),
                )
            })
            .collect();
        snapshot.categories.insert(
            "categories".into(),
            Entry::new(now, serde_json::json!(self.user_categories)),
        );
        snapshot.reader_prefs = self
            .reader_prefs
            .iter()
            .map(|(key, prefs)| {
                (
                    key.clone(),
                    Entry::new(now, serde_json::to_value(prefs).unwrap_or_default()),
                )
            })
            .collect();
        for (day, devices) in self.stats.export_days() {
            for (device, stats) in devices {
                snapshot.stats.insert(
                    crate::cloud::stats_entry_key(device, day),
                    Entry::new(now, serde_json::to_value(stats).unwrap_or_default()),
                );
            }
        }
        snapshot.history_hidden = self
            .history_hidden
            .iter()
            .map(|(key, at)| {
                (
                    key.clone(),
                    Entry::new(*at, serde_json::json!({ "hidden_at": at })),
                )
            })
            .collect();
        let repo_urls = self.sources.repo_urls();
        snapshot.sources = self
            .sources
            .installed_source_meta()
            .into_iter()
            .map(|(id, version)| {
                (
                    id,
                    Entry::new(
                        now,
                        serde_json::json!({
                            "version": version,
                            "repos": repo_urls,
                        }),
                    ),
                )
            })
            .collect();
        for (key, value) in [
            ("theme", serde_json::json!(self.theme)),
            ("palette", serde_json::json!(self.color_scheme)),
            ("font-scale", serde_json::json!(self.font_scale)),
            (
                "reading-direction",
                serde_json::json!(if self.rtl { "rtl" } else { "ltr" }),
            ),
        ] {
            snapshot
                .appearance
                .insert(key.into(), Entry::new(self.appearance_ts, value));
        }
        snapshot
    }

    /// `app-state.json` bytes for the backup zip — the same Snapshot the
    /// cloud sync carries (progress is merged in by the worker).
    pub(super) fn app_state_json(&self) -> Vec<u8> {
        serde_json::to_vec(&self.cloud_snapshot()).unwrap_or_else(|_| b"{}".to_vec())
    }

    /// Stamps appearance changes so a remote winner can outdate them.
    pub(super) fn mark_appearance_changed(&mut self) {
        self.appearance_ts = follows::now() as i64;
        self.setting("appearance-updated", self.appearance_ts.to_string());
    }

    /// Applies the merged groups of a finished cloud sync (or a restored
    /// backup's app-state.json) to their UI-side owners. Returns how many
    /// records moved — used for the completion notice.
    pub(super) fn apply_cloud_snapshot(&mut self, merged: &crate::cloud::Snapshot) -> usize {
        let mut applied = 0usize;
        // 在线阅读状态: skip the work currently open in the reader so a
        // live position is never yanked mid-session.
        let skip = if let Screen::Reader(id) = &self.screen {
            self.reading_metadata(id)
                .map(|metadata| (metadata.source_id, metadata.manga_id))
        } else {
            None
        };
        let states: Vec<crate::sources::OnlineReadState> = merged
            .online
            .iter()
            .filter_map(|(key, entry)| {
                let value = entry.value.as_object()?;
                let parts: Vec<&str> = key.split('\u{1f}').collect();
                if parts.len() != 3 {
                    return None;
                }
                Some(crate::sources::OnlineReadState {
                    source_id: parts[0].into(),
                    manga_id: parts[1].into(),
                    chapter_id: parts[2].into(),
                    read_page: value.get("read_page")?.as_u64()? as usize,
                    total: value.get("total")?.as_u64()? as usize,
                    read_at: value.get("read_at")?.as_i64()?,
                })
            })
            .collect();
        applied += self
            .sources
            .apply_online_state(&states, skip.as_ref().map(|(s, m)| (s.as_str(), m.as_str())));

        // 追更: the merged map is authoritative — replaces the local list.
        if !merged.follows.is_empty() {
            let follows: Vec<follows::Follow> = merged
                .follows
                .values()
                .filter_map(|entry| serde_json::from_value(entry.value.clone()).ok())
                .collect();
            if follows.len() == merged.follows.len() {
                self.sources.apply_synced_follows(follows);
                applied += merged.follows.len();
            }
        }

        // 书签
        let bookmarks: Vec<crate::bookmarks::Bookmark> = merged
            .bookmarks
            .values()
            .filter_map(|entry| serde_json::from_value(entry.value.clone()).ok())
            .collect();
        if self.bookmarks.entries().len() != bookmarks.len()
            || !merged.bookmarks.is_empty()
        {
            if self
                .bookmarks
                .replace_all(&self.root, bookmarks)
                .is_ok()
            {
                applied += merged.bookmarks.len();
            }
        }

        // 分类
        if let Some(entry) = merged.categories.get("categories") {
            if let Ok(categories) =
                serde_json::from_value::<Vec<String>>(entry.value.clone())
            {
                if categories != self.user_categories {
                    self.user_categories = categories;
                    applied += 1;
                }
            }
        }

        // 每部作品阅读设置
        let prefs: HashMap<String, crate::reader_prefs::ReaderPrefs> = merged
            .reader_prefs
            .iter()
            .filter_map(|(key, entry)| {
                serde_json::from_value::<crate::reader_prefs::ReaderPrefs>(
                    entry.value.clone(),
                )
                .ok()
                .map(|prefs| (key.clone(), prefs))
            })
            .collect();
        if prefs != self.reader_prefs {
            self.reader_prefs = prefs;
            let _ = crate::reader_prefs::save(&self.root, &self.reader_prefs);
            applied += 1;
        }

        // 阅读统计: per (device, day) max-merge.
        let mut days: BTreeMap<String, BTreeMap<String, crate::stats::DayStats>> =
            BTreeMap::new();
        for (key, entry) in &merged.stats {
            let parts: Vec<&str> = key.split('\u{1f}').collect();
            if parts.len() != 2 {
                continue;
            }
            if let Ok(stats) =
                serde_json::from_value::<crate::stats::DayStats>(entry.value.clone())
            {
                days.entry(parts[1].into())
                    .or_default()
                    .insert(parts[0].into(), stats);
            }
        }
        applied += self.stats.merge_remote(&days);

        // 隐藏的足迹行
        let hidden: BTreeMap<String, i64> = merged
            .history_hidden
            .iter()
            .filter_map(|(key, entry)| {
                entry
                    .value
                    .get("hidden_at")?
                    .as_i64()
                    .map(|at| (key.clone(), at))
            })
            .collect();
        if hidden != self.history_hidden {
            self.history_hidden = hidden;
            self.save_history_hidden();
            applied += 1;
        }

        // 外观
        for (key, value) in &merged.appearance {
            match key.as_str() {
                "theme" => {
                    if let Some(theme) = value.value.as_str() {
                        if theme != self.theme {
                            self.theme = theme.into();
                            self.setting("theme", self.theme.clone());
                            applied += 1;
                        }
                    }
                }
                "palette" => {
                    if let Some(scheme) = value.value.as_str() {
                        if scheme != self.color_scheme {
                            self.color_scheme = scheme.into();
                            self.setting("palette", self.color_scheme.clone());
                            applied += 1;
                        }
                    }
                }
                "font-scale" => {
                    if let Some(scale) = value.value.as_f64() {
                        let scale = (scale as f32).clamp(1., 2.);
                        if (scale - self.font_scale).abs() > f32::EPSILON {
                            self.font_scale = scale;
                            self.setting("font-scale", scale.to_string());
                            applied += 1;
                        }
                    }
                }
                "reading-direction" => {
                    if let Some(direction) = value.value.as_str() {
                        let rtl = direction == "rtl";
                        if rtl != self.rtl {
                            self.rtl = rtl;
                            self.reader_globals.rtl = rtl;
                            self.setting(
                                "reading-direction",
                                if rtl { "rtl" } else { "ltr" }.into(),
                            );
                            applied += 1;
                        }
                    }
                }
                _ => {}
            }
        }
        if applied > 0 {
            self.appearance_ts = follows::now() as i64;
        }
        applied
    }

    /// `history-hidden` entries [{key, hidden_at}] through the settings store.
    pub(super) fn settings_ui(&mut self, ui: &mut Ui, area: Rect) {
        if !self.storage_opened {
            self.storage_opened = true;
            self.send(Command::StorageReport);
            self.sources.request_storage();
        }
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
                                        self.mark_appearance_changed();
                                    }
                                }
                            });
                            ui.add_space(18.);
                            ui.label(
                                RichText::new("配色").color(palette.muted).small(),
                            );
                            let scheme_width = if ui.available_width() < 330. {
                                ((ui.available_width() - 10.) / 2.).floor() - 1.
                            } else {
                                ((ui.available_width() - 20.) / 3.).floor() - 1.
                            };
                            ui.horizontal_wrapped(|ui| {
                                for scheme in crate::design::Scheme::ALL {
                                    if crate::design::scheme_choice(
                                        ui,
                                        scheme,
                                        self.color_scheme == scheme.key(),
                                        scheme_width,
                                    ) {
                                        self.color_scheme = scheme.key().into();
                                        self.setting(
                                            "palette",
                                            self.color_scheme.clone(),
                                        );
                                        self.mark_appearance_changed();
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
                                self.mark_appearance_changed();
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
                                self.reader_globals.rtl = self.rtl;
                                self.setting(
                                    "reading-direction",
                                    if self.rtl { "rtl" } else { "ltr" }.into(),
                                );
                                self.mark_appearance_changed();
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
                                self.sources.set_private(self.private);
                            }
                            ui.separator();
                            ui.add_space(6.);
                            if crate::design::toggle(
                                ui,
                                &mut self.keep_awake_setting,
                                "阅读时屏幕常亮",
                                "打开书籍时不让屏幕自动熄灭",
                            ) {
                                self.setting(
                                    "keep-awake",
                                    self.keep_awake_setting.to_string(),
                                );
                                if !self.keep_awake_setting {
                                    self.keep_awake(false);
                                } else if matches!(self.screen, Screen::Reader(_)) {
                                    self.keep_awake(true);
                                }
                            }
                            ui.separator();
                            ui.add_space(6.);
                            if crate::design::toggle(
                                ui,
                                &mut self.reader_status,
                                "显示时间和电量",
                                "阅读时右下角显示时间与电量",
                            ) {
                                self.setting(
                                    "reader-status",
                                    self.reader_status.to_string(),
                                );
                            }
                            ui.separator();
                            ui.add_space(6.);
                            crate::design::section(ui, "点击翻页");
                            let zone_width = if ui.available_width() < 370. {
                                ((ui.available_width() - 10.) / 2.).floor() - 1.
                            } else {
                                ((ui.available_width() - 20.) / 3.).floor() - 1.
                            };
                            let mut zones = self.tap_zones.clone();
                            ui.horizontal_wrapped(|ui| {
                                for (key, name) in [
                                    ("lr", "左右翻页"),
                                    ("next", "两侧都是下一页"),
                                    ("off", "关闭点击翻页"),
                                ] {
                                    if ui
                                        .add(
                                            Button::new(name)
                                                .selected(zones == key)
                                                .min_size(vec2(zone_width, 48.))
                                                .wrap(),
                                        )
                                        .clicked()
                                    {
                                        zones = key.into();
                                    }
                                }
                            });
                            if zones != self.tap_zones {
                                self.tap_zones = zones.clone();
                                self.setting("tap-zones", zones);
                            }
                        });
                    ui.add_space(10.);
                    ui.separator();
                    Frame::new()
                        .inner_margin(Margin::symmetric(2, 16))
                        .show(ui, |ui| {
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
                            ui.add_space(10.);
                            ui.label(
                                RichText::new("自动下载新章节（会使用移动网络）")
                                    .color(palette.muted)
                                    .small(),
                            );
                            let current = self.sources.auto_download().to_string();
                            let mut mode = current.clone();
                            let width = if ui.available_width() < 340. {
                                ((ui.available_width() - 10.) / 2.).floor() - 1.
                            } else {
                                ((ui.available_width() - 30.) / 4.).floor() - 1.
                            };
                            ui.horizontal_wrapped(|ui| {
                                for (value, name) in [
                                    ("off", "关闭"),
                                    ("all", "新章节全部"),
                                    ("latest1", "只下最新 1 章"),
                                    ("latest3", "只下最新 3 章"),
                                ] {
                                    let button = ui.add(
                                        Button::new(name)
                                            .selected(mode == value)
                                            .min_size(vec2(width, 44.))
                                            .wrap(),
                                    );
                                    self.hitboxes.insert(
                                        format!("auto-download-{value}"),
                                        button.rect,
                                    );
                                    if button.clicked() {
                                        mode = value.to_string();
                                    }
                                }
                            });
                            if mode != current {
                                self.sources.set_auto_download(&mode);
                                self.setting("auto-download", mode);
                            }
                            ui.add_space(10.);
                            let mut notify = self.notify_enabled;
                            if crate::design::toggle(
                                ui,
                                &mut notify,
                                "新章节通知",
                                "追更发现新章节时发送系统通知；iOS 后台刷新由系统调度，LiveContainer 安装时可能不会运行",
                            ) {
                                if notify {
                                    self.notify_enabled = true;
                                    self.setting("notify-updates", "true".into());
                                    self.notify_permission_pending = true;
                                    let _ = yejian_platform::request_notification_permission();
                                } else {
                                    self.notify_enabled = false;
                                    self.notify_permission_pending = false;
                                    self.setting("notify-updates", "false".into());
                                }
                            }
                        });
                    ui.add_space(10.);
                    ui.separator();
                    Frame::new()
                        .inner_margin(Margin::symmetric(2, 16))
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            crate::design::section(ui, "进度跟踪（AniList / Kitsu / Bangumi）");
                            let mut sync = self.trackers.auto_sync();
                            if crate::design::toggle(
                                ui,
                                &mut sync,
                                "自动同步章节进度",
                                "读完一章就把远端进度推进到已读（私密阅读时不上传）",
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
                                crate::trackers::TrackerKind::Bangumi,
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
                    if yejian_platform::supports_app_lock() {
                        ui.add_space(10.);
                        ui.separator();
                        Frame::new()
                            .inner_margin(Margin::symmetric(2, 16))
                            .show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                crate::design::section(ui, "隐私");
                                let mut on = self.applock.enabled();
                                if crate::design::toggle(
                                    ui,
                                    &mut on,
                                    "应用锁（面容 ID / 触控 ID / 设备密码）",
                                    "回到应用时验证身份后再显示内容",
                                ) && self.applock.request_toggle(on)
                                {
                                    self.start_auth("验证以更改应用锁");
                                }
                                if self.applock.enabled() {
                                    ui.separator();
                                    ui.add_space(6.);
                                    ui.label(
                                        RichText::new("离开多久后重新锁定")
                                            .color(palette.muted)
                                            .small(),
                                    );
                                    let width = ((ui.available_width() - 20.) / 3.).floor() - 1.;
                                    let mut delay = self.applock.delay_secs();
                                    ui.horizontal_wrapped(|ui| {
                                        for (secs, name) in
                                            crate::applock::DELAY_CHOICES
                                        {
                                            if ui
                                                .add(
                                                    Button::new(*name)
                                                        .selected(delay == *secs)
                                                        .min_size(vec2(width, 44.))
                                                        .wrap(),
                                                )
                                                .clicked()
                                            {
                                                delay = *secs;
                                            }
                                        }
                                    });
                                    if delay != self.applock.delay_secs() {
                                        self.applock.set_delay(delay);
                                        self.setting(
                                            "app-lock-delay",
                                            delay.to_string(),
                                        );
                                    }
                                }
                                #[cfg(target_os = "android")]
                                ui.label(
                                    RichText::new("Android 的最近任务预览不会被遮挡")
                                        .color(palette.muted)
                                        .small(),
                                );
                            });
                    }
                    ui.add_space(10.);
                    ui.separator();
                    Frame::new()
                        .inner_margin(Margin::symmetric(2, 16))
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            crate::design::section(ui, "云同步（WebDAV）");
                            let mut enabled = self.cloud.enabled;
                            if crate::design::toggle(
                                ui,
                                &mut enabled,
                                "启用 WebDAV 云同步",
                                "进度、追更、书签、统计等在两台设备间同步；密码只存在本机",
                            ) {
                                self.cloud.enabled = enabled;
                                self.cloud.config.enabled = enabled;
                                self.setting("webdav-enabled", enabled.to_string());
                                if enabled {
                                    self.cloud.sync_now(self.cloud_snapshot());
                                }
                            }
                            ui.add_space(8.);
                            ui.label(
                                RichText::new("服务器地址（如 https://dav.jianguoyun.com/dav/）")
                                    .color(palette.muted)
                                    .small(),
                            );
                            if ui
                                .add(
                                    TextEdit::singleline(&mut self.cloud_url)
                                        .desired_width(f32::INFINITY)
                                        .hint_text("https://dav.example.com/dav/"),
                                )
                                .changed()
                            {
                                self.cloud.config.url = self.cloud_url.clone();
                                self.setting("webdav-url", self.cloud_url.clone());
                            }
                            ui.add_space(6.);
                            ui.horizontal(|ui| {
                                let half = (ui.available_width() - 8.) / 2.;
                                ui.label("用户名");
                                ui.add_space(4.);
                                if ui
                                    .add(
                                        TextEdit::singleline(&mut self.cloud_user)
                                            .desired_width(half),
                                    )
                                    .changed()
                                {
                                    self.cloud.config.user = self.cloud_user.clone();
                                    self.setting("webdav-user", self.cloud_user.clone());
                                }
                            });
                            ui.horizontal(|ui| {
                                ui.label("应用密码");
                                ui.add_space(4.);
                                if ui
                                    .add(
                                        TextEdit::singleline(&mut self.cloud_password)
                                            .password(true)
                                            .desired_width(
                                                (ui.available_width() - 8.) / 2.,
                                            ),
                                    )
                                    .changed()
                                {
                                    self.cloud.config.password =
                                        self.cloud_password.clone();
                                    let _ = crate::cloud::save_password(
                                        &self.root,
                                        &self.cloud_password,
                                    );
                                }
                            });
                            ui.horizontal(|ui| {
                                ui.label("目录");
                                ui.add_space(4.);
                                if ui
                                    .add(
                                        TextEdit::singleline(&mut self.cloud_dir)
                                            .desired_width(140.)
                                            .hint_text("yejian"),
                                    )
                                    .changed()
                                {
                                    self.cloud.config.dir = self.cloud_dir.clone();
                                    self.setting("webdav-dir", self.cloud_dir.clone());
                                }
                            });
                            ui.add_space(8.);
                            ui.horizontal(|ui| {
                                let sync_now = ui.add(
                                    Button::new("立即同步").min_size(vec2(96., 36.)),
                                );
                                self.hitboxes
                                    .insert("webdav-sync-now".into(), sync_now.rect);
                                if sync_now.clicked() {
                                    if let Err(error) =
                                        crate::cloud::validate_url(&self.cloud_url)
                                    {
                                        self.error = Some(error);
                                    } else {
                                        self.cloud.sync_now(self.cloud_snapshot());
                                    }
                                }
                                ui.add_space(8.);
                                ui.label(
                                    RichText::new(self.cloud.status())
                                        .color(palette.muted)
                                        .small(),
                                );
                            });
                        });
                    ui.add_space(10.);
                    ui.separator();
                    Frame::new()
                        .inner_margin(Margin::symmetric(2, 16))
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            crate::design::section(ui, "存储");
                            self.storage_group_ui(ui, palette);
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
                                    self.send(Command::Backup(
                                        self.backup_path.clone().into(),
                                        Some(self.app_state_json()),
                                    ));
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
                                include_str!("../../licenses/Wenku8-NOTICE"),
                                include_str!("../../licenses/Wenku8-GPL-3.0.txt"),
                                include_str!("../../licenses/ReadingFonts.md"),
                                include_str!("../../licenses/ReadingSans-OFL.txt"),
                                include_str!("../../licenses/ReadingSerif-OFL.txt")
                            ));
                        }
                    });
                });
        });
    }
    /// The 存储 settings group: per-bucket sizes plus confirmed cleanups.
    pub(super) fn storage_group_ui(&mut self, ui: &mut Ui, palette: crate::design::Palette) {
        let library = self.storage.clone();
        let sources = self.sources.storage().cloned();
        let total = library.as_ref().map(|r| r.total_bytes()).unwrap_or(0)
            + sources.as_ref().map(|r| r.total_bytes()).unwrap_or(0);
        ui.label(
            RichText::new(format!("共 {}", Self::format_bytes(total))).strong(),
        );
        ui.add_space(8.);
        let size = |bytes: Option<u64>| match bytes {
            Some(bytes) => Self::format_bytes(bytes),
            None => "统计中…".into(),
        };
        let mut row = |ui: &mut Ui, label: &str, bytes: Option<u64>, hitbox: Option<&str>| {
            ui.horizontal(|ui| {
                ui.label(label);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(RichText::new(size(bytes)).color(palette.muted));
                    if let Some(key) = hitbox {
                        let button = ui.add(Button::new("清理").min_size(vec2(64., 36.)));
                        self.hitboxes.insert(key.into(), button.rect);
                        if button.clicked() {
                            self.storage_confirm = Some(match key {
                                "storage-removed-clean" => StorageConfirm::RemovedBooks,
                                "storage-staging-clean" => StorageConfirm::Staging,
                                _ => StorageConfirm::Covers,
                            });
                        }
                    }
                });
            });
        };
        row(ui, "书架漫画", library.as_ref().map(|r| r.shelf_bytes), None);
        row(
            ui,
            "已移出的书",
            library.as_ref().map(|r| r.removed_bytes),
            Some("storage-removed-clean"),
        );
        let staging = sources
            .as_ref()
            .map(|r| r.staging_active + r.staging_done);
        row(ui, "下载暂存", staging, Some("storage-staging-clean"));
        row(
            ui,
            "封面缓存",
            sources.as_ref().map(|r| r.covers),
            Some("storage-covers-clean"),
        );
        ui.add_space(4.);
        ui.label(
            RichText::new("清理只删除文件，阅读进度与书架记录都会保留。")
                .small()
                .color(palette.muted),
        );
    }
    /// Both storage workers have answered at least once.
    #[cfg(debug_assertions)]
    pub fn storage_ready(&self) -> bool {
        self.storage.is_some() && self.sources.storage().is_some()
    }
    /// Human size like `1.2 GB`.
    pub(super) fn format_bytes(bytes: u64) -> String {
        const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
        let mut value = bytes as f64;
        let mut unit = 0;
        while value >= 1024. && unit + 1 < UNITS.len() {
            value /= 1024.;
            unit += 1;
        }
        if unit == 0 {
            format!("{bytes} B")
        } else {
            format!("{value:.1} {}", UNITS[unit])
        }
    }
    /// Confirmation dialog for every destructive 存储 / chapter delete.
    pub(super) fn storage_confirm_ui(&mut self, ctx: &Context) {
        let Some(action) = self.storage_confirm.clone() else {
            return;
        };
        let (title, copy) = match &action {
            StorageConfirm::RemovedBooks => (
                "清理已移出的书",
                "删除所有已移出书籍的漫画和封面文件。阅读进度保留，重新导入同一文件即可恢复。"
                    .to_string(),
            ),
            StorageConfirm::Staging => (
                "清理下载暂存",
                "删除已完成或失败任务的暂存文件，进行中的下载不受影响。".to_string(),
            ),
            StorageConfirm::Covers => (
                "清理封面缓存",
                "删除书源封面缓存文件，封面会在需要时重新下载。".to_string(),
            ),
            StorageConfirm::Chapters(ids) => (
                "删除已下载章节",
                format!(
                    "删除 {} 个已下载章节的文件。章节的阅读进度保留，重新下载可恢复。",
                    ids.len()
                ),
            ),
            StorageConfirm::Category(name) => (
                "删除分类",
                format!("分类“{name}”会删除，分类里的漫画会回到未分类。阅读进度保留。"),
            ),
        };
        let screen = ctx.content_rect();
        // Dim everything under the card and swallow the tap so it can only
        // close the dialog — nothing beneath is clickable.
        let scrim = Area::new(Id::unique("storage-confirm-scrim"))
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
            self.storage_confirm = None;
            return;
        }
        // fade_in(false) + surface fill: the card is opaque from the first
        // frame, nothing behind it can bleed through.
        let mut card_rect = Rect::NOTHING;
        Area::new(Id::unique("storage-confirm"))
            .order(Order::Foreground)
            .fade_in(false)
            .interactable(true)
            .movable(false)
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .show(ctx, |ui| {
                let frame = crate::design::surface(ui).show(ui, |ui| {
                    ui.set_width(
                        (screen.width() - 56.).clamp(220., 420.),
                    );
                    ui.heading(title);
                    ui.label(copy);
                    ui.add_space(6.);
                    ui.horizontal(|ui| {
                        if ui.button("取消").clicked() {
                            self.storage_confirm = None;
                        }
                        let confirm =
                            ui.add(crate::design::danger("删除", self.dark));
                        self.hitboxes
                            .insert("storage-confirm-go".into(), confirm.rect);
                        if confirm.clicked() {
                            match action {
                                StorageConfirm::RemovedBooks => {
                                    self.send(Command::PurgeRemoved)
                                }
                                StorageConfirm::Staging => {
                                    self.sources.clean_staging()
                                }
                                StorageConfirm::Covers => {
                                    self.sources.clear_cover_cache()
                                }
                                StorageConfirm::Chapters(ids) => {
                                    self.send(Command::PurgeBooks(ids));
                                    self.chapter_selected.clear();
                                    self.chapter_manage = false;
                                }
                                StorageConfirm::Category(name) => {
                                    self.send(Command::ClearCategory(name.clone()));
                                    self.user_categories.retain(|item| item != &name);
                                    self.save_categories();
                                    if self.category == name {
                                        self.category = String::new();
                                    }
                                }
                            }
                            self.storage_confirm = None;
                        }
                    });
                });
                card_rect = frame.response.rect;
            });
        self.hitboxes
            .insert("storage-confirm-card".into(), card_rect);
    }
    /// Categories shown in the manage panel / pick list: stored user order
    /// plus categories found on books that are not stored yet. Fixed entries
    /// (全部/未分类/在线追更) never appear here.
    pub(super) fn opaque_card<R>(
        &mut self,
        ctx: &Context,
        id: &str,
        open: &mut bool,
        body: impl FnOnce(&mut Self, &mut Ui) -> R,
    ) -> Option<R> {
        let screen = ctx.content_rect();
        let scrim = Area::new(Id::unique(format!("{id}-scrim")))
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
            *open = false;
            return None;
        }
        let mut card_rect = Rect::NOTHING;
        let mut out: Option<R> = None;
        Area::new(Id::unique(id))
            .order(Order::Foreground)
            .fade_in(false)
            .interactable(true)
            .movable(false)
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .show(ctx, |ui| {
                let frame = crate::design::surface(ui).show(ui, |ui| {
                    ui.set_width((screen.width() - 56.).clamp(240., 440.));
                    out = Some(body(self, ui));
                });
                card_rect = frame.response.rect;
            });
        self.hitboxes.insert(format!("{id}-card"), card_rect);
        out
    }
    /// 分类管理 panel and the multi-select 分类 picker.
    pub(super) fn tracker_login_row(&mut self, ui: &mut Ui, tracker: crate::trackers::TrackerKind) {
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
            crate::trackers::TrackerKind::Bangumi => {
                ui.label(
                    RichText::new("在 next.bgm.tv/demo/access-token 生成令牌后粘贴到下面")
                        .color(crate::design::palette(self.dark).muted)
                        .small(),
                );
                ui.add(
                    TextEdit::singleline(&mut self.tracker_bangumi_token)
                        .desired_width(ui.available_width())
                        .password(true)
                        .hint_text("粘贴 Bangumi Access Token"),
                );
                let token = self.tracker_bangumi_token.trim().to_string();
                let button = ui.add_enabled(
                    idle && !token.is_empty(),
                    Button::new("用粘贴的令牌登录").min_size(vec2(0., 44.)),
                );
                self.hitboxes.insert("tracker-bangumi-login".into(), button.rect);
                if button.clicked() {
                    self.trackers
                        .submit(crate::trackers::TrackCommand::BangumiToken(token));
                    self.tracker_bangumi_token.clear();
                }
            }
        }
    }
    pub(super) fn track_panel_ui(&mut self, ui: &mut Ui, key: &str) {
        use crate::trackers::{TrackCommand, TrackStatus, TrackerKind};
        ui.heading("进度跟踪");
        let palette = crate::design::palette(self.dark);
        let bound = self.trackers.binding(key).cloned();
        if let Some(binding) = &bound {
            let tracker = match binding.tracker.as_str() {
                "kitsu" => TrackerKind::Kitsu,
                "bangumi" => TrackerKind::Bangumi,
                _ => TrackerKind::Anilist,
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
            let (status_edit, progress_edit) = match &self.track_draft {
                Some((draft_key, media, status, progress))
                    if draft_key == key && *media == binding.media_id =>
                {
                    (*status, *progress)
                }
                _ => (None, None),
            };
            let mut status = status_edit.unwrap_or(binding.status);
            let mut progress = progress_edit.unwrap_or(binding.progress.max(0));
            let shown_progress = progress;
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
            let status_edit = (status != binding.status).then_some(status);
            let progress_edit = if progress != shown_progress {
                Some(progress)
            } else {
                progress_edit
            }
            .filter(|edited| *edited != binding.progress);
            let changed = status_edit.is_some() || progress_edit.is_some();
            self.track_draft = changed
                .then(|| (key.to_string(), binding.media_id.clone(), status_edit, progress_edit));
            let idle = self.trackers.idle();
            if ui
                .add_enabled(
                    idle && changed,
                    Button::new("保存并同步").min_size(vec2(0., 44.)),
                )
                .clicked()
            {
                self.trackers.submit(TrackCommand::Update {
                    key: key.to_string(),
                    status,
                    progress: progress_edit,
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
                    self.track_draft = None;
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
                for tracker in [TrackerKind::Anilist, TrackerKind::Kitsu, TrackerKind::Bangumi] {
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
                && self.trackers.account(TrackerKind::Bangumi).is_none()
            {
                ui.label(
                    RichText::new("先到 设置 → 进度跟踪 登录 AniList、Kitsu 或 Bangumi")
                        .color(palette.muted)
                        .small(),
                );
            }
            if let Some((tracker, list)) = self.trackers.searches.get(key).cloned() {
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
                                tracker,
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
    pub(super) fn sources_ui(&mut self, ui: &mut Ui, area: Rect) {
        let mut column = Self::page_column(area, 88., 88.);
        let width = (area.width() - 48.).min(1040.);
        column.min.x = area.center().x - width / 2.;
        column.max.x = area.center().x + width / 2.;
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
        // 云同步发现的书源: opaque banner with a one-tap install; the ids
        // resolve through the cached repository index, nothing auto-installs.
        if !self.cloud_missing.is_empty() {
            let palette = crate::design::palette(self.dark);
            let rect = Rect::from_min_size(
                column.min + vec2(16., 12.),
                vec2((column.width() - 32.).max(0.), 48.),
            );
            Area::new(Id::unique("cloud-missing-sources"))
                .order(Order::Foreground)
                .fade_in(false)
                .fixed_pos(rect.min)
                .show(ui.ctx(), |ui| {
                    Frame::new()
                        .fill(palette.surface)
                        .stroke(Stroke::new(1., palette.line))
                        .corner_radius(10.)
                        .inner_margin(Margin::symmetric(12, 6))
                        .show(ui, |ui| {
                            ui.set_width(rect.width() - 24.);
                            ui.horizontal(|ui| {
                                ui.label(format!(
                                    "云端发现 {} 个未安装书源",
                                    self.cloud_missing.len()
                                ));
                                ui.with_layout(
                                    Layout::right_to_left(Align::Center),
                                    |ui| {
                                        let install = ui.add(
                                            Button::new("一键安装")
                                                .min_size(vec2(88., 32.)),
                                        );
                                        self.hitboxes.insert(
                                            "cloud-install".into(),
                                            install.rect,
                                        );
                                        if install.clicked() {
                                            let mut missing =
                                                std::mem::take(&mut self.cloud_missing);
                                            missing.retain(|id| {
                                                !self.sources.install_cloud_source(id)
                                            });
                                            self.cloud_missing = missing;
                                        }
                                        let dismiss = ui.add(
                                            Button::new("忽略").min_size(vec2(56., 32.)),
                                        );
                                        if dismiss.clicked() {
                                            self.cloud_missing.clear();
                                        }
                                    },
                                );
                            });
                        });
                });
        }
        if self.sources.take_shelf_return() {
            self.tab = Tab::Library;
        }
        self.hitboxes
            .extend(self.sources.hitboxes.iter().map(|(id, rect)| (id.clone(), *rect)));
    }
    pub(super) fn pick_import(&mut self) {
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
    pub(super) fn pick_restore(&mut self) {
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
    /// Launch a device-owner authentication; failures to even show the
    /// prompt land as a finished error so the state machine never waits.
    pub(super) fn start_auth(&mut self, reason: &str) {
        if let Err(error) = yejian_platform::authenticate(reason) {
            self.applock.auth_failed_to_start(error.to_string());
        }
    }

    /// Topmost opaque lock screen: paper surface, app name and 解锁. The
    /// full-rect interaction swallows every touch so nothing beneath can be
    /// tapped while locked.
    pub(super) fn lock_ui(&mut self, ui: &mut Ui, full: Rect) {
        if self.applock.take_auto_attempt() {
            self.start_auth("解锁页间");
        }
        let palette = crate::design::palette(self.dark);
        egui::Area::new(Id::unique("app-lock"))
            .order(Order::Foreground)
            .fade_in(false)
            .interactable(true)
            .show(ui.ctx(), |ui| {
                ui.set_clip_rect(full);
                ui.allocate_rect(full, Sense::click());
                ui.painter().rect_filled(full, 0., palette.surface);
                let column = Rect::from_center_size(
                    pos2(full.center().x, full.center().y - 20.),
                    vec2((full.width() - 48.).min(360.), 240.),
                );
                ui.painter().text(
                    pos2(column.center().x, column.top() + 30.),
                    Align2::CENTER_CENTER,
                    "页间",
                    crate::typography::display(34. * self.font_scale.min(1.3)),
                    palette.ink,
                );
                ui.painter().text(
                    pos2(column.center().x, column.top() + 78.),
                    Align2::CENTER_CENTER,
                    "已锁定",
                    FontId::proportional(14. * self.font_scale),
                    palette.muted,
                );
                let row = Rect::from_center_size(
                    pos2(column.center().x, column.top() + 140.),
                    vec2(column.width().min(200.), 48.),
                );
                let button = ui.put(
                    row,
                    crate::design::primary("解锁", self.dark).min_size(row.size()),
                );
                self.hitboxes.insert("app-lock-unlock".into(), button.rect);
                if button.clicked() && self.applock.begin_unlock() {
                    self.start_auth("解锁页间");
                }
                if let Some(failure) = self.applock.failure() {
                    ui.painter().text(
                        pos2(column.center().x, column.top() + 190.),
                        Align2::CENTER_CENTER,
                        failure,
                        FontId::proportional(12. * self.font_scale),
                        palette.muted,
                    );
                }
            });
    }
}
