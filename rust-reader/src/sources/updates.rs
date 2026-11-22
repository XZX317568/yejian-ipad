
use crate::follows::self;
use crate::follows::Follow;




use egui::*;



use std::collections::HashSet;















use yejian_sources::*;
use super::*;
use super::downloads::DownloadStatus;
use super::tasks::DownloadTask;
use super::tasks::EnqueueOutcome;
use super::ui_detail::chapter_list;
use super::tasks::enqueue_task;

/// Default automatic follow-check cadence (minutes); `0` turns it off.
pub(crate) const DEFAULT_FOLLOW_INTERVAL_MIN: u64 = 60;

/// Selectable periods offered in 设置, in minutes.
pub(crate) const FOLLOW_INTERVAL_CHOICES: &[(u64, &str)] = &[
    (0, "关闭"),
    (30, "30 分钟"),
    (60, "1 小时"),
    (120, "2 小时"),
    (360, "6 小时"),
];

pub(super) struct UpdateEvent {
    pub(super) source: String,
    pub(super) manga: String,
    /// `None`: only the cover arrived, after the batch's chapter results.
    pub(super) chapters: Option<Result<Vec<Chapter>, String>>,
    /// `source-covers` key + decoded image delivered with `chapters: None`.
    pub(super) cover: Option<(String, ColorImage)>,
}

impl Sources {
    pub fn fresh_follows(&self) -> usize {
        self.follows
            .iter()
            .filter(|follow| !follow.fresh.is_empty())
            .count()
    }
    pub(super)fn follow_index(&self, source: &str, manga: &str) -> Option<usize> {
        self.follows
            .iter()
            .position(|follow| follow.source_id == source && follow.manga_id == manga)
    }
    pub(super)fn save_follows(&mut self) {
        if let Err(error) = follows::save(&self.root, &self.follows) {
            self.status = format!("追更列表保存失败：{error:#}");
        }
    }
    pub(super)fn toggle_follow(&mut self, manga: &Manga) {
        let source = self.source_for(manga);
        if let Some(index) = self.follow_index(&source, &manga.key) {
            self.follows.remove(index);
            self.status = "已取消追更".into();
            self.save_follows();
            return;
        }
        let mut snapshot_manga = manga.clone();
        snapshot_manga.source_key = source.clone();
        let snapshot = match save_manga_snapshot(&self.root, &snapshot_manga) {
            Ok(key) => key,
            Err(error) => {
                self.status = format!("无法保存漫画信息，未加入追更：{error:#}");
                return;
            }
        };
        let mut follow = Follow {
            source_id: source,
            manga_id: manga.key.clone(),
            title: manga.title.clone(),
            snapshot,
            ..Default::default()
        };
        let keys: Vec<String> = self
            .chapters
            .iter()
            .map(|chapter| chapter.key.clone())
            .collect();
        follow.reading_order = chapter_list(&self.chapters, "", false)
            .iter()
            .map(|chapter| chapter.key.clone())
            .collect();
        follow.mark_seen(&keys);
        follow.checked_at = Some(follows::now());
        follow.followed_at = Some(follows::now());
        self.follows.insert(0, follow);
        self.status = "已加入追更，有新章节会在书源页顶部提示".into();
        self.save_follows();
        // Fetches the cover right away; the chapter list is already known.
        self.check_follows(&[0], false);
    }
    pub(super)fn follow_manga(&self, follow: &Follow) -> anyhow::Result<Manga> {
        let bytes = std::fs::read(manga_snapshot_path(&self.root, &follow.snapshot)?)?;
        let mut manga = Manga::from_postcard_bytes(&bytes)
            .map_err(|error| anyhow::anyhow!("漫画快照已损坏：{error}"))?;
        manga.source_key = follow.source_id.clone();
        Ok(manga)
    }
    pub fn check_updates(&mut self, background: bool) {
        if self.checking > 0 {
            return;
        }
        let all: Vec<usize> = (0..self.follows.len()).collect();
        self.check_follows(&all, background);
    }
    pub(super)fn check_follows(&mut self, indices: &[usize], background: bool) {
        let mut batch = Vec::new();
        for index in indices.iter().copied() {
            let Some(follow) = self.follows.get(index) else {
                continue;
            };
            let installed_now = self
                .installed
                .iter()
                .any(|source| source.manifest.id == follow.source_id);
            if installed_now {
                if self.follows[index].source_removed {
                    self.follows[index].source_removed = false;
                    let _ = self.save_follows();
                }
            } else if follow.source_removed {
                // A follow on an uninstalled source cannot load its engine;
                // its row already says 书源已卸载 instead of erroring every round.
                continue;
            }
            let follow = &self.follows[index];
            match self.follow_manga(follow) {
                Ok(manga) => batch.push((follow.source_id.clone(), manga)),
                Err(error) => {
                    if !background {
                        self.status = format!("《{}》无法检查更新：{error:#}", follow.title);
                    }
                }
            }
        }
        if batch.is_empty() {
            return;
        }
        let count = batch.len();
        if self.up_tx.send(batch).is_ok() {
            self.checking += count;
            self.checking_background = background;
        } else {
            self.status = "更新检查线程已停止，请重新打开应用".into();
        }
    }
    pub(super)fn handle_update(&mut self, event: UpdateEvent) {
        let Some(chapters) = event.chapters else {
            // A follow cover arrived through the shared cache: hand it to the

            // same `cover_ready` map the grid reads, then re-texture lazily.
            if let Some((key, image)) = event.cover {
                self.cover_ready.insert(key, Some(image));
            }
            return;
        };
        self.checking = self.checking.saturating_sub(1);
        if let Some(index) = self.follow_index(&event.source, &event.manga) {
            let mut added = Vec::new();
            let mut ok_list = None;
            {
                let follow = &mut self.follows[index];
                match chapters {
                    Ok(list) => {
                        follow.reading_order = chapter_list(&list, "", false)
                            .iter()
                            .map(|chapter| chapter.key.clone())
                            .collect();
                        let keys: Vec<String> =
                            list.iter().map(|chapter| chapter.key.clone()).collect();
                        let seen: HashSet<&String> =
                            follow.known.iter().chain(follow.fresh.iter()).collect();
                        added = keys
                            .iter()
                            .filter(|key| !seen.contains(key))
                            .cloned()
                            .collect();
                        follow.apply_check(&keys, follows::now());
                        if !added.is_empty() {
                            self.notify_pending.push(crate::notify::FreshUpdate {
                                source: follow.source_id.clone(),
                                manga: follow.manga_id.clone(),
                                title: follow.title.clone(),
                                added: added.clone(),
                            });
                        }
                        ok_list = Some(list);
                    }
                    Err(error) => {
                        follow.error = Some(error);
                        follow.checked_at = Some(follows::now());
                    }
                }
            }
            if let Some(list) = ok_list {
                self.auto_download_added(index, &list, &added);
            }
        }
        if self.checking == 0 {
            let fresh = self.fresh_follows();
            if self.checking_background {
                // A scheduled check: surface only real news on the top notice

                // bar; the badge count already reflects every check.

                if fresh > 0 {
                    self.follow_notice = Some(format!("追更发现 {fresh} 部作品有新章节"));
                }
            } else {
                self.status = if fresh > 0 {
                    format!("{fresh} 部追更有新章节")
                } else {
                    "追更已是最新".into()
                };
            }
            self.checking_background = false;
            self.save_follows();
        }
    }
    pub(crate) fn set_auto_download(&mut self, mode: &str) {
        self.auto_download = if matches!(mode, "all" | "latest1" | "latest3") {
            mode.to_string()
        } else {
            "off".into()
        };
    }
    pub(crate) fn auto_download(&self) -> &str {
        &self.auto_download
    }
    /// Queues freshly found chapters per the `auto-download` setting: at
    /// most 5 per work per check, newest = last in reading order, skipping
    /// anything already downloaded or queued. Never touches `fresh`.
    pub(super)fn auto_download_added(&mut self, index: usize, list: &[Chapter], added: &[String]) {
        if self.auto_download == "off" || added.is_empty() {
            return;
        }
        let take = match self.auto_download.as_str() {
            "latest1" => 1,
            "latest3" => 3,
            _ => 5,
        };
        let added: std::collections::HashSet<&str> =
            added.iter().map(|key| key.as_str()).collect();
        let ordered = chapter_list(list, "", false);
        let mut pick: Vec<Chapter> = ordered
            .iter()
            .filter(|chapter| added.contains(chapter.key.as_str()))
            .rev()
            .take(take)
            .cloned()
            .collect();
        // Queue oldest-first so the download order matches reading order.
        pick.reverse();
        let Some(follow) = self.follows.get(index).cloned() else {
            return;
        };
        let Ok(mut manga) = self.follow_manga(&follow) else {
            return;
        };
        manga.source_key = follow.source_id.clone();
        let snapshot_key = match save_manga_snapshot(&self.root, &manga) {
            Ok(key) => key,
            Err(_) => return,
        };
        let mut queued = false;
        for chapter in pick {
            if !self.chapter_needs_download(&follow.source_id, &manga.key, &chapter) {
                continue;
            }
            let mut task = DownloadTask::from_chapter(&follow.source_id, &manga, &chapter);
            task.manga_snapshot = Some(snapshot_key.clone());
            task.auto = true;
            if matches!(
                enqueue_task(&mut self.tasks, task),
                EnqueueOutcome::Added
            ) {
                queued = true;
            }
        }
        if queued {
            let _ = self.persist();
            self.pump();
        }
    }
    /// 自动下载 tasks still in the queue for one followed work.
    pub(super)fn auto_downloading(&self, source: &str, manga: &str) -> usize {
        self.tasks
            .iter()
            .filter(|task| {
                task.auto
                    && task.source_id == source
                    && task.manga_id == manga
                    && matches!(
                        task.status,
                        DownloadStatus::Queued | DownloadStatus::Running | DownloadStatus::Paused
                    )
            })
            .count()
    }
    /// Minutes between scheduled checks; `0` disables the timer.
    pub fn follow_interval(&self) -> u64 {
        self.follow_interval_min
    }
    /// Persists a new cadence and re-arms the timer from now.
    pub fn set_follow_interval(&mut self, minutes: u64) {
        self.follow_interval_min = minutes;
        self.follow_next_due = None;
        self.schedule_follow_check();
    }
    /// keeps its `repaint_delay` far above the acceptance floor.
    pub(super)fn schedule_follow_check(&self) {
        if self.follow_interval_min == 0 {
            return;
        }
        self.ctx
            .request_repaint_after(std::time::Duration::from_secs(
                self.follow_interval_min * 60,
            ));
    }
    /// Runs on every frame; costs nothing while the timer is not due.
    pub(super)fn poll_follow_schedule(&mut self) {
        let Some(due) = self.follow_next_due else {
            // First frame (or a freshly enabled interval): arm and wait.
            if self.follow_interval_min > 0 && !self.follows.is_empty() {
                self.follow_next_due = Some(
                    std::time::Instant::now()
                        + std::time::Duration::from_secs(self.follow_interval_min * 60),
                );
                self.schedule_follow_check();
            }
            return;
        };
        let now = std::time::Instant::now();
        if now < due {
            // Another repaint may have consumed the earlier deadline; re-arm
            // the remainder so an idle app still wakes on time.
            self.ctx.request_repaint_after(due - now);
            return;
        }
        self.follow_next_due = None;
        if self.follow_interval_min == 0 || self.follows.is_empty() {
            return;
        }
        // Re-arm before checking so a long check cannot double-fire.
        self.follow_next_due = Some(
            std::time::Instant::now()
                + std::time::Duration::from_secs(self.follow_interval_min * 60),
        );
        self.schedule_follow_check();
        // Shares the manual-check mutex: an in-flight batch is skipped.
        self.check_updates(true);
    }
    /// Fresh-chapter items collected since the last drain — the shell turns
    /// them into system notifications (foregrounded frames only mark them).
    pub(crate) fn take_notify_updates(&mut self) -> Vec<crate::notify::FreshUpdate> {
        std::mem::take(&mut self.notify_pending)
    }
    /// The download worker restarted after a panic: reload the persisted
    /// task file and re-queue whatever was mid-flight.
    pub(crate) fn reload_download_queue(&mut self) {
        let (tasks, warning) = load_queue(&self.root);
        self.tasks = tasks;
        self.ready = pending_imports(&self.tasks).into();
        if let Some(warning) = warning {
            self.status = warning;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sources::test_support::*;

    #[test]
    fn update_checks_flip_newest_first_lists_into_reading_order() {
        let root = tempfile::tempdir().unwrap();
        let mut sources = test_sources(root.path());
        sources.selected_source = "source.a".into();
        sources.toggle_follow(&manga("source.a", "m"));
        sources.checking = 1;
        // Many sources hand chapters back newest-first; the persisted order
        // must match the detail page's reading order, oldest first.
        sources.handle_update(UpdateEvent {
            source: "source.a".into(),
            manga: "m".into(),
            chapters: Some(Ok(
                [("c3", 3.), ("c1", 1.), ("c2", 2.)]
                    .iter()
                    .map(|(key, number)| Chapter {
                        key: (*key).into(),
                        chapter_number: Some((*number) as f32),
                        ..Default::default()
                    })
                    .collect(),
            )),
            cover: None,
        });
        assert_eq!(
            sources.follows[0].reading_order,
            ["c1", "c2", "c3"],
            "update checks store chapters in reading order"
        );
    }
    #[test]
    fn followed_manga_counts_new_chapters_until_opened() {
        let root = tempfile::tempdir().unwrap();
        let mut sources = test_sources(root.path());
        sources.selected_source = "source.a".into();
        let book = manga("source.a", "m");
        let chapter = |key: &str| Chapter {
            key: key.into(),
            ..Default::default()
        };
        sources.chapters = vec![chapter("c1"), chapter("c2")];
        sources.toggle_follow(&book);
        assert_eq!(sources.follows.len(), 1);
        assert_eq!(
            follows::load(root.path()).unwrap().len(),
            1,
            "saved immediately"
        );
        assert_eq!(sources.follow_manga(&sources.follows[0]).unwrap().key, "m");

        sources.checking = 1;
        sources.handle_update(UpdateEvent {
            source: "source.a".into(),
            manga: "m".into(),
            chapters: Some(Ok(
                ["c1", "c2", "c3"]
                    .iter()
                    .map(|key| Chapter {
                        key: (*key).into(),
                        ..Default::default()
                    })
                    .collect(),
            )),
            cover: None,
        });
        assert_eq!(sources.checking, 0);
        assert_eq!(sources.fresh_follows(), 1);
        assert_eq!(
            follows::load(root.path()).unwrap()[0].fresh,
            vec!["c3".to_string()]
        );
        // The stored reading order is oldest-first even though the source
        // listed chapters in its own order.
        assert_eq!(
            follows::load(root.path()).unwrap()[0].reading_order,
            ["c1", "c2", "c3"]
        );

        sources.request_id = 7;
        sources.current_request.store(7, Ordering::Relaxed);
        let (tx, rx) = mpsc::channel();
        sources.rx = rx;
        tx.send((
            7,
            Ok(Reply::Chapters(
                "source.a".into(),
                book.clone(),
                vec![chapter("c1"), chapter("c2"), chapter("c3")],
            )),
        ))
        .unwrap();
        sources.poll();
        assert_eq!(
            sources.fresh_follows(),
            0,
            "opening the manga marks chapters seen"
        );

        sources.toggle_follow(&book);
        assert!(sources.follows.is_empty());
        assert!(follows::load(root.path()).unwrap().is_empty());
    }
    // -----------------------------------------------------------------
    #[test]
    fn follow_schedule_due_fires_once_and_rearms() {
        let root = tempfile::tempdir().unwrap();
        let mut sources = test_sources(root.path());
        let book = manga("source.a", "m");
        sources.chapters = vec![Chapter {
            key: "c1".into(),
            ..Default::default()
        }];
        sources.toggle_follow(&book);
        // A manga snapshot is needed for check_follows to build a batch —

        // toggle_follow saves one already, so the follow has a snapshot.
        assert!(sources.follow_manga(&sources.follows[0]).is_ok());
        // toggle_follow itself queued a cover check; the scheduler assertions

        // below start from a quiet state.
        sources.checking = 0;
        // Not due yet: nothing happens.
        sources.follow_next_due =
            Some(std::time::Instant::now() + std::time::Duration::from_secs(3600));
        sources.poll_follow_schedule();
        assert_eq!(sources.checking, 0);
        // Due: the batch starts once and the timer rearms for the full

        // interval instead of refiring on the next frame.
        sources.follow_next_due =
            Some(std::time::Instant::now() - std::time::Duration::from_secs(1));
        sources.poll_follow_schedule();
        assert_eq!(sources.checking, 1, "due check dispatched once");
        assert!(sources.checking_background, "scheduled check is background");
        let due = sources.follow_next_due.expect("timer rearmed");
        assert!(due > std::time::Instant::now() + std::time::Duration::from_secs(3500));
        // The in-flight batch blocks the next manual/scheduled start.
        sources.follow_next_due =
            Some(std::time::Instant::now() - std::time::Duration::from_secs(1));
        sources.poll_follow_schedule();
        assert_eq!(sources.checking, 1, "in-flight check is not duplicated");
    }
    #[test]
    fn follow_schedule_disabled_and_empty_never_fire() {
        let root = tempfile::tempdir().unwrap();
        let mut sources = test_sources(root.path());
        // No follows at all: even a due timer does nothing.
        sources.follow_next_due =
            Some(std::time::Instant::now() - std::time::Duration::from_secs(1));
        sources.poll_follow_schedule();
        assert_eq!(sources.checking, 0);
        assert!(sources.follow_next_due.is_none());
        // Disabled interval never arms.
        sources.set_follow_interval(0);
        let book = manga("source.a", "m");
        sources.chapters = vec![Chapter {
            key: "c1".into(),
            ..Default::default()
        }];
        sources.toggle_follow(&book);
        sources.checking = 0;
        sources.poll_follow_schedule();
        assert!(sources.follow_next_due.is_none());
        assert_eq!(sources.checking, 0);
    }
    #[test]
    fn background_check_completion_goes_to_notice_not_status() {
        let root = tempfile::tempdir().unwrap();
        let mut sources = test_sources(root.path());
        let book = manga("source.a", "m");
        sources.chapters = vec![Chapter {
            key: "c1".into(),
            ..Default::default()
        }];
        sources.toggle_follow(&book);
        sources.status.clear();
        sources.checking = 1;
        sources.checking_background = true;
        sources.handle_update(UpdateEvent {
            source: "source.a".into(),
            manga: "m".into(),
            chapters: Some(Ok(
                ["c1", "c2"]
                    .iter()
                    .map(|key| Chapter {
                        key: (*key).into(),
                        ..Default::default()
                    })
                    .collect(),
            )),
            cover: None,
        });
        assert_eq!(sources.checking, 0);
        assert_eq!(
            sources.follow_notice.as_deref(),
            Some("追更发现 1 部作品有新章节")
        );
        assert!(
            sources.status.is_empty(),
            "background checks keep the sources status clear"
        );
    }
    #[test]
    fn auto_download_enqueues_up_to_five_and_keeps_the_fresh_badge() {
        let root = tempfile::tempdir().unwrap();
        let mut sources = test_sources(root.path());
        sources.set_auto_download("all");
        sources.selected_source = "source.a".into();
        sources.toggle_follow(&manga("source.a", "m"));
        sources.follows[0].mark_seen(&["c1".to_string()]);
        sources.checking = 1;
        let chapters: Vec<Chapter> = (1..=7)
            .map(|n| numbered(&format!("c{n}"), n as f32))
            .collect();
        sources.handle_update(update("source.a", "m", chapters));
        // Six new chapters, but at most five queue per work per check — the
        // newest five in reading order, all flagged `auto`.
        let queued: Vec<&str> = sources
            .tasks
            .iter()
            .map(|task| task.chapter_id.as_str())
            .collect();
        assert_eq!(queued, ["c3", "c4", "c5", "c6", "c7"]);
        assert!(sources.tasks.iter().all(|task| task.auto));
        assert_eq!(
            sources.follows[0].fresh.len(),
            6,
            "auto-download must not clear the new-chapter badge"
        );
        assert_eq!(sources.auto_downloading("source.a", "m"), 5);
        // Nothing else enqueued on the next check of an unchanged list.
        sources.checking = 1;
        sources.handle_update(update(
            "source.a",
            "m",
            (1..=7)
                .map(|n| numbered(&format!("c{n}"), n as f32))
                .collect(),
        ));
        assert_eq!(sources.tasks.len(), 5);
    }
    #[test]
    fn auto_download_latest_modes_pick_the_newest_chapters() {
        let root = tempfile::tempdir().unwrap();
        let mut sources = test_sources(root.path());
        sources.set_auto_download("latest1");
        sources.selected_source = "source.a".into();
        sources.toggle_follow(&manga("source.a", "m"));
        sources.follows[0].mark_seen(&["c1".to_string()]);
        sources.checking = 1;
        sources.handle_update(update(
            "source.a",
            "m",
            vec![numbered("c1", 1.), numbered("c2", 2.), numbered("c3", 3.)],
        ));
        assert_eq!(
            sources
                .tasks
                .iter()
                .map(|task| task.chapter_id.as_str())
                .collect::<Vec<_>>(),
            ["c3"]
        );
        // latest3 picks the newest three; off queues nothing.
        sources.tasks.clear();
        sources.set_auto_download("latest3");
        sources.follows[0].mark_seen(
            &["c1", "c2", "c3"].iter().map(|s| s.to_string()).collect::<Vec<_>>(),
        );
        sources.checking = 1;
        sources.handle_update(update(
            "source.a",
            "m",
            (1..=7)
                .map(|n| numbered(&format!("c{n}"), n as f32))
                .collect(),
        ));
        assert_eq!(
            sources
                .tasks
                .iter()
                .map(|task| task.chapter_id.as_str())
                .collect::<Vec<_>>(),
            ["c5", "c6", "c7"]
        );
        sources.tasks.clear();
        sources.set_auto_download("off");
        sources.follows[0].mark_seen(&[]);
        sources.follows[0].known.clear();
        sources.checking = 1;
        sources.handle_update(update(
            "source.a",
            "m",
            vec![numbered("c8", 8.)],
        ));
        assert!(sources.tasks.is_empty());
    }
    #[test]
    fn follows_on_uninstalled_sources_are_not_checked() {
        let root = tempfile::tempdir().unwrap();
        let mut sources = test_sources(root.path());
        sources.follows.push(Follow {
            source_id: "gone.source".into(),
            manga_id: "m".into(),
            title: "离线作品".into(),
            source_removed: true,
            ..Default::default()
        });
        sources.check_updates(false);
        assert_eq!(sources.checking, 0, "no engine is ever loaded for it");
        assert!(sources.status.is_empty() || !sources.status.contains("无法检查更新"));
    }
}
