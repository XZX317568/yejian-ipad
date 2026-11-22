use crate::design;



use crate::source_repos::Lang;


use egui::*;



















use yejian_sources::*;
use super::*;
use super::repos::COMMUNITY_INDEX;
use super::covers::CoverReq;
use super::engine::Task;
use super::covers::cover_key;
use super::ui_browse::bordered_action;
use super::ui_browse::disclosure;
use super::downloads::manga_snapshot_key;
use super::ui_browse::quiet_action;
use super::ui_browse::source_item;

pub(super) fn language_label(language: &str) -> &str {
    match language {
        "zh" | "zh-CN" | "zh-Hans" => "中文",
        "zh-TW" | "zh-Hant" => "繁體中文",
        "en" => "English",
        "ja" => "日本語",
        "ko" => "한국어",
        _ => language,
    }
}

impl Sources {
    /// Display name of an installed source; falls back to its id.
    pub(crate) fn source_display_name(&self, source_id: &str) -> String {
        self.installed
            .iter()
            .find(|source| source.manifest.id == source_id)
            .map_or_else(|| source_id.to_string(), |source| source.manifest.name.clone())
    }
    /// Latest chapter read state for a work: (chapter key, display name, read_at).
    pub(crate) fn work_last_read(
        &self,
        source: &str,
        manga: &str,
    ) -> Option<(String, String, i64)> {
        self.tasks
            .iter()
            .filter(|task| task.source_id == source && task.manga_id == manga)
            .filter_map(|task| {
                task.read_at
                    .map(|at| (task.chapter_id.clone(), task.chapter_title.clone(), at))
            })
            .max_by_key(|(_, _, at)| *at)
    }
    /// Drops a follow; used when an online shelf entry is 移出'd.
    pub(crate) fn unfollow(&mut self, source: &str, manga: &str) -> bool {
        let Some(index) = self.follow_index(source, manga) else {
            return false;
        };
        self.follows.remove(index);
        self.status = "已取消追更".into();
        self.save_follows();
        true
    }
    /// One history row per work read online: the most recently read chapter
    /// of each (source, manga) pair that has a `read_at`.
    pub(crate) fn reading_history(&self) -> Vec<OnlineHistory> {
        let mut latest: std::collections::HashMap<(String, String), OnlineHistory> =
            Default::default();
        for task in &self.tasks {
            let Some(read_at) = task.read_at else {
                continue;
            };
            let key = (task.source_id.clone(), task.manga_id.clone());
            let entry = OnlineHistory {
                source_id: task.source_id.clone(),
                manga_id: task.manga_id.clone(),
                manga_title: task.manga_title.clone(),
                chapter_title: task.chapter_title.clone(),
                chapter_number: task.chapter_number,
                read_page: task.read_page.unwrap_or(0),
                pages_total: task.pages_total,
                read_at,
            };
            match latest.get(&key) {
                Some(existing) if existing.read_at >= read_at => {}
                _ => {
                    latest.insert(key, entry);
                }
            }
        }
        let mut entries: Vec<OnlineHistory> = latest.into_values().collect();
        entries.sort_by_key(|entry| std::cmp::Reverse(entry.read_at));
        entries
    }
    /// 阅读足迹 row tap: followed works resume through the shelf path;
    /// unfollowed ones open their detail page from the task snapshot.
    pub(crate) fn open_history_work(&mut self, source: &str, manga: &str) -> bool {
        if !self
            .installed
            .iter()
            .any(|installed| installed.manifest.id == source)
        {
            self.status = format!("书源 {source} 未安装，无法打开这部作品");
            return false;
        }
        if self.follow_index(source, manga).is_some() {
            return self.open_work(source, manga, true);
        }
        // Unfollowed work: rebuild the manga from the latest task's snapshot.
        let task = self
            .tasks
            .iter()
            .filter(|task| task.source_id == source && task.manga_id == manga)
            .max_by_key(|task| task.read_at.unwrap_or(0));
        let Some(task) = task else {
            return false;
        };
        let snapshot = task
            .manga_snapshot
            .clone()
            .unwrap_or_else(|| manga_snapshot_key(source, manga));
        let Ok(bytes) = manga_snapshot_path(&self.root, &snapshot).and_then(|p| {
            std::fs::read(&p).map_err(|error| anyhow::anyhow!("{error}"))
        }) else {
            self.status = "这部作品的网络详情已不在本地，请在书源中重新打开".into();
            return false;
        };
        let Ok(mut manga) = Manga::from_postcard_bytes(&bytes) else {
            self.status = "这部作品的网络详情已损坏".into();
            return false;
        };
        manga.source_key = source.to_string();
        self.select_source(source);
        self.pending_open = Some(manga);
        true
    }
    pub(super)fn follows_ui(&mut self, ui: &mut Ui) {
        if self.follows.is_empty() {
            return;
        }
        let palette = design::palette(ui.visuals().dark_mode);
        let now = ui.ctx().input(|i| i.time);
        design::section(ui, &format!("我的追更 · {}", self.follows.len()));
        let checking = self.checking > 0;
        if ui
            .add_enabled(
                !checking,
                quiet_action(if checking {
                    "正在检查更新…"
                } else {
                    "检查更新"
                }),
            )
            .clicked()
        {
            self.check_updates(false);
        }
        let mut order: Vec<usize> = (0..self.follows.len()).collect();
        order.sort_by_key(|&index| std::cmp::Reverse(self.follows[index].fresh.len()));
        let mut open = None;
        let mut want_covers = Vec::new();
        for index in order {
            let follow = &self.follows[index];
            let source = self
                .installed
                .iter()
                .find(|source| source.manifest.id == follow.source_id)
                .map_or_else(
                    || follow.source_id.clone(),
                    |source| source.manifest.name.clone(),
                );
            let uninstalled = !self
                .installed
                .iter()
                .any(|installed| installed.manifest.id == follow.source_id);
            let mut detail = if uninstalled {
                format!("{source} · 书源已卸载")
            } else if let Some(error) = &follow.error {
                format!("{source} · 检查失败：{error}")
            } else if !follow.fresh.is_empty() {
                format!("{source} · {} 话更新", follow.fresh.len())
            } else {
                format!("{source} · 共 {} 话", follow.total)
            };
            let auto_n = self.auto_downloading(&follow.source_id, &follow.manga_id);
            if auto_n > 0 {
                detail = format!("{detail} · 自动下载中 {auto_n} 章");
            }
            let snapshot = follow.snapshot.clone();
            let top = ui.cursor().top();
            let left = ui.cursor().left();
            if source_item(ui, ("follow", index), &follow.title, &detail, true, true) {
                open = Some(index);
            }
            let bottom = ui.min_rect().bottom();
            // Follow covers share the grid's `source-covers` pipeline: the

            // manga snapshot supplies the URL, the FNV key and LRU texture

            // path are the same, and `plan_covers` fetches what's missing.
            let source_id = follow.source_id.clone();
            let cover_url = self.follow_cover_url(index);
            let texture = match &cover_url {
                Some(url) => self.cover_image(ui.ctx(), &source_id, url),
                None => None,
            };
            // Only covers without a texture are fetched; asking for loaded
            // ones re-downloads them after every completed request.
            let missing = texture.is_none();
            let (texture, alpha) = match texture {
                Some(texture) => {
                    let alpha = cover_url
                        .as_ref()
                        .map_or(1., |url| self.cover_alpha(&cover_key(&source_id, url), now));
                    (Some(texture), alpha)
                }
                None => (
                    self.cover_texture(ui.ctx(), &snapshot),
                    self.cover_alpha(&snapshot, now),
                ),
            };
            if missing
                && let Some(url) = cover_url
            {
                want_covers.push(CoverReq {
                    key: cover_key(&source_id, &url),
                    source: source_id,
                    url,
                });
            }
            if let Some(texture) = texture {
                let cover =
                    Rect::from_center_size(pos2(left + 18., (top + bottom) / 2.), vec2(40., 54.));
                crate::shelf::paint_art_alpha(
                    ui.painter(),
                    crate::shelf::Plane::rect(cover),
                    cover.size(),
                    &texture,
                    palette.accent_soft,
                    alpha,
                );
                ui.painter().rect_stroke(
                    cover,
                    8.,
                    Stroke::new(1., palette.line),
                    StrokeKind::Inside,
                );
            }
        }
        if let Some(index) = open {
            self.open_follow(index);
        }
        self.plan_covers(want_covers);
        ui.add_space(20.);
    }
    pub(super)fn installed_ui(&mut self, ui: &mut Ui) {
        let palette = design::palette(ui.visuals().dark_mode);
        ui.horizontal_wrapped(|ui| {
            design::section(ui, "我的书源");
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let updates = self.updatable();
                let all = ui.add(quiet_action(&format!("全部更新 ({})", updates.len())));
                self.hitboxes.insert("sources-update-all".into(), all.rect);
                if all.clicked() {
                    self.update_all();
                }
                let test = ui.add(quiet_action("全部测试"));
                self.hitboxes.insert("sources-test-all".into(), test.rect);
                if test.clicked() {
                    self.test_all();
                }
                let check = ui.add_enabled(
                    !self.repo_fetching,
                    quiet_action(if self.repo_fetching {
                        "检查中…"
                    } else {
                        "检查更新"
                    }),
                );
                self.hitboxes.insert("sources-check-updates".into(), check.rect);
                if check.clicked() {
                    self.check_repo_updates();
                }
            });
        });
        let updates = self.updatable();
        for source in self.installed.clone() {
            let id = source.manifest.id.clone();
            let new_version = updates.get(&id).copied();
            let languages = source
                .manifest
                .languages
                .iter()
                .map(|lang| language_label(lang))
                .collect::<Vec<_>>()
                .join(" / ");
            let mut metadata = if languages.is_empty() {
                format!("v{}", source.manifest.version)
            } else {
                format!("{languages} · v{}", source.manifest.version)
            };
            if let Some(version) = new_version {
                metadata = format!("{metadata} · 有更新 v{version}");
            }
            if let Some(result) = self.repo_test_results.get(&id) {
                metadata = format!("{metadata} · {result}");
            }
            // Fixed-width right-aligned action slots so every row lines up:
            // 更新 | 测试 | 卸载 — unavailable actions stay visible, disabled.
            let width = ui.available_width();
            let narrow = width < 380.;
            let response = ui.scope(|ui| {
                ui.horizontal(|ui| {
                    ui.set_min_height(56.);
                    ui.vertical(|ui| {
                        ui.add_space(4.);
                        let mut title_job = egui::text::LayoutJob::simple(
                            source.manifest.name.clone(),
                            TextStyle::Body.resolve(ui.style()),
                            palette.ink,
                            width - 270.,
                        );
                        title_job.wrap.max_rows = 2;
                        ui.add(Label::new(title_job).selectable(false));
                        ui.add(
                            Label::new(RichText::new(&metadata).small().color(palette.muted))
                                .selectable(false),
                        );
                    });
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let remove = ui.add_enabled(true, quiet_action("卸载"));
                        self.hitboxes
                            .insert(format!("source-uninstall-{id}"), remove.rect);
                        if remove.clicked() {
                            self.ask_uninstall(source.clone());
                        }
                        let testing = self.repo_testing.contains_key(&id);
                        let test = ui.add_enabled(
                            !testing,
                            quiet_action(if testing { "测试中…" } else { "测试" }),
                        );
                        self.hitboxes
                            .insert(format!("source-test-{id}"), test.rect);
                        if test.clicked() {
                            self.test_source(&id);
                        }
                        let update = ui.add_enabled(
                            new_version.is_some() && !self.busy,
                            quiet_action("更新"),
                        );
                        self.hitboxes
                            .insert(format!("source-update-{id}"), update.rect);
                        if update.clicked() {
                            self.update_source(&id);
                        }
                    });
                });
                // Whole row (minus the action slots) opens the source.
                let rect = ui.min_rect();
                let open = ui
                    .interact(
                        Rect::from_min_max(
                            rect.min,
                            pos2(rect.right() - if narrow { 210. } else { 240. }, rect.max.y),
                        ),
                        ui.scope_id().with(("installed", &id)),
                        Sense::click(),
                    )
                    .clicked();
                ui.separator();
                open
            });
            if response.inner {
                self.select_source(&id);
            }
        }
    }
    /// Opaque 卸载 confirm: downloads are kept, follows stop updating.
    pub(super)fn uninstall_confirm_ui(&mut self, ctx: &Context) {
        let Some(source) = self.uninstall.clone() else {
            return;
        };
        let screen = ctx.content_rect();
        let modal = design::modal(ctx, "uninstall", |ui| {
            ui.set_width((screen.width() - 56.).clamp(220., 440.));
                    ui.heading(format!("卸载 {}", source.manifest.name));
                    ui.label("已下载的章节保留；这个书源上的追更会停止更新，并标为「书源已卸载」。");
                    ui.add_space(6.);
                    ui.horizontal(|ui| {
                        if ui.button("取消").clicked() {
                            self.uninstall = None;
                        }
                        let confirm =
                            ui.add(design::danger("卸载", ui.visuals().dark_mode));
                        self.hitboxes
                            .insert("source-uninstall-go".into(), confirm.rect);
                        if confirm.clicked() {
                            self.confirm_uninstall();
                        }
                    });
        });
        if modal.scrim_clicked {
            self.uninstall = None;
            return;
        }
        self.hitboxes.insert("source-uninstall-card".into(), modal.card_rect);
    }
    pub(super)fn repository_entry_ui(&mut self, ui: &mut Ui) {
        let palette = design::palette(ui.visuals().dark_mode);
        ui.vertical(|ui| {
            ui.set_min_width(ui.available_width());
            design::section(
                ui,
                if self.installed.is_empty() {
                    "添加你的第一个书源"
                } else {
                    "发现更多书源"
                },
            );
            if self.installed.is_empty() {
                ui.add(
                    Label::new(
                        RichText::new("从社区挑选，或使用你已有的仓库地址。").color(palette.muted),
                    )
                    .wrap(),
                );
                ui.add_space(8.);
            }
            if ui
                .add_enabled(
                    !self.busy,
                    design::primary("社区源", ui.visuals().dark_mode),
                )
                .clicked()
            {
                self.url = COMMUNITY_INDEX.into();
                self.submit(Task::Fetch(COMMUNITY_INDEX.into()));
            }
            ui.add_space(10.);
            disclosure(ui, "添加书源仓库", self.installed.is_empty(), |ui| {
                ui.add_enabled(
                    !self.busy,
                    TextEdit::singleline(&mut self.url)
                        .hint_text("粘贴书源仓库地址")
                        .desired_width(ui.available_width()),
                );
                if ui
                    .add_enabled(!self.busy, bordered_action("读取", ui.visuals().dark_mode))
                    .clicked()
                {
                    self.submit(Task::Fetch(self.url.trim().into()));
                }
            });
            // Saved repos: fetched indexes feed the 我的书源 update badges.
            if self.repos.urls.len() > 1 {
                ui.add_space(10.);
                ui.label(RichText::new("已保存的仓库").color(palette.muted).small());
                for url in self.repos.urls.clone() {
                    if url == COMMUNITY_INDEX {
                        continue;
                    }
                    ui.horizontal(|ui| {
                        let name = self
                            .repos
                            .cache
                            .get(&url)
                            .and_then(|repo| repo.name.clone());
                        ui.add(
                            Label::new(
                                RichText::new(name.unwrap_or_else(|| url.clone())).small(),
                            )
                            .wrap(),
                        );
                        let remove = ui.add(quiet_action("✕").min_size(vec2(44., 32.)));
                        self.hitboxes
                            .insert(format!("repo-remove-{}", url), remove.rect);
                        if remove.clicked() {
                            self.repos.remove_repo(&url);
                            self.write_setting("source-repos", self.repos.urls_setting());
                            let _ = self.repos.save_cache(&self.root);
                        }
                    });
                }
            }
        });
    }
    pub(super)fn repository_source_ui(&mut self, ui: &mut Ui, source: &RepositorySource) {
        let palette = design::palette(ui.visuals().dark_mode);
        let existing = self.installed.iter().find(|s| s.manifest.id == source.id);
        let installed = existing.is_some_and(|s| s.manifest.version >= source.version);
        let label = if installed {
            "已安装"
        } else if existing.is_some() {
            "更新"
        } else {
            "安装"
        };
        let metadata = source
            .languages
            .iter()
            .map(|lang| language_label(lang))
            .collect::<Vec<_>>()
            .join(" / ");
        let title = |ui: &mut Ui| {
            ui.add(Label::new(RichText::new(&source.name).strong()).wrap());
            if !metadata.is_empty() {
                ui.add(Label::new(RichText::new(&metadata).small().color(palette.muted)).wrap());
            }
        };
        let action = || {
            Button::new(RichText::new(label).color(palette.accent))
                .frame_when_inactive(false)
                .min_size(vec2(44., 44.))
                .wrap()
        };
        ui.add_space(4.);
        let scale = (ui.text_style_height(&TextStyle::Body) / 20.).max(1.);
        let clicked = if ui.available_width() < 300. * scale {
            title(ui);
            ui.add_enabled(!self.busy && !installed, action()).clicked()
        } else {
            // Manual two-slot row: title vertically centred on the left,
            // ≥44pt action vertically centred on the right.
            let line = ui.text_style_height(&TextStyle::Body);
            let title_h =
                (if metadata.is_empty() { line + 8. } else { line * 2. + 14. }).max(44.);
            let (rect, _) = ui.allocate_exact_size(
                vec2(ui.available_width(), title_h),
                Sense::hover(),
            );
            let mut title_ui = ui.new_child(
                UiBuilder::new()
                    .max_rect(Rect::from_min_size(
                        rect.min + vec2(0., (title_h - line * if metadata.is_empty() { 1. } else { 2. } - 8.) / 2.),
                        vec2((rect.width() - 72.).max(40.), title_h),
                    ))
                    .layout(Layout::top_down(Align::Min)),
            );
            title(&mut title_ui);
            let button = Rect::from_center_size(
                pos2(rect.right() - 32., rect.center().y),
                vec2(60., 44.),
            );
            ui.scope_builder(UiBuilder::new().max_rect(button), |ui| {
                ui.add_enabled(!self.busy && !installed, action()).clicked()
            })
            .inner
        };
        if clicked {
            self.submit(Task::Install(source.clone()));
        }
        ui.add_space(4.);
        ui.separator();
    }
    pub(super)fn repository_ui(&mut self, ui: &mut Ui) {
        let scale = (ui.text_style_height(&TextStyle::Body) / 20.).max(1.);
        if !self.installed.is_empty() && ui.available_width() >= 760. * scale {
            let width = ui.available_width();
            let left_width = (width - 40.) * 0.6;
            let item_gap = ui.spacing().item_spacing.x;
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = 40.;
                ui.allocate_ui_with_layout(
                    vec2(left_width, 0.),
                    Layout::top_down(Align::Min),
                    |ui| {
                        ui.spacing_mut().item_spacing.x = item_gap;
                        self.installed_ui(ui);
                    },
                );
                ui.allocate_ui_with_layout(
                    vec2(width - left_width - 40., 0.),
                    Layout::top_down(Align::Min),
                    |ui| {
                        ui.spacing_mut().item_spacing.x = item_gap;
                        self.repository_entry_ui(ui);
                    },
                );
            });
        } else {
            if !self.installed.is_empty() {
                self.installed_ui(ui);
                ui.add_space(28.);
            }
            self.repository_entry_ui(ui);
        }
        if let Some(repo) = self.repository.clone() {
            ui.add_space(28.);
            design::section(ui, &repo.name.unwrap_or_else(|| "可安装书源".into()));
            ui.add(
                TextEdit::singleline(&mut self.repository_search)
                    .hint_text("查找书源名称或语言（如 zh）")
                    .desired_width(ui.available_width()),
            );
            ui.add_space(6.);
            // Language chips narrow the list on top of the name search.
            ui.horizontal_wrapped(|ui| {
                for (lang, label) in Lang::CHOICES {
                    let chip = ui.selectable_value(&mut self.repo_lang, lang, label);
                    self.hitboxes
                        .insert(format!("repo-lang-{}", label), chip.rect);
                }
            });
            let needle = self.repository_search.trim().to_lowercase();
            ui.add_space(8.);
            let sources: Vec<&RepositorySource> = repo
                .sources
                .iter()
                .filter(|source| self.repo_lang.accepts(&source.languages))
                .filter(|source| {
                    format!(
                        "{} {} {}",
                        source.name,
                        source.id,
                        source.languages.join(" ")
                    )
                    .to_lowercase()
                    .contains(&needle)
                })
                .collect();
            if sources.is_empty() {
                ui.add(Label::new("没有匹配的书源").wrap());
            } else if ui.available_width() >= 760. * scale {
                let item_gap = ui.spacing().item_spacing.x;
                ui.scope(|ui| {
                    ui.spacing_mut().item_spacing.x = 32.;
                    ui.columns(2, |uis| {
                        for column in uis.iter_mut() {
                            column.spacing_mut().item_spacing.x = item_gap;
                        }
                        for (index, source) in sources.iter().enumerate() {
                            self.repository_source_ui(&mut uis[index % 2], source);
                        }
                    });
                });
            } else {
                for source in sources {
                    self.repository_source_ui(ui, source);
                }
            }
        }
    }
    /// Opaque scrim + card, matching the shelf delete confirmations.
    pub(super)fn history_confirm_ui(&mut self, ctx: &Context) {
        if !self.history_confirm {
            return;
        }
        let screen = ctx.content_rect();
        let modal = design::modal(ctx, "history-clear", |ui| {
            ui.set_width((screen.width() - 56.).clamp(220., 420.));
                    ui.heading("清空搜索历史");
                    ui.label("删除全部搜索记录，书架和阅读进度不受影响。");
                    ui.add_space(6.);
                    ui.horizontal(|ui| {
                        if ui.button("取消").clicked() {
                            self.history_confirm = false;
                        }
                        let confirm =
                            ui.add(crate::design::danger(
                                "清空",
                                ui.visuals().dark_mode,
                            ));
                        self.hitboxes
                            .insert("history-clear-go".into(), confirm.rect);
                        if confirm.clicked() {
                            self.search_history.clear();
                            self.history_dirty = true;
                            self.history_confirm = false;
                        }
                    });
        });
        if modal.scrim_clicked {
            self.history_confirm = false;
            return;
        }
        self.hitboxes
            .insert("history-clear-card".into(), modal.card_rect);
    }
}

#[cfg(test)]
mod tests {
    use crate::sources::test_support::*;

    #[test]
    fn reading_history_merges_into_one_row_per_work_newest_first() {
        let root = tempfile::tempdir().unwrap();
        let mut sources = test_sources(root.path());
        // Two chapters of one work → one row, latest read wins.
        let mut older = task("source.a", "m", "c1");
        older.read_at = Some(10);
        older.read_page = Some(0);
        older.pages_total = 20;
        let mut newer = task("source.a", "m", "c2");
        newer.read_at = Some(20);
        newer.read_page = Some(4);
        newer.pages_total = 20;
        let mut other = task("source.a", "other", "x");
        other.read_at = Some(30);
        // Unread tasks never produce rows.
        sources.tasks.push(task("source.a", "m", "c3"));
        sources.tasks.extend([older, newer, other]);
        let history = sources.reading_history();
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].manga_id, "other");
        assert_eq!(history[0].read_at, 30);
        assert_eq!(history[1].manga_id, "m");
        assert_eq!(history[1].chapter_title, "c2");
        assert_eq!(history[1].read_page, 4);
        assert_eq!(history[1].pages_total, 20);
    }
}
