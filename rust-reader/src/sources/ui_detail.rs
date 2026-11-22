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
use super::ui_library::*;
use super::ui_downloads::*;
#[cfg(any(test, debug_assertions))]
use super::test_support::*;

pub(super) fn chapter_name(chapter: &Chapter) -> String {
    chapter
        .title
        .clone()
        .filter(|title| !title.trim().is_empty())
        .unwrap_or_else(|| {
            chapter
                .chapter_number
                .map(|number| format!("第 {number} 话"))
                .unwrap_or_else(|| chapter.key.clone())
        })
}

/// Chapter number used by 换源 resume: the source's own number, else the
/// first integer parsed from the chapter title.
pub(super) fn chapter_number_value(chapter: &Chapter) -> Option<f64> {
    chapter
        .chapter_number
        .filter(|number| number.is_finite())
        .map(|number| number as f64)
        .or_else(|| {
            chapter
                .title
                .as_deref()
                .and_then(crate::trackers::chapter_number)
                .map(|number| number as f64)
        })
}

pub(super) fn chapter_list(chapters: &[Chapter], query: &str, newest_first: bool) -> Vec<Chapter> {
    let first_volume = chapters
        .iter()
        .filter_map(|chapter| chapter.volume_number.filter(|number| number.is_finite()))
        .min_by(f32::total_cmp)
        .unwrap_or(0.);
    let needle = query.trim().to_lowercase();
    let mut visible: Vec<_> = chapters
        .iter()
        .filter_map(|chapter| {
            let name = chapter_name(chapter);
            let number = chapter.chapter_number.filter(|number| number.is_finite());
            if !needle.is_empty()
                && !name.to_lowercase().contains(&needle)
                && !number.is_some_and(|number| number.to_string().contains(&needle))
            {
                return None;
            }
            let volume = chapter
                .volume_number
                .filter(|number| number.is_finite())
                .unwrap_or(first_volume);
            Some((chapter, name, volume, number))
        })
        .collect();
    visible.sort_by(|left, right| {
        left.2
            .total_cmp(&right.2)
            .then_with(|| match (left.3, right.3) {
                (Some(left), Some(right)) => left.total_cmp(&right),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => std::cmp::Ordering::Equal,
            })
            .then_with(|| yejian_core::natural_cmp(&left.1, &right.1))
            .then_with(|| left.0.key.cmp(&right.0.key))
    });
    if newest_first {
        visible.reverse();
    }
    visible
        .into_iter()
        .map(|(chapter, _, _, _)| chapter.clone())
        .collect()
}

pub(super) fn format_epoch_date(epoch: i64) -> String {
    let z = epoch.div_euclid(86_400) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year}-{month:02}-{day:02}")
}

pub(super) fn chapter_state_lines(
    locked: bool,
    downloaded: bool,
    can_import: bool,
    progress: Option<(usize, usize)>,
    task: Option<&ChapterQueueState>,
) -> Vec<String> {
    if locked {
        return vec!["暂不可阅读".into()];
    }
    let mut lines = Vec::new();
    let finished = progress.is_some_and(|(page, count)| count > 0 && page + 1 >= count)
        || task.is_some_and(|task| {
            task.total > 0 && task.read_page.is_some_and(|page| page + 1 >= task.total)
        });
    if finished {
        lines.push("已读".into());
    } else if let Some((page, count)) = progress.filter(|(_, count)| *count > 0) {
        lines.push(format!("读到 {}/{}", page + 1, count));
    } else if let Some(task) = task
        .filter(|task| task.total > 0 && task.read_page.is_some_and(|page| page + 1 < task.total))
    {
        lines.push(format!(
            "读到 {}/{}",
            task.read_page.unwrap() + 1,
            task.total
        ));
    }
    if downloaded {
        lines.push("已下载".into());
    } else if can_import {
        lines.push("待加入书架".into());
    } else if let Some(task) = task {
        match task.status {
            DownloadStatus::Failed => lines.push("失败 · 重试".into()),
            DownloadStatus::Done => lines.push("已下载".into()),
            _ if task.total > 0 => {
                lines.push(format!("下载中 {}/{}", task.done, task.total));
            }
            _ => lines.push(task.status.label().into()),
        }
    }
    lines
}

impl Sources {
    pub(super)fn open_follow(&mut self, index: usize) -> bool {
        let Some(follow) = self.follows.get(index).cloned() else {
            return false;
        };
        if !self
            .installed
            .iter()
            .any(|source| source.manifest.id == follow.source_id)
        {
            self.status = format!(
                "书源 {} 未安装，无法打开《{}》",
                follow.source_id, follow.title
            );
            return false;
        }
        let manga = match self.follow_manga(&follow) {
            Ok(manga) => manga,
            Err(error) => {
                self.status = format!("无法打开《{}》：{error:#}", follow.title);
                return false;
            }
        };
        if self.selected_source == follow.source_id {
            self.open_manga(manga);
        } else {
            self.select_source(&follow.source_id);
            self.pending_open = Some(manga);
        }
        true
    }
    pub(super)fn detail_ui(&mut self, ui: &mut Ui) {
        let dark = ui.visuals().dark_mode;
        let palette = design::palette(dark);
        let Some(manga) = self.selected_manga.clone() else {
            return;
        };
        let source = self.source_for(&manga);
        let width = ui.available_width();

        let back_label = if self.shelf_origin {
            "返回书架"
        } else {
            "返回搜索结果"
        };
        let back = ui.add(quiet_action(back_label).min_size(vec2(88., 44.)));
        self.hitboxes.insert("source-detail-back".into(), back.rect);
        if back.clicked() {
            self.selected_manga = None;
            self.chapters.clear();
            if self.shelf_origin {
                self.shelf_origin = false;
                self.shelf_return = true;
            }
            return;
        }
        ui.add_space(8.);

        let states = self.chapter_states(&source, &manga.key);
        let ordered = chapter_list(&self.chapters, "", false);
        let (resume, started) = self.resume_chapter(&source, &manga, &ordered, &states);
        let resume_key = resume.as_ref().map(|chapter| chapter.key.clone());
        let followed = self.follow_index(&source, &manga.key).is_some();
        let missing = self
            .chapters
            .iter()
            .filter(|chapter| {
                !chapter.locked
                    && !self.library_chapters.contains_key(&identity_id(
                        &source,
                        &manga.key,
                        &chapter.key,
                    ))
                    && states
                        .get(&chapter.key)
                        .is_none_or(|state| state.status == DownloadStatus::Failed)
            })
            .count();
        // Header: cover beside the meta column on wide layouts, centered above

        // it on narrow ones.

        let cover_url = manga.cover.clone().unwrap_or_default();
        let texture = self.cover_image(ui.ctx(), &source, &cover_url);
        let mut want = Vec::new();
        if texture.is_none() && !cover_url.is_empty() {
            want.push(CoverReq {
                key: cover_key(&source, &cover_url),
                source: source.clone(),
                url: cover_url,
            });
        }
        let wide = width >= 700.;
        let cover_size = if wide {
            vec2(200., 300.)
        } else {
            vec2(140., 210.)
        };
        let heading = TextStyle::Heading.resolve(ui.style());
        let mut read = false;
        let mut follow = false;
        let mut download_missing = false;
        let mut migrate = false;
        let mut meta = |ui: &mut Ui, this: &mut Self| {
            ui.add(Label::new(RichText::new(&manga.title).heading().strong()).wrap());
            if let Some(authors) = manga.authors.as_ref().filter(|list| !list.is_empty()) {
                ui.label(RichText::new(authors.join(" / ")).color(palette.muted));
            }
            let mut chips: Vec<String> = match manga.status {
                PublishingStatus::Ongoing => vec!["连载中".into()],
                PublishingStatus::Completed => vec!["已完结".into()],
                PublishingStatus::Cancelled => vec!["已停更".into()],
                PublishingStatus::Hiatus => vec!["休刊中".into()],
                PublishingStatus::Unknown => Vec::new(),
            };
            chips.extend(
                manga
                    .tags
                    .iter()
                    .flatten()
                    .take(6usize.saturating_sub(chips.len()))
                    .cloned(),
            );
            if !chips.is_empty() {
                ui.horizontal_wrapped(|ui| {
                    for chip in chips {
                        Frame::new()
                            .fill(palette.soft)
                            .corner_radius(10.)
                            .inner_margin(vec2(8., 3.))
                            .show(ui, |ui| {
                                ui.label(RichText::new(chip).small().color(palette.muted));
                            });
                    }
                });
            }
            ui.add_space(4.);
            ui.horizontal_wrapped(|ui| {
                let label = if started {
                    format!(
                        "继续阅读 · {}",
                        resume
                            .as_ref()
                            .map_or("".into(), |chapter| chapter_name(chapter))
                    )
                } else {
                    "开始阅读".into()
                };
                let button = ui.add_enabled(resume.is_some(), design::primary(&label, dark));
                this.hitboxes
                    .insert("source-detail-read".into(), button.rect);
                if button.clicked() {
                    read = true;
                }
                let button = if followed {
                    ui.add(quiet_action("已在书架 · 移出").min_size(vec2(0., 48.)))
                } else {
                    ui.add(quiet_action("加入书架").min_size(vec2(0., 48.)))
                };
                this.hitboxes
                    .insert("source-detail-follow".into(), button.rect);
                if button.clicked() {
                    follow = true;
                }
                let button = ui.add_enabled(
                    missing > 0 && !this.busy,
                    quiet_action(&format!("下载全部未下载（{missing}）")).min_size(vec2(0., 48.)),
                );
                this.hitboxes
                    .insert("source-detail-missing".into(), button.rect);
                if button.clicked() {
                    download_missing = true;
                }
                let button = ui.add_enabled(
                    followed,
                    quiet_action("换源").min_size(vec2(0., 48.)),
                );
                this.hitboxes
                    .insert("source-detail-migrate".into(), button.rect);
                if !followed {
                    ui.label(
                        RichText::new("加入书架后可换源")
                            .color(palette.muted)
                            .small(),
                    );
                }
                if button.clicked() {
                    migrate = true;
                }
            });
        };
        if wide {
            ui.horizontal(|ui| {
                let (cover_rect, _) = ui.allocate_exact_size(cover_size, Sense::hover());
                paint_cover(
                    ui.painter(),
                    cover_rect,
                    texture.as_ref(),
                    &manga.title,
                    heading.clone(),
                    &palette,
                );
                ui.add_space(16.);
                ui.vertical(|ui| {
                    ui.set_max_width(ui.available_width());
                    meta(ui, self);
                });
            });
        } else {
            ui.vertical_centered(|ui| {
                let (cover_rect, _) = ui.allocate_exact_size(cover_size, Sense::hover());
                paint_cover(
                    ui.painter(),
                    cover_rect,
                    texture.as_ref(),
                    &manga.title,
                    heading.clone(),
                    &palette,
                );
            });
            ui.add_space(8.);
            meta(ui, self);
        }
        self.plan_covers(want);
        if read && let Some(chapter) = &resume {
            self.read_detail_chapter(&manga, chapter, &states);
        }
        if follow {
            self.toggle_follow(&manga);
        }
        if download_missing {
            self.enqueue_missing(&manga);
        }
        if migrate {
            self.open_migrate();
        }
        // Description collapses to three lines; longer text offers 展开/收起.

        if let Some(description) = manga
            .description
            .as_ref()
            .filter(|text| !text.trim().is_empty())
        {
            ui.add_space(10.);
            let font = TextStyle::Body.resolve(ui.style());
            let line = ui.text_style_height(&TextStyle::Body);
            let painter = ui.painter().clone();
            let full = painter.layout_job(egui::text::LayoutJob::simple(
                description.clone(),
                font.clone(),
                palette.muted,
                width,
            ));
            let clipped = !self.description_expanded && full.size().y > line * 3.5;
            let galley = if clipped {
                let mut job =
                    egui::text::LayoutJob::simple(description.clone(), font, palette.muted, width);
                job.wrap.max_rows = 3;
                painter.layout_job(job)
            } else {
                full.clone()
            };
            let (text_rect, _) =
                ui.allocate_exact_size(vec2(width, galley.size().y), Sense::hover());
            painter.galley(text_rect.min, galley, palette.muted);
            if clipped || (self.description_expanded && full.size().y > line * 3.5) {
                let toggle = ui.add(quiet_action(if self.description_expanded {
                    "收起"
                } else {
                    "展开"
                }));
                self.hitboxes
                    .insert("source-detail-description".into(), toggle.rect);
                if toggle.clicked() {
                    self.description_expanded = !self.description_expanded;
                }
            }
        }
        ui.add_space(12.);
        // Tasks for chapters the source no longer lists still need a place to

        // retry; everything else is represented by row state.

        let listed: HashSet<&str> = self
            .chapters
            .iter()
            .map(|chapter| chapter.key.as_str())
            .collect();
        let orphans: Vec<DownloadTask> = self
            .tasks
            .iter()
            .filter(|task| {
                task.source_id == source
                    && task.manga_id == manga.key
                    && !listed.contains(task.chapter_id.as_str())
                    && (task.status != DownloadStatus::Done || !task.linked)
            })
            .cloned()
            .collect();
        if !orphans.is_empty() {
            disclosure(
                ui,
                &format!("其他下载任务（{}）", orphans.len()),
                false,
                |ui| {
                    for task in &orphans {
                        self.download_row(ui, task);
                    }
                },
            );
        }

        design::section(ui, &format!("章节 · {}", self.chapters.len()));
        ui.add(
            TextEdit::singleline(&mut self.chapter_query)
                .hint_text("查找章节名称或编号")
                .desired_width(ui.available_width()),
        );
        ui.horizontal_wrapped(|ui| {
            ui.selectable_value(&mut self.chapters_newest_first, false, "从头开始");
            ui.selectable_value(&mut self.chapters_newest_first, true, "最新在前");
            if !self.chapter_query.is_empty() && ui.add(quiet_action("清除查找")).clicked() {
                self.chapter_query.clear();
            }
        });
        let chapters = chapter_list(
            &self.chapters,
            &self.chapter_query,
            self.chapters_newest_first,
        );
        if chapters.is_empty() {
            ui.label(if self.chapters.is_empty() {
                "书源尚未提供章节。"
            } else {
                "没有匹配的章节，试试其他名称或编号。"
            });
        }
        let body = TextStyle::Body.resolve(ui.style());
        let small = TextStyle::Small.resolve(ui.style());
        let small_h = ui.text_style_height(&TextStyle::Small);
        let mut open: Option<Chapter> = None;
        let mut enqueue: Option<Chapter> = None;
        for (index, chapter) in chapters.iter().enumerate() {
            let name = chapter_name(chapter);
            let id = identity_id(&source, &manga.key, &chapter.key);
            let state = states.get(&chapter.key);
            let downloaded = self.library_chapters.get(&id).cloned();
            let can_import = downloaded.is_none()
                && state
                    .is_some_and(|state| state.status == DownloadStatus::Done && state.has_archive)
                && !self.import_attempts.contains(&id)
                && !self.ready.iter().any(|queued| queued == &id);
            let failed = state.is_some_and(|state| state.status == DownloadStatus::Failed);
            let show_download = downloaded.is_none()
                && !can_import
                && !chapter.locked
                && (state.is_none() || failed);
            let progress = self
                .chapter_progress
                .get(&id)
                .map(|&(page, count, _)| (page, count));
            let lines = chapter_state_lines(
                chapter.locked,
                downloaded.is_some(),
                can_import,
                progress,
                state,
            );
            let sub = chapter.date_uploaded.map(format_epoch_date).or_else(|| {
                chapter
                    .scanlators
                    .as_ref()
                    .and_then(|list| list.first().cloned())
            });
            // Layout: name + sub line left, state lines right-aligned, an

            // optional 仅下载 button at the far right edge.

            let state_w = 104.;
            let button_w = if show_download { 76. } else { 0. };
            let left_w = (width - state_w - button_w - 24.).max(60.);
            let painter = ui.painter().clone();
            let mut name_job = egui::text::LayoutJob::simple(
                name.clone(),
                body.clone(),
                if chapter.locked {
                    palette.muted
                } else {
                    palette.ink
                },
                left_w,
            );
            name_job.wrap.max_rows = 2;
            let name_galley = painter.layout_job(name_job);
            let sub_galley = sub.as_ref().map(|text| {
                let mut job = egui::text::LayoutJob::simple(
                    text.clone(),
                    small.clone(),
                    palette.muted,
                    left_w,
                );
                job.wrap.max_rows = 1;
                painter.layout_job(job)
            });
            let state_galleys: Vec<Arc<Galley>> = lines
                .iter()
                .map(|line| {
                    painter.layout_job(egui::text::LayoutJob::simple(
                        line.clone(),
                        small.clone(),
                        palette.muted,
                        state_w,
                    ))
                })
                .collect();
            let text_h = name_galley.size().y
                + sub_galley
                    .as_ref()
                    .map_or(0., |galley| galley.size().y + 2.);
            let row_h = (text_h + 24.).max(56.);
            let (row_rect, _) = ui.allocate_exact_size(vec2(width, row_h), Sense::hover());
            let click_rect = if show_download {
                Rect::from_min_max(
                    row_rect.min,
                    pos2(row_rect.right() - button_w - 8., row_rect.bottom()),
                )
            } else {
                row_rect
            };
            let response = ui.interact(
                click_rect,
                ui.scope_id().with(("source-chapter", index)),
                if chapter.locked {
                    Sense::hover()
                } else {
                    Sense::click()
                },
            );
            response.widget_info(|| WidgetInfo::labeled(Role::Button, !chapter.locked, &name));
            self.hitboxes
                .insert(format!("source-chapter-{index}"), click_rect);
            if response.clicked() {
                open = Some(chapter.clone());
            }
            if response.hovered() {
                painter.rect_filled(row_rect, 6., palette.soft);
            }
            if resume_key.as_deref() == Some(chapter.key.as_str()) {
                painter.rect_filled(
                    Rect::from_min_size(row_rect.min, vec2(3., row_rect.height())),
                    1.5,
                    palette.accent,
                );
            }
            let text_top = row_rect.top() + (row_h - text_h) / 2.;
            painter.galley(
                pos2(row_rect.left() + 4., text_top),
                name_galley,
                palette.ink,
            );
            if let Some(galley) = sub_galley {
                painter.galley(
                    pos2(
                        row_rect.left() + 4.,
                        row_rect.bottom() - 12. - galley.size().y,
                    ),
                    galley,
                    palette.muted,
                );
            }
            let lines_h = state_galleys.len() as f32 * (small_h + 2.) - 2.;
            for (row, galley) in state_galleys.iter().enumerate() {
                painter.galley(
                    pos2(
                        row_rect.right() - button_w - 8. - galley.size().x,
                        row_rect.top()
                            + (row_h - lines_h.max(0.)) / 2.
                            + row as f32 * (small_h + 2.),
                    ),
                    galley.clone(),
                    palette.muted,
                );
            }
            if show_download {
                let button_rect = Rect::from_center_size(
                    pos2(row_rect.right() - button_w / 2. - 4., row_rect.center().y),
                    vec2(button_w, 44.),
                );
                let label = if failed { "重试下载" } else { "仅下载" };
                let button = ui.put(
                    button_rect,
                    quiet_action(label).min_size(button_rect.size()),
                );
                self.hitboxes
                    .insert(format!("source-chapter-dl-{index}"), button.rect);
                if button.clicked() {
                    enqueue = Some(chapter.clone());
                }
            }
            painter.line_segment(
                [
                    pos2(row_rect.left() + 4., row_rect.bottom()),
                    pos2(row_rect.right() - 4., row_rect.bottom()),
                ],
                Stroke::new(1., palette.line),
            );
        }
        if let Some(chapter) = open {
            self.read_detail_chapter(&manga, &chapter, &states);
        }
        if let Some(chapter) = enqueue {
            self.enqueue_chapter(&manga, &chapter);
        }
    }
    /// Queue state per chapter key, as the detail page collects it.
    pub(super)fn chapter_states(
        &self,
        source: &str,
        manga: &str,
    ) -> std::collections::HashMap<String, ChapterQueueState> {
        self.tasks
            .iter()
            .filter(|task| task.source_id == source && task.manga_id == manga)
            .map(|task| {
                (
                    task.chapter_id.clone(),
                    ChapterQueueState {
                        status: task.status,
                        has_archive: task.cbz.is_some(),
                        done: task.pages_done,
                        total: task.pages_total,
                        read_page: task.read_page,
                        read_at: task.read_at,
                    },
                )
            })
            .collect()
    }
    /// The shelf asked to resume as soon as the chapter list landed.
    pub(super)fn auto_resume(&mut self) {
        let Some(manga) = self.selected_manga.clone() else {
            return;
        };
        let source = self.source_for(&manga);
        let states = self.chapter_states(&source, &manga.key);
        let ordered = chapter_list(&self.chapters, "", false);
        let (resume, _) = self.resume_chapter(&source, &manga, &ordered, &states);
        if let Some(chapter) = resume {
            self.read_detail_chapter(&manga, &chapter, &states);
        }
    }
    pub(super)fn read_detail_chapter(
        &mut self,
        manga: &Manga,
        chapter: &Chapter,
        states: &std::collections::HashMap<String, ChapterQueueState>,
    ) {
        let source = self.source_for(manga);
        let id = identity_id(&source, &manga.key, &chapter.key);
        if let Some(metadata) = self.library_chapters.get(&id) {
            self.open_chapter = Some(metadata.clone());
        } else if states
            .get(&chapter.key)
            .is_some_and(|state| state.status == DownloadStatus::Done && state.has_archive)
            && !self.import_attempts.contains(&id)
            && !self.ready.iter().any(|queued| queued == &id)
        {
            self.add_downloaded_to_library(&id);
        } else {
            self.read_online(manga, chapter, false);
        }
    }
}

