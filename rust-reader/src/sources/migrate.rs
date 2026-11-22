use crate::{
    design,
    follows::{self, Follow},
    source_filters::FilterEditor,
    source_repos::{Lang, SourceRepos},
};
use anyhow::Context as _;
use egui::*;
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU8, AtomicU64, AtomicUsize, Ordering},
        mpsc,
    },
    thread,
};
use yejian_core::{ChapterLink, ChapterMetadata};
use yejian_sources::*;
use super::*;
use super::engine::*;
use super::downloads::*;
use super::covers::*;
use super::updates::*;
use super::global_search::*;
use super::storage::*;
use super::web::*;
use super::repos::*;
use super::ui_browse::*;
use super::ui_detail::*;
use super::ui_library::*;
use super::ui_downloads::*;
#[cfg(any(test, debug_assertions))]
use super::test_support::*;

/// One source's candidate list inside the 换源 panel.
pub(crate) struct MigrateGroup {
    pub(crate) source: String,
    pub(crate) state: GlobalState,
}

impl Clone for MigrateGroup {
    fn clone(&self) -> Self {
        Self {
            source: self.source.clone(),
            state: self.state.clone(),
        }
    }
}

/// 换源 panel state. While `Some`, this panel owns the global-search
/// generation: `GlobalEvent::Search` routes to `groups` and
/// `GlobalEvent::Chapters` feeds `expanded`.
pub(crate) struct MigrateState {
    /// The generation this panel's search was launched with.
    pub(crate) generation: u64,
    /// Source the work migrates away from — excluded from the search.
    pub(crate) old_source: String,
    /// The work's manga key on the old source.
    pub(crate) old_manga: String,
    /// Candidate groups in installed order.
    pub(crate) groups: Vec<MigrateGroup>,
    /// Candidate whose chapter list is being fetched: (source, manga key).
    pub(crate) expanding: Option<(String, String)>,
    /// Chapter lists fetched once per candidate, keyed (source, manga key).
    pub(crate) expanded: HashMap<(String, String), Result<Vec<Chapter>, String>>,
    /// Chosen candidate + migration options, set once 换到这里 is clicked.
    pub(crate) confirm: Option<MigrateConfirm>,
    /// Off-thread migration in flight — blocks a second confirm and closing.
    pub(crate) running: bool,
    /// Migration failure to show inside the panel.
    pub(crate) error: Option<String>,
}

/// The confirm stage of a migration: the chosen candidate and its chapters
/// in reading order (oldest first).
#[derive(Clone)]
pub(crate) struct MigrateConfirm {
    pub(crate) source: String,
    pub(crate) manga: Manga,
    pub(crate) chapters: Vec<Chapter>,
    /// 同时删除旧书源已下载的章节 — off by default.
    pub(crate) purge_old: bool,
}

/// What the off-thread migration produced, applied on the UI thread.
pub(super) struct MigrateOutcome {
    /// Re-keyed follow record, when the work was followed.
    pub(super) follow: Option<Follow>,
    /// Position of the old record inside `self.follows`.
    pub(super) follow_index: Option<usize>,
    pub(super) source: String,
    pub(super) manga: Manga,
    /// Chapters of the new work, in listing order.
    pub(super) chapters: Vec<Chapter>,
    pub(super) old_source: String,
    pub(super) old_manga: String,
    pub(super) purge_old: bool,
    /// True when no chapter number could be parsed from the old reading
    /// record — the detail page resumes at chapter one and the user is told.
    pub(super) resume_unparsed: bool,
}

/// Migration side-effects the host still owes once: the tracker binding
/// re-key (one binding covers whichever service is bound) and the optional
/// old-download purge, which needs the library's book ids.
pub(crate) struct MigratePost {
    /// `binding_key` for the old work, then the new one.
    pub(crate) rebind: (String, String),
    /// Old source + manga the host should map to book ids before purging.
    pub(crate) purge: Option<(String, String)>,
    /// User-visible notice when no chapter number could be parsed.
    pub(crate) resume_unparsed: bool,
}

impl Sources {
    /// Detail-page 换源 button: searches the work's title across every
    /// installed source except the current one and opens the panel.
    pub(crate) fn open_migrate(&mut self) {
        let Some(manga) = self.selected_manga.clone() else {
            return;
        };
        // 换源 carries progress through the follow record; an unfollowed
        // work has nothing to carry and the button is disabled anyway.
        let old_source = self.source_for(&manga);
        if self.follow_index(&old_source, &manga.key).is_none() {
            return;
        }
        self.global_gen += 1;
        let generation = self.global_gen;
        let groups: Vec<MigrateGroup> = self
            .installed
            .iter()
            .filter(|source| source.manifest.id != old_source)
            .map(|source| MigrateGroup {
                source: source.manifest.id.clone(),
                state: GlobalState::Searching,
            })
            .collect();
        let sources: Vec<String> = groups.iter().map(|group| group.source.clone()).collect();
        self.migrate = Some(MigrateState {
            generation,
            old_source,
            old_manga: manga.key.clone(),
            groups,
            expanding: None,
            expanded: HashMap::new(),
            confirm: None,
            running: false,
            error: None,
        });
        let _ = self.global_tx.send(GlobalCmd::Search {
            generation,
            query: manga.title.clone(),
            sources,
        });
    }
    /// Closes the panel unless a migration is still running off-thread.
    pub(crate) fn close_migrate(&mut self) {
        if self.migrate.as_ref().is_some_and(|state| state.running) {
            return;
        }
        self.migrate = None;
    }
    /// Expands one candidate: fetches its chapter list once through the
    /// global worker and caches it inside the panel.
    pub(crate) fn migrate_expand(&mut self, source: &str, manga: &Manga) {
        let Some(state) = self.migrate.as_mut() else {
            return;
        };
        let key = (source.to_string(), manga.key.clone());
        if state.expanded.contains_key(&key) || state.expanding.is_some() {
            return;
        }
        state.expanding = Some(key);
        let _ = self.global_tx.send(GlobalCmd::Chapters {
            generation: state.generation,
            source: source.to_string(),
            manga: manga.clone(),
        });
    }
    /// 换到这里: opens the confirm stage once the chapter list has landed.
    pub(crate) fn migrate_choose(&mut self, source: &str, manga: &Manga) {
        let Some(state) = self.migrate.as_mut() else {
            return;
        };
        let key = (source.to_string(), manga.key.clone());
        let Some(Ok(chapters)) = state.expanded.get(&key) else {
            return;
        };
        state.confirm = Some(MigrateConfirm {
            source: source.to_string(),
            manga: manga.clone(),
            chapters: chapter_list(chapters, "", false),
            purge_old: false,
        });
    }
    pub(crate) fn migrate_set_purge(&mut self, on: bool) {
        if let Some(confirm) = self.migrate.as_mut().and_then(|s| s.confirm.as_mut()) {
            confirm.purge_old = on;
        }
    }
    pub(crate) fn migrate_cancel_confirm(&mut self) {
        if let Some(state) = self.migrate.as_mut()
            && !state.running
        {
            state.confirm = None;
        }
    }
    /// Runs the migration off the UI thread: the new snapshot + re-keyed
    /// follow record are written there, then applied back here.
    pub(crate) fn migrate_run(&mut self) {
        let Some(state) = self.migrate.as_mut() else {
            return;
        };
        if state.running {
            return;
        }
        let Some(confirm) = state.confirm.clone() else {
            return;
        };
        state.running = true;
        state.error = None;
        // Resume point: the most recently read task on the old source.
        let latest = self
            .tasks
            .iter()
            .filter(|task| {
                task.source_id == state.old_source
                    && task.manga_id == state.old_manga
                    && task.read_at.is_some()
            })
            .max_by_key(|task| task.read_at)
            .cloned();
        let had_record = latest.is_some();
        let (resume_number, resume_done) = latest.map_or((None, false), |task| {
            let number = task
                .chapter_number
                .or_else(|| {
                    crate::trackers::chapter_number(&task.chapter_title).map(|number| number as f64)
                });
            let done = task.pages_total > 0
                && task.read_page.is_some_and(|page| page + 1 >= task.pages_total);
            (number, done)
        });
        let (old_source, old_manga) = (state.old_source.clone(), state.old_manga.clone());
        let follow_index = self.follow_index(&old_source, &old_manga);
        let old_follow = follow_index.and_then(|index| self.follows.get(index).cloned());
        let root = self.root.clone();
        let tx = self.migrate_tx.clone();
        let ctx = self.ctx.clone();
        thread::spawn(move || {
            let outcome = (|| -> anyhow::Result<MigrateOutcome> {
                let mut manga = confirm.manga.clone();
                manga.source_key = confirm.source.clone();
                let snapshot = save_manga_snapshot(&root, &manga)?;
                let follow = old_follow.map(|mut follow| {
                    // 追更记录改指向新书源: ids + snapshot move over, known and
                    // reading_order use the new source's chapters, fresh is
                    // cleared, followed_at kept.
                    follow.source_id = confirm.source.clone();
                    follow.manga_id = manga.key.clone();
                    follow.title = manga.title.clone();
                    follow.snapshot = snapshot;
                    let keys: Vec<String> =
                        confirm.chapters.iter().map(|c| c.key.clone()).collect();
                    follow.known = keys.clone();
                    follow.reading_order = keys;
                    follow.fresh.clear();
                    follow.total = confirm.chapters.len();
                    follow.error = None;
                    follow.resume_number = resume_number;
                    follow.resume_done = resume_done;
                    follow.checked_at = Some(follows::now());
                    follow
                });
                Ok(MigrateOutcome {
                    follow,
                    follow_index,
                    source: confirm.source.clone(),
                    manga,
                    chapters: confirm.chapters.clone(),
                    old_source,
                    old_manga,
                    purge_old: confirm.purge_old,
                    resume_unparsed: had_record && resume_number.is_none(),
                })
            })();
            let _ = tx.send(outcome);
            ctx.request_repaint();
        });
    }
    /// Applies a finished migration on the UI thread: follow re-keyed,
    /// detail page switched to the new source, tracker/purge side-effects
    /// queued for the host.
    pub(super)fn apply_migrate(&mut self, done: anyhow::Result<MigrateOutcome>) {
        match done {
            Ok(outcome) => {
                if let Some(follow) = outcome.follow {
                    match outcome.follow_index {
                        Some(index) => self.follows[index] = follow,
                        None => self.follows.insert(0, follow),
                    }
                    self.save_follows();
                }
                self.migrate_post = Some(MigratePost {
                    rebind: (
                        crate::trackers::binding_key(
                            Some((&outcome.old_source, &outcome.old_manga)),
                            None,
                        ),
                        crate::trackers::binding_key(
                            Some((&outcome.source, &outcome.manga.key)),
                            None,
                        ),
                    ),
                    purge: outcome
                        .purge_old
                        .then(|| (outcome.old_source.clone(), outcome.old_manga.clone())),
                    resume_unparsed: outcome.resume_unparsed,
                });
                self.selected_manga = Some(outcome.manga);
                self.chapters = outcome.chapters;
                self.chapter_query.clear();
                self.migrate = None;
                self.status = if outcome.resume_unparsed {
                    "已换源；旧阅读记录里解析不出话数，将从第一章开始".into()
                } else {
                    "已换源".into()
                };
            }
            Err(error) => {
                if let Some(state) = self.migrate.as_mut() {
                    state.running = false;
                    state.error = Some(format!("{error:#}"));
                }
            }
        }
    }
    /// The host consumes the post-migration side-effects once.
    pub(crate) fn take_migrate_post(&mut self) -> Option<MigratePost> {
        self.migrate_post.take()
    }
    /// 换源 panel: opaque scrim + card (same recipe as the other modals).
    /// Shows grouped candidates → confirm listing every effect → run.
    pub(super)fn migrate_ui(&mut self, ctx: &Context) {
        let Some(state) = self.migrate.as_ref() else {
            return;
        };
        let _ = state;
        let screen = ctx.content_rect();
        let scrim = Area::new(egui::Id::unique("migrate-scrim"))
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
            self.close_migrate();
            return;
        }
        let mut card_rect = Rect::NOTHING;
        Area::new(egui::Id::unique("migrate"))
            .order(Order::Foreground)
            .fade_in(false)
            .interactable(true)
            .movable(false)
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .show(ctx, |ui| {
                let frame = crate::design::surface(ui).show(ui, |ui| {
                    ui.set_width((screen.width() - 56.).clamp(280., 560.));
                    ui.set_max_height((screen.height() - 80.).clamp(240., 640.));
                    self.migrate_card(ui);
                });
                card_rect = frame.response.rect;
            });
        self.hitboxes.insert("migrate-card".into(), card_rect);
    }
    /// Card body: header, candidate groups or the confirm stage.
    pub(super)fn migrate_card(&mut self, ui: &mut Ui) {
        let dark = ui.visuals().dark_mode;
        let palette = design::palette(dark);
        let running = self.migrate.as_ref().is_some_and(|s| s.running);
        ui.horizontal(|ui| {
            ui.heading("换源");
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let close = ui.add_enabled(!running, quiet_action("关闭"));
                self.hitboxes.insert("migrate-close".into(), close.rect);
                if close.clicked() {
                    self.close_migrate();
                }
            });
        });
        ui.label(
            RichText::new("把这部作品迁到别的书源；旧书源已下载的章节仍留在书架")
                .color(palette.muted)
                .small(),
        );
        if let Some(error) = self
            .migrate
            .as_ref()
            .and_then(|state| state.error.clone())
        {
            ui.label(RichText::new(format!("迁移失败：{error}")).color(palette.danger));
        }
        if running {
            ui.add_space(10.);
            ui.horizontal(|ui| {
                ui.add(Spinner::new());
                ui.label("正在迁移…");
            });
            return;
        }
        if self
            .migrate
            .as_ref()
            .is_some_and(|state| state.confirm.is_some())
        {
            self.migrate_confirm_card(ui, &palette);
            return;
        }
        let groups = match self.migrate.as_ref() {
            Some(state) => state.groups.clone(),
            None => return,
        };
        ui.add_space(6.);
        ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.set_max_width(ui.available_width());
                let mut expand: Option<(String, Manga)> = None;
                let mut choose: Option<(String, Manga)> = None;
                for group in &groups {
                    let name = self.source_display_name(&group.source);
                    design::section(ui, &name);
                    match &group.state {
                        GlobalState::Searching => {
                            ui.label(RichText::new("正在搜索…").color(palette.muted).small());
                        }
                        GlobalState::Failed(reason) => {
                            ui.label(
                                RichText::new(format!("搜索失败：{reason}"))
                                    .color(palette.muted)
                                    .small(),
                            );
                        }
                        GlobalState::Done(entries) => {
                            if entries.is_empty() {
                                ui.label(
                                    RichText::new("无结果").color(palette.muted).small(),
                                );
                            }
                            for (i, manga) in entries.iter().enumerate() {
                                let key = (group.source.clone(), manga.key.clone());
                                let expanding =
                                    self.migrate.as_ref().is_some_and(|s| s.expanding == Some(key.clone()));
                                let expanded = self
                                    .migrate
                                    .as_ref()
                                    .and_then(|s| s.expanded.get(&key).cloned());
                                let row = ui.add(
                                    quiet_action(&manga.title).min_size(vec2(0., 44.)),
                                );
                                self.hitboxes.insert(
                                    format!("migrate-candidate-{}-{i}", group.source),
                                    row.rect,
                                );
                                if row.clicked() && expanded.is_none() {
                                    expand = Some((group.source.clone(), manga.clone()));
                                }
                                if expanding {
                                    ui.label(
                                        RichText::new("　加载章节…")
                                            .color(palette.muted)
                                            .small(),
                                    );
                                }
                                if let Some(result) = &expanded {
                                    match result {
                                        Ok(chapters) => {
                                            let latest = chapter_list(chapters, "", false)
                                                .last()
                                                .map(|c| chapter_name(c))
                                                .unwrap_or_else(|| "—".into());
                                            ui.label(
                                                RichText::new(format!(
                                                    "　共 {} 话 · 最新 {latest}",
                                                    chapters.len()
                                                ))
                                                .color(palette.muted)
                                                .small(),
                                            );
                                            let button = ui.add(
                                                quiet_action("换到这里")
                                                    .min_size(vec2(0., 40.)),
                                            );
                                            self.hitboxes.insert(
                                                format!(
                                                    "migrate-choose-{}-{i}",
                                                    group.source
                                                ),
                                                button.rect,
                                            );
                                            if button.clicked() {
                                                choose =
                                                    Some((group.source.clone(), manga.clone()));
                                            }
                                        }
                                        Err(error) => {
                                            ui.label(
                                                RichText::new(format!(
                                                    "　章节列表失败：{error}"
                                                ))
                                                .color(palette.muted)
                                                .small(),
                                            );
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                if let Some((source, manga)) = expand {
                    self.migrate_expand(&source, &manga);
                }
                if let Some((source, manga)) = choose {
                    self.migrate_choose(&source, &manga);
                }
            });
    }
    /// Confirm stage: lists every migration effect + the optional purge.
    pub(super)fn migrate_confirm_card(&mut self, ui: &mut Ui, palette: &crate::design::Palette) {
        let dark = ui.visuals().dark_mode;
        let Some(confirm) = self
            .migrate
            .as_ref()
            .and_then(|state| state.confirm.clone())
        else {
            return;
        };
        let source_name = self.source_display_name(&confirm.source);
        ui.label(format!("换到 {source_name} 的《{}》", confirm.manga.title));
        ui.add_space(4.);
        for line in [
            "追更记录改指向新书源，新章节提醒重新开始",
            "阅读进度按话数近似对应（解析不出话数时从第一章开始并提示）",
            "跟踪绑定（AniList / Kitsu / Bangumi）改到新书源",
            "旧书源已下载的章节仍作为本地作品留在书架",
        ] {
            ui.label(RichText::new(format!("· {line}")).color(palette.muted).small());
        }
        let mut purge = confirm.purge_old;
        let check = ui.checkbox(&mut purge, "同时删除旧书源已下载的章节");
        self.hitboxes.insert("migrate-purge".into(), check.rect);
        if check.changed() {
            self.migrate_set_purge(purge);
        }
        ui.add_space(6.);
        ui.horizontal(|ui| {
            if ui.button("取消").clicked() {
                self.migrate_cancel_confirm();
            }
            let go = ui.add(design::primary("换源", dark));
            self.hitboxes.insert("migrate-go".into(), go.rect);
            if go.clicked() {
                self.migrate_run();
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sources::test_support::*;

    #[test]
    fn migrate_rekeys_the_follow_and_posts_side_effects() {
        let root = tempfile::tempdir().unwrap();
        let mut sources = test_sources(root.path());
        sources.selected_source = "source.a".into();
        sources.chapters = vec![numbered("c1", 1.), numbered("c2", 2.)];
        sources.toggle_follow(&manga("source.a", "m"));
        sources.follows[0].fresh = vec!["c2".into()];
        let new_manga = manga("source.b", "m2");
        let outcome = MigrateOutcome {
            follow: Some(Follow {
                source_id: "source.b".into(),
                manga_id: "m2".into(),
                title: "m2".into(),
                snapshot: "snap".into(),
                known: vec!["x1".into(), "x2".into()],
                fresh: vec![],
                total: 2,
                followed_at: sources.follows[0].followed_at,
                reading_order: vec!["x1".into(), "x2".into()],
                resume_number: Some(7.),
                resume_done: true,
                ..Default::default()
            }),
            follow_index: Some(0),
            source: "source.b".into(),
            manga: new_manga.clone(),
            chapters: vec![numbered("x1", 1.), numbered("x2", 2.)],
            old_source: "source.a".into(),
            old_manga: "m".into(),
            purge_old: true,
            resume_unparsed: false,
        };
        sources.apply_migrate(Ok(outcome));
        let follow = &sources.follows[0];
        assert_eq!(
            (follow.source_id.as_str(), follow.manga_id.as_str()),
            ("source.b", "m2")
        );
        assert!(follow.fresh.is_empty(), "fresh is cleared by migration");
        assert_eq!(follow.reading_order, ["x1", "x2"]);
        assert!(follow.followed_at.is_some(), "followed_at is kept");
        let post = sources.take_migrate_post().expect("host side-effects");
        assert_eq!(
            post.rebind,
            (
                "source:source.a\x1fm".to_string(),
                "source:source.b\x1fm2".to_string()
            )
        );
        assert_eq!(
            post.purge,
            Some(("source.a".to_string(), "m".to_string())),
            "purge checkbox asks the host to purge the old work"
        );
        assert_eq!(
            sources.selected_manga.as_ref().map(|m| m.key.as_str()),
            Some("m2"),
            "finishing opens the new source's detail page"
        );
    }
    #[test]
    fn migrate_run_computes_resume_number_from_the_last_read_task() {
        let root = tempfile::tempdir().unwrap();
        let mut sources = test_sources(root.path());
        sources.selected_source = "source.a".into();
        sources.chapters = vec![numbered("c9", 9.)];
        sources.toggle_follow(&manga("source.a", "m"));
        let mut download = task("source.a", "m", "c9");
        download.read_at = Some(10);
        download.read_page = Some(4);
        download.pages_total = 10;
        download.chapter_number = Some(9.);
        sources.tasks.push(download);
        sources.migrate = Some(MigrateState {
            generation: 0,
            old_source: "source.a".into(),
            old_manga: "m".into(),
            groups: Vec::new(),
            expanding: None,
            expanded: HashMap::new(),
            confirm: Some(MigrateConfirm {
                source: "source.b".into(),
                manga: manga("source.b", "m2"),
                chapters: vec![numbered("x1", 1.), numbered("x2", 2.)],
                purge_old: false,
            }),
            running: false,
            error: None,
        });
        sources.migrate_run();
        let outcome = sources
            .migrate_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap()
            .unwrap();
        let follow = outcome.follow.expect("work is followed");
        assert_eq!(follow.resume_number, Some(9.));
        assert!(!follow.resume_done, "page 5 of 10 is unfinished");
        assert!(!outcome.resume_unparsed);
        // A record exists but its title/number cannot be parsed → the user
        // is told the resume could not be carried over.
        sources.migrate.as_mut().unwrap().running = false;
        let mut untitled = task("source.a", "m", "cX");
        untitled.chapter_title = "外传".into();
        untitled.read_at = Some(20);
        sources.tasks.push(untitled);
        sources.migrate_run();
        let outcome = sources
            .migrate_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap()
            .unwrap();
        assert!(outcome.resume_unparsed);
        assert!(outcome.follow.unwrap().resume_number.is_none());
    }
    #[test]
    fn resume_chapter_uses_the_migrated_resume_number_without_records() {
        let root = tempfile::tempdir().unwrap();
        let mut sources = test_sources(root.path());
        let manga = manga("source.a", "m");
        let ordered = vec![
            numbered("c1", 1.),
            numbered("c2", 2.),
            numbered("c3", 3.),
            numbered("c4", 4.),
        ];
        // Chapter 2 was mid-read on the old source → resume lands on it.
        sources.follows.push(Follow {
            source_id: "source.a".into(),
            manga_id: "m".into(),
            resume_number: Some(2.),
            resume_done: false,
            ..Default::default()
        });
        let (resume, started) =
            sources.resume_chapter("source.a", &manga, &ordered, &Default::default());
        assert!(started);
        assert_eq!(resume.unwrap().key, "c2");
        // Chapter 2 was finished → resume lands on the next numbered chapter.
        sources.follows[0].resume_done = true;
        let (resume, _) =
            sources.resume_chapter("source.a", &manga, &ordered, &Default::default());
        assert_eq!(resume.unwrap().key, "c3");
        // No carried number → the normal first-chapter behaviour.
        sources.follows[0].resume_number = None;
        let (resume, started) =
            sources.resume_chapter("source.a", &manga, &ordered, &Default::default());
        assert!(!started);
        assert_eq!(resume.unwrap().key, "c1");
    }
    #[test]
    fn migrate_is_not_offered_for_an_unfollowed_work() {
        let root = tempfile::tempdir().unwrap();
        let mut sources = test_sources(root.path());
        sources.selected_source = "source.a".into();
        sources.selected_manga = Some(manga("source.a", "m"));
        sources.open_migrate();
        assert!(
            sources.migrate.is_none(),
            "an unfollowed work has no progress to carry over"
        );
        sources.toggle_follow(&manga("source.a", "m"));
        sources.open_migrate();
        assert!(sources.migrate.is_some(), "followed works may migrate");
    }
    #[test]
    fn resume_chapter_falls_back_to_the_newest_chapter_when_number_is_beyond() {
        let root = tempfile::tempdir().unwrap();
        let mut sources = test_sources(root.path());
        let manga = manga("source.a", "m");
        let ordered = vec![
            numbered("c1", 1.),
            numbered("c2", 2.),
            numbered("c3", 3.),
        ];
        // Read chapter 10 on the old source but the new source only has 3:
        // resume at the newest chapter, not the first.
        sources.follows.push(Follow {
            source_id: "source.a".into(),
            manga_id: "m".into(),
            resume_number: Some(10.),
            resume_done: false,
            ..Default::default()
        });
        let (resume, started) =
            sources.resume_chapter("source.a", &manga, &ordered, &Default::default());
        assert!(started);
        assert_eq!(resume.unwrap().key, "c3");
        // Locked tail chapters are skipped for the newest reachable one.
        let mut locked_tail = ordered.clone();
        locked_tail[2].locked = true;
        let (resume, _) =
            sources.resume_chapter("source.a", &manga, &locked_tail, &Default::default());
        assert_eq!(resume.unwrap().key, "c2");
    }
}
