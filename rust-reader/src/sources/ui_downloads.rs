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
use super::migrate::*;
use super::storage::*;
use super::web::*;
use super::repos::*;
use super::ui_browse::*;
use super::ui_detail::*;
use super::ui_library::*;
#[cfg(any(test, debug_assertions))]
use super::test_support::*;

impl Sources {
    pub(super)fn download_row(&mut self, ui: &mut Ui, task: &DownloadTask) {
        // A long failure message used to share one horizontal row with the

        // title and the action buttons. That pushed the buttons past the right

        // edge on a 390px screen and also widened the whole page, which in turn

        // stretched the repository URL input. Keep the row inside the page

        // width and stack title / state / reason / actions.

        let palette = design::palette(ui.visuals().dark_mode);
        ui.vertical(|ui| {
            ui.set_max_width(ui.available_width());
            let title = if self.selected_manga.is_none() {
                format!("{} · {}", task.manga_title, task.chapter_title)
            } else {
                task.chapter_title.clone()
            };
            ui.add(Label::new(RichText::new(title).strong()).wrap());
            let state = match task.status {
                DownloadStatus::Running if task.pages_total > 0 => format!(
                    "{} {}/{}",
                    task.status.label(),
                    task.pages_done,
                    task.pages_total
                ),
                DownloadStatus::Done if task.cbz.is_some() && task.linked => "已完成".to_string(),
                DownloadStatus::Done if task.cbz.is_some() => "已完成，待加入书架".to_string(),
                status => status.label().to_string(),
            };
            ui.add(Label::new(RichText::new(state).color(palette.muted)).wrap());
            if task.pages_total > 0 && task.status == DownloadStatus::Running {
                ui.add(
                    ProgressBar::new(task.pages_done as f32 / task.pages_total as f32)
                        .fill(palette.accent)
                        .desired_height(4.),
                );
            }
            if let Some(error) = &task.error {
                ui.add(Label::new(error).wrap());
            }
            ui.add_space(4.);
            ui.horizontal_wrapped(|ui| match task.status {
                DownloadStatus::Queued => {
                    if ui.add(quiet_action("开始")).clicked() {
                        self.set_task_queued(&task.id);
                    }
                    if ui.add(quiet_action("取消")).clicked() {
                        self.request_cancel(&task.id);
                    }
                }
                DownloadStatus::Running => {
                    if ui.add(quiet_action("暂停")).clicked() {
                        self.request_pause();
                    }
                    if ui.add(quiet_action("取消")).clicked() {
                        self.request_cancel(&task.id);
                    }
                }
                DownloadStatus::Paused => {
                    if ui.add(quiet_action("继续")).clicked() {
                        self.set_task_queued(&task.id);
                    }
                    if ui.add(quiet_action("取消")).clicked() {
                        self.request_cancel(&task.id);
                    }
                }
                DownloadStatus::Failed => {
                    if ui.add(quiet_action("重试")).clicked() {
                        self.retry_task(task);
                    }
                    if ui.add(quiet_action("取消")).clicked() {
                        self.request_cancel(&task.id);
                    }
                }
                DownloadStatus::Done => {
                    if self.library_chapters.contains_key(&task.id) {
                        if ui.add(quiet_action("阅读")).clicked() {
                            self.read_downloaded(task);
                        }
                    } else {
                        if self.import_attempts.contains(&task.id) {
                            ui.add(Label::new("正在加入书架…"));
                        } else if ui.add(quiet_action("加入书架 / 重试导入")).clicked() {
                            self.requeue_import(&task.id);
                        }
                    }
                }
            });
        });
        ui.separator();
        ui.add_space(10.);
    }
}

