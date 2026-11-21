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
/// The Aidoku community catalogue, kept as an explicit one-tap choice rather

/// than a silently preinstalled repository.

const COMMUNITY_INDEX: &str = "https://aidoku-community.github.io/sources/index.min.json";
fn language_label(language: &str) -> &str {
    match language {
        "zh" | "zh-CN" | "zh-Hans" => "中文",
        "zh-TW" | "zh-Hant" => "繁體中文",
        "en" => "English",
        "ja" => "日本語",
        "ko" => "한국어",
        _ => language,
    }
}
fn quiet_action(label: &str) -> Button<'_> {
    Button::new(label)
        .min_size(vec2(44., 44.))
        .wrap_mode(egui::TextWrapMode::Extend)
}
fn disclosure(ui: &mut Ui, label: &str, default_open: bool, body: impl FnOnce(&mut Ui)) {
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
/// A readable row instead of using the whole title as a flat grey button.

/// The initial represents a source name; it is not a substitute cover image.

fn source_item(
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
#[derive(Clone)]
enum Task {
    List,
    Fetch(String),
    Install(RepositorySource),
    /// Remove one source directory; follows/downloaded chapters are kept.
    Uninstall(String),
    /// Refresh every saved repository index for update badges.
    FetchAllRepos(Vec<String>),
    Metadata(String),
    Search {
        source: String,
        query: String,
        page: i32,
        filters: Vec<FilterValue>,
    },
    Browse {
        source: String,
        listing: String,
        page: i32,
    },
    Chapters(String, Manga),
    Login {
        source: String,
        key: String,
        /// `Ok((user, password))` for basic login, `Err(cookies)` for web.
        credentials: Result<(String, String), Vec<(String, String)>>,
    },
    /// A `settings.json` button notification forwarded to the source.
    Notify {
        source: String,
        notification: String,
    },
}
enum Reply {
    Installed {
        list: Vec<InstalledSource>,
        updated: Option<String>,
    },
    Repository(RepositoryIndex),
    /// One result per requested repo URL; failures keep the old cache.
    RepoAll(Vec<(String, Result<RepositoryIndex, String>)>),
    Login {
        key: String,
        user: String,
        ok: bool,
        web: bool,
    },
    Metadata {
        filters: Vec<Filter>,
        listings: Vec<Listing>,
        filter_error: Option<String>,
        listing_error: Option<String>,
    },
    Results {
        source: String,
        query: String,
        page: i32,
        entries: Vec<Manga>,
        has_next: bool,
    },
    Chapters(String, Manga, Vec<Chapter>),
    Notified,
}
#[derive(Default, Clone)]
struct BrowseSession {
    query: String,
    filters: Vec<FilterValue>,
    listing: Option<String>,
}
fn chapter_name(chapter: &Chapter) -> String {
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
/// Use one comparable volume group for chapters without explicit volumes,

/// matching the library's reading order (including decimal extra chapters).

/// Result-grid columns: as many `min_cell` covers as the row fits, never

/// fewer than one and never overflowing horizontally.

fn grid_columns(width: f32) -> usize {
    let min_cell = if width >= 600. { 150. } else { 112. };
    (((width + 16.) / (min_cell + 16.)).floor() as usize).max(1)
}
/// Retains queued covers that are still wanted, appends new wants in order

/// and drops the oldest entries once the queue exceeds `COVER_QUEUE_CAP`.

/// `skip` holds keys already in flight or known failed, so they never queue.

fn plan_covers(queue: &mut VecDeque<CoverReq>, want: Vec<CoverReq>, skip: &HashSet<String>) {
    let wanted: HashSet<&str> = want.iter().map(|req| req.key.as_str()).collect();
    queue.retain(|req| wanted.contains(req.key.as_str()));
    for req in want {
        if skip.contains(&req.key) || queue.iter().any(|queued| queued.key == req.key) {
            continue;
        }
        queue.push_back(req);
    }
    while queue.len() > COVER_QUEUE_CAP {
        queue.pop_front();
    }
}
/// Chapter number used by 换源 resume: the source's own number, else the
/// first integer parsed from the chapter title.
fn chapter_number_value(chapter: &Chapter) -> Option<f64> {
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

fn chapter_list(chapters: &[Chapter], query: &str, newest_first: bool) -> Vec<Chapter> {
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
/// `YYYY-MM-DD` for a unix timestamp, so chapter rows can show an upload date

/// without pulling in a calendar dependency.

fn format_epoch_date(epoch: i64) -> String {
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
/// Right-column state lines for one chapter row: reading progress first, then

/// download state. Kept pure so the exact strings are testable.

fn chapter_state_lines(
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
/// Paints a cover texture or the placeholder tile into `rect`, shared by the

/// result grid and the detail header. Both get a 1px `line` border so tiles

/// stay visible on the page; the placeholder uses the tinted `soft`

/// surface rather than blending into the background.

fn paint_cover(
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
/// One finished chapter ready for `Command::ImportChapter`.

////// `id` is the queue task id: after `import` and `link_chapter` both succeed

/// the caller acknowledges it with [`Sources::ack_downloaded`].

pub struct DownloadedChapter {
    pub id: String,
    pub path: PathBuf,
    pub metadata: ChapterMetadata,
    /// Page reached while reading online, carried into the library book.
    pub read_page: Option<usize>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
enum DownloadStatus {
    Queued,
    Running,
    Paused,
    Failed,
    Done,
}
impl DownloadStatus {
    fn label(self) -> &'static str {
        match self {
            Self::Queued => "排队中",
            Self::Running => "下载中",
            Self::Paused => "已暂停",
            Self::Failed => "失败",
            Self::Done => "已完成",
        }
    }
}
/// One persisted download job. It is self-contained: the source engine can be

/// reopened on any thread and `pages_with` is fed the reconstructed

/// Manga/Chapter, so an app restart never needs the original search session.

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DownloadTask {
    id: String,
    source_id: String,
    manga_id: String,
    manga_title: String,
    #[serde(default)]
    manga_url: Option<String>,
    #[serde(default)]
    manga_cover: Option<String>,
    chapter_id: String,
    chapter_title: String,
    #[serde(default)]
    chapter_number: Option<f64>,
    #[serde(default)]
    volume_number: Option<f64>,
    #[serde(default)]
    chapter_url: Option<String>,
    #[serde(default)]
    chapter_language: Option<String>,
    /// Id of the stored snapshot of the complete `Manga` object passed to

    /// `get_page_list`. The bytes live once per manga under

    /// `download-work/snapshots`, so many chapter tasks of the same work never

    /// duplicate its chapter list in this JSON.

    #[serde(default)]
    manga_snapshot: Option<String>,
    /// Postcard bytes of the exact `Chapter` object the source is asked for

    /// pages, keeping scanlators/date/thumbnail/locked etc. across restarts.

    #[serde(default)]
    chapter_snapshot: Option<Vec<u8>>,
    /// Last page read online before the chapter joined the library.

    #[serde(default)]
    read_page: Option<usize>,
    /// When `read_page` last moved; lets 继续阅读 pick the most recently read

    /// chapter across library books and online streams.

    #[serde(default)]
    read_at: Option<i64>,
    status: DownloadStatus,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    pages_done: usize,
    #[serde(default)]
    pages_total: usize,
    /// Page-list fingerprint the cached page files belong to. A different list

    /// clears this chapter's own cache before refetching.

    #[serde(default)]
    pages_signature: Option<String>,
    #[serde(default)]
    cbz: Option<PathBuf>,
    /// True once the library import + chapter link succeeded. Unlinked

    /// finished files are offered again on the next start instead of being

    /// downloaded again.

    #[serde(default)]
    linked: bool,
    /// Queued by 自动下载新章节 — the 我的追更 row reports these as
    /// "自动下载中 N 章" while they wait or run.
    #[serde(default)]
    auto: bool,
}
impl DownloadTask {
    fn from_chapter(source: &str, manga: &Manga, chapter: &Chapter) -> Self {
        let chapter_title = chapter.title.clone().unwrap_or_else(|| {
            chapter
                .chapter_number
                .map(|n| format!("第 {n} 话"))
                .unwrap_or_else(|| chapter.key.clone())
        });
        Self {
            id: identity_id(source, &manga.key, &chapter.key),
            source_id: source.to_string(),
            manga_id: manga.key.clone(),
            manga_title: manga.title.clone(),
            manga_url: manga.url.clone(),
            manga_cover: manga.cover.clone(),
            chapter_id: chapter.key.clone(),
            chapter_title,
            chapter_number: chapter.chapter_number.map(|number| number as f64),
            volume_number: chapter.volume_number.map(|number| number as f64),
            chapter_url: chapter.url.clone(),
            chapter_language: chapter.language.clone(),
            manga_snapshot: None,
            chapter_snapshot: Some(chapter.to_postcard_bytes()),
            read_page: None,
            read_at: None,
            status: DownloadStatus::Queued,
            error: None,
            pages_done: 0,
            pages_total: 0,
            pages_signature: None,
            cbz: None,
            linked: false,
            auto: false,
        }
    }
    fn manga(&self) -> Manga {
        Manga {
            source_key: self.source_id.clone(),
            key: self.manga_id.clone(),
            title: self.manga_title.clone(),
            cover: self.manga_cover.clone(),
            url: self.manga_url.clone(),
            ..Default::default()
        }
    }
    fn chapter(&self) -> Chapter {
        Chapter {
            key: self.chapter_id.clone(),
            title: Some(self.chapter_title.clone()),
            chapter_number: self.chapter_number.map(|n| n as f32),
            volume_number: self.volume_number.map(|n| n as f32),
            url: self.chapter_url.clone(),
            language: self.chapter_language.clone(),
            ..Default::default()
        }
    }
    /// The original `Manga` for this task. Only queues written before

    /// snapshots existed fall back to the reduced legacy fields; a task that

    /// *has* a snapshot must restore it exactly, otherwise it fails visibly

    /// instead of sending a half-empty object to the source.

    fn manga_for(&self, root: &Path) -> anyhow::Result<Manga> {
        let Some(key) = self.manga_snapshot.as_deref() else {
            return Ok(self.manga());
        };
        let path = manga_snapshot_path(root, key)?;
        let bytes = std::fs::read(&path).map_err(|error| {
            anyhow::anyhow!(
                "漫画快照不可用（{key}）：{error}；请回到作品详情重新加入该章节以重建快照"
            )
        })?;
        let mut manga = Manga::from_postcard_bytes(&bytes).map_err(|error| {
            anyhow::anyhow!(
                "漫画快照已损坏（{key}）：{error}；请回到作品详情重新加入该章节以重建快照"
            )
        })?;
        anyhow::ensure!(
            manga.key == self.manga_id,
            "漫画快照与任务不匹配（{key}）；请回到作品详情重新加入该章节以重建快照"
        );
        manga.source_key = self.source_id.clone();
        Ok(manga)
    }
    /// The original `Chapter` for this task. Legacy queues without a snapshot

    /// keep working; a broken snapshot fails instead of silently degrading.

    fn chapter_for(&self) -> anyhow::Result<Chapter> {
        let Some(bytes) = self.chapter_snapshot.as_deref() else {
            return Ok(self.chapter());
        };
        let chapter = Chapter::from_postcard_bytes(bytes).map_err(|error| {
            anyhow::anyhow!("章节快照已损坏：{error}；请回到作品详情重新加入该章节以重建快照")
        })?;
        anyhow::ensure!(
            chapter.key == self.chapter_id,
            "章节快照与任务不匹配；请回到作品详情重新加入该章节以重建快照"
        );
        Ok(chapter)
    }
    fn metadata(&self) -> ChapterMetadata {
        ChapterMetadata {
            source_id: self.source_id.clone(),
            manga_id: self.manga_id.clone(),
            manga_title: self.manga_title.clone(),
            chapter_id: self.chapter_id.clone(),
            chapter_title: self.chapter_title.clone(),
            chapter_number: self.chapter_number,
            volume_number: self.volume_number,
        }
    }
}
#[derive(Default, Serialize, Deserialize)]
struct TaskStore {
    #[serde(default)]
    tasks: Vec<DownloadTask>,
}
/// Keep chapter rendering independent from the potentially large persisted

/// manga/chapter snapshots carried by each task.
#[derive(Clone)]
struct ChapterQueueState {
    status: DownloadStatus,
    has_archive: bool,
    done: usize,
    total: usize,
    read_page: Option<usize>,
    read_at: Option<i64>,
}
const COVER_QUEUE_CAP: usize = 24;
const COVER_INFLIGHT_CAP: usize = 8;
const COVER_TEXTURE_CAP: usize = 120;
const COVER_MAX_HEIGHT: u32 = 360;
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
/// One cover the results grid wants on screen.

#[derive(Clone, Debug, PartialEq, Eq)]
struct CoverReq {
    key: String,
    source: String,
    url: String,
}
/// `CoverReq` resolved through the source's interpreter, ready for raw HTTP on

/// any of the pool workers. The handler shares the source's cookie jar.

struct CoverHttpJob {
    key: String,
    root: PathBuf,
    handler: RequestHandler,
    url: String,
    headers: Vec<(String, String)>,
    body: Option<Vec<u8>>,
}
/// Decoded cover delivered to the UI; `None` marks the URL as failed for the

/// session so a placeholder is drawn instead of retrying every frame.

struct CoverDone {
    key: String,
    image: Option<ColorImage>,
}
enum DlCmd {
    Run(DownloadTask),
}
enum DlEvent {
    Progress {
        id: String,
        done: usize,
        total: usize,
        signature: Option<String>,
    },
    Finished {
        id: String,
        path: PathBuf,
    },
    Failed {
        id: String,
        error: String,
    },
    Aborted {
        id: String,
        how: u8,
    },
    LockUnavailable {
        id: String,
        error: String,
    },
}
const STOP_NONE: u8 = 0;
const STOP_PAUSE: u8 = 1;
const STOP_CANCEL: u8 = 2;
/// The result list plus the exact search that produced it.

////// Every entry keeps its own `source_key`, and results are only accepted for

/// the source/query/page that is still current, so a late reply from an

/// abandoned search can never be shown under another source.
#[derive(Default)]
struct ResultsState {
    source: String,
    query: String,
    page: i32,
    has_more: bool,
    entries: Vec<Manga>,
    revision: u64,
}
impl ResultsState {
    fn clear(&mut self) {
        let revision = self.revision.wrapping_add(1);
        *self = Self {
            revision,
            ..Self::default()
        };
    }
    fn begin(&mut self, source: &str, query: &str) {
        self.revision = self.revision.wrapping_add(1);
        self.entries.clear();
        self.source = source.to_string();
        self.query = query.to_string();
        self.page = 0;
        self.has_more = false;
    }
    /// Source, query and page for the next request, or `None` at the end.

    fn next_page(&self) -> Option<(String, String, i32)> {
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

    fn accept(
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
/// Applies a source selection: returns `true` when the source actually changed.

/// Any old result list, detail and chapter state belongs to the previous

/// source and is dropped, so the next tap can never load a foreign manga key.

fn apply_source_change(
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
// ---------------------------------------------------------------------------

// Persisted task identity and per-chapter page staging

// ---------------------------------------------------------------------------

/// Stable 64-bit FNV-1a over source/manga/chapter, so the same chapter always

/// maps to one task id across restarts and concurrent enqueues.

pub(crate) fn identity_id(source: &str, manga: &str, chapter: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for part in [source, manga, chapter] {
        for byte in part.as_bytes().iter().chain(std::iter::once(&0xffu8)) {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    format!("{hash:016x}")
}
fn work_root(root: &Path) -> PathBuf {
    root.join("download-work")
}
/// Reader book ids that point at a chapter still being downloaded.

pub const STREAM_PREFIX: &str = "stream:";
/// Bytes of one zero-based page already staged by a running or paused

/// download, or `None` while that page has not arrived yet.

pub(crate) fn stream_page(root: &Path, id: &str, page: usize) -> anyhow::Result<Option<Vec<u8>>> {
    let dir = task_dir(root, id)?;
    match stored_page(&dir, page + 1) {
        Some(path) => Ok(Some(std::fs::read(path)?)),
        None => Ok(None),
    }
}
/// A chapter the user opened for reading before its download finished.

#[derive(Debug, Clone, PartialEq)]
pub struct StreamChapter {
    pub id: String,
    pub metadata: ChapterMetadata,
    /// Entered backwards from the following chapter: start on its last page.
    pub from_end: bool,
}
/// Download progress of a chapter being read online.

#[derive(Debug, Clone, Default, PartialEq)]
pub struct StreamState {
    pub total: usize,
    pub done: usize,
    pub stopped: Option<String>,
    /// Page-list signature; staged pages from another list are discarded.
    pub signature: Option<String>,
    /// Where the user left off in this chapter, kept across app restarts.
    pub page: Option<usize>,
}
/// Task ids are always the 16 lowercase hex digits produced by `identity_id`.

/// Anything else is treated as a corrupt queue entry, never as a path.

fn valid_task_id(id: &str) -> bool {
    id.len() == 16
        && id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}
/// Resolves one task's staging directory and proves it stays inside

/// `download-work`, including the target of an existing symlink/junction.

fn task_dir(root: &Path, id: &str) -> anyhow::Result<PathBuf> {
    anyhow::ensure!(valid_task_id(id), "非法的下载任务 id：{id:?}");
    let work = work_root(root);
    let dir = work.join(format!("task-{id}"));
    anyhow::ensure!(
        dir.parent() == Some(work.as_path()),
        "下载任务路径越界：{}",
        dir.display()
    );
    match std::fs::symlink_metadata(&dir) {
        Ok(_) => {
            std::fs::create_dir_all(&work)?;
            let work_real = std::fs::canonicalize(&work)?;
            let dir_real = std::fs::canonicalize(&dir)?;
            anyhow::ensure!(
                dir_real.starts_with(&work_real),
                "下载任务路径指向 download-work 之外：{}",
                dir.display()
            );
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(anyhow::anyhow!("检查下载任务路径失败：{error}")),
    }
    Ok(dir)
}
/// Deletes only one validated task's staging directory. A malformed id or a

/// link that resolves outside `download-work` is reported instead of followed.

fn remove_task_cache(root: &Path, id: &str) -> anyhow::Result<()> {
    let dir = task_dir(root, id)?;
    match std::fs::symlink_metadata(&dir) {
        Ok(_) => std::fs::remove_dir_all(&dir)
            .map_err(|error| anyhow::anyhow!("清理缓存 {} 失败：{error}", dir.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(anyhow::anyhow!("检查缓存 {} 失败：{error}", dir.display())),
    }
}
fn snapshot_root(root: &Path) -> PathBuf {
    work_root(root).join("snapshots")
}
fn manga_snapshot_key(source: &str, manga: &str) -> String {
    identity_id(source, manga, "manga-snapshot")
}
pub(crate) fn manga_snapshot_path(root: &Path, key: &str) -> anyhow::Result<PathBuf> {
    anyhow::ensure!(valid_task_id(key), "非法的漫画快照 id：{key:?}");
    Ok(snapshot_root(root).join(format!("{key}.manga")))
}
/// Stores the complete `Manga` from the search result once per work. The exact

/// postcard bytes the interpreter would send to `get_page_list` are written, so

/// a restored task passes authors/tags/viewer/description unchanged and never

/// needs a new details request. Existing identical bytes are reused.

pub(crate) fn save_manga_snapshot(root: &Path, manga: &Manga) -> anyhow::Result<String> {
    let key = manga_snapshot_key(&manga.source_key, &manga.key);
    let path = manga_snapshot_path(root, &key)?;
    let bytes = manga.to_postcard_bytes();
    if std::fs::read(&path).ok().as_deref() == Some(bytes.as_slice()) {
        return Ok(key);
    }
    let directory = snapshot_root(root);
    std::fs::create_dir_all(&directory)
        .map_err(|error| anyhow::anyhow!("创建漫画快照目录失败：{error}"))?;
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or(0);
    let part = directory.join(format!("{key}-{}-{nonce}.part", std::process::id()));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&part)
        .map_err(|error| anyhow::anyhow!("写入漫画快照失败：{error}"))?;
    let cleanup = PartCleanup::new(&part);
    file.write_all(&bytes)
        .and_then(|_| file.sync_all())
        .map_err(|error| anyhow::anyhow!("写入漫画快照失败：{error}"))?;
    drop(file);
    std::fs::rename(&part, &path)
        .map_err(|error| anyhow::anyhow!("保存漫画快照 {} 失败：{error}", path.display()))?;
    cleanup.keep();
    Ok(key)
}
/// One OS-level exclusive lock per data root. It is held only while a task is

/// actually running and is released automatically when the guard is dropped.

fn acquire_download_lock(root: &Path) -> anyhow::Result<std::fs::File> {
    let work = work_root(root);
    std::fs::create_dir_all(&work)?;
    let path = work.join(".download.lock");
    let file = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(&path)?;
    file.try_lock()
        .map_err(|error| anyhow::anyhow!("上一次下载仍在退出，请稍后重试（{error}）"))?;
    Ok(file)
}
fn stored_page(dir: &Path, number: usize) -> Option<PathBuf> {
    ["jpg", "png", "webp"]
        .into_iter()
        .map(|extension| dir.join(format!("page-{number:04}.{extension}")))
        .find(|path| path.is_file())
}
fn existing_pages(dir: &Path) -> HashSet<usize> {
    let mut found = HashSet::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return found;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with("page-") || name.ends_with(".part") {
            continue;
        }
        let number = &name["page-".len()..];
        let Some((digits, extension)) = number.split_once('.') else {
            continue;
        };
        if matches!(extension, "jpg" | "png" | "webp")
            && let Ok(value) = digits.parse::<usize>()
            && value > 0
            && entry.file_type().is_ok_and(|kind| kind.is_file())
        {
            found.insert(value);
        }
    }
    found
}
/// Fingerprint of the chapter's page list. URL pages, zip entries, embedded

/// images and text pages all contribute; a changed list means cached page

/// files cannot be trusted and are cleared for this task only.

fn page_signature(pages: &[Page]) -> String {
    use std::fmt::Write as _;
    let mut text = String::with_capacity(pages.len() * 48);
    for page in pages {
        match &page.content {
            PageContent::Url { url, context } => {
                let _ = write!(text, "u|{url}|");
                if let Some(context) = context {
                    for (name, value) in context {
                        let _ = write!(text, "{name}={value};");
                    }
                }
                text.push('\n');
            }
            PageContent::ZipFile { url, file_path } => {
                let _ = writeln!(text, "z|{url}|{file_path}");
            }
            PageContent::Text(body) => {
                let _ = writeln!(text, "t|{}", body.len());
            }
            PageContent::Image {
                image_ref,
                image_data,
            } => {
                let _ = writeln!(
                    text,
                    "i|{image_ref}|{}",
                    image_data.as_ref().map_or(0, |bytes| bytes.len())
                );
            }
        }
    }
    text
}
fn reset_page_cache(dir: &Path) -> anyhow::Result<()> {
    if !dir.exists() {
        return Ok(());
    }
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with("page-") {
            std::fs::remove_file(entry.path())?;
        }
    }
    Ok(())
}
#[derive(Debug)]
struct FetchSummary {
    done: usize,
    aborted: bool,
}
/// The page-list signature is only a fingerprint, and inline images can reuse

/// the same `image_ref` while two different 4x4 PNGs still have exactly the

/// same byte length. For an inline page that already has a cached file, compare

/// the actual bytes; a mismatch invalidates only that one page.

fn cached_inline_page_matches(dir: &Path, number: usize, page: &Page) -> anyhow::Result<bool> {
    let PageContent::Image { image_data, .. } = &page.content else {
        return Ok(true);
    };
    let Some(expected) = image_data else {
        return Ok(false);
    };
    let Some(path) = stored_page(dir, number) else {
        return Ok(false);
    };
    Ok(std::fs::read(path)? == *expected)
}
/// Pages are fetched on this many workers at once, handed out in ascending

/// order so the page the reader waits for arrives first.

const PAGE_CONCURRENCY: usize = 4;
/// Downloads only the pages that are not yet stored. Each page is validated by

/// its real image header, written to `page-NNNN.part`, synced and renamed, so

/// a page file existing means it was fully written. A stop request between

/// pages lets in-flight pages finish and leaves every completed page

/// reusable. The first error stops new pages from starting and is returned

/// once the workers drain.

fn fetch_missing_pages(
    dir: &Path,
    pages: &[Page],
    fetch: impl Fn(&Page) -> anyhow::Result<Vec<u8>> + Sync,
    mut progress: impl FnMut(usize, usize),
    stop: &AtomicU8,
) -> anyhow::Result<FetchSummary> {
    std::fs::create_dir_all(dir)?;
    let mut stored = existing_pages(dir);
    let total = pages.len();
    let mut done = stored.iter().filter(|number| **number <= total).count();
    let mut missing = Vec::new();
    for (index, page) in pages.iter().enumerate() {
        let number = index + 1;
        if stored.contains(&number) {
            if cached_inline_page_matches(dir, number, page)? {
                continue;
            }
            if let Some(path) = stored_page(dir, number) {
                std::fs::remove_file(path)?;
            }
            stored.remove(&number);
            done = done.saturating_sub(1);
        }
        missing.push(index);
    }
    let next = AtomicUsize::new(0);
    let failed = AtomicBool::new(false);
    let (tx, rx) = mpsc::channel::<anyhow::Result<()>>();
    let mut first_error = None;
    let missing = &missing;
    let next = &next;
    let failed = &failed;
    let fetch = &fetch;
    std::thread::scope(|scope| {
        for _ in 0..PAGE_CONCURRENCY.min(missing.len().max(1)) {
            let tx = tx.clone();
            scope.spawn(move || {
                loop {
                    if failed.load(Ordering::SeqCst) || stop.load(Ordering::SeqCst) != STOP_NONE {
                        return;
                    }
                    let Some(&index) = missing.get(next.fetch_add(1, Ordering::SeqCst)) else {
                        return;
                    };
                    let number = index + 1;
                    let result = (|| -> anyhow::Result<()> {
                        let bytes = fetch(&pages[index])?;
                        let reader = crate::worker::image_reader(&bytes)?;
                        let extension = match reader.format() {
                            Some(image::ImageFormat::Jpeg) => "jpg",
                            Some(image::ImageFormat::Png) => "png",
                            Some(image::ImageFormat::WebP) => "webp",
                            _ => anyhow::bail!("第 {number} 页图像格式暂不支持"),
                        };
                        // A server can return a truncated image with a valid

                        // signature. Decode with the reader's limits before

                        // marking the page complete.

                        drop(
                            reader
                                .decode()
                                .with_context(|| format!("第 {number} 页图片不完整或无法解码"))?,
                        );
                        let part = dir.join(format!("page-{number:04}.part"));
                        let target = dir.join(format!("page-{number:04}.{extension}"));
                        let mut file = std::fs::OpenOptions::new()
                            .write(true)
                            .create(true)
                            .truncate(true)
                            .open(&part)?;
                        file.write_all(&bytes)?;
                        file.sync_all()?;
                        drop(file);
                        std::fs::rename(&part, &target)?;
                        Ok(())
                    })();
                    if result.is_err() {
                        failed.store(true, Ordering::SeqCst);
                    }
                    let failed_page = result.is_err();
                    if tx.send(result).is_err() || failed_page {
                        return;
                    }
                }
            });
        }
        drop(tx);
        for result in rx {
            match result {
                Ok(()) => {
                    done += 1;
                    progress(done, total);
                }
                Err(error) => {
                    if first_error.is_none() {
                        first_error = Some(error);
                    }
                }
            }
        }
    });
    if let Some(error) = first_error {
        return Err(error);
    }
    Ok(FetchSummary {
        done,
        aborted: stop.load(Ordering::SeqCst) != STOP_NONE,
    })
}
fn safe_name(title: &str) -> String {
    let mut safe = String::new();
    for ch in title.chars().take(80) {
        let ch = if ch.is_control() || "<>:\"/\\|?*".contains(ch) {
            '_'
        } else {
            ch
        };
        if safe.len() + ch.len_utf8() > 180 {
            break;
        }
        safe.push(ch);
    }
    if safe.trim().is_empty() {
        "chapter".into()
    } else {
        safe
    }
}
/// Builds the final CBZ from the validated page files. The archive is written

/// to a fresh `.part` and renamed only after `finish + sync_all`; any failure

/// removes just that file, and an existing `.cbz` is never touched.

fn chapter_archive(root: &Path, task: &DownloadTask) -> PathBuf {
    let title = safe_name(&format!("{} - {}", task.manga_title, task.chapter_title));
    root.join("downloads")
        .join(format!("{title}-{}.cbz", task.id))
}
fn build_cbz(
    root: &Path,
    task: &DownloadTask,
    dir: &Path,
    total: usize,
) -> anyhow::Result<PathBuf> {
    let directory = root.join("downloads");
    std::fs::create_dir_all(&directory)?;
    let path = chapter_archive(root, task);
    if path.is_file() {
        return Ok(path);
    }
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_nanos();
    let part = path.with_extension(format!("{nonce}.part"));
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&part)?;
    let cleanup = PartCleanup::new(&part);
    let mut zip = zip::ZipWriter::new(file);
    let options =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    for number in 1..=total {
        let page =
            stored_page(dir, number).ok_or_else(|| anyhow::anyhow!("第 {number} 页缓存缺失"))?;
        let bytes = std::fs::read(&page)?;
        let extension = page
            .extension()
            .and_then(|ext| ext.to_str())
            .unwrap_or("jpg");
        zip.start_file(format!("{number:05}.{extension}"), options)?;
        zip.write_all(&bytes)?;
    }
    zip.finish()?.sync_all()?;
    cleanup.keep();
    std::fs::rename(&part, &path)?;
    Ok(path)
}
#[derive(Debug)]
struct LoadedTasks {
    tasks: Vec<DownloadTask>,
    /// Present when part of the file was unreadable. The original file is

    /// never overwritten just because startup could not parse all of it.
    warning: Option<String>,
}
fn load_tasks(root: &Path) -> anyhow::Result<LoadedTasks> {
    let path = root.join("download-tasks.json");
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(LoadedTasks {
                tasks: Vec::new(),
                warning: None,
            });
        }
        Err(error) => return Err(anyhow::anyhow!("读取 {} 失败：{error}", path.display())),
    };
    let value = serde_json::from_slice::<serde_json::Value>(&bytes)
        .map_err(|error| anyhow::anyhow!("下载队列 JSON 损坏：{error}"))?;
    let Some(items) = value.get("tasks").and_then(|tasks| tasks.as_array()) else {
        return Err(anyhow::anyhow!("下载队列 JSON 缺少 tasks 数组"));
    };
    // Parse task by task: one damaged entry must not discard the rest of the

    // queue, but tell the user that something was skipped.

    let mut tasks = Vec::new();
    let mut skipped = 0usize;
    for item in items {
        match serde_json::from_value::<DownloadTask>(item.clone()) {
            Ok(task) if valid_task_id(&task.id) => tasks.push(task),
            Err(_) => skipped += 1,
            Ok(_) => skipped += 1,
        }
    }
    let warning =
        (skipped > 0).then(|| format!("下载队列有 {skipped} 条损坏记录已跳过，原文件已保留"));
    Ok(LoadedTasks { tasks, warning })
}
/// Loads and normalizes the persisted queue. A whole-file failure is surfaced

/// as a warning and leaves the file untouched; it never turns into a silent

/// empty queue.

fn load_queue(root: &Path) -> (Vec<DownloadTask>, Option<String>) {
    match load_tasks(root) {
        Ok(loaded) => {
            let mut tasks = loaded.tasks;
            normalize_loaded(root, &mut tasks);
            (tasks, loaded.warning)
        }
        Err(error) => (
            Vec::new(),
            Some(format!(
                "下载队列读取失败：{error:#}；原文件已保留，未开始任何下载"
            )),
        ),
    }
}
/// Applies the restart policy to tasks loaded from disk: an interrupted

/// download waits for an explicit "继续"; a finished task whose file vanished

/// becomes retryable instead of pretending success.

fn recover_completed_archive(root: &Path, task: &DownloadTask) -> Option<PathBuf> {
    let archive = chapter_archive(root, task);
    archive.is_file().then_some(archive)
}
fn normalize_loaded(root: &Path, tasks: &mut [DownloadTask]) {
    for task in tasks {
        if let Some(path) = recover_completed_archive(root, task) {
            // iOS can relocate the data container during an update. A done

            // task still records the old absolute CBZ path; prefer this

            // library's own complete archive and keep its import state.

            if task.status != DownloadStatus::Done {
                task.status = DownloadStatus::Done;
                task.error = None;
                task.linked = false;
                task.pages_done = task.pages_total;
            }
            task.cbz = Some(path);
        } else if task.status == DownloadStatus::Running {
            task.status = DownloadStatus::Paused;
            task.error = Some("上次下载中断，点“继续”恢复".into());
        } else if task.status == DownloadStatus::Done
            && !task.cbz.as_ref().is_some_and(|path| path.exists())
        {
            task.status = DownloadStatus::Failed;
            task.error = Some("下载文件已丢失，可重试".into());
            task.cbz = None;
        }
    }
}
/// Finished files that still need a library import, in queue order. Used on

/// start so an import that failed before the last shutdown is retried without

/// downloading anything again.

fn pending_imports(tasks: &[DownloadTask]) -> Vec<String> {
    tasks
        .iter()
        .filter(|task| {
            task.status == DownloadStatus::Done
                && !task.linked
                && task.cbz.as_ref().is_some_and(|path| path.exists())
        })
        .map(|task| task.id.clone())
        .collect()
}
enum EnqueueOutcome {
    Added,
    Duplicate,
    AlreadyDownloaded,
}
/// Adds one task, or explains why not. A finished task whose CBZ still exists

/// is protected; a finished task whose file disappeared goes back to the queue

/// and keeps its page cache (the page signature check decides whether that

/// cache is still valid).

fn enqueue_task(tasks: &mut Vec<DownloadTask>, task: DownloadTask) -> EnqueueOutcome {
    if let Some(existing) = tasks.iter_mut().find(|existing| existing.id == task.id) {
        if existing.status == DownloadStatus::Done {
            if existing.cbz.as_ref().is_some_and(|path| path.exists()) {
                return EnqueueOutcome::AlreadyDownloaded;
            }
            existing.status = DownloadStatus::Queued;
            existing.error = None;
            existing.cbz = None;
        }
        // Re-adding from the manga detail page carries freshly saved complete

        // objects. Replace the references so a task whose snapshot file was

        // lost or corrupted can be retried without losing its cached pages.

        existing.manga_snapshot = task.manga_snapshot;
        existing.chapter_snapshot = task.chapter_snapshot;
        if existing.status == DownloadStatus::Failed {
            existing.status = DownloadStatus::Queued;
            existing.error = None;
        }
        return EnqueueOutcome::Duplicate;
    }
    tasks.push(task);
    EnqueueOutcome::Added
}
fn next_queued(tasks: &[DownloadTask]) -> Option<usize> {
    tasks
        .iter()
        .position(|task| task.status == DownloadStatus::Queued)
}
pub struct Sources {
    root: PathBuf,
    tx: mpsc::Sender<(u64, Task)>,
    rx: mpsc::Receiver<(u64, Result<Reply, String>)>,
    installed: Vec<InstalledSource>,
    repository: Option<RepositoryIndex>,
    results: ResultsState,
    chapters: Vec<Chapter>,
    chapter_query: String,
    chapters_newest_first: bool,
    open_chapter: Option<ChapterMetadata>,
    library_chapters: std::collections::HashMap<String, ChapterMetadata>,
    selected_source: String,
    selected_manga: Option<Manga>,
    url: String,
    query: String,
    /// "current" or "global": whether the search field covers one or all sources.
    search_scope: String,
    global_gen: u64,
    global_query: String,
    global_groups: Vec<GlobalGroup>,
    /// New scope choice for `ReaderApp` to persist under `search-scope`.
    scope_dirty: Option<String>,
    /// Submitted keywords, newest first, capped at 20. Persisted by
    /// `ReaderApp` under `search-history`.
    search_history: Vec<String>,
    /// History changed and `ReaderApp` should persist it.
    history_dirty: bool,
    /// A tap on 清空 asks for confirmation before dropping the history.
    history_confirm: bool,
    /// Private reading suppresses history recording (mirrors the setting).
    private: bool,
    global_tx: mpsc::Sender<GlobalCmd>,
    global_rx: mpsc::Receiver<GlobalEvent>,
    storage_tx: mpsc::Sender<StorageCmd>,
    storage_rx: mpsc::Receiver<StorageEvent>,
    /// Latest sizes from `yejian-source-storage`; all scans run off-thread.
    storage: Option<SourceStorage>,
    request_id: u64,
    current_request: Arc<AtomicU64>,
    pending: Option<Task>,
    failed: Option<Task>,
    filters: Vec<Filter>,
    listings: Vec<Listing>,
    applied_filters: Vec<FilterValue>,
    listing: Option<String>,
    filter_editor: Option<FilterEditor>,
    metadata_loaded: bool,
    filter_error: Option<String>,
    listing_error: Option<String>,
    sessions: std::collections::HashMap<String, BrowseSession>,
    repository_search: String,
    #[cfg(debug_assertions)]
    preview_mode: Option<String>,
    busy: bool,
    status: String,
    /// When the current status string was first observed by the UI. Success
    /// notices fade out four seconds later; errors stay until the next
    /// action replaces them.
    status_seen: Option<(String, std::time::Instant)>,
    tasks: Vec<DownloadTask>,
    running: Option<String>,
    ready: VecDeque<String>,
    /// Import attempts that are really in flight, kept only in memory.

    /// `ack_downloaded` or `import_failed` clears the entry; while it is

    /// present the UI never offers a duplicate retry for the same file.
    import_attempts: HashSet<String>,
    dl_tx: mpsc::Sender<DlCmd>,
    dl_rx: mpsc::Receiver<DlEvent>,
    dl_stop: Arc<AtomicU8>,
    stream_request: Option<StreamChapter>,
    /// A download paused only so that a chapter being read can run first.

    /// It goes back to the queue instead of waiting for a manual "继续".
    preempted: Option<String>,
    follows: Vec<Follow>,
    /// A followed manga to open once its source has finished loading.
    pending_open: Option<Manga>,
    /// Detail was opened from the shelf: 返回 pops back to 书架 instead of
    /// the source browser.
    shelf_origin: bool,
    shelf_return: bool,
    /// A shelf 继续阅读 asked for the resume action once chapters arrive.
    pending_resume: bool,
    checking: usize,
    /// Whether the in-flight batch came from the scheduler — its completion

    /// summary goes to the notice bar rather than the sources status line.
    checking_background: bool,
    auto_checked: bool,
    /// egui context the scheduler uses for one-shot wake repaints.
    ctx: Context,
    /// Minutes between automatic follow checks; `0` disables them. Persisted

    /// as `follow-interval` through the library settings store.
    follow_interval_min: u64,
    /// egui-time deadline of the next scheduled check. Kept as wall-clock

    /// `Instant` so a suspended frame clock cannot skip a whole interval.
    follow_next_due: Option<std::time::Instant>,
    /// Fresh chapters collected by finished checks, waiting for the shell
    /// to turn into system notifications (deduped in notify.rs).
    notify_pending: Vec<crate::notify::FreshUpdate>,
    /// New-chapter summary surfaced to the app shell's notice bar once a

    /// background (scheduled) check finishes.
    pub(crate) follow_notice: Option<String>,
    up_tx: mpsc::Sender<Vec<(String, Manga)>>,
    up_rx: mpsc::Receiver<UpdateEvent>,
    /// Decoded follow covers by snapshot key; `None` means not on disk yet.
    covers: std::collections::HashMap<String, Option<TextureHandle>>,
    /// Each followed manga's cover URL resolved from its snapshot, once per

    /// session — the grid's `source-covers` cache keys are URL-based, so the

    /// follow rows reuse the same pipeline instead of `follow-covers/`.
    follow_cover_urls: std::collections::HashMap<String, Option<String>>,
    /// Result-grid covers. `cover_queue` only holds cells near the viewport;

    /// `cover_inflight` dedupes sent-but-unfinished requests and

    /// `cover_outstanding` bounds how many are with the workers at once.
    cover_req_tx: mpsc::Sender<CoverReq>,
    cover_done_rx: mpsc::Receiver<CoverDone>,
    cover_queue: VecDeque<CoverReq>,
    cover_inflight: HashSet<String>,
    cover_outstanding: usize,
    /// URLs that failed once this session; placeholders render instead of

    /// hammering a dead image host every frame.
    cover_failed: HashSet<String>,
    cover_ready: std::collections::HashMap<String, Option<ColorImage>>,
    cover_missing_disk: HashSet<String>,
    cover_textures: std::collections::HashMap<String, TextureHandle>,
    cover_lru: VecDeque<String>,
    /// Page already auto-loaded when its last grid row scrolled into view, so

    /// the marker cannot fire the same request again on the next frame.
    auto_next_page: Option<i32>,
    /// Reading progress per `identity_id` chapter, fed by the host each frame

    /// so the detail page can offer 继续阅读 and row read states.
    chapter_progress: std::collections::HashMap<String, (usize, usize, Option<i64>)>,
    description_expanded: bool,
    /// Clickable rects registered this frame, merged into the app's hitboxes

    /// so acceptance and automation can find grid cells and detail actions.
    pub(crate) hitboxes: std::collections::BTreeMap<String, Rect>,
    settings_open: bool,
    settings_changed: bool,
    /// User choices per source, persisted in `source-settings.json`.
    source_settings: SettingsStore,
    /// Sources whose loaded engines must restart to read changed settings.
    stale_engines: Arc<std::sync::Mutex<HashSet<String>>>,
    /// Origin a Cloudflare challenge blocked, until the user verifies.
    challenge: Option<String>,
    /// `auto-download` setting: "off" (default), "all", "latest1", "latest3".
    auto_download: String,
    /// 换源 panel state; while `Some` it owns the global-generation events.
    migrate: Option<MigrateState>,
    /// Background migration completions (one-shot threads report here).
    migrate_tx: mpsc::Sender<anyhow::Result<MigrateOutcome>>,
    migrate_rx: mpsc::Receiver<anyhow::Result<MigrateOutcome>>,
    /// Migration side-effects the host still owes the user once (tracker
    /// re-key + optional old-download purge + opening the new detail).
    migrate_post: Option<MigratePost>,
    /// The in-flight verification/login page — an in-app WebView on mobile

    /// or the system-browser + paste dialog on desktop.
    web_session: Option<WebSession>,
    login_user: String,
    login_pass: String,
    /// Cookies from verification/login pages by URL, in `web-cookies.json`.
    web_cookies: std::collections::BTreeMap<String, WebSiteCookies>,
    /// Saved repo URLs + cached indexes backing update badges (source_repos.rs).
    repos: SourceRepos,
    /// Language chip filter on the installable-sources list.
    repo_lang: Lang,
    /// A repo-index refresh (manual or the once-a-day auto check) is in flight.
    repo_fetching: bool,
    /// The daily auto check fires once per process, guarded by `checked_day`.
    repo_auto_done: bool,
    /// 测试 results per source id: "可用 · 820 ms" or a Chinese reason.
    repo_test_results: std::collections::HashMap<String, String>,
    /// Tests currently running (≤4), keyed by source id for the 15 s timeout.
    repo_testing: std::collections::HashMap<String, std::time::Instant>,
    /// Tests waiting for a slot.
    repo_test_queue: VecDeque<String>,
    test_tx: mpsc::Sender<(String, Result<u64, String>)>,
    test_rx: mpsc::Receiver<(String, Result<u64, String>)>,
    /// Pending 卸载 confirmation card.
    uninstall: Option<InstalledSource>,
    /// Setting writes (source-repos, source-repos-checked-at) the host owes
    /// the settings store.
    setting_writes: VecDeque<(String, String)>,
}
/// One online work's latest read chapter — a 阅读足迹 row.
pub(crate) struct OnlineHistory {
    pub source_id: String,
    pub manga_id: String,
    pub manga_title: String,
    pub chapter_title: String,
    pub chapter_number: Option<f64>,
    pub read_page: usize,
    pub pages_total: usize,
    pub read_at: i64,
}

/// What one verification/login attempt is for and how it presents.
struct WebSession {
    /// Source the session belongs to (engine reload/login target).
    source: String,
    /// `settings.json` login key, or `None` for a Cloudflare verification.
    login: Option<String>,
    /// The URL the user should verify against.
    url: String,
    /// Pasted cookie text, kept across frames while the dialog is open.
    paste: String,
    /// Optional browser User-Agent for the site (Cloudflare UA pinning).
    agent: String,
}
/// One site's saved web cookies plus the browser UA pinned with them.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
struct WebSiteCookies {
    cookies: Vec<(String, String)>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    user_agent: Option<String>,
}
const WEB_COOKIES_FILE: &str = "web-cookies.json";
/// Reads `web-cookies.json` in either shape: the original

/// `{url: [[name, value], …]}` map or the current

/// `{url: {"cookies": [[name, value], …], "user_agent": "…"}}` document.

/// Unknown shapes degrade to an empty site instead of dropping the file.
fn load_web_cookies(root: &Path) -> std::collections::BTreeMap<String, WebSiteCookies> {
    let Ok(bytes) = std::fs::read(root.join(WEB_COOKIES_FILE)) else {
        return Default::default();
    };
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return Default::default();
    };
    let Some(map) = value.as_object() else {
        return Default::default();
    };
    let mut sites = std::collections::BTreeMap::new();
    for (url, entry) in map {
        let pairs = |value: &serde_json::Value| -> Vec<(String, String)> {
            value
                .as_array()
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|pair| {
                            Some((
                                pair.get(0)?.as_str()?.to_string(),
                                pair.get(1)?.as_str()?.to_string(),
                            ))
                        })
                        .collect()
                })
                .unwrap_or_default()
        };
        let site = if entry.is_array() {
            // Legacy format: bare cookie pair list.
            WebSiteCookies {
                cookies: pairs(entry),
                user_agent: None,
            }
        } else {
            WebSiteCookies {
                cookies: entry.get("cookies").map(&pairs).unwrap_or_default(),
                user_agent: entry
                    .get("user_agent")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string),
            }
        };
        if !site.cookies.is_empty() || site.user_agent.is_some() {
            sites.insert(url.clone(), site);
        }
    }
    sites
}
type SettingsStore =
    std::collections::BTreeMap<String, std::collections::BTreeMap<String, SettingValue>>;
const SETTINGS_FILE: &str = "source-settings.json";
fn load_source_settings(root: &Path) -> SettingsStore {
    std::fs::read(root.join(SETTINGS_FILE))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}
fn save_source_settings(root: &Path, store: &SettingsStore) -> anyhow::Result<()> {
    std::fs::create_dir_all(root)?;
    let part = root.join(format!("{SETTINGS_FILE}.part"));
    std::fs::write(&part, serde_json::to_vec_pretty(store)?)?;
    std::fs::rename(&part, root.join(SETTINGS_FILE))?;
    Ok(())
}
/// Draws one editable setting; returns the new value when the user changed it.

fn setting_row(
    ui: &mut Ui,
    item: &SettingItem,
    current: Option<SettingValue>,
) -> Option<SettingValue> {
    let mut changed = None;
    let label = |ui: &mut Ui| {
        ui.add(Label::new(RichText::new(&item.title).strong()).wrap());
    };
    match &item.kind {
        SettingKind::Login { .. } => {}
        SettingKind::Button { .. } | SettingKind::Link { .. } => {}
        SettingKind::Toggle => {
            let mut on = matches!(current, Some(SettingValue::Bool(true)));
            if ui.add(Checkbox::new(&mut on, &item.title)).changed() {
                changed = Some(SettingValue::Bool(on));
            }
        }
        SettingKind::Select { values, titles } => {
            label(ui);
            let selected = match &current {
                Some(SettingValue::Str(value)) => Some(value.as_str()),
                _ => None,
            };
            ui.horizontal_wrapped(|ui| {
                for (value, title) in values.iter().zip(titles) {
                    if ui
                        .selectable_label(selected == Some(value.as_str()), title)
                        .clicked()
                    {
                        changed = Some(SettingValue::Str(value.clone()));
                    }
                }
            });
        }
        SettingKind::Segment { titles } => {
            label(ui);
            let selected = match current {
                Some(SettingValue::Int(index)) => index,
                Some(SettingValue::Float(index)) => index as i32,
                _ => -1,
            };
            ui.horizontal_wrapped(|ui| {
                for (index, title) in titles.iter().enumerate() {
                    if ui
                        .selectable_label(selected == index as i32, title)
                        .clicked()
                    {
                        changed = Some(SettingValue::Int(index as i32));
                    }
                }
            });
        }
        SettingKind::MultiSelect { values, titles } => {
            label(ui);
            let mut chosen = match current {
                Some(SettingValue::StrList(list)) => list,
                _ => Vec::new(),
            };
            ui.horizontal_wrapped(|ui| {
                for (value, title) in values.iter().zip(titles) {
                    let on = chosen.contains(value);
                    let shown = if item.key == "languages" {
                        language_label(title).to_string()
                    } else {
                        title.clone()
                    };
                    if ui.selectable_label(on, shown).clicked() {
                        if on {
                            chosen.retain(|v| v != value);
                        } else {
                            chosen.push(value.clone());
                        }
                        changed = Some(SettingValue::StrList(chosen.clone()));
                    }
                }
            });
        }
        SettingKind::Text { placeholder } => {
            label(ui);
            let mut text = match current {
                Some(SettingValue::Str(text)) => text,
                _ => String::new(),
            };
            let edit = TextEdit::singleline(&mut text)
                .hint_text(placeholder.as_str())
                .desired_width(ui.available_width());
            if ui.add(edit).changed() {
                changed = Some(SettingValue::Str(text));
            }
        }
        SettingKind::Stepper { min, max, step } => {
            let value = match current {
                Some(SettingValue::Int(v)) => v as f64,
                Some(SettingValue::Float(v)) => v as f64,
                _ => *min,
            };
            let integral = step.fract() == 0.0 && min.fract() == 0.0;
            ui.horizontal_wrapped(|ui| {
                label(ui);
                let mut next = None;
                if ui
                    .add_enabled(value > *min, Button::new("−").min_size(vec2(40., 36.)))
                    .clicked()
                {
                    next = Some((value - step).max(*min));
                }
                ui.label(if integral {
                    format!("{}", value as i64)
                } else {
                    format!("{value:.2}")
                });
                if ui
                    .add_enabled(value < *max, Button::new("+").min_size(vec2(40., 36.)))
                    .clicked()
                {
                    next = Some((value + step).min(*max));
                }
                changed = next.map(|v| {
                    if integral {
                        SettingValue::Int(v as i32)
                    } else {
                        SettingValue::Float(v as f32)
                    }
                });
            });
        }
    }
    changed
}
fn url_host(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    host.split(':').next().unwrap_or("").to_ascii_lowercase()
}
fn follow_cover_path(root: &Path, snapshot: &str) -> anyhow::Result<PathBuf> {
    anyhow::ensure!(valid_task_id(snapshot), "非法的封面 id：{snapshot:?}");
    Ok(root.join("follow-covers").join(format!("{snapshot}.img")))
}
/// Result-grid covers are keyed by the exact URL they came from so the same

/// image shared by two manga is fetched and cached once.

fn cover_key(source: &str, url: &str) -> String {
    identity_id(source, url, "cover")
}
fn source_cover_path(root: &Path, key: &str) -> anyhow::Result<PathBuf> {
    anyhow::ensure!(valid_task_id(key), "非法的封面 id：{key:?}");
    Ok(root.join("source-covers").join(format!("{key}.img")))
}
/// Downscales to the grid's display size; small covers are kept as-is rather

/// than upscaled into a blur.

fn shrink_cover(image: image::DynamicImage) -> image::DynamicImage {
    if image.width() > 240 || image.height() > COVER_MAX_HEIGHT {
        image.thumbnail(240, COVER_MAX_HEIGHT)
    } else {
        image
    }
}
/// Writes a downscaled cover into the disk cache and returns the decoded

/// image. The `.part` + rename pattern matches the rest of the cache so a

/// crash mid-write never leaves a half file behind.

fn store_source_cover(path: &Path, image: &image::DynamicImage) -> anyhow::Result<ColorImage> {
    std::fs::create_dir_all(path.parent().unwrap())?;
    let mut encoded = std::io::Cursor::new(Vec::new());
    image.write_to(&mut encoded, image::ImageFormat::Png)?;
    let part = path.with_extension("part");
    std::fs::write(&part, encoded.get_ref())?;
    std::fs::rename(&part, path)?;
    let rgba = image.to_rgba8();
    Ok(ColorImage::from_rgba_unmultiplied(
        [rgba.width() as usize, rgba.height() as usize],
        rgba.as_raw(),
    ))
}
/// Downloads one cover through the source's own request handler, keeps the

/// downscaled image in the disk cache and decodes it for the UI. The disk

/// copy skips the network entirely on later launches.
#[cfg(test)]
fn fetch_source_cover(
    root: &Path,
    engine: &SourceEngine,
    source: &str,
    url: &str,
) -> anyhow::Result<ColorImage> {
    let key = cover_key(source, url);
    let path = source_cover_path(root, &key)?;
    if let Ok(bytes) = std::fs::read(&path) {
        if let Ok(image) = image::load_from_memory(&bytes) {
            return store_source_cover(&path, &image);
        }
    }
    let handler = engine.request_handler();
    let bytes = match pollster::block_on(engine.image_request(url, None))? {
        Some(req) => request(&handler, req.url, req.headers, req.body)?,
        None => request(&handler, url.to_string(), vec![], None)?,
    };
    let image = shrink_cover(image::load_from_memory(&bytes)?);
    store_source_cover(&path, &image)
}
/// Downloads a follow's cover into the shared `source-covers/` cache —

/// same key, same atomic write and same downscaled PNG the result grid uses.

/// Returns the cache key and decoded thumbnail for the UI's `cover_ready`.
fn fetch_follow_cover(
    root: &Path,
    engine: &SourceEngine,
    manga: &Manga,
) -> anyhow::Result<(String, ColorImage)> {
    let url = manga
        .cover
        .as_deref()
        .filter(|url| !url.is_empty())
        .ok_or_else(|| anyhow::anyhow!("no cover url"))?;
    let key = cover_key(&manga.source_key, url);
    let path = source_cover_path(root, &key)?;
    if let Ok(bytes) = std::fs::read(&path) {
        if let Ok(image) = image::load_from_memory(&bytes) {
            let rgba = image.to_rgba8();
            return Ok((
                key,
                ColorImage::from_rgba_unmultiplied(
                    [rgba.width() as usize, rgba.height() as usize],
                    rgba.as_raw(),
                ),
            ));
        }
    }
    let handler = engine.request_handler();
    let bytes = match pollster::block_on(engine.image_request(url, None))? {
        Some(req) => request(&handler, req.url, req.headers, req.body)?,
        None => request(&handler, url.to_string(), vec![], None)?,
    };
    let image = shrink_cover(image::load_from_memory(&bytes)?);
    let color = store_source_cover(&path, &image)?;
    Ok((key, color))
}
/// Result of checking one followed manga for new chapters.

struct UpdateEvent {
    source: String,
    manga: String,
    /// `None`: only the cover arrived, after the batch's chapter results.
    chapters: Option<Result<Vec<Chapter>, String>>,
    /// `source-covers` key + decoded image delivered with `chapters: None`.
    cover: Option<(String, ColorImage)>,
}

// -- global (all-sources) search ------------------------------------------

/// How many sources may search at once in the global scope.
const GLOBAL_SEARCH_LIMIT: usize = 4;
/// One source's slice of a global search; longer means "超时".
const GLOBAL_SEARCH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

/// A global search request; retries reuse the generation of their run.
enum GlobalCmd {
    Search {
        generation: u64,
        query: String,
        sources: Vec<String>,
    },
    /// 换源 candidate expansion: one manga's chapter list, fetched once on
    /// the engine thread and cached panel-side per candidate.
    Chapters {
        generation: u64,
        source: String,
        manga: Manga,
    },
}
/// One source's answer to a global search or a 换源 chapter expansion,
/// streamed as soon as it lands.
enum GlobalEvent {
    Search {
        generation: u64,
        source: String,
        result: Result<Vec<Manga>, String>,
    },
    Chapters {
        generation: u64,
        source: String,
        manga: Manga,
        result: Result<Vec<Chapter>, String>,
    },
}
/// Test seam: the calls the worker makes for one source.
trait GlobalEngine: Send + Sync {
    fn search(&self, query: &str) -> Result<Vec<Manga>, String>;
    fn chapters(&self, manga: Manga) -> Result<Vec<Chapter>, String>;
}
struct RealGlobalEngine(Arc<SourceEngine>);
impl GlobalEngine for RealGlobalEngine {
    fn search(&self, query: &str) -> Result<Vec<Manga>, String> {
        pollster::block_on(self.0.search_page(query, 1, vec![]))
            .map(|result| result.entries)
            .map_err(|error| format!("{error}"))
    }
    fn chapters(&self, manga: Manga) -> Result<Vec<Chapter>, String> {
        pollster::block_on(self.0.chapters_with(manga)).map_err(|error| format!("{error}"))
    }
}

/// `yejian-global-search` loop. Per-source workers are detached threads that
/// send completions over a channel: a hung `search_page` can keep its thread
/// forever, but the loop never joins it — after `timeout` the slot is freed,
/// marked "超时", and any late answer is dropped by the generation/inflight
/// check. That keeps a wedged source from stalling the next generation the
/// way `std::thread::scope` (which joins) would.
fn global_search_run(
    rx: mpsc::Receiver<GlobalCmd>,
    event_tx: mpsc::Sender<GlobalEvent>,
    load: impl Fn(&str) -> Result<Arc<dyn GlobalEngine>, String>,
    timeout: std::time::Duration,
    stale: Arc<std::sync::Mutex<HashSet<String>>>,
    ctx: Context,
) {
    let mut engines: std::collections::HashMap<String, Arc<dyn GlobalEngine>> =
        std::collections::HashMap::new();
    let (done_tx, done_rx) = mpsc::channel::<(u64, String, Result<Vec<Manga>, String>)>();
    let (done_ch_tx, done_ch_rx) =
        mpsc::channel::<(u64, String, Manga, Result<Vec<Chapter>, String>)>();
    let mut generation = 0u64;
    let mut query = String::new();
    let mut pending: VecDeque<String> = VecDeque::new();
    let mut inflight: std::collections::HashMap<String, std::time::Instant> =
        std::collections::HashMap::new();
    // Chapter-expansion jobs keyed `(source, manga key)`; they share the
    // timeout and detached-thread policy but never block search slots.
    let mut chapters_inflight: std::collections::HashMap<
        (String, String),
        (std::time::Instant, Manga, u64),
    > = std::collections::HashMap::new();
    // When the UI drops its sender, keep draining already-started jobs so
    // their completions/timeouts are still delivered before the loop exits.
    let mut closed = false;
    loop {
        for id in stale.lock().unwrap().drain() {
            engines.remove(&id);
        }
        let engine_for = |engines: &mut std::collections::HashMap<String, Arc<dyn GlobalEngine>>,
                              source: &str,
                              load: &dyn Fn(&str) -> Result<Arc<dyn GlobalEngine>, String>|
         -> Result<Arc<dyn GlobalEngine>, String> {
            match engines.entry(source.to_string()) {
                std::collections::hash_map::Entry::Occupied(entry) => Ok(entry.get().clone()),
                std::collections::hash_map::Entry::Vacant(entry) => match load(source) {
                    Ok(engine) => Ok(entry.insert(engine).clone()),
                    Err(error) => Err(error),
                },
            }
        };
        while inflight.len() < GLOBAL_SEARCH_LIMIT && let Some(source) = pending.pop_front() {
            match engine_for(&mut engines, &source, &load) {
                Ok(engine) => {
                    inflight.insert(source.clone(), std::time::Instant::now());
                    let (tx, query, generation, source) =
                        (done_tx.clone(), query.clone(), generation, source);
                    thread::spawn(move || {
                        let result = engine.search(&query);
                        let _ = tx.send((generation, source, result));
                    });
                }
                Err(error) => {
                    let _ = event_tx.send(GlobalEvent::Search {
                        generation,
                        source,
                        result: Err(error),
                    });
                    ctx.request_repaint();
                }
            }
        }
        for (done_gen, source, result) in done_rx.try_iter() {
            if done_gen == generation && inflight.remove(&source).is_some() {
                let _ = event_tx.send(GlobalEvent::Search {
                    generation: done_gen,
                    source,
                    result,
                });
                ctx.request_repaint();
            }
        }
        for (done_gen, source, manga, result) in done_ch_rx.try_iter() {
            let key = (source.clone(), manga.key.clone());
            if chapters_inflight
                .get(&key)
                .is_some_and(|(_, _, job_gen)| *job_gen == done_gen)
                && chapters_inflight.remove(&key).is_some()
            {
                let _ = event_tx.send(GlobalEvent::Chapters {
                    generation: done_gen,
                    source,
                    manga,
                    result,
                });
                ctx.request_repaint();
            }
        }
        let now = std::time::Instant::now();
        let expired_ch: Vec<(String, String)> = chapters_inflight
            .iter()
            .filter(|(_, (started, _, _))| now.duration_since(*started) > timeout)
            .map(|(key, _)| key.clone())
            .collect();
        for key in expired_ch {
            if let Some((_, manga, job_gen)) = chapters_inflight.remove(&key) {
                let _ = event_tx.send(GlobalEvent::Chapters {
                    generation: job_gen,
                    source: key.0,
                    manga,
                    result: Err("超时".into()),
                });
                ctx.request_repaint();
            }
        }
        let expired: Vec<String> = inflight
            .iter()
            .filter(|(_, started)| now.duration_since(**started) > timeout)
            .map(|(source, _)| source.clone())
            .collect();
        for source in expired {
            inflight.remove(&source);
            let _ = event_tx.send(GlobalEvent::Search {
                generation,
                source,
                result: Err("超时".into()),
            });
            ctx.request_repaint();
        }
        match rx.recv_timeout(std::time::Duration::from_millis(50)) {
            Ok(GlobalCmd::Search {
                generation: cmd_gen,
                query: cmd_query,
                sources,
            }) => {
                if cmd_gen < generation {
                    continue;
                }
                if cmd_gen > generation {
                    generation = cmd_gen;
                    query = cmd_query.clone();
                    pending.clear();
                    // The slots clear conceptually; the detached workers may
                    // still run, but their results no longer match.
                    inflight.clear();
                }
                for source in sources {
                    if !inflight.contains_key(&source) && !pending.contains(&source) {
                        pending.push_back(source);
                    }
                }
            }
            Ok(GlobalCmd::Chapters {
                generation: cmd_gen,
                source,
                manga,
            }) => {
                let key = (source.clone(), manga.key.clone());
                if chapters_inflight.contains_key(&key) {
                    continue;
                }
                match engine_for(&mut engines, &source, &load) {
                    Ok(engine) => {
                        chapters_inflight
                            .insert(key, (std::time::Instant::now(), manga.clone(), cmd_gen));
                        let tx = done_ch_tx.clone();
                        thread::spawn(move || {
                            let result = engine.chapters(manga.clone());
                            let _ = tx.send((cmd_gen, source, manga, result));
                        });
                    }
                    Err(error) => {
                        let _ = event_tx.send(GlobalEvent::Chapters {
                            generation: cmd_gen,
                            source,
                            manga,
                            result: Err(error),
                        });
                        ctx.request_repaint();
                    }
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => closed = true,
        }
        if closed
            && pending.is_empty()
            && inflight.is_empty()
            && chapters_inflight.is_empty()
        {
            return;
        }
    }
}

/// One source's slice of the grouped global results.
struct GlobalGroup {
    source: String,
    state: GlobalState,
}

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
struct MigrateOutcome {
    /// Re-keyed follow record, when the work was followed.
    follow: Option<Follow>,
    /// Position of the old record inside `self.follows`.
    follow_index: Option<usize>,
    source: String,
    manga: Manga,
    /// Chapters of the new work, in listing order.
    chapters: Vec<Chapter>,
    old_source: String,
    old_manga: String,
    purge_old: bool,
    /// True when no chapter number could be parsed from the old reading
    /// record — the detail page resumes at chapter one and the user is told.
    resume_unparsed: bool,
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

/// Bytes held under the sources root, split by where the space goes.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SourceStorage {
    /// Staging for queued/running/paused tasks plus page snapshots —
    /// everything an in-flight or resumable download still needs.
    pub staging_active: u64,
    /// Staging left behind by finished/failed tasks or orphaned dirs —
    /// the only part 清理 may delete.
    pub staging_done: u64,
    /// Number of task directories `staging_done` is made of.
    pub staging_done_count: usize,
    /// Imported archives still sitting under `downloads/`.
    pub downloads: u64,
    /// Cached source cover images.
    pub covers: u64,
}
impl SourceStorage {
    pub fn total_bytes(&self) -> u64 {
        self.staging_active + self.staging_done + self.downloads + self.covers
    }
}
enum StorageAction {
    Report,
    CleanStaging,
    ClearCovers,
}
/// Task ids snapshotted on the UI thread: `all` knows every queued task,
/// `finished` the Done/Failed subset whose staging is safe to remove.
struct StorageCmd {
    action: StorageAction,
    all: HashSet<String>,
    finished: HashSet<String>,
}
enum StorageEvent {
    Report(SourceStorage),
    /// `source-covers/` was wiped; the in-memory caches must go too.
    CoversCleared,
    Failed(String),
}

fn tree_size(path: &Path) -> u64 {
    let meta = match std::fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(_) => return 0,
    };
    if meta.is_file() {
        meta.len()
    } else if meta.is_dir() {
        let mut total = 0;
        if let Ok(entries) = std::fs::read_dir(path) {
            for entry in entries.flatten() {
                total += tree_size(&entry.path());
            }
        }
        total
    } else {
        0
    }
}
fn source_storage(root: &Path, all: &HashSet<String>, finished: &HashSet<String>) -> SourceStorage {
    let mut report = SourceStorage::default();
    if let Ok(entries) = std::fs::read_dir(work_root(root)) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if let Some(id) = name.strip_prefix("task-") {
                // A task-* dir with no queue entry is orphaned staging —
                // nothing can still be writing to it.
                if finished.contains(id) || !all.contains(id) {
                    report.staging_done += tree_size(&entry.path());
                    report.staging_done_count += 1;
                } else {
                    report.staging_active += tree_size(&entry.path());
                }
            } else {
                report.staging_active += tree_size(&entry.path());
            }
        }
    }
    report.downloads = tree_size(&root.join("downloads"));
    report.covers = tree_size(&root.join("source-covers"));
    report
}
/// Removes staging for finished/failed and orphaned task dirs only — the
/// `task_dir`/`remove_task_cache` guards still validate every id, and any
/// rejection is reported instead of followed.
fn clean_finished_tasks(
    root: &Path,
    all: &HashSet<String>,
    finished: &HashSet<String>,
) -> anyhow::Result<usize> {
    let mut removed = 0;
    if let Ok(entries) = std::fs::read_dir(work_root(root)) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            let Some(id) = name.strip_prefix("task-") else {
                continue;
            };
            if !(finished.contains(id) || !all.contains(id)) {
                continue;
            }
            remove_task_cache(root, id)?;
            removed += 1;
        }
    }
    Ok(removed)
}
/// `yejian-source-storage` loop: all disk walks and deletions happen here,
/// never on the UI thread. Every action answers with a fresh report so the
/// settings rows reflect the result immediately.
fn storage_worker_run(
    rx: mpsc::Receiver<StorageCmd>,
    tx: mpsc::Sender<StorageEvent>,
    root: PathBuf,
    ctx: Context,
) {
    while let Ok(cmd) = rx.recv() {
        match cmd.action {
            StorageAction::CleanStaging => {
                if let Err(error) =
                    clean_finished_tasks(&root, &cmd.all, &cmd.finished)
                {
                    let _ = tx.send(StorageEvent::Failed(format!("{error:#}")));
                    ctx.request_repaint();
                    continue;
                }
            }
            StorageAction::ClearCovers => {
                let dir = root.join("source-covers");
                if dir.exists()
                    && let Err(error) = std::fs::remove_dir_all(&dir)
                {
                    let _ = tx.send(StorageEvent::Failed(format!(
                        "清理封面缓存失败：{error}"
                    )));
                    ctx.request_repaint();
                    continue;
                }
                let _ = tx.send(StorageEvent::CoversCleared);
            }
            StorageAction::Report => {}
        }
        let _ = tx.send(StorageEvent::Report(source_storage(
            &root,
            &cmd.all,
            &cmd.finished,
        )));
        ctx.request_repaint();
    }
}
#[derive(Clone)]
pub(crate) enum GlobalState {
    Searching,
    Done(Vec<Manga>),
    Failed(String),
}
impl Clone for GlobalGroup {
    fn clone(&self) -> Self {
        Self {
            source: self.source.clone(),
            state: self.state.clone(),
        }
    }
}

impl Sources {
    #[cfg(debug_assertions)]
    pub(crate) fn preview_community(&mut self) {
        self.clear_status();
        self.url = COMMUNITY_INDEX.into();
        self.submit(Task::Fetch(COMMUNITY_INDEX.into()));
    }

    #[cfg(debug_assertions)]
    pub(crate) fn repository_size(&self) -> usize {
        self.repository
            .as_ref()
            .map_or(0, |repo| repo.sources.len())
    }

    #[cfg(debug_assertions)]
    pub(crate) fn preview_browser(&mut self, mode: &str) -> anyhow::Result<()> {
        self.clear_status();
        // Only explicit debug snapshot/acceptance flags install this labelled,

        // local WASM fixture. Release apps never embed it.

        let directory = self.root.join("sources/local.filter-acceptance");
        std::fs::create_dir_all(&directory)?;
        for (name, bytes) in [
            (
                "source.json",
                include_bytes!("../crates/source-engine/tests/fixtures/browser/source.json")
                    .as_slice(),
            ),
            (
                "filters.json",
                include_bytes!("../crates/source-engine/tests/fixtures/browser/filters.json")
                    .as_slice(),
            ),
            (
                "main.wasm",
                include_bytes!("../crates/source-engine/tests/fixtures/browser/main.wasm")
                    .as_slice(),
            ),
        ] {
            std::fs::write(directory.join(name), bytes)?;
        }
        self.installed = SourceCatalog::new(self.root.join("sources")).list()?;
        self.preview_mode = Some(mode.into());
        self.select_source("");
        self.select_source("local.filter-acceptance");
        Ok(())
    }

    /// Second acceptance source: valid manifest, corrupt WASM, so a global
    /// search reports one success group and one failure group.
    #[cfg(debug_assertions)]
    pub(crate) fn preview_global_search(&mut self) -> anyhow::Result<()> {
        self.clear_status();
        self.preview_browser("browse")?;
        let broken = self.root.join("sources/local.broken-acceptance");
        std::fs::create_dir_all(&broken)?;
        std::fs::write(
            broken.join("source.json"),
            r#"{"info":{"id":"local.broken-acceptance","name":"故障验收源","version":1,"languages":["zh"],"urls":["https://fixture.invalid"]}}"#,
        )?;
        std::fs::write(broken.join("main.wasm"), b"not a wasm module")?;
        self.installed = SourceCatalog::new(self.root.join("sources")).list()?;
        self.set_search_scope("global");
        self.query = "冒险".into();
        self.start_search();
        Ok(())
    }

    #[cfg(debug_assertions)]
    pub(crate) fn browser_report(&self) -> serde_json::Value {
        serde_json::json!({
            "source":self.selected_source,"loaded":self.metadata_loaded,
            "filter_count":self.filters.len(),"listing_count":self.listings.len(),
            "filter_editor":self.filter_editor.is_some(),"busy":self.busy,
            "status":self.status,"failed":self.failed.is_some(),
            "has_more":self.results.has_more,"pending":self.pending.is_some(),
            "results":self.results.entries.iter().map(|manga|manga.title.as_str()).collect::<Vec<_>>(),
            "applied":self.applied_filters,
            "scope":self.search_scope,
            "groups":self.global_groups.iter().map(|group|serde_json::json!({
                "source":group.source,
                "state":match &group.state{
                    GlobalState::Searching=>"searching".into(),
                    GlobalState::Done(entries)=>format!("done:{}",entries.len()),
                    GlobalState::Failed(reason)=>format!("failed:{reason}"),
                }})).collect::<Vec<_>>(),
        })
    }

    #[cfg(debug_assertions)]
    pub(crate) fn preview_installed_source(&mut self, id: &str) -> anyhow::Result<()> {
        self.clear_status();
        self.installed = SourceCatalog::new(self.root.join("sources")).list()?;
        anyhow::ensure!(
            self.installed.iter().any(|source| source.manifest.id == id),
            "验收书源未安装：{id}"
        );
        self.preview_mode = Some("filters".into());
        self.select_source(id);
        Ok(())
    }

    /// Acceptance seed: a followed work with auto-download tasks in flight,
    /// so 我的追更 renders "自动下载中 N 章".
    #[cfg(debug_assertions)]
    pub(crate) fn preview_auto_download(&mut self) {
        self.clear_status();
        // 我的追更 only renders on the sources root, not inside a source.
        self.selected_source.clear();
        self.selected_manga = None;
        let manga = Manga {
            source_key: "local.filter-acceptance".into(),
            key: "fx-pop-1".into(),
            title: "自动下载示例".into(),
            ..Default::default()
        };
        if self
            .follow_index(&manga.source_key, &manga.key)
            .is_none()
        {
            let snapshot = save_manga_snapshot(&self.root, &manga).unwrap_or_default();
            self.follows.insert(
                0,
                Follow {
                    source_id: manga.source_key.clone(),
                    manga_id: manga.key.clone(),
                    title: manga.title.clone(),
                    snapshot,
                    fresh: vec!["d4".into(), "d5".into()],
                    total: 5,
                    followed_at: Some(1),
                    ..Default::default()
                },
            );
        }
        for (key, number) in [("d4", 4.), ("d5", 5.)] {
            if self
                .tasks
                .iter()
                .any(|task| task.manga_id == manga.key && task.chapter_id == key)
            {
                continue;
            }
            let chapter = Chapter {
                key: key.into(),
                title: Some(format!("第 {number} 话")),
                chapter_number: Some(number),
                ..Default::default()
            };
            let mut task = DownloadTask::from_chapter(&manga.source_key, &manga, &chapter);
            task.auto = true;
            task.status = DownloadStatus::Queued;
            self.tasks.push(task);
        }
    }

    /// Acceptance seed: one finished online chapter with a read position so
    /// the 阅读足迹 page renders an 在线 row.
    #[cfg(debug_assertions)]
    pub(crate) fn preview_history(&mut self) {
        self.clear_status();
        let manga = Manga {
            source_key: "local.filter-acceptance".into(),
            key: "fx-history-1".into(),
            title: "在线足迹示例".into(),
            ..Default::default()
        };
        if self.tasks.iter().any(|task| task.manga_id == manga.key) {
            return;
        }
        let chapter = Chapter {
            key: "h7".into(),
            title: Some("第 7 话".into()),
            chapter_number: Some(7.),
            ..Default::default()
        };
        let mut task = DownloadTask::from_chapter(&manga.source_key, &manga, &chapter);
        task.status = DownloadStatus::Done;
        task.read_at = Some(follows::now() as i64);
        task.read_page = Some(3);
        task.pages_total = 24;
        self.tasks.push(task);
    }

    /// Acceptance seed: two fake installed sources — one with a cached repo
    /// update badge and one with a finished 测试 result — plus a repository
    /// index so the list and language chips render.
    #[cfg(debug_assertions)]
    pub(crate) fn preview_sources_manage(&mut self) {
        self.clear_status();
        self.selected_source.clear();
        self.selected_manga = None;
        for (id, name, version, languages) in [
            (
                "local.manage-a",
                "纸间图源 · 中文",
                1i64,
                vec!["zh".to_string()],
            ),
            (
                "local.manage-b",
                "Mirror Garden EN",
                5i64,
                vec!["en".to_string()],
            ),
        ] {
            if self
                .installed
                .iter()
                .any(|source| source.manifest.id == id)
            {
                continue;
            }
            self.installed.push(InstalledSource {
                manifest: SourceManifest {
                    id: id.into(),
                    name: name.into(),
                    version,
                    alt_names: Vec::new(),
                    languages,
                    urls: vec![format!("https://{id}.example")],
                    content_rating: 0,
                    listings: Vec::new(),
                    allows_base_url_select: false,
                    hides_filters_while_searching: false,
                },
                directory: self.root.join("sources").join(id),
                wasm_path: self.root.join("sources").join(id).join("main.wasm"),
                settings_path: None,
                filters_path: None,
                icon_path: None,
            });
        }
        let repo_url = "https://preview.repo/index.json";
        let index = RepositoryIndex {
            name: Some("示例仓库".into()),
            website: None,
            index_url: repo_url.into(),
            sources: vec![
                RepositorySource {
                    id: "local.manage-a".into(),
                    name: "纸间图源 · 中文".into(),
                    version: 3,
                    download_url: format!("{repo_url}/a.aix"),
                    icon_url: None,
                    languages: vec!["zh".into()],
                    content_rating: 0,
                    base_url: None,
                },
                RepositorySource {
                    id: "local.manage-b".into(),
                    name: "Mirror Garden EN".into(),
                    version: 5,
                    download_url: format!("{repo_url}/b.aix"),
                    icon_url: None,
                    languages: vec!["en".into()],
                    content_rating: 0,
                    base_url: None,
                },
                RepositorySource {
                    id: "local.preview-c".into(),
                    name: "さくら文庫".into(),
                    version: 2,
                    download_url: format!("{repo_url}/c.aix"),
                    icon_url: None,
                    languages: vec!["ja".into()],
                    content_rating: 0,
                    base_url: None,
                },
            ],
        };
        self.repos.merge_index(repo_url, &index, follows::now());
        if self.repos.record_repo(repo_url) {
            self.write_setting("source-repos", self.repos.urls_setting());
        }
        self.repository = Some(index);
        self.repo_test_results
            .insert("local.manage-b".into(), "可用 · 820 ms".into());
    }

    /// Acceptance lookup for the uninstall-confirm seam.
    #[cfg(debug_assertions)]
    pub(crate) fn installed_source(&self, id: &str) -> Option<InstalledSource> {
        self.installed
            .iter()
            .find(|source| source.manifest.id == id)
            .cloned()
    }

    /// Acceptance teardown: drop every preview follow/task so nothing the
    /// captures seeded leaks into the real saved follows/tasks on exit.
    #[cfg(debug_assertions)]
    pub(crate) fn clear_acceptance_previews(&mut self) {
        const PREVIEW_SOURCES: [&str; 5] = [
            "local.filter-acceptance",
            "local.second-source",
            "local.mirror-b",
            "local.manage-a",
            "local.manage-b",
        ];
        let before = self.follows.len();
        self.follows
            .retain(|f| !PREVIEW_SOURCES.contains(&f.source_id.as_str()));
        self.tasks
            .retain(|t| !PREVIEW_SOURCES.contains(&t.source_id.as_str()));
        if self.follows.len() != before {
            self.save_follows();
        }
        let _ = self.persist();
        self.migrate = None;
        self.migrate_post = None;
        self.uninstall = None;
        self.installed.retain(|source| {
            !source.manifest.id.starts_with("local.manage-")
        });
        self.repo_test_results
            .retain(|id, _| !id.starts_with("local.manage-"));
        if self.repos.remove_repo("https://preview.repo/index.json") {
            self.write_setting("source-repos", self.repos.urls_setting());
        }
        self.repos
            .cache
            .remove("https://preview.repo/index.json");
        if self
            .repository
            .as_ref()
            .is_some_and(|repo| repo.index_url == "https://preview.repo/index.json")
        {
            self.repository = None;
        }
    }

    /// Acceptance seed: the 换源 panel at a chosen stage — candidates with
    /// one expanded row, the confirm card, or the finished new detail page.
    #[cfg(debug_assertions)]
    pub(crate) fn preview_migrate(&mut self, stage: &str) {
        self.clear_status();
        let old = Manga {
            source_key: "local.filter-acceptance".into(),
            key: "fx-pop-1".into(),
            title: "自动下载示例".into(),
            authors: Some(vec!["示例作者".into()]),
            ..Default::default()
        };
        let chapter = |key: &str, number: f32| Chapter {
            key: key.into(),
            title: Some(format!("第 {number} 话")),
            chapter_number: Some(number),
            ..Default::default()
        };
        self.selected_source = old.source_key.clone();
        self.selected_manga = Some(old.clone());
        self.chapters = vec![chapter("d1", 1.), chapter("d2", 2.)];
        let new_source = "local.second-source".to_string();
        let cand = Manga {
            source_key: new_source.clone(),
            key: "m-new".into(),
            title: old.title.clone(),
            authors: old.authors.clone(),
            ..Default::default()
        };
        let cand_chapters = vec![chapter("x1", 1.), chapter("x2", 2.), chapter("x3", 3.)];
        let mut expanded = HashMap::new();
        expanded.insert(
            (new_source.clone(), cand.key.clone()),
            Ok(cand_chapters.clone()),
        );
        self.migrate = Some(MigrateState {
            generation: self.global_gen,
            old_source: old.source_key.clone(),
            old_manga: old.key.clone(),
            groups: vec![
                MigrateGroup {
                    source: new_source.clone(),
                    state: GlobalState::Done(vec![cand.clone()]),
                },
                MigrateGroup {
                    source: "local.mirror-b".into(),
                    state: GlobalState::Done(Vec::new()),
                },
                MigrateGroup {
                    source: "local.mirror-c".into(),
                    state: GlobalState::Failed("网络错误".into()),
                },
            ],
            expanding: None,
            expanded: if stage == "candidates" {
                expanded
            } else {
                HashMap::new()
            },
            confirm: (stage == "confirm").then(|| MigrateConfirm {
                source: new_source.clone(),
                manga: cand.clone(),
                chapters: chapter_list(&cand_chapters, "", false),
                purge_old: false,
            }),
            running: false,
            error: None,
        });
        if stage == "done" {
            if self.follow_index(&old.source_key, &old.key).is_none() {
                let snapshot = save_manga_snapshot(&self.root, &old).unwrap_or_default();
                self.follows.insert(
                    0,
                    Follow {
                        source_id: old.source_key.clone(),
                        manga_id: old.key.clone(),
                        title: old.title.clone(),
                        snapshot,
                        known: vec!["d1".into()],
                        followed_at: Some(1),
                        ..Default::default()
                    },
                );
            }
            let mut cand_snapshot = cand.clone();
            cand_snapshot.source_key = new_source.clone();
            let snapshot = save_manga_snapshot(&self.root, &cand_snapshot).unwrap_or_default();
            self.apply_migrate(Ok(MigrateOutcome {
                follow: Some(Follow {
                    source_id: new_source.clone(),
                    manga_id: cand.key.clone(),
                    title: cand.title.clone(),
                    snapshot,
                    known: cand_chapters.iter().map(|c| c.key.clone()).collect(),
                    reading_order: cand_chapters.iter().map(|c| c.key.clone()).collect(),
                    total: cand_chapters.len(),
                    followed_at: Some(1),
                    resume_number: Some(2.),
                    ..Default::default()
                }),
                follow_index: Some(0),
                source: new_source,
                manga: cand,
                chapters: cand_chapters,
                old_source: old.source_key.clone(),
                old_manga: old.key.clone(),
                purge_old: false,
                resume_unparsed: false,
            }));
        }
    }

    #[cfg(debug_assertions)]
    pub(crate) fn browser_ready(&self, mode: &str) -> bool {
        self.metadata_loaded
            && !self.busy
            && !self.results.entries.is_empty()
            && (mode != "filters" || self.filter_editor.is_some())
            && (mode != "filtered"
                || self
                    .results
                    .entries
                    .iter()
                    .any(|manga| manga.title == "筛选:冒险题材"))
    }

    pub fn new(root: PathBuf, ctx: Context) -> Self {
        let (tx, rx_task) = mpsc::channel();
        let (tx_reply, rx) = mpsc::channel();
        let meta_root = root.clone();
        let meta_ctx = ctx.clone();
        let current_request = Arc::new(AtomicU64::new(0));
        let worker_request = current_request.clone();
        let source_settings = load_source_settings(&root);
        for (source, values) in &source_settings {
            for (key, value) in values {
                set_setting(source, key, value.clone());
            }
        }
        let stale_engines = Arc::new(std::sync::Mutex::new(HashSet::<String>::new()));
        let web_cookies = load_web_cookies(&root);
        for (url, site) in &web_cookies {
            add_web_cookies(url, &site.cookies);
            if let Some(agent) = &site.user_agent {
                set_site_user_agent(url, agent);
            }
        }
        let worker_stale = stale_engines.clone();
        thread::Builder::new()
            .name("yejian-sources".into())
            .spawn(move || {
                let catalog = SourceCatalog::new(meta_root.join("sources"));
                let mut engines = std::collections::HashMap::<String, SourceEngine>::new();
                while let Ok((request_id, task)) = rx_task.recv() {
                    for id in worker_stale.lock().unwrap().drain() {
                        engines.remove(&id);
                    }
                    // Skip queued reads already replaced by a newer user choice.

                    // An installation is an explicit mutation and must still finish.

                    if request_id != worker_request.load(Ordering::Relaxed)
                        && !matches!(
                            &task,
                            Task::Install(_) | Task::Uninstall(_) | Task::FetchAllRepos(_)
                        )
                    {
                        continue;
                    }
                    let result = (|| -> anyhow::Result<Reply> {
                        Ok(match task {
                            Task::List => Reply::Installed {
                                list: catalog.list()?,
                                updated: None,
                            },
                            Task::Fetch(url) => {
                                let installer =
                                    SourceInstaller::new(SourceCatalog::new(catalog.root()))?;
                                Reply::Repository(installer.fetch_index(&url)?)
                            }
                            Task::Install(entry) => {
                                let installer =
                                    SourceInstaller::new(SourceCatalog::new(catalog.root()))?;
                                installer.install_entry(&entry)?;
                                engines.remove(&entry.id);
                                Reply::Installed {
                                    list: catalog.list()?,
                                    updated: Some(entry.id),
                                }
                            }
                            Task::Uninstall(id) => {
                                catalog.remove(&id)?;
                                engines.remove(&id);
                                Reply::Installed {
                                    list: catalog.list()?,
                                    updated: None,
                                }
                            }
                            Task::FetchAllRepos(urls) => {
                                let installer =
                                    SourceInstaller::new(SourceCatalog::new(catalog.root()))?;
                                let results = urls
                                    .into_iter()
                                    .map(|url| {
                                        let result = installer
                                            .fetch_index(&url)
                                            .map_err(|error| format!("{error}"));
                                        (url, result)
                                    })
                                    .collect();
                                Reply::RepoAll(results)
                            }
                            Task::Metadata(source) => {
                                if !engines.contains_key(&source) {
                                    engines.insert(
                                        source.clone(),
                                        SourceEngine::load(&catalog, &source)?,
                                    );
                                }
                                let engine = &engines[&source];
                                let (filters, filter_error) =
                                    match pollster::block_on(engine.filters()) {
                                        Ok(filters) => (filters, None),
                                        Err(error) => {
                                            // A remote dynamic-filter failure must not

                                            // conceal static options already installed.

                                            let static_filters = catalog
                                                .get(&source)
                                                .ok()
                                                .and_then(|source| source.filters_path)
                                                .and_then(|path| std::fs::read_to_string(path).ok())
                                                .and_then(|text| parse_filters_json(&text).ok())
                                                .unwrap_or_default();
                                            (static_filters, Some(error.to_string()))
                                        }
                                    };
                                let (listings, listing_error) =
                                    match pollster::block_on(engine.listings()) {
                                        Ok(listings) => (listings, None),
                                        Err(error) => (
                                            engine.manifest().listings.clone(),
                                            Some(error.to_string()),
                                        ),
                                    };
                                Reply::Metadata {
                                    filters,
                                    listings,
                                    filter_error,
                                    listing_error,
                                }
                            }
                            Task::Search {
                                source,
                                query,
                                page,
                                filters,
                            } => {
                                if !engines.contains_key(&source) {
                                    engines.insert(
                                        source.clone(),
                                        SourceEngine::load(&catalog, &source)?,
                                    );
                                }
                                let result = pollster::block_on(
                                    engines[&source].search_page(&query, page, filters),
                                )?;
                                Reply::Results {
                                    source,
                                    query,
                                    page,
                                    entries: result.entries,
                                    has_next: result.has_next_page,
                                }
                            }
                            Task::Browse {
                                source,
                                listing,
                                page,
                            } => {
                                if !engines.contains_key(&source) {
                                    engines.insert(
                                        source.clone(),
                                        SourceEngine::load(&catalog, &source)?,
                                    );
                                }
                                let result =
                                    pollster::block_on(engines[&source].browse(&listing, page))?;
                                Reply::Results {
                                    source,
                                    query: String::new(),
                                    page,
                                    entries: result.entries,
                                    has_next: result.has_next_page,
                                }
                            }
                            Task::Chapters(source, manga) => {
                                if !engines.contains_key(&source) {
                                    engines.insert(
                                        source.clone(),
                                        SourceEngine::load(&catalog, &source)?,
                                    );
                                }
                                let engine = &engines[&source];
                                let detail = pollster::block_on(engine.manga_details(&manga.key))?;
                                let chapters = pollster::block_on(engine.chapters(&manga.key))?;
                                Reply::Chapters(source, detail, chapters)
                            }
                            Task::Login {
                                source,
                                key,
                                credentials,
                            } => {
                                if !engines.contains_key(&source) {
                                    engines.insert(
                                        source.clone(),
                                        SourceEngine::load(&catalog, &source)?,
                                    );
                                }
                                let engine = &engines[&source];
                                let web = credentials.is_err();
                                let (user, ok) = match credentials {
                                    Ok((user, password)) => {
                                        let ok = pollster::block_on(
                                            engine.basic_login(&key, &user, &password),
                                        )?;
                                        (user, ok)
                                    }
                                    Err(cookies) => {
                                        let ok =
                                            pollster::block_on(engine.web_login(&key, cookies))?;
                                        ("网页登录".to_string(), ok)
                                    }
                                };
                                Reply::Login { key, user, ok, web }
                            }
                            Task::Notify {
                                source,
                                notification,
                            } => {
                                if !engines.contains_key(&source) {
                                    engines.insert(
                                        source.clone(),
                                        SourceEngine::load(&catalog, &source)?,
                                    );
                                }
                                pollster::block_on(
                                    engines[&source].handle_notification(notification),
                                )?;
                                Reply::Notified
                            }
                        })
                    })();
                    let _ =
                        tx_reply.send((request_id, result.map_err(|error| format!("{error:#}"))));
                    meta_ctx.request_repaint();
                }
            })
            .expect("Unable to start source worker");
        // Update checks get their own thread so a slow source never blocks

        // browsing or downloads.

        let (up_tx, up_cmd_rx) = mpsc::channel::<Vec<(String, Manga)>>();
        let (up_event_tx, up_rx) = mpsc::channel();
        let up_root = root.clone();
        let up_ctx = ctx.clone();
        thread::Builder::new()
            .name("yejian-updates".into())
            .spawn(move || {
                let catalog = SourceCatalog::new(up_root.join("sources"));
                while let Ok(batch) = up_cmd_rx.recv() {
                    // Reload each round so an updated source is picked up.

                    let mut engines = std::collections::HashMap::<String, SourceEngine>::new();
                    for (source, manga) in batch.iter().cloned() {
                        let chapters = (|| -> anyhow::Result<Vec<Chapter>> {
                            if !engines.contains_key(&source) {
                                engines
                                    .insert(source.clone(), SourceEngine::load(&catalog, &source)?);
                            }
                            Ok(pollster::block_on(
                                engines[&source].chapters_with(manga.clone()),
                            )?)
                        })()
                        .map_err(|error| format!("{error:#}"));
                        let event = UpdateEvent {
                            source,
                            manga: manga.key,
                            chapters: Some(chapters),
                            cover: None,
                        };
                        if up_event_tx.send(event).is_err() {
                            return;
                        }
                        up_ctx.request_repaint();
                    }
                    // Covers come last so a slow image host never delays

                    // new-chapter results; a missing cover is not an error.

                    for (source, manga) in batch {
                        let Some(engine) = engines.get(&source) else {
                            continue;
                        };
                        if let Ok((key, image)) = fetch_follow_cover(&up_root, engine, &manga) {
                            let event = UpdateEvent {
                                source,
                                manga: manga.key,
                                chapters: None,
                                cover: Some((key, image)),
                            };
                            if up_event_tx.send(event).is_err() {
                                return;
                            }
                            up_ctx.request_repaint();
                        }
                    }
                }
            })
            .expect("Unable to start update worker");
        // Result covers resolve through each source's interpreter on a single

        // thread, then fetch bytes on a small HTTP pool so a slow image host

        // never blocks browsing. Both stages share the source's cookie jar.

        let (cover_req_tx, cover_req_rx) = mpsc::channel::<CoverReq>();
        let (cover_done_tx, cover_done_rx) = mpsc::channel::<CoverDone>();
        let cover_jobs: Arc<(std::sync::Mutex<VecDeque<CoverHttpJob>>, std::sync::Condvar)> =
            Arc::new((
                std::sync::Mutex::new(VecDeque::new()),
                std::sync::Condvar::new(),
            ));
        {
            let cover_root = root.clone();
            let cover_ctx = ctx.clone();
            let queue = cover_jobs.clone();
            let done_tx = cover_done_tx.clone();
            let stale = stale_engines.clone();
            thread::Builder::new()
                .name("yejian-covers".into())
                .spawn(move || {
                    let catalog = SourceCatalog::new(cover_root.join("sources"));
                    let mut engines = std::collections::HashMap::<String, SourceEngine>::new();
                    while let Ok(req) = cover_req_rx.recv() {
                        for id in stale.lock().unwrap().drain() {
                            engines.remove(&id);
                        }
                        let job = (|| -> anyhow::Result<CoverHttpJob> {
                            if !engines.contains_key(&req.source) {
                                engines.insert(
                                    req.source.clone(),
                                    SourceEngine::load(&catalog, &req.source)?,
                                );
                            }
                            let engine = &engines[&req.source];
                            let (url, headers, body) =
                                match pollster::block_on(engine.image_request(&req.url, None))? {
                                    Some(image) => (image.url, image.headers, image.body),
                                    None => (req.url.clone(), Vec::new(), None),
                                };
                            Ok(CoverHttpJob {
                                key: req.key.clone(),
                                root: cover_root.clone(),
                                handler: engine.request_handler(),
                                url,
                                headers,
                                body,
                            })
                        })();
                        match job {
                            Ok(job) => {
                                let (lock, cvar) = &*queue;
                                lock.lock().unwrap().push_back(job);
                                cvar.notify_one();
                            }
                            Err(_) => {
                                if done_tx
                                    .send(CoverDone {
                                        key: req.key,
                                        image: None,
                                    })
                                    .is_err()
                                {
                                    return;
                                }
                                cover_ctx.request_repaint();
                            }
                        }
                    }
                })
                .expect("Unable to start cover worker");
        }
        for _ in 0..4 {
            let queue = cover_jobs.clone();
            let done_tx = cover_done_tx.clone();
            let ctx = ctx.clone();
            thread::Builder::new()
                .name("yejian-cover-http".into())
                .spawn(move || {
                    let (lock, cvar) = &*queue;
                    loop {
                        let job = {
                            let mut guard = lock.lock().unwrap();
                            loop {
                                if let Some(job) = guard.pop_front() {
                                    break job;
                                }
                                guard = cvar.wait(guard).unwrap();
                            }
                        };
                        let image = request(&job.handler, job.url, job.headers, job.body)
                            .and_then(|bytes| {
                                let image = shrink_cover(image::load_from_memory(&bytes)?);
                                let path = source_cover_path(&job.root, &job.key)?;
                                store_source_cover(&path, &image)
                            })
                            .ok();
                        if done_tx
                            .send(CoverDone {
                                key: job.key,
                                image,
                            })
                            .is_err()
                        {
                            return;
                        }
                        ctx.request_repaint();
                    }
                })
                .expect("Unable to start cover http worker");
        }
        // Downloads live on their own thread and run one task at a time, so a

        // slow image server never blocks search, chapter loading or the UI.

        let (dl_tx, dl_cmd_rx) = mpsc::channel::<DlCmd>();
        let (dl_ev_tx, dl_rx) = mpsc::channel::<DlEvent>();
        let dl_stop = Arc::new(AtomicU8::new(STOP_NONE));
        {
            let root = root.clone();
            let ctx = ctx.clone();
            let stop = dl_stop.clone();
            thread::Builder::new()
                .name("yejian-downloads".into())
                .spawn(move || {
                    let catalog = SourceCatalog::new(root.join("sources"));
                    let mut engines = std::collections::HashMap::<String, SourceEngine>::new();
                    let send = |event| {
                        let _ = dl_ev_tx.send(event);
                        ctx.request_repaint();
                    };
                    while let Ok(DlCmd::Run(task)) = dl_cmd_rx.recv() {
                        let id = task.id.clone();
                        let lock = match acquire_download_lock(&root) {
                            Ok(lock) => lock,
                            Err(error) => {
                                let _ = send(DlEvent::LockUnavailable {
                                    id,
                                    error: format!("{error:#}"),
                                });
                                continue;
                            }
                        };
                        let outcome = (|| -> anyhow::Result<Outcome> {
                            if stop.load(Ordering::SeqCst) != STOP_NONE {
                                return Ok(Outcome::Aborted {
                                    how: stop.load(Ordering::SeqCst),
                                });
                            }
                            if let Some(path) = recover_completed_archive(&root, &task) {
                                return Ok(Outcome::Done { path });
                            }
                            if !engines.contains_key(&task.source_id) {
                                engines.insert(
                                    task.source_id.clone(),
                                    SourceEngine::load(&catalog, &task.source_id)?,
                                );
                            }
                            let engine = &engines[&task.source_id];
                            let pages = pollster::block_on(
                                engine.pages_with(task.manga_for(&root)?, task.chapter_for()?),
                            )?;
                            execute_task(
                                &root,
                                &task,
                                pages,
                                &stop,
                                |page| page_bytes(engine, page),
                                |done, total, signature| {
                                    send(DlEvent::Progress {
                                        id: id.clone(),
                                        done,
                                        total,
                                        signature,
                                    });
                                },
                            )
                        })();
                        drop(lock);
                        match outcome {
                            Ok(Outcome::Done { path }) => {
                                let _ = send(DlEvent::Finished { id, path });
                            }
                            Ok(Outcome::Aborted { how }) => {
                                let _ = send(DlEvent::Aborted { id, how });
                            }
                            Err(error) => {
                                let _ = send(DlEvent::Failed {
                                    id,
                                    error: format!("{error:#}"),
                                });
                            }
                        }
                    }
                })
                .expect("Unable to start download worker");
        }
        // Cross-source searches keep their own engine map so a source update
        // invalidates them like every other worker.
        let (global_tx, global_rx) = mpsc::channel::<GlobalEvent>();
        let (global_cmd_tx, global_cmd_rx) = mpsc::channel::<GlobalCmd>();
        {
            let (catalog_root, stale, ctx) =
                (root.clone(), stale_engines.clone(), ctx.clone());
            thread::Builder::new()
                .name("yejian-global-search".into())
                .spawn(move || {
                    let catalog = SourceCatalog::new(catalog_root.join("sources"));
                    global_search_run(
                        global_cmd_rx,
                        global_tx,
                        move |source| {
                            SourceEngine::load(&catalog, source)
                                .map(|engine| {
                                    Arc::new(RealGlobalEngine(Arc::new(engine)))
                                        as Arc<dyn GlobalEngine>
                                })
                                .map_err(|error| format!("{error:#}"))
                        },
                        GLOBAL_SEARCH_TIMEOUT,
                        stale,
                        ctx,
                    );
                })
                .expect("Unable to start global search worker");
        }
        // Storage scans and cleanup run on their own worker so large
        // download-work trees never stall a frame.
        let (storage_tx, storage_cmd_rx) = mpsc::channel::<StorageCmd>();
        let (storage_event_tx, storage_rx) = mpsc::channel::<StorageEvent>();
        {
            let (root, ctx) = (root.clone(), ctx.clone());
            thread::Builder::new()
                .name("yejian-source-storage".into())
                .spawn(move || storage_worker_run(storage_cmd_rx, storage_event_tx, root, ctx))
                .expect("Unable to start storage worker");
        }

        let (migrate_tx, migrate_rx) = mpsc::channel();
        let (test_tx, test_rx) = mpsc::channel();
        let (tasks, load_warning) = load_queue(&root);
        let ready: VecDeque<String> = pending_imports(&tasks).into();
        let sources = Self {
            root,
            tx,
            rx,
            installed: Vec::new(),
            repository: None,
            results: ResultsState::default(),
            chapters: Vec::new(),
            chapter_query: String::new(),
            chapters_newest_first: true,
            open_chapter: None,
            library_chapters: Default::default(),
            selected_source: String::new(),
            selected_manga: None,
            url: String::new(),
            query: String::new(),
            search_scope: "current".into(),
            global_gen: 0,
            global_query: String::new(),
            global_groups: Vec::new(),
            scope_dirty: None,
            search_history: Vec::new(),
            history_dirty: false,
            history_confirm: false,
            private: false,
            global_tx: global_cmd_tx,
            global_rx,
            storage_tx,
            storage_rx,
            storage: None,
            request_id: 0,
            current_request,
            pending: None,
            failed: None,
            filters: Vec::new(),
            listings: Vec::new(),
            applied_filters: Vec::new(),
            listing: None,
            filter_editor: None,
            metadata_loaded: false,
            filter_error: None,
            listing_error: None,
            sessions: Default::default(),
            repository_search: String::new(),
            #[cfg(debug_assertions)]
            preview_mode: None,
            busy: false,
            status: load_warning.clone().unwrap_or_default(),
            status_seen: None,
            tasks,
            running: None,
            ready,
            import_attempts: HashSet::new(),
            dl_tx,
            dl_rx,
            dl_stop,
            stream_request: None,
            preempted: None,
            follows: Vec::new(),
            pending_open: None,
            shelf_origin: false,
            shelf_return: false,
            pending_resume: false,
            checking: 0,
            checking_background: false,
            auto_checked: false,
            ctx,
            follow_interval_min: DEFAULT_FOLLOW_INTERVAL_MIN,
            follow_next_due: None,
            follow_notice: None,
            notify_pending: Vec::new(),
            up_tx,
            up_rx,
            covers: Default::default(),
            follow_cover_urls: Default::default(),
            cover_req_tx,
            cover_done_rx,
            cover_queue: VecDeque::new(),
            cover_inflight: HashSet::new(),
            cover_outstanding: 0,
            cover_failed: HashSet::new(),
            cover_ready: Default::default(),
            cover_missing_disk: HashSet::new(),
            cover_textures: Default::default(),
            cover_lru: VecDeque::new(),
            auto_next_page: None,
            chapter_progress: Default::default(),
            description_expanded: false,
            hitboxes: Default::default(),
            settings_open: false,
            settings_changed: false,
            source_settings,
            stale_engines,
            challenge: None,
            auto_download: "off".into(),
            migrate: None,
            migrate_tx,
            migrate_rx,
            migrate_post: None,
            web_session: None,
            login_user: String::new(),
            login_pass: String::new(),
            web_cookies,
            repos: SourceRepos::default(),
            repo_lang: Lang::default(),
            repo_fetching: false,
            repo_auto_done: false,
            repo_test_results: Default::default(),
            repo_testing: Default::default(),
            repo_test_queue: VecDeque::new(),
            test_tx,
            test_rx,
            uninstall: None,
            setting_writes: VecDeque::new(),
        };
        let mut sources = sources;
        sources.repos.load_cache(&sources.root);
        match follows::load(&sources.root) {
            Ok(list) => sources.follows = list,
            Err(error) => sources.status = format!("{error:#}"),
        }
        // Never overwrite a file that startup could not fully parse.

        if load_warning.is_none() {
            let mut sources = sources;
            if let Err(error) = sources.persist() {
                sources.status = format!("初始化下载队列失败：{error:#}；原有存档未被覆盖");
            }
            sources.submit(Task::List);
            return sources;
        }
        let mut sources = sources;
        sources.submit(Task::List);
        sources
    }
    /// Pops one finished chapter for the library importer. Call in a loop:

    /// several downloads can finish between two frames, and every id is kept

    /// until taken.

    pub fn take_downloaded(&mut self) -> Option<DownloadedChapter> {
        while let Some(id) = self.ready.pop_front() {
            let Some(task) = self.tasks.iter().find(|task| task.id == id) else {
                continue;
            };
            if task.status != DownloadStatus::Done || task.linked {
                continue;
            }
            let Some(path) = task.cbz.clone() else {
                continue;
            };
            if !path.exists() {
                continue;
            }
            self.import_attempts.insert(task.id.clone());
            return Some(DownloadedChapter {
                id: task.id.clone(),
                path,
                metadata: task.metadata(),
                read_page: task.read_page,
            });
        }
        None
    }
    /// Opens the exact source/manga/chapter already imported into the library.

    /// The host resolves the link to its book, preserving the current source

    /// page as the reader's return destination.

    pub fn take_open_chapter(&mut self) -> Option<ChapterMetadata> {
        self.open_chapter.take()
    }
    // -- follows -------------------------------------------------------------

    /// Followed manga with chapters the user has not opened yet.

    pub fn fresh_follows(&self) -> usize {
        self.follows
            .iter()
            .filter(|follow| !follow.fresh.is_empty())
            .count()
    }

    fn follow_index(&self, source: &str, manga: &str) -> Option<usize> {
        self.follows
            .iter()
            .position(|follow| follow.source_id == source && follow.manga_id == manga)
    }

    fn save_follows(&mut self) {
        if let Err(error) = follows::save(&self.root, &self.follows) {
            self.status = format!("追更列表保存失败：{error:#}");
        }
    }

    fn toggle_follow(&mut self, manga: &Manga) {
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

    fn follow_manga(&self, follow: &Follow) -> anyhow::Result<Manga> {
        let bytes = std::fs::read(manga_snapshot_path(&self.root, &follow.snapshot)?)?;
        let mut manga = Manga::from_postcard_bytes(&bytes)
            .map_err(|error| anyhow::anyhow!("漫画快照已损坏：{error}"))?;
        manga.source_key = follow.source_id.clone();
        Ok(manga)
    }

    fn open_follow(&mut self, index: usize) -> bool {
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

    /// Opens a followed work's detail page, remembering whether it came from
    /// the shelf so 返回 can hand back to 书架. `resume` makes the chapter
    /// list auto-fire the existing 继续阅读 once it lands.
    pub(crate) fn open_work(&mut self, source: &str, manga: &str, resume: bool) -> bool {
        let Some(index) = self.follow_index(source, manga) else {
            return false;
        };
        if !self.open_follow(index) {
            return false;
        }
        self.shelf_origin = true;
        self.pending_resume = resume;
        true
    }
    /// `open_work` marked this visit as shelf-originated: 返回 pops to 书架.
    pub(crate) fn take_shelf_return(&mut self) -> bool {
        std::mem::take(&mut self.shelf_return)
    }
    /// A resume queued by the shelf is still waiting for the chapter list.
    pub(crate) fn resume_pending(&self) -> bool {
        self.pending_resume
    }
    /// The followed works, in follow order (newest first).
    pub(crate) fn follows(&self) -> &[Follow] {
        &self.follows
    }
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
    #[cfg(test)]
    pub(crate) fn seed_read_at(
        &mut self,
        source: &str,
        manga: &str,
        chapter: &str,
        title: &str,
        at: i64,
    ) {
        let mut task = DownloadTask::from_chapter(
            source,
            &Manga {
                key: manga.into(),
                title: manga.into(),
                ..Default::default()
            },
            &Chapter {
                key: chapter.into(),
                title: Some(title.into()),
                ..Default::default()
            },
        );
        task.read_at = Some(at);
        self.tasks.push(task);
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
    /// Cover texture for a followed work via the `source-covers` pipeline;
    /// queues a fetch while it is still missing.
    pub(crate) fn work_cover(
        &mut self,
        ctx: &Context,
        source: &str,
        manga: &str,
    ) -> Option<TextureHandle> {
        let index = self.follow_index(source, manga)?;
        let url = self.follow_cover_url(index)?;
        let texture = self.cover_image(ctx, source, &url);
        if texture.is_none() {
            self.plan_covers(vec![CoverReq {
                key: cover_key(source, &url),
                source: source.to_string(),
                url,
            }]);
        }
        texture
    }
    /// The followed manga's cover URL, read from its snapshot once per

    /// session and cached. `None` until the snapshot loads or when the manga

    /// has no cover, which also avoids a per-frame file read.
    fn follow_cover_url(&mut self, index: usize) -> Option<String> {
        let snapshot = self.follows.get(index)?.snapshot.clone();
        if let Some(url) = self.follow_cover_urls.get(&snapshot) {
            return url.clone();
        }
        let url = manga_snapshot_path(&self.root, &snapshot)
            .ok()
            .and_then(|path| std::fs::read(path).ok())
            .and_then(|bytes| Manga::from_postcard_bytes(&bytes).ok())
            .and_then(|manga| manga.cover.filter(|url| !url.is_empty()));
        self.follow_cover_urls.insert(snapshot, url.clone());
        url
    }

    fn cover_texture(&mut self, ctx: &Context, snapshot: &str) -> Option<TextureHandle> {
        // Covers saved before the `source-covers` unification stay readable;

        // nothing is ever written under `follow-covers/` any more.

        if let Some(cached) = self.covers.get(snapshot) {
            return cached.clone();
        }
        let texture = follow_cover_path(&self.root, snapshot)
            .ok()
            .and_then(|path| std::fs::read(path).ok())
            .and_then(|bytes| image::load_from_memory(&bytes).ok())
            .map(|image| {
                let image = image.thumbnail(120, 160).to_rgba8();
                let size = [image.width() as usize, image.height() as usize];
                ctx.load_texture(
                    format!("follow-cover-{snapshot}"),
                    ColorImage::from_rgba_unmultiplied(size, image.as_raw()),
                    TextureOptions::LINEAR,
                )
            });
        self.covers.insert(snapshot.to_string(), texture.clone());
        texture
    }
    /// Returns the texture for a result-grid cover, or `None` while it is

    /// still missing (the caller then queues a fetch). Freshly delivered

    /// images are textured on first paint; the disk cache is probed once per

    /// URL so a cold start never hits the filesystem twice for the same key.

    fn cover_image(&mut self, ctx: &Context, source: &str, url: &str) -> Option<TextureHandle> {
        if url.is_empty() {
            return None;
        }
        let key = cover_key(source, url);
        if let Some(texture) = self.cover_textures.get(&key) {
            let texture = texture.clone();
            self.cover_lru.retain(|entry| entry != &key);
            self.cover_lru.push_back(key);
            return Some(texture);
        }
        let image = match self.cover_ready.remove(&key) {
            Some(image) => image.clone(),
            None if self.cover_missing_disk.contains(&key) => None,
            None => {
                let image = source_cover_path(&self.root, &key)
                    .ok()
                    .and_then(|path| std::fs::read(path).ok())
                    .and_then(|bytes| image::load_from_memory(&bytes).ok())
                    .map(|image| {
                        let rgba = image.to_rgba8();
                        ColorImage::from_rgba_unmultiplied(
                            [rgba.width() as usize, rgba.height() as usize],
                            rgba.as_raw(),
                        )
                    });
                if image.is_none() {
                    self.cover_missing_disk.insert(key.clone());
                }
                image
            }
        }?;
        let texture =
            ctx.load_texture(format!("source-cover-{key}"), image, TextureOptions::LINEAR);
        self.cover_textures.insert(key.clone(), texture.clone());
        self.cover_lru.push_back(key);
        while self.cover_textures.len() > COVER_TEXTURE_CAP {
            match self.cover_lru.pop_front() {
                Some(evicted) => {
                    self.cover_textures.remove(&evicted);
                }
                None => break,
            }
        }
        Some(texture)
    }
    /// Keeps the cover queue to what is nearly visible: cells that scrolled

    /// away drop out, new wants append in order, and the queue never exceeds

    /// `COVER_QUEUE_CAP` (the oldest requests are dropped first).

    fn plan_covers(&mut self, want: Vec<CoverReq>) {
        let skip: HashSet<String> = self
            .cover_inflight
            .iter()
            .chain(self.cover_failed.iter())
            .cloned()
            .collect();
        plan_covers(&mut self.cover_queue, want, &skip);
        while self.cover_outstanding < COVER_INFLIGHT_CAP {
            let Some(req) = self.cover_queue.pop_back() else {
                break;
            };
            if self.cover_req_tx.send(req.clone()).is_err() {
                break;
            }
            self.cover_inflight.insert(req.key);
            self.cover_outstanding += 1;
        }
    }
    /// Reading progress the host knows about, by `identity_id` chapter key.

    /// Called once per sources frame so 继续阅读 and row states stay current.

    pub fn set_chapter_progress(
        &mut self,
        map: std::collections::HashMap<String, (usize, usize, Option<i64>)>,
    ) {
        self.chapter_progress = map;
    }
    /// The chapter 继续阅读 should open: the one touched most recently across

    /// library progress and online streams; when it is finished, the next

    /// unlocked chapter in reading order. Unread works start at the first

    /// unlocked chapter. The flag reports whether any progress existed.

    fn resume_chapter(
        &self,
        source: &str,
        manga: &Manga,
        ordered: &[Chapter],
        states: &std::collections::HashMap<String, ChapterQueueState>,
    ) -> (Option<Chapter>, bool) {
        let mut best: Option<(usize, i64)> = None;
        for (index, chapter) in ordered.iter().enumerate() {
            let id = identity_id(source, &manga.key, &chapter.key);
            let read_at = [
                self.chapter_progress.get(&id).and_then(|(_, _, at)| *at),
                states.get(&chapter.key).and_then(|state| state.read_at),
            ]
            .into_iter()
            .flatten()
            .max();
            if let Some(at) = read_at {
                if best.is_none_or(|(_, stamp)| at > stamp) {
                    best = Some((index, at));
                }
            }
        }
        match best {
            Some((index, _)) => {
                let chapter = &ordered[index];
                let id = identity_id(source, &manga.key, &chapter.key);
                let finished = self
                    .chapter_progress
                    .get(&id)
                    .is_some_and(|&(page, count, _)| count > 0 && page + 1 >= count)
                    || states.get(&chapter.key).is_some_and(|state| {
                        state.total > 0
                            && state.read_page.is_some_and(|page| page + 1 >= state.total)
                    });
                if finished {
                    let next = ordered[index + 1..].iter().find(|item| !item.locked);
                    (next.or(Some(chapter)).cloned(), true)
                } else {
                    (Some(chapter.clone()), true)
                }
            }
            None => {
                // A migrated work with no reading record on this source yet
                // resumes by chapter number: the chapter equal to the carried
                // number (it was not finished) or the first one past it.
                if let Some(follow) = self
                    .follows
                    .iter()
                    .find(|follow| follow.source_id == source && follow.manga_id == manga.key)
                    && let Some(number) = follow.resume_number
                {
                    let target = ordered
                        .iter()
                        .find(|chapter| {
                            let value = chapter_number_value(chapter);
                            value.is_some_and(|value| {
                                if follow.resume_done {
                                    value > number
                                } else {
                                    value >= number
                                }
                            })
                        })
                        .or_else(|| {
                            // No chapter reaches the carried number: resume at
                            // the newest available chapter, not the first.
                            ordered.iter().rev().find(|chapter| !chapter.locked)
                        })
                        .or_else(|| ordered.last());
                    return (target.cloned(), true);
                }
                (
                    ordered
                        .iter()
                        .find(|chapter| !chapter.locked)
                        .or_else(|| ordered.first())
                        .cloned(),
                    false,
                )
            }
        }
    }
    /// `background` marks a scheduler run: it keeps the same single-flight

    /// mutex as the manual button and reports its result to the app's notice

    /// bar instead of overwriting the sources-page status line.

    pub fn check_updates(&mut self, background: bool) {
        if self.checking > 0 {
            return;
        }
        let all: Vec<usize> = (0..self.follows.len()).collect();
        self.check_follows(&all, background);
    }

    fn check_follows(&mut self, indices: &[usize], background: bool) {
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

    // -- 书源管理: repos, update badges, tests, uninstall ------------------

    /// `source-repos` setting restored at startup.
    pub(crate) fn set_repos_setting(&mut self, json: &str) {
        self.repos.apply_urls(json);
    }

    /// `source-repos-checked-at` setting restored at startup (epoch day).
    pub(crate) fn set_repos_checked_day(&mut self, day: Option<i64>) {
        self.repos.checked_day = day;
    }

    /// Setting writes the host persists (`source-repos`, `-checked-at`).
    pub(crate) fn take_setting_writes(&mut self) -> Vec<(String, String)> {
        self.setting_writes.drain(..).collect()
    }

    fn write_setting(&mut self, key: &str, value: String) {
        self.setting_writes.push_back((key.to_string(), value));
    }

    /// Number of installed sources a cached index offers a newer build of.
    pub(crate) fn updatable_count(&self) -> usize {
        self.updatable().len()
    }

    fn updatable(&self) -> std::collections::BTreeMap<String, i64> {
        self.repos.updates(
            self.installed
                .iter()
                .map(|source| (source.manifest.id.as_str(), source.manifest.version)),
        )
    }

    /// Manual or daily auto refresh of every saved repo index. Runs as a
    /// background task (request id 0): it never shows the busy spinner and
    /// its reply is accepted even when the user navigated elsewhere.
    pub(crate) fn check_repo_updates(&mut self) {
        if self.repo_fetching || self.repos.urls.is_empty() {
            return;
        }
        self.repo_fetching = true;
        let today = crate::source_repos::epoch_day(follows::now());
        self.repos.checked_day = Some(today);
        self.write_setting("source-repos-checked-at", today.to_string());
        if self
            .tx
            .send((0, Task::FetchAllRepos(self.repos.urls.clone())))
            .is_err()
        {
            self.repo_fetching = false;
            self.status = "书源线程已停止，请重新打开应用".into();
        }
    }

    /// Once per launch: refresh repo indexes when `source-repos-checked-at`
    /// is not today. Check only — nothing installs itself.
    fn maybe_auto_repo_check(&mut self) {
        if self.repo_auto_done {
            return;
        }
        self.repo_auto_done = true;
        let today = crate::source_repos::epoch_day(follows::now());
        if self.repos.checked_day != Some(today) {
            self.check_repo_updates();
        }
    }

    /// Merges a FetchAllRepos result: successful indexes refresh the cache,
    /// failed repos keep their previous data.
    fn apply_repo_all(&mut self, results: Vec<(String, Result<RepositoryIndex, String>)>) {
        self.repo_fetching = false;
        let now = follows::now();
        let mut fetched = 0usize;
        let mut failed = 0usize;
        for (url, result) in results {
            match result {
                Ok(index) => {
                    fetched += 1;
                    if self.repos.record_repo(&url) {
                        self.write_setting("source-repos", self.repos.urls_setting());
                    }
                    self.repos.merge_index(&url, &index, now);
                }
                Err(_) => failed += 1,
            }
        }
        if fetched > 0
            && let Err(error) = self.repos.save_cache(&self.root)
        {
            self.status = format!("书源仓库缓存保存失败：{error:#}");
            return;
        }
        let updates = self.updatable_count();
        self.status = if failed > 0 {
            format!("{fetched} 个仓库已更新（{failed} 个读取失败），{updates} 个书源有新版本")
        } else if updates > 0 {
            format!("{updates} 个书源有新版本")
        } else {
            "书源全部是最新版本".into()
        };
    }

    /// Install the newest repo build of one source.
    pub(crate) fn update_source(&mut self, id: &str) {
        if let Some(entry) = self.repos.update_entry(id) {
            self.submit(Task::Install(entry));
        }
    }

    /// 全部更新: install every source the cache says is behind.
    fn update_all(&mut self) {
        let updates = self.updatable();
        for id in updates.keys() {
            self.update_source(id);
        }
    }

    /// 测试 one source: first listing page (or a search for "a"), ≤4
    /// concurrently, 15 s budget enforced by `pump_repo_tests`.
    pub(crate) fn test_source(&mut self, id: &str) {
        if self.repo_testing.contains_key(id) || self.repo_test_queue.iter().any(|q| q == id) {
            return;
        }
        self.repo_test_queue.push_back(id.to_string());
        self.pump_repo_tests();
    }

    fn test_all(&mut self) {
        for source in self.installed.clone() {
            self.test_source(&source.manifest.id);
        }
    }

    /// Starts queued tests while fewer than four run, marks timeouts.
    fn pump_repo_tests(&mut self) {
        let now = std::time::Instant::now();
        let timed_out: Vec<String> = self
            .repo_testing
            .iter()
            .filter(|(_, started)| started.elapsed() > std::time::Duration::from_secs(15))
            .map(|(id, _)| id.clone())
            .collect();
        for id in timed_out {
            self.repo_testing.remove(&id);
            self.repo_test_results
                .insert(id, "测试超时（15 秒）".into());
        }
        let _ = now;
        while self.repo_testing.len() < 4
            && let Some(id) = self.repo_test_queue.pop_front()
        {
            let catalog = SourceCatalog::new(self.root.join("sources"));
            let tx = self.test_tx.clone();
            self.repo_testing
                .insert(id.clone(), std::time::Instant::now());
            thread::Builder::new()
                .name(format!("yejian-source-test-{id}"))
                .spawn(move || {
                    let started = std::time::Instant::now();
                    let result = (|| -> Result<u64, String> {
                        let engine = SourceEngine::load(&catalog, &id)
                            .map_err(|error| format!("书源无法加载：{error:#}"))?;
                        let listings = pollster::block_on(engine.listings())
                            .unwrap_or_else(|_| engine.manifest().listings.clone());
                        if let Some(listing) = listings.first() {
                            pollster::block_on(engine.browse(&listing.id, 1))
                                .map_err(|error| format!("首页列表读取失败：{error:#}"))?;
                        } else {
                            pollster::block_on(engine.search_page("a", 1, Vec::new()))
                                .map_err(|error| format!("搜索失败：{error:#}"))?;
                        }
                        Ok(started.elapsed().as_millis() as u64)
                    })();
                    let _ = tx.send((id, result));
                })
                .ok();
        }
    }

    /// Finished test results land in `repo_test_results`.
    fn drain_repo_tests(&mut self) {
        while let Ok((id, result)) = self.test_rx.try_recv() {
            if self.repo_testing.remove(&id).is_none() {
                continue; // already timed out; drop the late answer
            }
            let text = match result {
                Ok(ms) => format!("可用 · {ms} ms"),
                Err(reason) => reason,
            };
            self.repo_test_results.insert(id, text);
        }
        self.pump_repo_tests();
    }

    /// 卸载 asks first; the card explains downloads stay and follows stop.
    pub(crate) fn ask_uninstall(&mut self, source: InstalledSource) {
        self.uninstall = Some(source);
    }

    /// Runs the removal: source dir via the worker, then settings/cookies,
    /// the engine cache and any selection pointing at it.
    pub(crate) fn confirm_uninstall(&mut self) {
        let Some(source) = self.uninstall.take() else {
            return;
        };
        let id = source.manifest.id.clone();
        // Forget the engine everywhere so nothing re-loads the deleted dir.
        self.stale_engines.lock().unwrap().insert(id.clone());
        self.repo_test_results.remove(&id);
        self.repo_testing.remove(&id);
        self.repo_test_queue.retain(|queued| queued != &id);
        // Per-source settings and web cookies go with the source.
        self.source_settings.remove(&id);
        if let Err(error) = save_source_settings(&self.root, &self.source_settings) {
            self.status = format!("书源设置清理失败：{error:#}");
        }
        let hosts: HashSet<String> = source
            .manifest
            .urls
            .iter()
            .map(|url| url_host(url))
            .filter(|host| !host.is_empty())
            .collect();
        if !hosts.is_empty() {
            self.web_cookies
                .retain(|url, _| !hosts.contains(&url_host(url)));
            if let Err(error) = self.save_web_cookies() {
                self.status = format!("网页登录信息清理失败：{error:#}");
            }
        }
        if self.selected_source == id {
            self.select_source("");
        }
        // Follows stay on the shelf but stop checking until the source returns.
        let mut touched = false;
        for follow in &mut self.follows {
            if follow.source_id == id && !follow.source_removed {
                follow.source_removed = true;
                touched = true;
            }
        }
        if touched {
            self.save_follows();
        }
        self.submit(Task::Uninstall(id));
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

    fn handle_update(&mut self, event: UpdateEvent) {
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

    fn follows_ui(&mut self, ui: &mut Ui) {
        if self.follows.is_empty() {
            return;
        }
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
            if texture.is_none()
                && let Some(url) = cover_url
            {
                want_covers.push(CoverReq {
                    key: cover_key(&source_id, &url),
                    source: source_id,
                    url,
                });
            }
            let texture = match texture {
                Some(texture) => Some(texture),
                None => self.cover_texture(ui.ctx(), &snapshot),
            };
            if let Some(texture) = texture {
                let cover =
                    Rect::from_center_size(pos2(left + 18., (top + bottom) / 2.), vec2(40., 54.));
                ui.painter().image(
                    texture.id(),
                    cover,
                    Rect::from_min_max(pos2(0., 0.), pos2(1., 1.)),
                    Color32::WHITE,
                );
            }
        }
        if let Some(index) = open {
            self.open_follow(index);
        }
        self.plan_covers(want_covers);
        ui.add_space(20.);
    }
    /// Pops a chapter the user started reading online.

    pub fn take_stream(&mut self) -> Option<StreamChapter> {
        self.stream_request.take()
    }

    pub fn stream_state(&self, id: &str) -> Option<StreamState> {
        let task = self.tasks.iter().find(|task| task.id == id)?;
        let stopped = match task.status {
            DownloadStatus::Failed => Some(task.error.clone().unwrap_or_else(|| "下载失败".into())),
            DownloadStatus::Paused if self.preempted.as_deref() != Some(id) => {
                Some(task.error.clone().unwrap_or_else(|| "下载已暂停".into()))
            }
            _ => None,
        };
        Some(StreamState {
            total: task.pages_total,
            done: task.pages_done,
            stopped,
            signature: task.pages_signature.clone(),
            page: task.read_page,
        })
    }
    /// Remembers the page of a chapter read online so it reopens there.

    pub fn set_stream_page(&mut self, id: &str, page: usize) {
        let Some(task) = self.tasks.iter_mut().find(|task| task.id == id) else {
            return;
        };
        if task.read_page == Some(page) {
            return;
        }
        task.read_page = Some(page);
        task.read_at = Some(follows::now() as i64);
        if let Err(error) = self.persist() {
            self.status = format!("阅读位置保存失败：{error:#}");
        }
    }
    /// Runs this chapter's download next. A different running download is

    /// paused at its next page boundary and re-queued behind it.

    fn prioritize(&mut self, id: &str) {
        if self.running.as_deref() == Some(id) {
            return;
        }
        if !self.tasks.iter().any(|task| {
            task.id == id
                && matches!(
                    task.status,
                    DownloadStatus::Queued | DownloadStatus::Paused | DownloadStatus::Failed
                )
        }) {
            return;
        }
        if let Some(running) = self.running.clone() {
            self.preempted = Some(running);
            self.dl_stop.store(STOP_PAUSE, Ordering::SeqCst);
        }
        self.set_task_queued(id);
    }

    pub fn resume_stream(&mut self, id: &str) {
        self.prioritize(id);
    }
    /// A bookmarked online chapter reopens as a stream when its download
    /// task exists; returns false when nothing for the chapter was queued.
    pub(crate) fn open_bookmarked(&mut self, source: &str, manga: &str, chapter: &str) -> bool {
        let Some(task) = self.tasks.iter().find(|task| {
            let meta = task.metadata();
            meta.source_id == source && meta.manga_id == manga && meta.chapter_id == chapter
        }) else {
            return false;
        };
        let id = task.id.clone();
        let metadata = task.metadata();
        self.prioritize(&id);
        self.stream_request = Some(StreamChapter {
            id,
            metadata,
            from_end: false,
        });
        true
    }
    #[cfg(debug_assertions)]
    /// A paused online chapter with no staged pages — the acceptance
    /// share-disabled state needs bytes that never arrive.
    pub(crate) fn preview_share_stream(&mut self) -> Option<StreamChapter> {
        self.clear_status();
        let manga = Manga {
            source_key: "acceptance".into(),
            key: "share-preview".into(),
            title: "分享验收".into(),
            ..Default::default()
        };
        let chapter = Chapter {
            key: "c1".into(),
            title: Some("第 1 话".into()),
            ..Default::default()
        };
        let mut task = DownloadTask::from_chapter("acceptance", &manga, &chapter);
        task.pages_total = 4;
        task.status = DownloadStatus::Paused;
        if self.tasks.iter().any(|queued| queued.id == task.id) {
            return None;
        }
        let id = task.id.clone();
        let metadata = task.metadata();
        self.tasks.push(task);
        Some(StreamChapter {
            id,
            metadata,
            from_end: false,
        })
    }
    /// Reads a chapter without waiting for the whole download: the chapter

    /// is downloaded first, the reader shows pages as they arrive, and the

    /// following chapter is queued so it is ready when this one ends.

    fn read_online(&mut self, manga: &Manga, chapter: &Chapter, from_end: bool) {
        let source = self.source_for(manga);
        let id = identity_id(&source, &manga.key, &chapter.key);
        if let Some(metadata) = self.library_chapters.get(&id) {
            self.open_chapter = Some(metadata.clone());
            return;
        }
        if chapter.locked {
            self.status = "这一章暂不可阅读".into();
            return;
        }
        let queued = self
            .tasks
            .iter()
            .any(|task| task.id == id && task.status != DownloadStatus::Failed);
        if !queued {
            self.enqueue_chapter(manga, chapter);
        }
        let Some(task) = self.tasks.iter().find(|task| task.id == id) else {
            return;
        };
        if task.status == DownloadStatus::Done {
            if task.linked || self.import_attempts.contains(&id) {
                return;
            }
            self.add_downloaded_to_library(&id);
            self.status = "这一章已下载，正在加入书架".into();
            return;
        }
        let metadata = task.metadata();
        self.prioritize(&id);
        self.stream_request = Some(StreamChapter {
            id,
            metadata,
            from_end,
        });
        if let Some(next) = self.adjacent_online(&source, &manga.key, &chapter.key, 1)
            && self.chapter_needs_download(&source, &manga.key, &next)
        {
            let status = std::mem::take(&mut self.status);
            self.enqueue_chapter(manga, &next);
            self.status = status;
        }
    }
    /// The neighbouring chapter in reading order, when that manga's chapter

    /// list is the one currently loaded.

    fn adjacent_online(
        &self,
        source: &str,
        manga: &str,
        chapter: &str,
        delta: i32,
    ) -> Option<Chapter> {
        let selected = self.selected_manga.as_ref()?;
        if selected.key != manga || self.source_for(selected) != source {
            return None;
        }
        let chapters = chapter_list(&self.chapters, "", false);
        let index = chapters.iter().position(|item| item.key == chapter)? as i64 + delta as i64;
        usize::try_from(index)
            .ok()
            .and_then(|index| chapters.get(index).cloned())
    }
    /// Chapter key of the neighbour in the source's reading order, so the

    /// reader does not skip a chapter that is not in the library yet.

    pub fn adjacent_online_key(&self, metadata: &ChapterMetadata, delta: i32) -> Option<String> {
        self.adjacent_online(
            &metadata.source_id,
            &metadata.manga_id,
            &metadata.chapter_id,
            delta,
        )
        .map(|chapter| chapter.key)
    }

    /// Display name of the neighbouring chapter in reading order.
    pub fn adjacent_online_name(&self, metadata: &ChapterMetadata, delta: i32) -> Option<String> {
        self.adjacent_online(
            &metadata.source_id,
            &metadata.manga_id,
            &metadata.chapter_id,
            delta,
        )
        .map(|chapter| chapter_name(&chapter))
    }

    pub fn has_adjacent_online(&self, metadata: &ChapterMetadata, delta: i32) -> bool {
        self.adjacent_online(
            &metadata.source_id,
            &metadata.manga_id,
            &metadata.chapter_id,
            delta,
        )
        .is_some_and(|chapter| !chapter.locked)
    }
    /// Queues the chapter after `metadata` for download without opening it,

    /// so the reader can keep scrolling straight into it. Returns the stream

    /// task id and its metadata once the chapter is tracked; `None` when the

    /// chapter is unknown, locked, already imported, or cannot be queued

    /// here (its manga detail is not loaded).

    pub fn prepare_next(
        &mut self,
        metadata: &ChapterMetadata,
    ) -> Option<(String, ChapterMetadata)> {
        let chapter = self.adjacent_online(
            &metadata.source_id,
            &metadata.manga_id,
            &metadata.chapter_id,
            1,
        )?;
        if chapter.locked {
            return None;
        }
        let id = identity_id(&metadata.source_id, &metadata.manga_id, &chapter.key);
        if self.library_chapters.contains_key(&id) {
            return None;
        }
        if self.chapter_needs_download(&metadata.source_id, &metadata.manga_id, &chapter) {
            let manga = self.selected_manga.clone().filter(|manga| {
                self.source_for(manga) == metadata.source_id && manga.key == metadata.manga_id
            })?;
            let status = std::mem::take(&mut self.status);
            self.enqueue_chapter(&manga, &chapter);
            self.status = status;
        }
        let task = self.tasks.iter().find(|task| task.id == id)?;
        let metadata = task.metadata();
        match task.status {
            // Its staged pages are already archived: push the finished file

            // at the import queue so the segment becomes a library book.
            DownloadStatus::Done => {
                if !task.linked {
                    self.add_downloaded_to_library(&id);
                }
                None
            }
            // A chapter paused earlier keeps downloading now that the reader

            // approaches it; the preempted stream itself is left alone.
            DownloadStatus::Paused | DownloadStatus::Failed
                if self.preempted.as_deref() != Some(id.as_str()) =>
            {
                self.set_task_queued(&id);
                Some((id, metadata))
            }
            _ => Some((id, metadata)),
        }
    }
    /// Task metadata of a tracked download/stream chapter.

    pub fn stream_metadata(&self, id: &str) -> Option<ChapterMetadata> {
        self.tasks
            .iter()
            .find(|task| task.id == id)
            .map(|task| task.metadata())
    }
    /// Pretend a manga detail page is loaded so neighbour lookups work.

    #[cfg(test)]
    pub(crate) fn seed_online_detail(&mut self, manga: Manga, chapters: Vec<Chapter>) {
        self.selected_source = manga.source_key.clone();
        self.selected_manga = Some(manga);
        self.chapters = chapters;
    }
    /// Marks a tracked task's page count as known, as its first progress

    /// report would.

    #[cfg(test)]
    pub(crate) fn force_pages(&mut self, id: &str, total: usize, done: usize) {
        if let Some(task) = self.tasks.iter_mut().find(|task| task.id == id) {
            task.pages_total = total;
            task.pages_done = done;
        }
    }
    /// The source's preferred reading direction for this work, from the

    /// manga details still in memory or its saved snapshot. `Unknown` counts

    /// as no hint.

    pub fn viewer_hint(&self, source_id: &str, manga_id: &str) -> Option<Viewer> {
        let viewer = self
            .selected_manga
            .as_ref()
            .filter(|manga| self.source_for(manga) == source_id && manga.key == manga_id)
            .map(|manga| manga.viewer)
            .or_else(|| {
                let path =
                    manga_snapshot_path(&self.root, &manga_snapshot_key(source_id, manga_id))
                        .ok()?;
                let bytes = std::fs::read(path).ok()?;
                Manga::from_postcard_bytes(&bytes)
                    .ok()
                    .map(|manga| manga.viewer)
            });
        viewer.filter(|viewer| *viewer != Viewer::Unknown)
    }
    /// Opens the previous/next chapter of `metadata`, from the library when it

    /// is already there and online otherwise. Returns false when unknown.

    pub fn open_adjacent_online(&mut self, metadata: &ChapterMetadata, delta: i32) -> bool {
        let Some(chapter) = self.adjacent_online(
            &metadata.source_id,
            &metadata.manga_id,
            &metadata.chapter_id,
            delta,
        ) else {
            return false;
        };
        if chapter.locked {
            return false;
        }
        let Some(manga) = self.selected_manga.clone() else {
            return false;
        };
        self.read_online(&manga, &chapter, delta < 0);
        true
    }

    pub fn set_library_chapters(&mut self, links: &[ChapterLink]) {
        self.library_chapters = links
            .iter()
            .map(|link| {
                let metadata = &link.metadata;
                (
                    identity_id(
                        &metadata.source_id,
                        &metadata.manga_id,
                        &metadata.chapter_id,
                    ),
                    metadata.clone(),
                )
            })
            .collect();
    }

    fn read_downloaded(&mut self, task: &DownloadTask) {
        if let Some(metadata) = self.library_chapters.get(&task.id) {
            self.open_chapter = Some(metadata.clone());
        }
    }
    /// Marks one finished chapter as imported and linked. Call only after both

    /// `import` and `link_chapter` succeeded; otherwise the file stays pending

    /// and is offered again on the next start without re-downloading it.

    pub fn ack_downloaded(&mut self, id: &str) {
        self.import_attempts.remove(id);
        let mut changed = false;
        if let Some(task) = self.tasks.iter_mut().find(|task| task.id == id) {
            task.linked = true;
            task.error = None;
            changed = true;
        }
        if changed {
            if let Err(error) = self.persist() {
                self.status =
                    format!("已加入书架，但队列状态保存失败：{error:#}；重启后可能重复导入此文件");
            }
        }
    }
    /// Puts a chapter back at the front of the import queue, for a same-session

    /// retry after a failed import. The CBZ is never re-downloaded.

    pub fn requeue_import(&mut self, id: &str) {
        self.import_attempts.remove(id);
        if self.tasks.iter().any(|task| {
            task.id == id
                && task.status == DownloadStatus::Done
                && !task.linked
                && task.cbz.as_ref().is_some_and(|path| path.exists())
        }) && !self.ready.iter().any(|queued| queued == id)
        {
            self.ready.push_front(id.to_string());
        }
    }

    fn add_downloaded_to_library(&mut self, id: &str) {
        let Some(task) = self.tasks.iter_mut().find(|task| task.id == id) else {
            return;
        };
        if task.status != DownloadStatus::Done {
            return;
        }
        if !task.cbz.as_ref().is_some_and(|path| path.is_file()) {
            task.status = DownloadStatus::Failed;
            task.error = Some("下载文件已丢失，可重试下载".into());
            task.cbz = None;
            self.status = "下载文件已丢失，请重试下载此章".into();
            if let Err(error) = self.persist() {
                self.status
                    .push_str(&format!("；队列状态保存失败：{error:#}"));
            }
            return;
        }
        task.linked = false;
        self.requeue_import(id);
    }
    /// Reports a failed library import/link for a downloaded file. The CBZ is

    /// kept and the task stays `Done`/unlinked; the UI shows an immediate

    /// manual retry instead of silently re-offering or re-downloading it.

    pub fn import_failed(&mut self, id: &str, error: &str) {
        self.import_attempts.remove(id);
        if let Some(task) = self.tasks.iter_mut().find(|task| task.id == id) {
            task.error = Some(format!("加入书架失败：{error}"));
        }
        self.status = format!("加入书架失败：{error}；文件已保留，可点“重试导入”");
        if let Err(persist_error) = self.persist() {
            self.status
                .push_str(&format!("；队列状态保存失败：{persist_error:#}"));
        }
    }

    fn persist(&self) -> anyhow::Result<()> {
        let store = TaskStore {
            tasks: self.tasks.clone(),
        };
        let bytes = serde_json::to_vec(&store)
            .map_err(|error| anyhow::anyhow!("序列化下载队列失败：{error}"))?;
        std::fs::create_dir_all(&self.root)
            .map_err(|error| anyhow::anyhow!("创建数据目录失败：{error}"))?;
        let path = self.root.join("download-tasks.json");
        let tmp = self.root.join("download-tasks.json.tmp");
        let result: std::io::Result<()> = (|| {
            std::fs::write(&tmp, &bytes)?;
            std::fs::rename(&tmp, &path)?;
            Ok(())
        })();
        if let Err(error) = result {
            let _ = std::fs::remove_file(&tmp);
            return Err(anyhow::anyhow!("写入 {} 失败：{error}", path.display()));
        }
        Ok(())
    }

    fn submit(&mut self, task: Task) {
        self.request_id = self.request_id.wrapping_add(1);
        self.current_request
            .store(self.request_id, Ordering::Relaxed);
        self.failed = None;
        self.pending = Some(task.clone());
        if self.tx.send((self.request_id, task)).is_err() {
            self.busy = false;
            self.failed = self.pending.take();
            self.status = "书源线程已停止，请重新打开应用".into();
        } else {
            self.busy = true;
            self.status = "正在加载…".into();
        }
    }
    fn select_source(&mut self, id: &str) {
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
    /// True while the search field targets every installed source.
    fn global_mode(&self) -> bool {
        self.search_scope == "global"
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
    fn choose_scope(&mut self, scope: &str) {
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
    fn record_history(&mut self) {
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

    /// Whether the page's original bytes already exist locally — required
    /// before sharing is enabled.
    pub(crate) fn page_ready(&self, id: &str, page: usize) -> bool {
        let Some(task_id) = id.strip_prefix(STREAM_PREFIX) else {
            return true;
        };
        let Some(task) = self.tasks.iter().find(|task| task.id == task_id) else {
            return false;
        };
        if task.pages_done > page {
            return true;
        }
        task_dir(&self.root, task_id)
            .ok()
            .and_then(|dir| stored_page(&dir, page + 1))
            .is_some()
    }
    /// Width of the two-segment scope control, so the search row can budget.
    fn scope_segment_width(&self, ui: &Ui) -> f32 {
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
    fn scope_segment(&mut self, ui: &mut Ui, width: f32) {
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
    /// Short Chinese reason for a global-search failure, plus whether the
    /// existing web-verification flow applies.
    fn global_error_label(reason: &str) -> (&'static str, bool) {
        if reason == "超时" {
            ("超时", false)
        } else if reason.contains("cloudflare")
            || reason.contains("Cloudflare")
            || reason.contains("verify")
            || reason.contains("验证")
        {
            ("需要网页验证", true)
        } else if reason.contains("load")
            || reason.contains("加载")
            || reason.contains("WebAssembly")
        {
            ("书源无法加载", false)
        } else {
            ("网络错误", false)
        }
    }
    /// Fires one search per installed source; results stream back by group.
    fn global_search(&mut self) {
        self.listing = None;
        self.selected_manga = None;
        self.chapters.clear();
        // Global work reports through GlobalEvent, not the request worker —
        // no request is in flight, so a stale "正在加载…" must not linger.
        self.status.clear();
        self.failed = None;
        let query = self.query.trim().to_string();
        self.query = query.clone();
        self.global_query = query.clone();
        self.global_gen += 1;
        self.global_groups = self
            .installed
            .iter()
            .map(|source| GlobalGroup {
                source: source.manifest.id.clone(),
                state: GlobalState::Searching,
            })
            .collect();
        let sources: Vec<String> = self
            .global_groups
            .iter()
            .map(|group| group.source.clone())
            .collect();
        let _ = self.global_tx.send(GlobalCmd::Search {
            generation: self.global_gen,
            query,
            sources,
        });
    }
    /// Re-runs one source inside the current generation.
    fn retry_global(&mut self, source: &str) {
        if let Some(group) = self
            .global_groups
            .iter_mut()
            .find(|group| group.source == source)
        {
            group.state = GlobalState::Searching;
        }
        let _ = self.global_tx.send(GlobalCmd::Search {
            generation: self.global_gen,
            query: self.global_query.clone(),
            sources: vec![source.into()],
        });
    }

    // -- 换源 (source migration) -------------------------------------------

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
    fn apply_migrate(&mut self, done: anyhow::Result<MigrateOutcome>) {
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

    // -- 自动下载新章节 -----------------------------------------------------

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
    fn auto_download_added(&mut self, index: usize, list: &[Chapter], added: &[String]) {
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
    fn auto_downloading(&self, source: &str, manga: &str) -> usize {
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

    /// Group header: jump into that source and run the normal search.
    fn dive_into(&mut self, source: &str) {
        self.select_source(source);
        self.search_scope = "current".into();
        self.scope_dirty = Some("current".into());
        self.start_search();
    }
    /// A cover from another source opens under that source, like 我的追更.
    fn open_global_manga(&mut self, manga: Manga) {
        if manga.source_key == self.selected_source {
            self.open_manga(manga);
        } else {
            self.select_source(&manga.source_key.clone());
            self.pending_open = Some(manga);
        }
    }

    /// New search from the text field: page one of this source and query.

    fn start_search(&mut self) {
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
    fn filters_hidden(&self) -> bool {
        !self.query.trim().is_empty()
            && self.installed.iter().any(|source| {
                source.manifest.id == self.selected_source
                    && source.manifest.hides_filters_while_searching
            })
    }
    fn start_listing(&mut self, id: &str) {
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
    fn load_next_page(&mut self) {
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
    fn committed_filters(&self) -> Vec<FilterValue> {
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
    fn open_manga(&mut self, manga: Manga) {
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
    // -- download queue ----------------------------------------------------

    fn source_for(&self, manga: &Manga) -> String {
        if manga.source_key.is_empty() {
            self.selected_source.clone()
        } else {
            manga.source_key.clone()
        }
    }
    fn chapter_state(&self, source: &str, manga: &str, chapter: &str) -> Option<String> {
        let id = identity_id(source, manga, chapter);
        if self.library_chapters.contains_key(&id) {
            return Some("已在书架".into());
        }
        self.tasks
            .iter()
            .find(|task| task.id == id)
            .map(|task| task.status.label().to_string())
    }
    /// A failed chapter may be joined again from its manga detail page; that

    /// path also rewrites the saved Manga/Chapter snapshots.

    fn chapter_failed(&self, source: &str, manga: &str, chapter: &str) -> bool {
        let id = identity_id(source, manga, chapter);
        if self.library_chapters.contains_key(&id) {
            return false;
        }
        self.tasks
            .iter()
            .any(|task| task.id == id && task.status == DownloadStatus::Failed)
    }
    fn chapter_needs_download(&self, source: &str, manga: &str, chapter: &Chapter) -> bool {
        !chapter.locked
            && (self.chapter_state(source, manga, &chapter.key).is_none()
                || self.chapter_failed(source, manga, &chapter.key))
    }
    fn enqueue_chapter(&mut self, manga: &Manga, chapter: &Chapter) {
        let source = self.source_for(manga);
        if source.is_empty() {
            self.status = "请先选择并搜索一个书源".into();
            return;
        }
        let mut snapshot_manga = manga.clone();
        snapshot_manga.source_key = source.clone();
        let snapshot_key = match save_manga_snapshot(&self.root, &snapshot_manga) {
            Ok(key) => key,
            Err(error) => {
                self.status = format!("无法保存漫画快照，未开始下载：{error:#}");
                return;
            }
        };
        let mut task = DownloadTask::from_chapter(&source, manga, chapter);
        task.manga_snapshot = Some(snapshot_key);
        let id = task.id.clone();
        let title = task.chapter_title.clone();
        let was_failed = self
            .tasks
            .iter()
            .any(|task| task.id == id && task.status == DownloadStatus::Failed);
        let previous = self.tasks.clone();
        match enqueue_task(&mut self.tasks, task) {
            EnqueueOutcome::Added => {
                if let Err(error) = self.persist() {
                    self.tasks = previous;
                    self.status = format!("无法保存下载队列，未开始下载：{error:#}");
                    return;
                }
                self.status = format!("已加入下载队列：{title}");
                self.pump();
            }
            EnqueueOutcome::Duplicate => {
                if let Err(error) = self.persist() {
                    self.tasks = previous;
                    self.status = format!("无法保存下载队列，未开始下载：{error:#}");
                    return;
                }
                self.status = if was_failed {
                    format!("已用完整快照重新加入：{title}")
                } else {
                    format!("{title} 已在下载队列中")
                };
                self.pump();
            }
            EnqueueOutcome::AlreadyDownloaded => self.status = format!("{title} 已下载完成"),
        }
    }
    fn enqueue_missing(&mut self, manga: &Manga) {
        let source = self.source_for(manga);
        if source.is_empty() {
            return;
        }
        let mut snapshot_manga = manga.clone();
        snapshot_manga.source_key = source.clone();
        let snapshot_key = match save_manga_snapshot(&self.root, &snapshot_manga) {
            Ok(key) => key,
            Err(error) => {
                self.status = format!("无法保存漫画快照，未开始下载：{error:#}");
                return;
            }
        };
        let previous = self.tasks.clone();
        let mut added = 0usize;
        for chapter in chapter_list(&self.chapters, "", false) {
            if !self.chapter_needs_download(&source, &manga.key, &chapter) {
                continue;
            }
            let mut task = DownloadTask::from_chapter(&source, manga, &chapter);
            task.manga_snapshot = Some(snapshot_key.clone());
            if !matches!(
                enqueue_task(&mut self.tasks, task),
                EnqueueOutcome::AlreadyDownloaded
            ) {
                added += 1;
            }
        }
        if let Err(error) = self.persist() {
            self.tasks = previous;
            self.status = format!("无法保存下载队列，未开始下载：{error:#}");
            return;
        }
        self.status = if added == 0 {
            "没有需要加入的章节".into()
        } else {
            format!("已加入 {added} 个章节")
        };
        self.pump();
    }
    /// Dispatches the next queued task when the single download slot is free.

    fn pump(&mut self) {
        if self.running.is_some() {
            return;
        }
        let Some(index) = next_queued(&self.tasks) else {
            return;
        };
        let previous = self.tasks[index].status;
        self.tasks[index].status = DownloadStatus::Running;
        self.tasks[index].error = None;
        let id = self.tasks[index].id.clone();
        let task = self.tasks[index].clone();
        if let Err(error) = self.persist() {
            self.tasks[index].status = previous;
            self.status = format!("无法保存下载队列，暂不开始下载：{error:#}");
            return;
        }
        self.dl_stop.store(STOP_NONE, Ordering::SeqCst);
        self.running = Some(id.clone());
        if self.dl_tx.send(DlCmd::Run(task)).is_err() {
            self.running = None;
            if let Some(task) = self.tasks.iter_mut().find(|task| task.id == id) {
                task.status = DownloadStatus::Failed;
                task.error = Some("下载线程已停止，请重新打开应用".into());
            }
            if let Err(error) = self.persist() {
                self.status = format!("下载线程已停止，且队列状态保存失败：{error:#}");
            } else {
                self.status = "下载线程已停止，请重新打开应用".into();
            }
        }
    }
    fn set_task_queued(&mut self, id: &str) {
        let Some(position) = self.tasks.iter().position(|task| task.id == id) else {
            return;
        };
        let mut task = self.tasks.remove(position);
        let previous = task.clone();
        task.status = DownloadStatus::Queued;
        task.error = None;
        // Explicit continue/retry runs this task next, even if older

        // queued tasks are still waiting.

        self.tasks.insert(0, task);
        if let Err(error) = self.persist() {
            let _ = self.tasks.remove(0);
            self.tasks.insert(position.min(self.tasks.len()), previous);
            self.status = format!("无法保存下载队列，未开始下载：{error:#}");
            return;
        }
        self.pump();
    }
    fn request_pause(&mut self) {
        self.dl_stop.store(STOP_PAUSE, Ordering::SeqCst);
        self.status = "已请求暂停，当前图片请求结束后生效".into();
    }
    /// Retry a failed task. In a manga detail view the fresh complete

    /// Manga/Chapter are on hand, so rebuild the snapshots through the normal

    /// enqueue path instead of retrying a broken one; elsewhere just requeue.

    fn retry_task(&mut self, task: &DownloadTask) {
        if let Some(manga) = self.selected_manga.clone()
            && manga.key == task.manga_id
            && self.source_for(&manga) == task.source_id
        {
            if let Some(chapter) = self
                .chapters
                .iter()
                .find(|chapter| chapter.key == task.chapter_id)
                .cloned()
            {
                self.enqueue_chapter(&manga, &chapter);
                return;
            }
        }
        self.set_task_queued(&task.id);
    }
    fn request_cancel(&mut self, id: &str) {
        if self.running.as_deref() == Some(id) {
            self.dl_stop.store(STOP_CANCEL, Ordering::SeqCst);
            self.status = "已请求取消，当前图片请求结束后生效".into();
        } else {
            if self.remove_task_now(id, "已从下载队列移除") {
                self.pump();
            }
        }
    }
    /// Removes a task and only that task's staging directory. Downloaded CBZ

    /// files, other tasks and the library are never touched.

    fn remove_task_now(&mut self, id: &str, success: &str) -> bool {
        self.import_attempts.remove(id);
        let Some(position) = self.tasks.iter().position(|task| task.id == id) else {
            return true;
        };
        let removed = self.tasks.remove(position);
        if let Err(error) = self.persist() {
            self.tasks.insert(position.min(self.tasks.len()), removed);
            self.status = format!("无法保存下载队列，任务未移除：{error:#}");
            return false;
        }
        let cleanup = remove_task_cache(&self.root, id);
        self.status = match cleanup {
            Ok(()) => success.to_string(),
            Err(error) => format!("{success}，但缓存未清理：{error:#}"),
        };
        true
    }
    fn handle_download_event(&mut self, event: DlEvent) {
        if let DlEvent::Finished { id, .. } = &event
            && self.preempted.as_deref() == Some(id.as_str())
        {
            self.preempted = None;
        }
        match event {
            DlEvent::Progress {
                id,
                done,
                total,
                signature,
            } => {
                if let Some(task) = self.tasks.iter_mut().find(|task| task.id == id) {
                    task.pages_done = done;
                    task.pages_total = total;
                    if signature.is_some() {
                        task.pages_signature = signature;
                    }
                }
                if let Err(error) = self.persist() {
                    self.status = format!("下载队列保存失败：{error:#}；下载继续进行");
                }
            }
            DlEvent::Finished { id, path } => {
                if let Some(task) = self.tasks.iter_mut().find(|task| task.id == id) {
                    task.status = DownloadStatus::Done;
                    task.error = None;
                    task.cbz = Some(path.clone());
                    task.pages_done = task.pages_total;
                    task.linked = false;
                }
                if !self.ready.iter().any(|queued| queued == &id) {
                    self.ready.push_back(id.clone());
                }
                self.running = None;
                self.status = "章节下载完成，正在加入书架".into();
                if let Err(error) = self.persist() {
                    self.status = format!("章节已下载，但队列状态保存失败：{error:#}；文件已保留");
                    return;
                }
                self.pump();
            }
            DlEvent::Failed { id, error } => {
                // A timeout can race with a pause/cancel tap. Honor the

                // explicit user action once the in-flight request returns.

                let stop = self.dl_stop.load(Ordering::SeqCst);
                if self.running.as_deref() == Some(id.as_str()) && stop != STOP_NONE {
                    self.handle_download_event(DlEvent::Aborted { id, how: stop });
                    return;
                }
                if self.preempted.as_deref() == Some(id.as_str()) {
                    self.preempted = None;
                }
                if let Some(task) = self.tasks.iter_mut().find(|task| task.id == id) {
                    task.status = DownloadStatus::Failed;
                    task.error = Some(error.clone());
                }
                self.running = None;
                self.status = format!("章节下载失败：{error}");
                if let Err(error) = self.persist() {
                    self.status = format!("章节下载失败，且队列状态保存失败：{error:#}");
                    return;
                }
                // A failed chapter must not block the rest of the queue.

                self.pump();
            }
            DlEvent::Aborted { id, how } => {
                self.running = None;
                if how == STOP_CANCEL {
                    if self.remove_task_now(&id, "已取消下载") {
                        self.pump();
                    }
                } else {
                    let preempted = self.preempted.as_deref() == Some(id.as_str());
                    if preempted {
                        self.preempted = None;
                    }
                    if let Some(task) = self.tasks.iter_mut().find(|task| task.id == id) {
                        task.status = if preempted {
                            DownloadStatus::Queued
                        } else {
                            DownloadStatus::Paused
                        };
                        task.error = None;
                    }
                    self.status = if preempted {
                        "正在优先下载正在阅读的章节".into()
                    } else {
                        "下载已暂停，可稍后继续".into()
                    };
                    if let Err(error) = self.persist() {
                        self.status = format!("下载已暂停，但队列状态保存失败：{error:#}");
                        return;
                    }
                    self.pump();
                }
            }
            DlEvent::LockUnavailable { id, error } => {
                self.running = None;
                if let Some(task) = self.tasks.iter_mut().find(|task| task.id == id) {
                    task.status = DownloadStatus::Paused;
                    task.error = Some(error.clone());
                }
                self.status = format!("{error}；未改动任何缓存");
                if let Err(persist_error) = self.persist() {
                    self.status = format!("{error}；且队列状态保存失败：{persist_error:#}");
                }
                // Another instance still owns the lock. Do not immediately

                // retry the next queued task or hammer the same lock.
            }
        }
    }

    pub fn poll(&mut self) {
        self.drain_repo_tests();
        while let Ok(done) = self.cover_done_rx.try_recv() {
            self.cover_outstanding = self.cover_outstanding.saturating_sub(1);
            self.cover_inflight.remove(&done.key);
            if done.image.is_none() {
                self.cover_failed.insert(done.key.clone());
            }
            self.cover_ready.insert(done.key, done.image);
        }
        if let Some(origin) = take_web_challenge() {
            self.challenge = Some(origin);
        }
        match yejian_platform::poll_web() {
            Ok(Some(result)) => self.finish_web(result),
            Ok(None) => {}
            Err(error) => self.status = format!("网页结果读取失败：{error:?}"),
        }
        while let Ok((request_id, result)) = self.rx.try_recv() {
            // Every operation (including failures and detail loads) belongs to

            // one request. Same text with different filters/listing is a new

            // identity; old replies cannot clear its spinner or replace data.

            if request_id != self.request_id {
                // A completed installation still updates the catalogue even

                // when the user has navigated to another source in the meantime.

                if let Ok(Reply::Installed {
                    list,
                    updated: Some(_),
                }) = result
                {
                    self.installed = list;
                } else if let Ok(Reply::RepoAll(results)) = result {
                    self.apply_repo_all(results);
                }
                continue;
            }
            self.busy = false;
            let completed = self.pending.take();
            let reply = match result {
                Ok(reply) => reply,
                Err(error) => {
                    self.status = match &completed {
                        Some(Task::Search { page, .. } | Task::Browse { page, .. })
                            if *page > 1 =>
                        {
                            format!("第 {page} 页加载失败：{error}")
                        }
                        Some(Task::Metadata(_)) => format!("书源选项加载失败：{error}"),
                        Some(Task::Login { .. }) => format!("登录失败：{error}"),
                        Some(Task::Notify { .. }) => format!("操作失败：{error}"),
                        _ => format!("加载失败：{error}"),
                    };
                    self.failed = completed;
                    self.pending_resume = false;
                    if let Some(manga) = self.pending_open.take() {
                        self.open_manga(manga);
                    }
                    continue;
                }
            };
            match reply {
                Reply::Installed { list, updated } => {
                    self.installed = list;
                    self.status = "书源列表已更新".into();
                    if let Some(id) = updated {
                        if self.selected_source == id {
                            self.metadata_loaded = false;
                            self.applied_filters.clear();
                            self.submit(Task::Metadata(id));
                        }
                    }
                }
                Reply::Repository(index) => {
                    self.status = format!("找到 {} 个书源", index.sources.len());
                    // A fetched repo joins the saved list and its index
                    // feeds the update badges right away.
                    if self.repos.record_repo(&index.index_url) {
                        self.write_setting("source-repos", self.repos.urls_setting());
                    }
                    self.repos
                        .merge_index(&index.index_url, &index, follows::now());
                    let _ = self.repos.save_cache(&self.root);
                    self.repository = Some(index);
                }
                Reply::RepoAll(results) => {
                    self.apply_repo_all(results);
                }
                Reply::Metadata {
                    filters,
                    listings,
                    filter_error,
                    listing_error,
                } => {
                    self.filters = filters;
                    self.listings = listings;
                    self.filter_error = filter_error;
                    self.listing_error = listing_error;
                    self.metadata_loaded = true;
                    let pending = self.pending_open.take();
                    // Source updates can remove options; a session must never

                    // submit an identifier absent from the current schema.

                    self.applied_filters = crate::source_filters::overrides(
                        &self.filters,
                        &crate::source_filters::effective_values(
                            &self.filters,
                            &self.applied_filters,
                        ),
                    );
                    #[cfg(debug_assertions)]
                    if self.preview_mode.as_deref() == Some("filtered") {
                        self.applied_filters = vec![FilterValue::Select {
                            id: "genre".into(),
                            value: "adv".into(),
                        }];
                        self.start_search();
                        continue;
                    }
                    // A followed manga opened from another source replaces the

                    // default listing; both would share one request slot.

                    if let Some(manga) = pending {
                        self.open_manga(manga);
                        continue;
                    }
                    let listing = self
                        .listing
                        .clone()
                        .filter(|id| self.listings.iter().any(|listing| listing.id == *id))
                        .or_else(|| {
                            if self.sessions.contains_key(&self.selected_source) {
                                None
                            } else {
                                self.listings.first().map(|listing| listing.id.clone())
                            }
                        });
                    if let Some(id) = listing {
                        self.start_listing(&id);
                    } else {
                        self.start_search();
                    }
                }
                Reply::Results {
                    source,
                    query,
                    page,
                    entries,
                    has_next,
                } => {
                    if self
                        .results
                        .accept(&source, &query, page, entries, has_next)
                    {
                        self.selected_source = source;
                        self.status = if has_next {
                            format!("已加载第 {page} 页，可继续加载下一页")
                        } else if page > 1 {
                            format!("已加载到第 {page} 页，没有更多结果")
                        } else if self.results.entries.is_empty() {
                            "没有找到漫画，可调整关键词或筛选".into()
                        } else {
                            format!("共 {} 部漫画", self.results.entries.len())
                        };
                        #[cfg(debug_assertions)]
                        if let Some(mode) = self.preview_mode.take() {
                            if mode == "filters" {
                                self.filter_editor =
                                    Some(FilterEditor::new(&self.filters, &self.applied_filters));
                            }
                        }
                    }
                }
                Reply::Login { key, user, ok, web } => {
                    if ok {
                        self.login_pass.clear();
                        let marker = format!("{key}.username");
                        if web {
                            self.change_setting(&marker, SettingValue::Str(user));
                        } else {
                            // A basic-login session lives only in this process,

                            // so the form returns after a restart.

                            set_setting(&self.selected_source, &marker, SettingValue::Str(user));
                        }
                        self.status = "登录成功".into();
                    } else {
                        self.status = "登录失败，请检查账号或稍后再试".into();
                    }
                }
                Reply::Chapters(source, manga, list) => {
                    if source == self.selected_source {
                        if let Some(index) = self.follow_index(&source, &manga.key) {
                            let keys: Vec<String> =
                                list.iter().map(|chapter| chapter.key.clone()).collect();
                            self.follows[index].reading_order =
                                chapter_list(&list, "", false)
                                    .iter()
                                    .map(|chapter| chapter.key.clone())
                                    .collect();
                            self.follows[index].mark_seen(&keys);
                            self.follows[index].title = manga.title.clone();
                            self.save_follows();
                        }
                        self.selected_manga = Some(manga);
                        self.chapters = list;
                        self.status.clear();
                        if self.pending_resume {
                            self.pending_resume = false;
                            self.auto_resume();
                        }
                    }
                }
                Reply::Notified => {
                    self.status = "已完成".into();
                }
            }
        }
        while let Ok(event) = self.dl_rx.try_recv() {
            self.handle_download_event(event);
        }
        while let Ok(event) = self.up_rx.try_recv() {
            self.handle_update(event);
        }
        while let Ok(event) = self.global_rx.try_recv() {
            match event {
                GlobalEvent::Search {
                    generation,
                    source,
                    result,
                } => {
                    if generation != self.global_gen {
                        continue;
                    }
                    // The 换源 panel owns this generation's events while open.
                    if let Some(migrate) = self.migrate.as_mut()
                        && migrate.generation == generation
                    {
                        if let Some(group) = migrate
                            .groups
                            .iter_mut()
                            .find(|group| group.source == source)
                        {
                            group.state = match result {
                                Ok(mut entries) => {
                                    for manga in &mut entries {
                                        manga.source_key = source.clone();
                                    }
                                    GlobalState::Done(entries)
                                }
                                Err(reason) => GlobalState::Failed(reason),
                            };
                        }
                        continue;
                    }
                    if let Some(group) = self
                        .global_groups
                        .iter_mut()
                        .find(|group| group.source == source)
                    {
                        group.state = match result {
                            Ok(mut entries) => {
                                for manga in &mut entries {
                                    manga.source_key = source.clone();
                                }
                                GlobalState::Done(entries)
                            }
                            Err(reason) => GlobalState::Failed(reason),
                        };
                    }
                }
                GlobalEvent::Chapters {
                    generation,
                    source,
                    manga,
                    result,
                } => {
                    let Some(migrate) = self.migrate.as_mut() else {
                        continue;
                    };
                    if generation != migrate.generation {
                        continue;
                    }
                    migrate.expanding = None;
                    migrate.expanded.insert((source, manga.key.clone()), result);
                }
            }
        }
        while let Ok(done) = self.migrate_rx.try_recv() {
            self.apply_migrate(done);
        }
        while let Ok(event) = self.storage_rx.try_recv() {
            match event {
                StorageEvent::Report(report) => self.storage = Some(report),
                // Disk cache is gone; drop decoded textures and the
                // failure set so covers fetch again on demand.
                StorageEvent::CoversCleared => {
                    self.cover_textures.clear();
                    self.cover_failed.clear();
                    self.cover_ready.clear();
                    self.cover_missing_disk.clear();
                }
                StorageEvent::Failed(error) => self.status = error,
            }
        }
        if !self.auto_checked && !self.installed.is_empty() {
            self.auto_checked = true;
            self.check_updates(false);
        }
        self.maybe_auto_repo_check();
        self.poll_follow_schedule();
    }
    /// Task-id snapshot the storage worker classifies staging with:
    /// `all` covers every known task, `finished` the Done/Failed subset.
    fn storage_snapshot(&self) -> (HashSet<String>, HashSet<String>) {
        let mut all = HashSet::new();
        let mut finished = HashSet::new();
        for task in &self.tasks {
            all.insert(task.id.clone());
            if matches!(task.status, DownloadStatus::Done | DownloadStatus::Failed) {
                finished.insert(task.id.clone());
            }
        }
        (all, finished)
    }
    /// Asks the storage worker for fresh sizes.
    pub fn request_storage(&mut self) {
        let (all, finished) = self.storage_snapshot();
        let _ = self.storage_tx.send(StorageCmd {
            action: StorageAction::Report,
            all,
            finished,
        });
    }
    /// 清理已完成任务的暂存 — only Done/Failed or orphaned task dirs.
    pub fn clean_staging(&mut self) {
        let (all, finished) = self.storage_snapshot();
        let _ = self.storage_tx.send(StorageCmd {
            action: StorageAction::CleanStaging,
            all,
            finished,
        });
    }
    /// 清理封面缓存 — disk files; the texture caches clear when the
    /// worker confirms the delete.
    pub fn clear_cover_cache(&mut self) {
        let (all, finished) = self.storage_snapshot();
        let _ = self.storage_tx.send(StorageCmd {
            action: StorageAction::ClearCovers,
            all,
            finished,
        });
    }
    /// Latest storage numbers, if the worker has answered once.
    pub fn storage(&self) -> Option<&SourceStorage> {
        self.storage.as_ref()
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
    /// Arms the next wakeup. Only `request_repaint_after` is used, at the

    /// full interval — no per-frame work, no timer thread, so an idle shelf

    /// keeps its `repaint_delay` far above the acceptance floor.
    fn schedule_follow_check(&self) {
        if self.follow_interval_min == 0 {
            return;
        }
        self.ctx
            .request_repaint_after(std::time::Duration::from_secs(
                self.follow_interval_min * 60,
            ));
    }
    /// Runs on every frame; costs nothing while the timer is not due.
    fn poll_follow_schedule(&mut self) {
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

    fn download_row(&mut self, ui: &mut Ui, task: &DownloadTask) {
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

    fn filter_page(&mut self, ui: &mut Ui) {
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

    fn browser_header(&mut self, ui: &mut Ui) {
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

    fn current_settings(&self) -> Vec<SettingItem> {
        self.installed
            .iter()
            .find(|source| source.manifest.id == self.selected_source)
            .map(|source| source_settings(&source.manifest, source.settings_json().as_deref()))
            .unwrap_or_default()
    }

    fn change_setting(&mut self, key: &str, value: SettingValue) {
        let source = self.selected_source.clone();
        set_setting(&source, key, value.clone());
        self.source_settings
            .entry(source.clone())
            .or_default()
            .insert(key.to_string(), value);
        self.stale_engines.lock().unwrap().insert(source);
        self.settings_changed = true;
        if let Err(error) = save_source_settings(&self.root, &self.source_settings) {
            self.status = format!("书源设置保存失败：{error:#}");
        }
    }
    /// Opens `url` for verification (`login` = None) or a source's web

    /// login. Mobile uses the in-app WebView; desktop opens the system

    /// browser and shows the paste-cookies dialog.

    fn open_web(&mut self, url: &str, login: Option<String>) {
        if yejian_platform::has_in_app_web() {
            match yejian_platform::open_web(url, DEFAULT_USER_AGENT) {
                Ok(()) => {
                    self.web_session = Some(WebSession {
                        source: self.selected_source.clone(),
                        login,
                        url: url.to_string(),
                        paste: String::new(),
                        agent: String::new(),
                    });
                }
                Err(error) => self.status = format!("无法打开网页：{error}"),
            }
            return;
        }
        // Desktop: hand the page to the system browser; the modal collects

        // the cookies the user copies back.
        match yejian_platform::open_external_url(url) {
            Ok(()) => {
                self.web_session = Some(WebSession {
                    source: self.selected_source.clone(),
                    login,
                    url: url.to_string(),
                    paste: String::new(),
                    agent: String::new(),
                });
                self.status = "已在浏览器打开，请完成验证后回到这里".into();
            }
            Err(error) => self.status = format!("无法打开浏览器：{error}"),
        }
    }
    /// A `settings.json` `link` item. `external = true` (or any platform

    /// without an in-app browser) goes to the system browser; everything

    /// else opens inside the app. Shell string invocation is never used.
    fn open_link(&mut self, url: &str, external: bool) {
        if external || !yejian_platform::has_in_app_web() {
            match yejian_platform::open_external_url(url) {
                Ok(()) => {}
                Err(error) => self.status = format!("无法打开链接：{error}"),
            }
            return;
        }
        match yejian_platform::open_web(url, DEFAULT_USER_AGENT) {
            // Plain links don't feed cookies back into the verification flow,

            // so `web_session` stays empty and `finish_web` ignores the page.
            Ok(()) => {}
            Err(error) => self.status = format!("无法打开链接：{error}"),
        }
    }

    fn finish_web(&mut self, result: yejian_platform::WebResult) {
        let Some(session) = self.web_session.take() else {
            return;
        };
        let Some(cookies) = result.cookies else {
            self.status = "已取消网页验证".into();
            return;
        };
        self.commit_web_session(session.source, session.login, &result.url, cookies, None);
    }
    /// Shared tail of the in-app and paste-cookie flows: write the jar,

    /// persist the site (cookies + optional UA), then run the source's web

    /// login or reload it.
    fn commit_web_session(
        &mut self,
        source: String,
        login: Option<String>,
        url: &str,
        cookies: Vec<(String, String)>,
        user_agent: Option<String>,
    ) {
        add_web_cookies(url, &cookies);
        if let Some(agent) = user_agent.as_ref().filter(|agent| !agent.trim().is_empty()) {
            set_site_user_agent(url, agent.trim());
        }
        let site = self.web_cookies.entry(url.to_string()).or_default();
        site.cookies = cookies.clone();
        if let Some(agent) = user_agent.filter(|agent| !agent.trim().is_empty()) {
            site.user_agent = Some(agent.trim().to_string());
        }
        if let Err(error) = self.save_web_cookies() {
            self.status = format!("网页验证结果保存失败：{error:#}");
        }
        if let Some(key) = login {
            self.submit(Task::Login {
                source,
                key,
                credentials: Err(cookies),
            });
            return;
        }
        self.challenge = None;
        self.status = "网页验证完成，正在重新加载".into();
        if self.selected_source == source && !source.is_empty() {
            self.select_source("");
            self.select_source(&source);
        }
    }
    /// Modal for the desktop verification flow: the URL, a re-open action,

    /// the default UA to copy, the paste box and an optional browser UA.

    /// Drawn every frame while `web_session` is set and the platform has no

    /// in-app browser; every control meets the 44pt touch target.
    pub(crate) fn web_dialog(&mut self, ctx: &egui::Context) {
        if yejian_platform::has_in_app_web() {
            return;
        }
        let Some(session) = self.web_session.as_mut() else {
            return;
        };
        // Editable fields move out for the frame so the modal can mutate

        // `self` freely (status line, commit on finish).
        let url = session.url.clone();
        let mut paste = std::mem::take(&mut session.paste);
        let mut agent = std::mem::take(&mut session.agent);
        let mut finish = false;
        let mut cancel = false;
        egui::Modal::new(egui::Id::unique("web-verify-dialog")).show(ctx, |ui| { ui.set_width(ui.available_width().min(460.).max(280.)); design::section(ui, "网页验证与登录"); ui.add( Label::new( RichText::new(&url) .color(design::palette(ui.visuals().dark_mode).muted) .small(), ) .wrap(), ); ui.add_space(8.); ui.horizontal_wrapped(|ui| { if ui.add(quiet_action("重新在浏览器打开")).clicked() && let Err(error) = yejian_platform::open_external_url(&url) { self.status = format!("无法打开浏览器：{error}"); } if ui.add(quiet_action("复制默认 UA")).clicked() { ui.ctx().copy_text(DEFAULT_USER_AGENT.into()); } }); ui.add_space(8.); ui.label("把浏览器里的 Cookie 粘贴到这里（支持 name=value 串、Cookie: 整行、cookies.txt 或 JSON）："); ui.add( TextEdit::multiline(&mut paste) .desired_width(ui.available_width()) .desired_rows(4), ); ui.add_space(6.); ui.label( RichText::new( "浏览器 User-Agent（Cloudflare 验证需与浏览器一致，留空保持默认）：", ) .small(), ); ui.add( TextEdit::singleline(&mut agent) .desired_width(ui.available_width()), ); ui.add_space(10.); ui.horizontal_wrapped(|ui| { let ready = !crate::trackers::parse_cookie_paste(&paste).is_empty(); if ui .add_enabled(ready, design::primary("完成导入", ui.visuals().dark_mode)) .clicked() { finish = true; } if ui.add(quiet_action("取消")).clicked() { cancel = true; } }); if finish { } });
        if finish {
            let session = self.web_session.take().unwrap();
            let cookies = crate::trackers::parse_cookie_paste(&paste);
            let agent = (!agent.trim().is_empty()).then_some(agent);
            self.commit_web_session(session.source, session.login, &session.url, cookies, agent);
        } else if cancel {
            self.web_session = None;
            self.status = "已取消网页验证".into();
        } else if let Some(session) = self.web_session.as_mut() {
            session.paste = paste;
            session.agent = agent;
        }
    }
    /// Expires this source's cookies and drops its saved web cookies.

    fn forget_source_cookies(&mut self) {
        let urls: Vec<String> = self
            .installed
            .iter()
            .find(|source| source.manifest.id == self.selected_source)
            .map(|source| source.manifest.urls.clone())
            .unwrap_or_default();
        let hosts: Vec<String> = urls.iter().map(|url| url_host(url)).collect();
        let matches = |host: &str| {
            hosts.iter().any(|known| {
                host == known
                    || host.ends_with(&format!(".{known}"))
                    || known.ends_with(&format!(".{host}"))
            })
        };
        let mut forget: Vec<String> = urls;
        forget.extend(
            self.web_cookies
                .keys()
                .filter(|url| matches(&url_host(url)))
                .cloned(),
        );
        for url in &forget {
            clear_site_cookies(url);
        }
        self.web_cookies.retain(|url, _| !matches(&url_host(url)));
        if let Err(error) = self.save_web_cookies() {
            self.status = format!("网页登录信息清除失败：{error:#}");
        }
    }

    fn save_web_cookies(&self) -> anyhow::Result<()> {
        let part = self.root.join(format!("{WEB_COOKIES_FILE}.part"));
        std::fs::write(&part, serde_json::to_vec_pretty(&self.web_cookies)?)?;
        std::fs::rename(&part, self.root.join(WEB_COOKIES_FILE))?;
        Ok(())
    }

    fn login_row(&mut self, ui: &mut Ui, item: &SettingItem, web: bool, url: Option<&str>) {
        let source = self.selected_source.clone();
        let marker = format!("{}.username", item.key);
        let user = match setting_value(&source, &marker) {
            Some(SettingValue::Str(user)) if !user.is_empty() => Some(user),
            _ => None,
        };
        ui.add(Label::new(RichText::new(&item.title).strong()).wrap());
        if let Some(user) = user {
            ui.label(format!("已登录：{user}"));
            if ui.add(quiet_action("退出登录")).clicked() {
                self.change_setting(&marker, SettingValue::Str(String::new()));
                self.forget_source_cookies();
            }
            return;
        }
        if web {
            if ui
                .add_enabled(!self.busy, quiet_action("打开网页登录"))
                .clicked()
            {
                if let Some(url) = url {
                    self.open_web(url, Some(item.key.clone()));
                }
            }
            return;
        }
        ui.add(
            TextEdit::singleline(&mut self.login_user)
                .hint_text("账号")
                .desired_width(ui.available_width()),
        );
        ui.add(
            TextEdit::singleline(&mut self.login_pass)
                .password(true)
                .hint_text("密码")
                .desired_width(ui.available_width()),
        );
        let ready = !self.busy && !self.login_user.is_empty() && !self.login_pass.is_empty();
        if ui
            .add_enabled(ready, design::primary("登录", ui.visuals().dark_mode))
            .clicked()
        {
            self.submit(Task::Login {
                source,
                key: item.key.clone(),
                credentials: Ok((self.login_user.clone(), self.login_pass.clone())),
            });
        }
    }

    fn close_settings(&mut self) {
        self.settings_open = false;
        if std::mem::take(&mut self.settings_changed) {
            // Reloads listings and results with the restarted engine.

            let source = self.selected_source.clone();
            self.select_source("");
            self.select_source(&source);
        }
    }

    fn settings_page(&mut self, ui: &mut Ui) {
        if ui.add(quiet_action("‹ 返回书源")).clicked() {
            self.close_settings();
            return;
        }
        let items = self.current_settings();
        let source = self.selected_source.clone();
        ScrollArea::vertical()
            .id_salt(("source-settings", &source))
            .show(ui, |ui| {
                ui.set_max_width(ui.available_width());
                design::section(ui, "书源设置");
                ui.label(RichText::new("修改后返回书源即生效。").small());
                let status_visible = self.status_fresh(ui.ctx());
                if self.busy || status_visible {
                    ui.add(
                        Label::new(
                            RichText::new(&self.status)
                                .color(design::palette(ui.visuals().dark_mode).muted),
                        )
                        .wrap(),
                    );
                }
                let mut group = String::new();
                for item in items {
                    if item.group != group {
                        group = item.group.clone();
                        if !group.is_empty() {
                            design::section(ui, &group);
                        }
                    }
                    ui.add_space(6.);
                    if let SettingKind::Login { web, url } = &item.kind {
                        self.login_row(ui, &item, *web, url.as_deref());
                        continue;
                    }
                    match &item.kind {
                        SettingKind::Button {
                            notification,
                            destructive,
                            refreshes: _,
                        } => {
                            let palette = design::palette(ui.visuals().dark_mode);
                            let label = RichText::new(&item.title).color(if *destructive {
                                palette.danger
                            } else {
                                palette.ink
                            });
                            let button = Button::new(label)
                                .min_size(vec2(ui.available_width().min(220.), 44.))
                                .wrap();
                            if ui.add_enabled(!self.busy, button).clicked() {
                                let note = notification
                                    .clone()
                                    .filter(|n| !n.is_empty())
                                    .unwrap_or_else(|| item.key.clone());
                                if note.is_empty() {
                                    self.status = "此按钮没有可发送的通知".into();
                                } else {
                                    self.submit(Task::Notify {
                                        source: source.clone(),
                                        notification: note,
                                    });
                                    self.status = format!("已发送：{}", item.title);
                                }
                            }
                            continue;
                        }
                        SettingKind::Link { url, external } => {
                            let palette = design::palette(ui.visuals().dark_mode);
                            ui.horizontal_wrapped(|ui| {
                                ui.add(Label::new(RichText::new(&item.title).strong()).wrap());
                                let button = Button::new(
                                    RichText::new(url).color(palette.accent).underline(),
                                )
                                .frame_when_inactive(false)
                                .min_size(vec2(0., 44.))
                                .wrap();
                                if ui.add(button).clicked() {
                                    self.open_link(url, *external);
                                }
                            });
                            continue;
                        }
                        _ => {}
                    }
                    let current = setting_value(&source, &item.key).or(item.default.clone());
                    if let Some(value) = setting_row(ui, &item, current) {
                        self.change_setting(&item.key, value);
                    }
                }
            });
    }

    fn installed_ui(&mut self, ui: &mut Ui) {
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
    fn uninstall_confirm_ui(&mut self, ctx: &Context) {
        let Some(source) = self.uninstall.clone() else {
            return;
        };
        let screen = ctx.content_rect();
        let scrim = Area::new(egui::Id::unique("uninstall-scrim"))
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
            self.uninstall = None;
            return;
        }
        let mut card_rect = Rect::NOTHING;
        Area::new(egui::Id::unique("uninstall-card"))
            .order(Order::Foreground)
            .fade_in(false)
            .interactable(true)
            .movable(false)
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .show(ctx, |ui| {
                let frame = crate::design::surface(ui).show(ui, |ui| {
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
                card_rect = frame.response.rect;
            });
        self.hitboxes.insert("source-uninstall-card".into(), card_rect);
    }

    fn repository_entry_ui(&mut self, ui: &mut Ui) {
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
                if ui.add_enabled(!self.busy, quiet_action("读取")).clicked() {
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

    fn repository_source_ui(&mut self, ui: &mut Ui, source: &RepositorySource) {
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

    fn repository_ui(&mut self, ui: &mut Ui) {
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
    /// Search and listing results as a cover grid: cover on top, a two-line

    /// title and one muted metadata line below, the whole cell a single tap

    /// target. Covers are requested only for cells near the viewport and the

    /// next page auto-loads when the last row scrolls into view.

    /// Manga detail: cover + metadata header, resume/follow/download actions,

    /// a collapsible description, then the touch-friendly chapter list. Rows

    /// are painted like the result grid so long names wrap instead of pushing

    /// the page wide.

    fn detail_ui(&mut self, ui: &mut Ui) {
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
    /// The read action shared by the resume button and chapter rows: library

    /// copy when linked, import when the download only waits for the shelf,

    /// otherwise online streaming.

    /// Queue state per chapter key, as the detail page collects it.
    fn chapter_states(
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
    fn auto_resume(&mut self) {
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

    fn read_detail_chapter(
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

    fn results_grid(&mut self, ui: &mut Ui) {
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
    fn global_cell(
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
    fn global_results_ui(&mut self, ui: &mut Ui) {
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
    const STATUS_TTL: std::time::Duration = std::time::Duration::from_secs(4);
    /// Errors stay until the next action replaces them; everything else is
    /// a transient success notice that fades after `STATUS_TTL`.
    fn status_is_error(text: &str) -> bool {
        [
            "失败", "错误", "无法", "未安装", "已丢失", "已停止", "损坏", "过期", "被拒绝",
            "超时", "请先", "暂不可", "不支持", "不正确",
        ]
        .iter()
        .any(|mark| text.contains(mark))
    }
    /// True while the status line should render. Tracks when the current
    /// string was first observed and schedules the repaint that fades it.
    fn status_fresh(&mut self, ctx: &Context) -> bool {
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

    /// 换源 panel: opaque scrim + card (same recipe as the other modals).
    /// Shows grouped candidates → confirm listing every effect → run.
    fn migrate_ui(&mut self, ctx: &Context) {
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
    fn migrate_card(&mut self, ui: &mut Ui) {
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
    fn migrate_confirm_card(&mut self, ui: &mut Ui, palette: &crate::design::Palette) {
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

    /// Opaque scrim + card, matching the shelf delete confirmations.
    fn history_confirm_ui(&mut self, ctx: &Context) {
        if !self.history_confirm {
            return;
        }
        let screen = ctx.content_rect();
        let scrim = Area::new(egui::Id::unique("history-clear-scrim"))
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
            self.history_confirm = false;
            return;
        }
        let mut card_rect = Rect::NOTHING;
        Area::new(egui::Id::unique("history-clear"))
            .order(Order::Foreground)
            .fade_in(false)
            .interactable(true)
            .movable(false)
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .show(ctx, |ui| {
                let frame = crate::design::surface(ui).show(ui, |ui| {
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
                card_rect = frame.response.rect;
            });
        self.hitboxes
            .insert("history-clear-card".into(), card_rect);
    }
}
impl Drop for Sources {
    fn drop(&mut self) {
        // Do not block window/Activity teardown. The worker notices this after

        // the current HTTP request finishes, then exits and releases its lock.

        self.dl_stop.store(STOP_PAUSE, Ordering::SeqCst);
    }
}
#[derive(Debug)]
enum Outcome {
    Done { path: PathBuf },
    Aborted { how: u8 },
}
/// Performs the HTTP on the calling thread through the source's own

/// transport (shared cookies and headers), so page downloads do not queue on

/// the single WASM worker.

fn request(
    handler: &RequestHandler,
    url: String,
    headers: Vec<(String, String)>,
    body: Option<Vec<u8>>,
) -> anyhow::Result<Vec<u8>> {
    let response = handler(NetRequestData {
        method: if body.is_some() { "POST" } else { "GET" }.into(),
        url,
        headers,
        body,
        timeout_ms: 60000,
    });
    if let Some(error) = response.error {
        anyhow::bail!("{error}");
    }
    anyhow::ensure!(
        (200..300).contains(&response.status_code),
        "图片服务器返回 HTTP {}",
        response.status_code
    );
    anyhow::ensure!(
        response.body.len() <= 64 * 1024 * 1024,
        "单页图片超过 64 MB"
    );
    Ok(response.body)
}
fn page_bytes(engine: &SourceEngine, page: &Page) -> anyhow::Result<Vec<u8>> {
    let handler = engine.request_handler();
    match &page.content {
        PageContent::Url { url, context } => {
            let modified = pollster::block_on(engine.image_request(url, context.clone()))?;
            let bytes = if let Some(req) = modified {
                request(&handler, req.url, req.headers, req.body)
            } else {
                request(&handler, url.clone(), vec![], None)
            }?;
            // Obfuscated-image sources deobfuscate through

            // `process_page_image`; it runs on the engine's single worker

            // thread, which serializes the WASM calls even though the HTTP

            // fetches above run concurrently.
            if !engine.features().processes_pages {
                return Ok(bytes);
            }
            match pollster::block_on(engine.process_page_image(bytes, context.clone()))? {
                Some(processed) => Ok(processed),
                None => anyhow::bail!("书源的图片处理未返回有效图像"),
            }
        }
        PageContent::Image {
            image_data: Some(bytes),
            ..
        } => Ok(bytes.clone()),
        PageContent::ZipFile { url, file_path } => {
            let bytes = request(&handler, url.clone(), vec![], None)?;
            let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes))?;
            let mut entry = zip.by_name(file_path)?;
            anyhow::ensure!(entry.size() <= 64 * 1024 * 1024, "单页图片超过 64 MB");
            let mut data = Vec::new();
            entry.read_to_end(&mut data)?;
            Ok(data)
        }
        PageContent::Text(_) => anyhow::bail!("此章节包含文字页，当前漫画阅读模式无法显示"),
        _ => anyhow::bail!("书源未返回有效图像数据"),
    }
}
/// Runs one download job to completion (or a pause/cancel checkpoint).

////// Kept separate from the worker thread so tests can execute the real page

/// staging, cache reuse and CBZ build paths with local pages instead of only

/// flipping task statuses.

fn execute_task(
    root: &Path,
    task: &DownloadTask,
    pages: Vec<Page>,
    stop: &AtomicU8,
    fetch: impl Fn(&Page) -> anyhow::Result<Vec<u8>> + Sync,
    mut report: impl FnMut(usize, usize, Option<String>),
) -> anyhow::Result<Outcome> {
    anyhow::ensure!(!pages.is_empty(), "这一章没有可读页面");
    let dir = task_dir(root, &task.id)?;
    let signature = page_signature(&pages);
    let signature_file = dir.join("page-list.v2");
    let recorded = match std::fs::read_to_string(&signature_file) {
        Ok(previous) => Some(previous),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };
    if recorded.as_deref() != Some(signature.as_str()) {
        if recorded.is_none() && task.pages_signature.as_deref() == Some(signature.as_str()) {
            // Old queues kept only the signature and accepted image headers.

            // Keep every legacy page the current reader can actually display.

            for number in existing_pages(&dir) {
                if let Some(path) = stored_page(&dir, number) {
                    let bytes = std::fs::read(&path)?;
                    if crate::worker::image_reader(&bytes)
                        .and_then(|r| Ok(r.decode()?))
                        .is_err()
                    {
                        std::fs::remove_file(path)?;
                    }
                }
            }
        } else {
            reset_page_cache(&dir)?;
        }
        std::fs::create_dir_all(&dir)?;
        let part = dir.join("page-list.part");
        let mut file = std::fs::File::create(&part)?;
        file.write_all(signature.as_bytes())?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&part, &signature_file)?;
    }
    if task.pages_signature.as_deref() != Some(signature.as_str()) {
        report(0, pages.len(), Some(signature));
    }
    let summary = fetch_missing_pages(
        &dir,
        &pages,
        fetch,
        |done, total| report(done, total, None),
        stop,
    )?;
    let how = stop.load(Ordering::SeqCst);
    if summary.aborted || how != STOP_NONE {
        return Ok(Outcome::Aborted {
            how: if how == STOP_NONE { STOP_PAUSE } else { how },
        });
    }
    anyhow::ensure!(summary.done == pages.len(), "章节存在未完成的图片");
    let path = build_cbz(root, task, &dir, pages.len())?;
    // The CBZ is already complete; a cleanup problem must not turn a real

    // download into a failed task or delete anything outside this cache.

    let _ = remove_task_cache(root, &task.id);
    Ok(Outcome::Done { path })
}
/// Removes the `.part` file created by one CBZ build when writing fails before

/// the archive is complete. Final `.cbz` files are never touched.

struct PartCleanup {
    path: PathBuf,
    armed: bool,
}
impl PartCleanup {
    fn new(path: &Path) -> Self {
        Self {
            path: path.to_path_buf(),
            armed: true,
        }
    }
    fn keep(mut self) {
        self.armed = false;
    }
}
impl Drop for PartCleanup {
    fn drop(&mut self) {
        if self.armed {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn manga(source: &str, key: &str) -> Manga {
        Manga {
            source_key: source.into(),
            key: key.into(),
            title: key.into(),
            ..Default::default()
        }
    }
    fn url_page(url: &str) -> Page {
        Page {
            content: PageContent::Url {
                url: url.into(),
                context: None,
            },
            thumbnail: None,
            has_description: false,
            description: None,
        }
    }
    fn png() -> Vec<u8> {
        colored_png([36, 82, 94, 255])
    }
    fn colored_png(pixel: [u8; 4]) -> Vec<u8> {
        let image = image::RgbaImage::from_pixel(4, 4, image::Rgba(pixel));
        let mut out = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(image)
            .write_to(&mut out, image::ImageFormat::Png)
            .unwrap();
        out.into_inner()
    }
    fn source_manifest() -> SourceManifest {
        SourceManifest {
            id: "test.source".into(),
            name: "Test Source".into(),
            version: 1,
            alt_names: Vec::new(),
            languages: vec!["en".into()],
            urls: vec!["http://127.0.0.1".into()],
            content_rating: 0,
            listings: Vec::new(),
            allows_base_url_select: false,
            hides_filters_while_searching: false,
        }
    }
    fn leb_u32(mut value: u32, out: &mut Vec<u8>) {
        loop {
            let mut byte = (value & 0x7f) as u8;
            value >>= 7;
            if value != 0 {
                byte |= 0x80;
            }
            out.push(byte);
            if value == 0 {
                break;
            }
        }
    }
    fn wasm_section(id: u8, payload: &[u8], out: &mut Vec<u8>) {
        out.push(id);
        leb_u32(payload.len() as u32, out);
        out.extend_from_slice(payload);
    }
    /// Minimal self-contained WASM module exporting `get_page_list` that

    /// returns the same postcard `Vec<Page>` the real Aidoku source ABI uses.

    fn pages_wasm(urls: &[String]) -> Vec<u8> {
        let mut payload = Vec::new();
        leb_u32(urls.len() as u32, &mut payload);
        for url in urls {
            payload.push(0);
            leb_u32(url.len() as u32, &mut payload);
            payload.extend_from_slice(url.as_bytes());
            payload.push(0);
            payload.push(0);
            payload.push(0);
            payload.push(0);
        }
        let mut data = Vec::with_capacity(payload.len() + 8);
        data.extend_from_slice(&((payload.len() + 8) as u32).to_le_bytes());
        data.extend_from_slice(&[0u8; 4]);
        data.extend_from_slice(&payload);

        let mut module = b"\0asm\x01\0\0\0".to_vec();
        wasm_section(1, &[0x01, 0x60, 0x02, 0x7f, 0x7f, 0x01, 0x7f], &mut module);
        wasm_section(3, &[0x01, 0x00], &mut module);
        wasm_section(5, &[0x01, 0x00, 0x01], &mut module);
        let mut exports = Vec::new();
        leb_u32(2, &mut exports);
        leb_u32(6, &mut exports);
        exports.extend_from_slice(b"memory");
        exports.push(0x02);
        leb_u32(0, &mut exports);
        leb_u32(13, &mut exports);
        exports.extend_from_slice(b"get_page_list");
        exports.push(0x00);
        leb_u32(0, &mut exports);
        wasm_section(7, &exports, &mut module);
        let body = [0x00, 0x41, 0x80, 0x08, 0x0b];
        let mut code = Vec::new();
        leb_u32(1, &mut code);
        leb_u32(body.len() as u32, &mut code);
        code.extend_from_slice(&body);
        wasm_section(10, &code, &mut module);
        let mut segment = Vec::new();
        leb_u32(1, &mut segment);
        segment.push(0x00);
        segment.push(0x41);
        leb_u32(1024, &mut segment);
        segment.push(0x0b);
        leb_u32(data.len() as u32, &mut segment);
        segment.extend_from_slice(&data);
        wasm_section(11, &segment, &mut module);
        module
    }
    fn serve_png(count: usize) -> (String, thread::JoinHandle<()>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let handle = thread::spawn(move || {
            for _ in 0..count {
                let Ok((mut stream, _)) = listener.accept() else {
                    break;
                };
                let mut request = [0u8; 1024];
                let _ = stream.read(&mut request);
                let body = png();
                let header = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(header.as_bytes());
                let _ = stream.write_all(&body);
            }
        });
        (format!("http://{address}"), handle)
    }
    fn task(source: &str, manga_id: &str, chapter: &str) -> DownloadTask {
        DownloadTask::from_chapter(
            source,
            &manga(source, manga_id),
            &Chapter {
                key: chapter.into(),
                title: Some(chapter.into()),
                ..Default::default()
            },
        )
    }
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

    fn test_sources(root: &Path) -> Sources {
        let (dl_tx, dl_cmd_rx) = mpsc::channel();
        // Keep the command receiver alive so `pump()` can actually dispatch the

        // next task in tests. The task itself is exercised directly.

        std::mem::forget(dl_cmd_rx);
        let (migrate_tx, migrate_rx) = mpsc::channel();
        Sources {
            root: root.to_path_buf(),
            tx: mpsc::channel().0,
            rx: mpsc::channel().1,
            installed: Vec::new(),
            repository: None,
            results: ResultsState::default(),
            chapters: Vec::new(),
            chapter_query: String::new(),
            chapters_newest_first: true,
            open_chapter: None,
            library_chapters: Default::default(),
            selected_source: String::new(),
            selected_manga: None,
            url: String::new(),
            query: String::new(),
            search_scope: "current".into(),
            global_gen: 0,
            global_query: String::new(),
            global_groups: Vec::new(),
            scope_dirty: None,
            search_history: Vec::new(),
            history_dirty: false,
            history_confirm: false,
            private: false,
            global_tx: mpsc::channel().0,
            global_rx: mpsc::channel().1,
            storage_tx: mpsc::channel().0,
            storage_rx: mpsc::channel().1,
            storage: None,
            request_id: 0,
            current_request: Arc::new(AtomicU64::new(0)),
            pending: None,
            failed: None,
            filters: Vec::new(),
            listings: Vec::new(),
            applied_filters: Vec::new(),
            listing: None,
            filter_editor: None,
            metadata_loaded: false,
            filter_error: None,
            listing_error: None,
            sessions: Default::default(),
            repository_search: String::new(),
            #[cfg(debug_assertions)]
            preview_mode: None,
            busy: false,
            status: String::new(),
            status_seen: None,
            tasks: Vec::new(),
            running: None,
            ready: VecDeque::new(),
            import_attempts: HashSet::new(),
            dl_tx,
            dl_rx: mpsc::channel().1,
            dl_stop: Arc::new(AtomicU8::new(STOP_NONE)),
            stream_request: None,
            preempted: None,
            follows: Vec::new(),
            pending_open: None,
            shelf_origin: false,
            shelf_return: false,
            pending_resume: false,
            checking: 0,
            checking_background: false,
            auto_checked: true,
            ctx: Context::default(),
            follow_interval_min: DEFAULT_FOLLOW_INTERVAL_MIN,
            follow_next_due: None,
            follow_notice: None,
            notify_pending: Vec::new(),
            // Keep the update receiver alive so `check_follows` batches do not

            // fail in tests that never drain `up_rx`.
            up_tx: {
                let (tx, rx) = mpsc::channel();
                std::mem::forget(rx);
                tx
            },
            up_rx: mpsc::channel().1,
            covers: Default::default(),
            follow_cover_urls: Default::default(),
            cover_req_tx: mpsc::channel().0,
            cover_done_rx: mpsc::channel().1,
            cover_queue: VecDeque::new(),
            cover_inflight: HashSet::new(),
            cover_outstanding: 0,
            cover_failed: HashSet::new(),
            cover_ready: Default::default(),
            cover_missing_disk: HashSet::new(),
            cover_textures: Default::default(),
            cover_lru: VecDeque::new(),
            auto_next_page: None,
            chapter_progress: Default::default(),
            description_expanded: false,
            hitboxes: Default::default(),
            settings_open: false,
            settings_changed: false,
            source_settings: Default::default(),
            stale_engines: Default::default(),
            challenge: None,
            auto_download: "off".into(),
            migrate: None,
            migrate_tx,
            migrate_rx,
            migrate_post: None,
            web_session: None,
            login_user: String::new(),
            login_pass: String::new(),
            web_cookies: Default::default(),
            repos: SourceRepos::default(),
            repo_lang: Lang::default(),
            repo_fetching: false,
            repo_auto_done: true,
            repo_test_results: Default::default(),
            repo_testing: Default::default(),
            repo_test_queue: VecDeque::new(),
            test_tx: mpsc::channel().0,
            test_rx: mpsc::channel().1,
            uninstall: None,
            setting_writes: VecDeque::new(),
        }
    }

    #[test]
    fn online_reading_runs_first_requeues_the_preempted_download_and_queues_the_next_chapter() {
        let root = tempfile::tempdir().unwrap();
        let mut sources = test_sources(root.path());
        let book = manga("source.a", "m");
        let chapter = |key: &str, number: f32| Chapter {
            key: key.into(),
            title: Some(key.into()),
            chapter_number: Some(number),
            ..Default::default()
        };
        sources.selected_source = "source.a".into();
        sources.selected_manga = Some(book.clone());
        sources.chapters = vec![chapter("c2", 2.), chapter("c1", 1.), chapter("c3", 3.)];
        let mut other = task("source.a", "other", "x");
        other.status = DownloadStatus::Running;
        sources.running = Some(other.id.clone());
        sources.tasks.push(other.clone());

        sources.read_online(&book, &chapter("c1", 1.), false);
        let stream = sources.take_stream().expect("reader opens immediately");
        assert_eq!(stream.id, identity_id("source.a", "m", "c1"));
        assert_eq!(
            sources.tasks[0].id, stream.id,
            "read chapter downloads first"
        );
        assert_eq!(sources.preempted.as_deref(), Some(other.id.as_str()));
        assert_eq!(sources.dl_stop.load(Ordering::SeqCst), STOP_PAUSE);
        assert!(
            sources.tasks.iter().any(|task| task.chapter_id == "c2"),
            "next chapter in reading order is prefetched"
        );
        assert!(!sources.tasks.iter().any(|task| task.chapter_id == "c3"));
        assert_eq!(sources.stream_state(&stream.id).unwrap().stopped, None);

        sources.handle_download_event(DlEvent::Aborted {
            id: other.id.clone(),
            how: STOP_PAUSE,
        });
        let requeued = sources
            .tasks
            .iter()
            .find(|task| task.id == other.id)
            .unwrap();
        assert_eq!(
            requeued.status,
            DownloadStatus::Queued,
            "no manual continue needed"
        );
        assert!(sources.preempted.is_none());
        assert_eq!(sources.running.as_deref(), Some(stream.id.as_str()));

        assert!(sources.has_adjacent_online(&stream.metadata, 1));
        assert!(!sources.has_adjacent_online(&stream.metadata, -1));
        assert_eq!(
            sources.adjacent_online_key(&stream.metadata, 1).as_deref(),
            Some("c2")
        );

        assert!(sources.open_adjacent_online(&sources.tasks[0].metadata(), 0));
        let next = sources
            .tasks
            .iter()
            .find(|task| task.chapter_id == "c2")
            .unwrap()
            .metadata();
        assert!(sources.open_adjacent_online(&next, -1));
        assert!(
            sources.take_stream().unwrap().from_end,
            "backwards starts on the last page"
        );
    }

    #[test]
    fn a_preempted_download_whose_request_fails_is_still_requeued() {
        let root = tempfile::tempdir().unwrap();
        let mut sources = test_sources(root.path());
        let mut other = task("source.a", "other", "x");
        other.status = DownloadStatus::Running;
        sources.running = Some(other.id.clone());
        sources.tasks.push(other.clone());
        let mut read = task("source.a", "m", "c");
        read.status = DownloadStatus::Queued;
        sources.tasks.push(read.clone());
        sources.prioritize(&read.id);
        sources.handle_download_event(DlEvent::Failed {
            id: other.id.clone(),
            error: "timeout".into(),
        });
        let other = sources
            .tasks
            .iter()
            .find(|task| task.id == other.id)
            .unwrap();
        assert_eq!(other.status, DownloadStatus::Queued);
        assert_eq!(sources.running.as_deref(), Some(read.id.as_str()));
    }

    #[test]
    fn source_settings_survive_a_restart() {
        let root = tempfile::tempdir().unwrap();
        let mut store = SettingsStore::default();
        store
            .entry("demo.source".into())
            .or_default()
            .insert("quality".into(), SettingValue::Str("hd".into()));
        save_source_settings(root.path(), &store).unwrap();
        assert_eq!(load_source_settings(root.path()), store);
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

    #[test]
    fn online_pages_are_served_from_the_download_staging_directory() {
        let root = tempfile::tempdir().unwrap();
        let id = identity_id("source.a", "m", "c");
        assert!(stream_page(root.path(), &id, 0).unwrap().is_none());
        let dir = task_dir(root.path(), &id).unwrap();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("page-0001.png"), png()).unwrap();
        std::fs::write(dir.join("page-0002.part"), b"half").unwrap();
        assert_eq!(stream_page(root.path(), &id, 0).unwrap(), Some(png()));
        assert!(
            stream_page(root.path(), &id, 1).unwrap().is_none(),
            "partial page is not ready"
        );
        assert!(stream_page(root.path(), "../escape", 0).is_err());
    }

    #[test]
    fn a_paused_or_failed_online_chapter_reports_why_it_stopped() {
        let root = tempfile::tempdir().unwrap();
        let mut sources = test_sources(root.path());
        let mut item = task("source.a", "m", "c");
        item.status = DownloadStatus::Failed;
        item.error = Some("图片服务器返回 HTTP 403".into());
        item.pages_total = 12;
        item.pages_done = 4;
        sources.tasks.push(item.clone());
        let state = sources.stream_state(&item.id).unwrap();
        assert_eq!((state.total, state.done), (12, 4));
        assert_eq!(state.stopped.as_deref(), Some("图片服务器返回 HTTP 403"));
        sources.resume_stream(&item.id);
        assert_eq!(sources.running.as_deref(), Some(item.id.as_str()));
        assert_eq!(sources.stream_state(&item.id).unwrap().stopped, None);
        sources.set_stream_page(&item.id, 7);
        assert_eq!(sources.stream_state(&item.id).unwrap().page, Some(7));
        let saved = load_tasks(root.path()).unwrap().tasks;
        assert_eq!(
            saved
                .iter()
                .find(|task| task.id == item.id)
                .unwrap()
                .read_page,
            Some(7)
        );
    }

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
    fn restored_chapters_are_readable_without_a_download_queue_and_hidden_links_are_removed() {
        let root = tempfile::tempdir().unwrap();
        let mut sources = test_sources(root.path());
        let item = task("source.a", "m", "c");
        let link = ChapterLink {
            book_id: "book".into(),
            metadata: item.metadata(),
        };
        sources.set_library_chapters(&[link]);
        assert!(
            sources.tasks.is_empty(),
            "restore does not need download-tasks.json"
        );
        assert_eq!(
            sources.chapter_state("source.a", "m", "c").as_deref(),
            Some("已在书架")
        );
        assert!(!sources.chapter_needs_download("source.a", "m", &item.chapter()));
        sources.read_downloaded(&item);
        assert_eq!(sources.take_open_chapter(), Some(item.metadata()));
        assert!(
            sources.take_open_chapter().is_none(),
            "consume one request once"
        );
        assert!(sources.chapter_state("source.b", "m", "c").is_none());
        sources.set_library_chapters(&[]);
        sources.read_downloaded(&item);
        assert!(sources.take_open_chapter().is_none());
    }

    #[test]
    fn retry_keeps_the_task_identity_when_another_manga_reuses_its_chapter_key() {
        let root = tempfile::tempdir().unwrap();
        for (other_source, other_manga) in [("source.a", "other"), ("source.b", "m")] {
            let mut sources = test_sources(root.path());
            let mut original = task("source.a", "m", "1");
            original.status = DownloadStatus::Failed;
            sources.tasks.push(original.clone());
            sources.selected_source = other_source.into();
            sources.selected_manga = Some(manga(other_source, other_manga));
            sources.chapters = vec![Chapter {
                key: "1".into(),
                ..Default::default()
            }];
            sources.retry_task(&original);
            assert_eq!(
                sources.tasks.len(),
                1,
                "must not enqueue the current foreign manga"
            );
            assert_eq!(sources.running.as_deref(), Some(original.id.as_str()));
            assert_eq!(sources.tasks[0].manga_id, "m");
            assert_eq!(sources.tasks[0].source_id, "source.a");
        }
    }

    #[test]
    fn pause_or_cancel_is_honored_when_the_current_request_fails() {
        let root = tempfile::tempdir().unwrap();
        for stop in [STOP_PAUSE, STOP_CANCEL] {
            let mut sources = test_sources(root.path());
            let mut current = task("source.a", "m", "1");
            current.status = DownloadStatus::Running;
            let id = current.id.clone();
            sources.tasks.push(current);
            sources.running = Some(id.clone());
            sources.dl_stop.store(stop, Ordering::SeqCst);
            sources.handle_download_event(DlEvent::Failed {
                id,
                error: "request timed out".into(),
            });
            assert!(sources.running.is_none());
            if stop == STOP_PAUSE {
                assert_eq!(sources.tasks[0].status, DownloadStatus::Paused);
                assert!(sources.tasks[0].error.is_none());
            } else {
                assert!(sources.tasks.is_empty());
            }
        }
    }

    #[test]
    fn bulk_download_runs_in_reading_order_and_requeues_failed_chapters() {
        let root = tempfile::tempdir().unwrap();
        let mut sources = test_sources(root.path());
        sources.selected_source = "source.a".into();
        sources.chapters = [10., 2., 2.5, 1.]
            .into_iter()
            .map(|number| Chapter {
                key: number.to_string(),
                chapter_number: Some(number),
                ..Default::default()
            })
            .collect();
        let mut failed =
            DownloadTask::from_chapter("source.a", &manga("source.a", "m"), &sources.chapters[1]);
        failed.status = DownloadStatus::Failed;
        sources.tasks.push(failed);
        sources.enqueue_missing(&manga("source.a", "m"));
        assert_eq!(sources.tasks.len(), 4);
        assert!(sources.tasks.iter().all(|task| matches!(
            task.status,
            DownloadStatus::Running | DownloadStatus::Queued
        )));
        assert!(
            sources.status.contains("4"),
            "retry count should match action"
        );
        // A failed task keeps its existing queue position; newly added tasks

        // are queued from the first chapter rather than the source's newest.

        assert_eq!(
            sources
                .tasks
                .iter()
                .skip(1)
                .map(|task| task.chapter_id.as_str())
                .collect::<Vec<_>>(),
            ["1", "2.5", "10"],
        );
    }

    #[test]
    fn resume_fetches_only_missing_pages() {
        let dir = tempfile::tempdir().unwrap();
        let work = dir.path().join("task");
        let pages = vec![url_page("p1"), url_page("p2")];
        let stop = AtomicU8::new(STOP_NONE);
        let error = fetch_missing_pages(
            &work,
            &pages,
            |page| match &page.content {
                PageContent::Url { url, .. } if url == "p1" => Ok(png()),
                _ => anyhow::bail!("第 2 页上游返回 500"),
            },
            |_, _| {},
            &stop,
        )
        .unwrap_err();
        assert!(error.to_string().contains("500"));
        assert!(stored_page(&work, 1).is_some());
        assert!(stored_page(&work, 2).is_none());
        let fetched = std::sync::Mutex::new(Vec::new());
        let summary = fetch_missing_pages(
            &work,
            &pages,
            |page| {
                if let PageContent::Url { url, .. } = &page.content {
                    fetched.lock().unwrap().push(url.clone());
                }
                Ok(png())
            },
            |_, _| {},
            &stop,
        )
        .unwrap();
        assert_eq!(*fetched.lock().unwrap(), vec!["p2"]);
        assert_eq!(summary.done, 2);
        assert!(stored_page(&work, 2).is_some());
    }

    #[test]
    fn a_truncated_image_cannot_finish_and_retry_reuses_pages_before_the_ui_saves() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let task = task("fixture", "m", "chapter");
        let pages = vec![url_page("1"), url_page("2")];
        let stop = AtomicU8::new(STOP_NONE);
        let failure = execute_task(
            root,
            &task,
            pages.clone(),
            &stop,
            |page| {
                if matches!(&page.content, PageContent::Url { url, .. } if url == "2") {
                    return Ok(b"\x89PNG\r\n\x1a\n".to_vec());
                }
                Ok(png())
            },
            |_, _, _| {},
        )
        .unwrap_err();
        assert!(format!("{failure:#}").contains("第 2 页"));
        let dir = task_dir(root, &task.id).unwrap();
        assert!(stored_page(&dir, 1).is_some());
        assert!(stored_page(&dir, 2).is_none());
        assert!(!chapter_archive(root, &task).exists());
        // Simulate the app closing before either progress or completion is

        // drained by its UI. Both stages recover using the worker's files.

        let recovered = execute_task(
            root,
            &task,
            pages,
            &stop,
            |page| {
                assert!(matches!(&page.content, PageContent::Url { url, .. } if url == "2"));
                Ok(png())
            },
            |_, _, _| {},
        )
        .unwrap();
        let Outcome::Done { path } = recovered else {
            panic!("retry did not complete")
        };
        let mut on_disk = task;
        on_disk.status = DownloadStatus::Running;
        let mut tasks = [on_disk];
        normalize_loaded(root, &mut tasks);
        assert_eq!(tasks[0].status, DownloadStatus::Done);
        assert_eq!(tasks[0].cbz.as_ref(), Some(&path));
        assert_eq!(pending_imports(&tasks), vec![tasks[0].id.clone()]);
    }

    #[test]
    fn completed_downloads_follow_a_moved_container_without_reimporting_linked_chapters() {
        let temp = tempfile::tempdir().unwrap();
        let old_root = temp.path().join("old");
        let new_root = temp.path().join("new");
        let mut tasks = Vec::new();
        let stop = AtomicU8::new(STOP_NONE);
        for linked in [false, true] {
            let mut item = task("fixture", "m", if linked { "linked" } else { "pending" });
            let Outcome::Done { path } = execute_task(
                &old_root,
                &item,
                vec![url_page("1")],
                &stop,
                |_| Ok(png()),
                |_, _, _| {},
            )
            .unwrap() else {
                panic!("fixture did not complete")
            };
            item.status = DownloadStatus::Done;
            item.cbz = Some(path);
            item.linked = linked;
            tasks.push(item);
        }
        std::fs::write(
            old_root.join("download-tasks.json"),
            serde_json::to_vec(&TaskStore {
                tasks: tasks.clone(),
            })
            .unwrap(),
        )
        .unwrap();
        std::fs::rename(&old_root, &new_root).unwrap();
        // A leftover old container must never take precedence over the

        // complete archive inside the library we actually opened.

        let stale = tasks[0].cbz.as_ref().unwrap();
        std::fs::create_dir_all(stale.parent().unwrap()).unwrap();
        std::fs::write(stale, b"stale").unwrap();
        let (loaded, warning) = load_queue(&new_root);
        assert!(warning.is_none());
        for item in &loaded {
            assert_eq!(item.status, DownloadStatus::Done);
            assert_eq!(item.cbz.as_ref(), Some(&chapter_archive(&new_root, item)));
            let archive = std::fs::File::open(item.cbz.as_ref().unwrap()).unwrap();
            assert_eq!(zip::ZipArchive::new(archive).unwrap().len(), 1);
        }
        assert!(!loaded[0].linked);
        assert!(loaded[1].linked);
        assert_eq!(pending_imports(&loaded), vec![loaded[0].id.clone()]);
    }

    #[test]
    fn inline_pages_with_equal_length_but_different_bytes_are_rewritten() {
        let dir = tempfile::tempdir().unwrap();
        let work = dir.path().join("task");
        std::fs::create_dir_all(&work).unwrap();
        let old = colored_png([255, 0, 0, 255]);
        let new = colored_png([0, 0, 255, 255]);
        assert_eq!(
            old.len(),
            new.len(),
            "fixture must have different bytes but equal length"
        );
        assert_ne!(old, new);
        std::fs::write(work.join("page-0001.png"), &old).unwrap();
        let page = Page {
            content: PageContent::Image {
                image_ref: 7,
                image_data: Some(new.clone()),
            },
            thumbnail: None,
            has_description: false,
            description: None,
        };
        let stop = AtomicU8::new(STOP_NONE);
        let fetched = AtomicUsize::new(0);
        let summary = fetch_missing_pages(
            &work,
            &[page],
            |_| {
                fetched.fetch_add(1, Ordering::SeqCst);
                Ok(new.clone())
            },
            |_, _| {},
            &stop,
        )
        .unwrap();
        assert_eq!(
            fetched.load(Ordering::SeqCst),
            1,
            "equal-length inline bytes must not be trusted from the signature"
        );
        assert_eq!(summary.done, 1);
        assert_eq!(std::fs::read(work.join("page-0001.png")).unwrap(), new);
    }

    #[test]
    fn paused_download_stops_before_fetching() {
        let dir = tempfile::tempdir().unwrap();
        let pages = vec![url_page("p1"), url_page("p2")];
        let stop = AtomicU8::new(STOP_PAUSE);
        let summary = fetch_missing_pages(
            &dir.path().join("task"),
            &pages,
            |_| panic!("must not fetch while paused"),
            |_, _| {},
            &stop,
        )
        .unwrap();
        assert!(summary.aborted);
        assert_eq!(summary.done, 0);
        assert!(existing_pages(&dir.path().join("task")).is_empty());
    }

    #[test]
    fn missing_pages_download_in_parallel_in_page_order() {
        let dir = tempfile::tempdir().unwrap();
        let work = dir.path().join("task");
        let pages: Vec<_> = (0..8).map(|index| url_page(&format!("p{index}"))).collect();
        let stop = AtomicU8::new(STOP_NONE);
        let in_flight = AtomicUsize::new(0);
        let peak = AtomicUsize::new(0);
        let order = std::sync::Mutex::new(Vec::new());
        let summary = fetch_missing_pages(
            &work,
            &pages,
            |page| {
                if let PageContent::Url { url, .. } = &page.content {
                    order.lock().unwrap().push(url.clone());
                }
                let now = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(now, Ordering::SeqCst);
                std::thread::sleep(std::time::Duration::from_millis(40));
                in_flight.fetch_sub(1, Ordering::SeqCst);
                Ok(png())
            },
            |_, _| {},
            &stop,
        )
        .unwrap();
        assert_eq!(summary.done, 8);
        let peak = peak.load(Ordering::SeqCst);
        assert!(
            (2..=PAGE_CONCURRENCY).contains(&peak),
            "pages did not overlap as expected: peak {peak}"
        );
        let started = order.lock().unwrap().clone();
        let first_batch = &started[..PAGE_CONCURRENCY.min(started.len())];
        assert!(
            first_batch
                .iter()
                .all(|url| url.starts_with('p')
                    && url[1..].parse::<usize>().unwrap() < PAGE_CONCURRENCY),
            "first pages were not handed out in ascending order: {started:?}"
        );
        for number in 1..=8 {
            assert!(stored_page(&work, number).is_some());
        }
    }

    #[test]
    fn a_failed_page_keeps_completed_siblings_and_resumes_the_rest() {
        let dir = tempfile::tempdir().unwrap();
        let work = dir.path().join("task");
        let pages: Vec<_> = (0..6).map(|index| url_page(&format!("p{index}"))).collect();
        let stop = AtomicU8::new(STOP_NONE);
        let error = fetch_missing_pages(
            &work,
            &pages,
            |page| {
                if matches!(&page.content, PageContent::Url { url, .. } if url == "p2") {
                    anyhow::bail!("第 3 页上游返回 500")
                } else {
                    std::thread::sleep(std::time::Duration::from_millis(30));
                    Ok(png())
                }
            },
            |_, _| {},
            &stop,
        )
        .unwrap_err();
        assert!(error.to_string().contains("500"));
        let stored = existing_pages(&work);
        assert!(
            stored.len() >= 2,
            "in-flight siblings of the failed page stay reusable: {stored:?}"
        );
        assert!(!stored.contains(&3));
        assert!(
            !stored.contains(&5) && !stored.contains(&6),
            "pages after the failure must not have started: {stored:?}"
        );

        let fetched = AtomicUsize::new(0);
        let summary = fetch_missing_pages(
            &work,
            &pages,
            |_| {
                fetched.fetch_add(1, Ordering::SeqCst);
                Ok(png())
            },
            |_, _| {},
            &stop,
        )
        .unwrap();
        assert_eq!(summary.done, 6);
        assert_eq!(fetched.load(Ordering::SeqCst), 6 - stored.len());
    }

    #[test]
    fn viewer_hint_reads_memory_then_the_saved_snapshot() {
        let root = tempfile::tempdir().unwrap();
        let mut sources = test_sources(root.path());
        assert!(sources.viewer_hint("source.a", "m").is_none());
        let mut stored = manga("source.a", "m");
        stored.viewer = Viewer::RightToLeft;
        save_manga_snapshot(root.path(), &stored).unwrap();
        assert_eq!(
            sources.viewer_hint("source.a", "m"),
            Some(Viewer::RightToLeft)
        );
        let mut selected = manga("source.a", "m");
        selected.viewer = Viewer::LeftToRight;
        sources.selected_manga = Some(selected);
        assert_eq!(
            sources.viewer_hint("source.a", "m"),
            Some(Viewer::LeftToRight),
            "loaded details win over the snapshot"
        );
        let mut unknown = manga("source.a", "other");
        unknown.viewer = Viewer::Unknown;
        sources.selected_manga = Some(unknown);
        assert!(sources.viewer_hint("source.a", "other").is_none());
        assert_eq!(
            sources.viewer_hint("source.a", "m"),
            Some(Viewer::RightToLeft),
            "a different open detail still falls back to the snapshot"
        );
    }

    #[test]
    fn page_list_change_clears_only_this_task_pages() {
        let dir = tempfile::tempdir().unwrap();
        let work = dir.path().join("task");
        std::fs::create_dir_all(&work).unwrap();
        std::fs::write(work.join("page-0001.png"), png()).unwrap();
        std::fs::write(work.join("notes.txt"), b"keep").unwrap();
        reset_page_cache(&work).unwrap();
        assert!(existing_pages(&work).is_empty());
        assert_eq!(std::fs::read(work.join("notes.txt")).unwrap(), b"keep");
    }

    #[test]
    fn enqueue_dedupes_task_and_protects_existing_cbz() {
        let dir = tempfile::tempdir().unwrap();
        let mut tasks = Vec::new();
        assert!(matches!(
            enqueue_task(&mut tasks, task("source.a", "m", "c")),
            EnqueueOutcome::Added
        ));
        assert!(matches!(
            enqueue_task(&mut tasks, task("source.a", "m", "c")),
            EnqueueOutcome::Duplicate
        ));
        assert_eq!(tasks.len(), 1);
        let cbz = dir.path().join("chapter.cbz");
        std::fs::write(&cbz, b"done").unwrap();
        tasks[0].status = DownloadStatus::Done;
        tasks[0].cbz = Some(cbz);
        assert!(matches!(
            enqueue_task(&mut tasks, task("source.a", "m", "c")),
            EnqueueOutcome::AlreadyDownloaded
        ));
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].status, DownloadStatus::Done);
    }

    #[test]
    fn reopened_download_fetches_only_missing_pages() {
        let root = tempfile::tempdir().unwrap();
        let mut item = task("source.a", "m", "c");
        let pages = vec![url_page("p1"), url_page("p2")];
        let stop = AtomicU8::new(STOP_NONE);
        let mut signature = None;
        let first = execute_task(
            root.path(),
            &item,
            pages.clone(),
            &stop,
            |page| match &page.content {
                PageContent::Url { url, .. } if url == "p2" => anyhow::bail!("第 2 页上游返回 500"),
                _ => Ok(png()),
            },
            |_, _, value| {
                if value.is_some() {
                    signature = value;
                }
            },
        );
        assert!(first.is_err());
        let work = task_dir(root.path(), &item.id).unwrap();
        assert!(stored_page(&work, 1).is_some());
        assert!(stored_page(&work, 2).is_none());
        // Persist the page fingerprint the UI received before the failure and

        // reload, as after an app restart.

        item.pages_signature = signature;
        item.status = DownloadStatus::Failed;
        let store = TaskStore { tasks: vec![item] };
        std::fs::write(
            root.path().join("download-tasks.json"),
            serde_json::to_vec(&store).unwrap(),
        )
        .unwrap();
        let loaded = load_tasks(root.path()).unwrap();
        assert_eq!(loaded.tasks.len(), 1);
        let reopened = loaded.tasks[0].clone();
        assert!(reopened.pages_signature.is_some());

        let fetched = std::sync::Mutex::new(Vec::new());
        let outcome = execute_task(
            root.path(),
            &reopened,
            pages,
            &stop,
            |page| {
                if let PageContent::Url { url, .. } = &page.content {
                    fetched.lock().unwrap().push(url.clone());
                }
                Ok(png())
            },
            |_, _, _| {},
        )
        .unwrap();
        assert_eq!(
            *fetched.lock().unwrap(),
            vec!["p2"],
            "the complete first page must not be fetched again"
        );
        assert!(matches!(outcome, Outcome::Done { .. }));
    }

    #[test]
    fn failed_download_does_not_block_the_next_task() {
        let root = tempfile::tempdir().unwrap();
        let mut sources = test_sources(root.path());
        let first = task("source.a", "m", "c1");
        let second = task("source.a", "m", "c2");
        sources.tasks = vec![first.clone(), second.clone()];
        sources.tasks[0].status = DownloadStatus::Running;
        sources.running = Some(first.id.clone());
        let stop = AtomicU8::new(STOP_NONE);
        let error = execute_task(
            root.path(),
            &first,
            vec![url_page("a1"), url_page("a2")],
            &stop,
            |page| match &page.content {
                PageContent::Url { url, .. } if url == "a2" => anyhow::bail!("a2 上游返回 500"),
                _ => Ok(png()),
            },
            |_, _, _| {},
        )
        .unwrap_err();
        sources.handle_download_event(DlEvent::Failed {
            id: first.id.clone(),
            error: error.to_string(),
        });
        assert_eq!(sources.tasks[0].status, DownloadStatus::Failed);
        assert_eq!(sources.running.as_deref(), Some(second.id.as_str()));

        let outcome = execute_task(
            root.path(),
            &second,
            vec![url_page("b1")],
            &stop,
            |_| Ok(png()),
            |_, _, _| {},
        )
        .unwrap();
        let Outcome::Done { path } = outcome else {
            panic!("second task did not finish")
        };
        sources.handle_download_event(DlEvent::Finished {
            id: second.id.clone(),
            path: path.clone(),
        });
        assert_eq!(sources.tasks[1].status, DownloadStatus::Done);
        assert!(sources.ready.contains(&second.id));
        assert!(path.exists());
    }

    #[test]
    fn finished_downloads_survive_import_failure_and_drain_in_order() {
        let root = tempfile::tempdir().unwrap();
        let mut sources = test_sources(root.path());
        let mut items = Vec::new();
        for chapter in ["c1", "c2", "c3"] {
            let mut item = task("source.a", "m", chapter);
            let path = root.path().join(format!("{chapter}.cbz"));
            std::fs::write(&path, b"cbz").unwrap();
            item.status = DownloadStatus::Done;
            item.cbz = Some(path);
            items.push(item);
        }
        sources.tasks = items.clone();
        for item in &items {
            sources.ready.push_back(item.id.clone());
        }

        let first = sources.take_downloaded().unwrap();
        let second = sources.take_downloaded().unwrap();
        let third = sources.take_downloaded().unwrap();
        assert_eq!(first.id, items[0].id);
        assert_eq!(second.id, items[1].id);
        assert_eq!(third.id, items[2].id);
        sources.ack_downloaded(&first.id);
        // The second import failed: put only that file back and retry it

        // without touching the third file or re-downloading anything.

        sources.requeue_import(&second.id);
        let retried = sources.take_downloaded().unwrap();
        assert_eq!(retried.id, second.id);
        assert!(retried.path.exists());
        sources.ack_downloaded(&third.id);
        sources.ack_downloaded(&retried.id);
        assert!(sources.take_downloaded().is_none());
        assert!(sources.tasks.iter().all(|item| item.linked));
        assert!(
            sources
                .tasks
                .iter()
                .all(|item| item.status == DownloadStatus::Done)
        );
    }

    #[test]
    fn import_failure_keeps_the_file_and_allows_manual_retry_only() {
        let root = tempfile::tempdir().unwrap();
        let mut sources = test_sources(root.path());
        let mut item = task("source.a", "m", "c");
        let path = root.path().join("chapter.cbz");
        std::fs::write(&path, b"cbz").unwrap();
        item.status = DownloadStatus::Done;
        item.cbz = Some(path.clone());
        sources.tasks = vec![item.clone()];
        sources.ready.push_back(item.id.clone());

        let offered = sources.take_downloaded().unwrap();
        assert!(
            sources.take_downloaded().is_none(),
            "an in-flight import must not be offered twice"
        );
        sources.import_failed(&offered.id, "cbz 解压失败");
        assert!(sources.status.contains("加入书架失败"));
        assert_eq!(sources.tasks[0].status, DownloadStatus::Done);
        assert!(!sources.tasks[0].linked);
        assert!(path.exists());
        assert!(
            sources.take_downloaded().is_none(),
            "failure must wait for the explicit retry entry"
        );

        sources.requeue_import(&offered.id);
        let retried = sources.take_downloaded().unwrap();
        assert_eq!(retried.id, item.id);
        sources.ack_downloaded(&retried.id);
        assert!(sources.tasks[0].linked);
    }

    #[test]
    fn wasm_source_pages_flow_through_download_and_import_handoff() {
        let root = tempfile::tempdir().unwrap();
        let (base, server) = serve_png(2);
        let urls = vec![format!("{base}/page-1.png"), format!("{base}/page-2.png")];
        let engine = SourceEngine::load_wasm(
            source_manifest(),
            pages_wasm(&urls),
            EngineOptions::default(),
        )
        .unwrap();
        let item = task("test.source", "m1", "c1");
        let pages = pollster::block_on(engine.pages_with(item.manga(), item.chapter())).unwrap();
        assert_eq!(pages.len(), 2);
        assert!(matches!(&pages[0].content,PageContent::Url{url,..} if url==&urls[0]));

        let stop = AtomicU8::new(STOP_NONE);
        let fetched = AtomicUsize::new(0);
        let outcome = execute_task(
            root.path(),
            &item,
            pages,
            &stop,
            |page| {
                fetched.fetch_add(1, Ordering::SeqCst);
                page_bytes(&engine, page)
            },
            |_, _, _| {},
        )
        .unwrap();
        let Outcome::Done { path } = outcome else {
            panic!("download did not finish")
        };
        assert_eq!(fetched.load(Ordering::SeqCst), 2);

        let mut sources = test_sources(root.path());
        sources.tasks = vec![item.clone()];
        sources.running = Some(item.id.clone());
        sources.handle_download_event(DlEvent::Finished {
            id: item.id.clone(),
            path: path.clone(),
        });
        let downloaded = sources
            .take_downloaded()
            .expect("finished chapter offered to the importer");
        assert_eq!(downloaded.id, item.id);
        assert_eq!(downloaded.metadata.source_id, "test.source");
        assert_eq!(downloaded.metadata.manga_id, "m1");
        assert_eq!(downloaded.metadata.chapter_id, "c1");
        assert!(downloaded.path.exists());
        let library = yejian_core::Library::open(&root.path().join("library")).unwrap();
        let book = library.import(&downloaded.path).unwrap();
        library
            .link_chapter(&book.id, &downloaded.metadata)
            .unwrap();
        assert_eq!(book.page_count, 2);
        for page in 0..book.page_count {
            let bytes = library.page_bytes(&book.id, page).unwrap();
            assert_eq!(crate::worker::decode(&bytes, false).unwrap().size, [4, 4]);
        }
        sources.ack_downloaded(&downloaded.id);
        assert!(sources.take_downloaded().is_none());
        server.join().unwrap();
    }

    #[test]
    fn restart_pauses_running_downloads_and_detects_missing_cbz() {
        let dir = tempfile::tempdir().unwrap();
        let existing = dir.path().join("ok.cbz");
        std::fs::write(&existing, b"done").unwrap();
        let mut running = task("source.a", "m", "c1");
        running.status = DownloadStatus::Running;
        let mut done_ok = task("source.a", "m", "c2");
        done_ok.status = DownloadStatus::Done;
        done_ok.cbz = Some(existing);
        let mut done_missing = task("source.a", "m", "c3");
        done_missing.status = DownloadStatus::Done;
        done_missing.cbz = Some(dir.path().join("gone.cbz"));
        let mut tasks = vec![running, done_ok, done_missing];
        normalize_loaded(dir.path(), &mut tasks);
        assert_eq!(tasks[0].status, DownloadStatus::Paused);
        assert!(tasks[0].error.is_some());
        assert_eq!(tasks[1].status, DownloadStatus::Done);
        assert_eq!(tasks[2].status, DownloadStatus::Failed);
        assert!(tasks[2].cbz.is_none());
    }

    #[test]
    fn corrupt_queue_entry_does_not_discard_the_rest() {
        let dir = tempfile::tempdir().unwrap();
        let valid = task("source.a", "m", "c");
        let mut bad_id = task("source.a", "m", "other");
        bad_id.id = "../escape".into();
        let value = serde_json::json!({"tasks":[serde_json::to_value(&valid).unwrap(),{"id":5},serde_json::to_value(&bad_id).unwrap()]});
        std::fs::write(
            dir.path().join("download-tasks.json"),
            serde_json::to_vec(&value).unwrap(),
        )
        .unwrap();
        let loaded = load_tasks(dir.path()).unwrap();
        assert_eq!(loaded.tasks.len(), 1);
        assert_eq!(loaded.tasks[0].id, valid.id);
        assert!(loaded.warning.unwrap().contains("2 条损坏记录"));
    }

    #[test]
    fn corrupt_queue_file_is_reported_and_preserved() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("download-tasks.json");
        let original = b"{ this is not json";
        std::fs::write(&path, original).unwrap();
        let error = load_tasks(dir.path()).unwrap_err();
        assert!(error.to_string().contains("损坏"));
        assert_eq!(std::fs::read(&path).unwrap(), original);

        let (tasks, warning) = load_queue(dir.path());
        assert!(tasks.is_empty());
        assert!(warning.unwrap().contains("读取失败"));
        assert_eq!(std::fs::read(&path).unwrap(), original);
    }

    #[test]
    fn persist_failure_is_reported_and_never_starts_an_unrecorded_task() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("not-a-directory");
        std::fs::write(&root, b"keep").unwrap();
        let sources = test_sources(&root);
        let error = sources.persist().unwrap_err();
        assert!(!error.to_string().is_empty());
        assert_eq!(std::fs::read(&root).unwrap(), b"keep");
        // Allow the new metadata snapshot to be written, then fail the queue

        // file's atomic replacement rather than the earlier snapshot stage.

        let queue_root = dir.path().join("queue-write-failure");
        let blocked = queue_root.join("download-tasks.json");
        std::fs::create_dir_all(&blocked).unwrap();
        std::fs::write(blocked.join("keep.txt"), b"keep").unwrap();
        let mut sources = test_sources(&queue_root);
        sources.selected_source = "source.a".into();
        let book = manga("source.a", "m");
        let chapter = Chapter {
            key: "c".into(),
            title: Some("c".into()),
            ..Default::default()
        };
        sources.enqueue_chapter(&book, &chapter);
        assert!(
            sources.tasks.is_empty(),
            "an unpersisted task must be rolled back"
        );
        assert!(
            sources.running.is_none(),
            "an unpersisted task must not start"
        );
        assert!(sources.status.contains("无法保存下载队列"));
        assert_eq!(std::fs::read(blocked.join("keep.txt")).unwrap(), b"keep");
        // Retrying an existing failed task and bulk-adding chapters must also

        // roll back status/snapshot changes if the atomic queue write fails.

        let mut failed = DownloadTask::from_chapter("source.a", &book, &chapter);
        failed.status = DownloadStatus::Failed;
        failed.error = Some("original failure".into());
        failed.manga_snapshot = None;
        sources.tasks.push(failed.clone());
        sources.enqueue_chapter(&book, &chapter);
        assert_eq!(sources.tasks[0].status, DownloadStatus::Failed);
        assert_eq!(sources.tasks[0].manga_snapshot, None);
        assert_eq!(sources.tasks[0].error, failed.error);
        sources.chapters = vec![
            chapter,
            Chapter {
                key: "new".into(),
                ..Default::default()
            },
        ];
        sources.enqueue_missing(&book);
        assert_eq!(sources.tasks.len(), 1);
        assert_eq!(sources.tasks[0].status, DownloadStatus::Failed);
        assert_eq!(sources.tasks[0].manga_snapshot, None);
        assert!(sources.running.is_none());
    }

    #[test]
    fn invalid_or_linked_task_paths_never_delete_neighbour_files() {
        let root = tempfile::tempdir().unwrap();
        let work = work_root(root.path());
        std::fs::create_dir_all(&work).unwrap();
        let outside = root.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("keep.txt"), b"keep").unwrap();

        let error = remove_task_cache(root.path(), "x/../../outside").unwrap_err();
        assert!(error.to_string().contains("非法"));
        assert_eq!(std::fs::read(outside.join("keep.txt")).unwrap(), b"keep");

        let id = "0123456789abcdef";
        let link = work.join(format!("task-{id}"));
        #[cfg(windows)]
        let linked = std::os::windows::fs::symlink_dir(&outside, &link).is_ok();
        #[cfg(unix)]
        let linked = std::os::unix::fs::symlink(&outside, &link).is_ok();
        #[cfg(not(any(windows, unix)))]
        let linked = false;
        if linked {
            let error = remove_task_cache(root.path(), id).unwrap_err();
            assert!(error.to_string().contains("download-work 之外"));
            assert_eq!(std::fs::read(outside.join("keep.txt")).unwrap(), b"keep");
            let _ = std::fs::remove_dir(&link);
        }
    }

    #[test]
    fn pending_imports_skip_linked_and_missing_files() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("chapter.cbz");
        std::fs::write(&file, b"cbz").unwrap();
        let mut waiting = task("source.a", "m", "c1");
        waiting.status = DownloadStatus::Done;
        waiting.cbz = Some(file.clone());
        let mut linked = task("source.a", "m", "c2");
        linked.status = DownloadStatus::Done;
        linked.cbz = Some(file.clone());
        linked.linked = true;
        let mut missing = task("source.a", "m", "c3");
        missing.status = DownloadStatus::Done;
        missing.cbz = Some(dir.path().join("gone.cbz"));
        let ids = pending_imports(&[waiting.clone(), linked, missing]);
        assert_eq!(ids, vec![waiting.id]);
    }

    #[test]
    fn import_ack_keeps_finished_file_without_downloading_again() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("chapter.cbz");
        std::fs::write(&file, b"cbz").unwrap();
        let mut done = task("source.a", "m", "c");
        done.status = DownloadStatus::Done;
        done.cbz = Some(file);
        let mut sources = test_sources(dir.path());
        sources.ready.push_back(done.id.clone());
        sources.tasks = vec![done.clone()];
        let item = sources.take_downloaded().unwrap();
        assert_eq!(item.id, done.id);
        assert!(item.path.exists());
        // A failed import can put it back; the file is not downloaded again.

        sources.requeue_import(&done.id);
        assert!(sources.take_downloaded().is_some());
        sources.ack_downloaded(&done.id);
        assert!(sources.tasks[0].linked);
        assert!(sources.take_downloaded().is_none());
    }

    #[test]
    fn cancel_removes_only_its_own_work_dir() {
        let root = tempfile::tempdir().unwrap();
        let id = identity_id("source.a", "m", "c");
        let own = task_dir(root.path(), &id).unwrap();
        let other = task_dir(root.path(), "0000000000000000").unwrap();
        std::fs::create_dir_all(&own).unwrap();
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(own.join("page-0001.png"), png()).unwrap();
        std::fs::write(other.join("page-0001.png"), png()).unwrap();
        let downloads = root.path().join("downloads");
        std::fs::create_dir_all(&downloads).unwrap();
        std::fs::write(downloads.join("keep.cbz"), b"keep").unwrap();
        let mut sources = test_sources(root.path());
        sources.tasks = vec![task("source.a", "m", "c")];
        assert!(sources.remove_task_now(&id, "已从下载队列移除"));
        assert!(sources.tasks.is_empty());
        assert!(!own.exists());
        assert!(other.exists());
        assert_eq!(std::fs::read(downloads.join("keep.cbz")).unwrap(), b"keep");
    }

    #[test]
    fn dropping_sources_requests_pause_without_blocking() {
        let root = tempfile::tempdir().unwrap();
        let sources = test_sources(root.path());
        let stop = sources.dl_stop.clone();
        assert_eq!(stop.load(Ordering::SeqCst), STOP_NONE);
        drop(sources);
        assert_eq!(stop.load(Ordering::SeqCst), STOP_PAUSE);
    }

    #[test]
    fn download_lock_is_exclusive_between_instances() {
        let root = tempfile::tempdir().unwrap();
        let first = acquire_download_lock(root.path()).unwrap();
        let error = acquire_download_lock(root.path()).unwrap_err();
        assert!(error.to_string().contains("上一次下载仍在退出"));
        drop(first);
        let _second = acquire_download_lock(root.path()).unwrap();
    }

    #[test]
    fn incomplete_cbz_build_removes_its_part_and_keeps_existing_files() {
        let root = tempfile::tempdir().unwrap();
        let downloads = root.path().join("downloads");
        std::fs::create_dir_all(&downloads).unwrap();
        let existing = downloads.join("previous.cbz");
        std::fs::write(&existing, b"previous download").unwrap();
        let work = root.path().join("download-work").join("task");
        std::fs::create_dir_all(&work).unwrap();
        std::fs::write(work.join("page-0001.png"), png()).unwrap();
        let task = task("source.a", "m", "c");
        let error = build_cbz(root.path(), &task, &work, 2).unwrap_err();
        assert!(error.to_string().contains("缓存缺失"));
        assert_eq!(std::fs::read(&existing).unwrap(), b"previous download");
        let parts: Vec<_> = std::fs::read_dir(&downloads)
            .unwrap()
            .flatten()
            .filter(|entry| {
                entry
                    .path()
                    .extension()
                    .is_some_and(|ext| ext.to_string_lossy() == "part")
            })
            .collect();
        assert!(
            parts.is_empty(),
            "failed CBZ build must not leave a .part file"
        );
    }

    #[test]
    fn full_manga_and_chapter_snapshots_survive_restart_and_page_requests() {
        let root = tempfile::tempdir().unwrap();
        let mut book = manga("test.source", "m42");
        book.title = "Full Work".into();
        book.cover = Some("cover.png".into());
        book.artists = Some(vec!["Artist A".into(), "Artist B".into()]);
        book.authors = Some(vec!["Author A".into()]);
        book.description = Some("简介".into());
        book.url = Some("https://example.test/m42".into());
        book.tags = Some(vec!["action".into(), "drama".into()]);
        book.status = PublishingStatus::Ongoing;
        book.content_rating = ContentRating::Safe;
        book.viewer = Viewer::Vertical;
        book.update_strategy = UpdateStrategy::Never;
        book.next_update_time = Some(1_700_000_000);
        book.chapters = Some(vec![Chapter {
            key: "c0".into(),
            title: Some("Chapter 0".into()),
            ..Default::default()
        }]);
        let chapter = Chapter {
            key: "c42".into(),
            title: Some("Chapter 42".into()),
            chapter_number: Some(42.5),
            volume_number: Some(4.),
            date_uploaded: Some(1_690_000_000),
            scanlators: Some(vec!["Group X".into()]),
            url: Some("https://example.test/c42".into()),
            language: Some("zh".into()),
            thumbnail: Some("thumb.png".into()),
            locked: true,
        };
        // Real enqueue path: writes the shared manga snapshot and the queue.

        let mut sources = test_sources(root.path());
        sources.enqueue_chapter(&book, &chapter);
        let id = identity_id("test.source", "m42", "c42");
        assert!(
            sources.tasks.iter().any(|task| task.id == id),
            "task queued"
        );
        sources.enqueue_chapter(
            &book,
            &Chapter {
                key: "c43".into(),
                title: Some("Chapter 43".into()),
                ..Default::default()
            },
        );
        // One snapshot per work, not one per chapter task.

        let snapshots: Vec<_> = std::fs::read_dir(snapshot_root(root.path()))
            .unwrap()
            .flatten()
            .filter(|entry| {
                entry
                    .path()
                    .extension()
                    .is_some_and(|ext| ext.to_string_lossy() == "manga")
            })
            .collect();
        assert_eq!(snapshots.len(), 1, "same manga must be stored once");
        let json = std::fs::read_to_string(root.path().join("download-tasks.json")).unwrap();
        assert!(
            !json.contains("\"c0\""),
            "the manga chapter list must not be duplicated into task JSON"
        );
        assert!(
            json.contains("\"manga_snapshot\""),
            "task references the shared snapshot"
        );
        // Restart: reload the queue from disk and restore both objects.

        let (tasks, warning) = load_queue(root.path());
        assert!(warning.is_none());
        let restored = tasks
            .into_iter()
            .find(|task| task.id == id)
            .expect("task survives restart");
        assert_eq!(
            restored.manga_title, "Full Work",
            "legacy fields stay readable"
        );
        let manga = restored.manga_for(root.path()).unwrap();
        let restored_chapter = restored.chapter_for().unwrap();
        assert_eq!(manga.authors, book.authors);
        assert_eq!(manga.artists, book.artists);
        assert_eq!(manga.tags, book.tags);
        assert_eq!(manga.description, book.description);
        assert_eq!(manga.viewer, Viewer::Vertical);
        assert_eq!(manga.status, PublishingStatus::Ongoing);
        assert_eq!(manga.chapters.as_ref().map(Vec::len), Some(1));
        assert_eq!(restored_chapter.scanlators, chapter.scanlators);
        assert_eq!(restored_chapter.date_uploaded, chapter.date_uploaded);
        assert_eq!(restored_chapter.thumbnail, chapter.thumbnail);
        assert!(restored_chapter.locked);
        // These are exactly the bytes the interpreter sends to get_page_list.

        assert_eq!(
            manga.to_postcard_bytes(),
            book.to_postcard_bytes(),
            "source receives the identical manga bytes"
        );
        assert_eq!(
            restored_chapter.to_postcard_bytes(),
            chapter.to_postcard_bytes(),
            "source receives the identical chapter bytes"
        );
        // A real WASM engine still accepts the restored objects for page listing.

        let urls = vec!["https://example.test/page-1.png".to_string()];
        let engine = SourceEngine::load_wasm(
            source_manifest(),
            pages_wasm(&urls),
            EngineOptions::default(),
        )
        .unwrap();
        let pages = pollster::block_on(engine.pages_with(
            restored.manga_for(root.path()).unwrap(),
            restored.chapter_for().unwrap(),
        ))
        .unwrap();
        assert_eq!(pages.len(), 1);
    }

    #[test]
    fn broken_recorded_snapshot_fails_while_legacy_tasks_keep_the_fallback() {
        let root = tempfile::tempdir().unwrap();
        let mut book = manga("test.source", "m7");
        book.authors = Some(vec!["Author A".into()]);
        book.tags = Some(vec!["drama".into()]);
        book.viewer = Viewer::RightToLeft;
        let chapter = Chapter {
            key: "c7".into(),
            title: Some("Chapter 7".into()),
            scanlators: Some(vec!["Group".into()]),
            ..Default::default()
        };
        let task = DownloadTask::from_chapter("test.source", &book, &chapter);
        assert!(
            task.chapter_snapshot.is_some(),
            "new tasks record snapshots"
        );
        // Old queues have no snapshot at all and keep the reduced DTO fallback.

        let mut legacy = task.clone();
        legacy.manga_snapshot = None;
        legacy.chapter_snapshot = None;
        assert_eq!(legacy.manga_for(root.path()).unwrap().title, "m7");
        assert_eq!(legacy.chapter_for().unwrap().key, "c7");
        // A task that recorded a snapshot never degrades to that fallback.

        let key = manga_snapshot_key("test.source", "m7");
        let mut recorded = task.clone();
        recorded.manga_snapshot = Some(key.clone());
        let error = recorded.manga_for(root.path()).unwrap_err().to_string();
        assert!(
            error.contains("漫画快照不可用") && error.contains("请回到作品详情"),
            "missing snapshot is explicit: {error}"
        );

        recorded.manga_snapshot = Some("../escape".into());
        assert!(
            recorded
                .manga_for(root.path())
                .unwrap_err()
                .to_string()
                .contains("非法")
        );

        std::fs::create_dir_all(snapshot_root(root.path())).unwrap();
        let path = manga_snapshot_path(root.path(), &key).unwrap();
        std::fs::write(&path, b"not a postcard").unwrap();
        recorded.manga_snapshot = Some(key.clone());
        let error = recorded.manga_for(root.path()).unwrap_err().to_string();
        assert!(
            error.contains("已损坏"),
            "corrupt snapshot is explicit: {error}"
        );

        let mut other = book.clone();
        other.key = "another".into();
        std::fs::write(&path, other.to_postcard_bytes()).unwrap();
        let error = recorded.manga_for(root.path()).unwrap_err().to_string();
        assert!(
            error.contains("不匹配"),
            "snapshot of another manga is rejected: {error}"
        );

        let mut bad_chapter = task.clone();
        bad_chapter.manga_snapshot = None;
        bad_chapter.chapter_snapshot = Some(vec![0, 1, 2]);
        assert!(
            bad_chapter
                .chapter_for()
                .unwrap_err()
                .to_string()
                .contains("章节快照已损坏")
        );
        // Re-adding the chapter from the manga detail page rebuilds the

        // snapshot from the new complete object and requeues the task.

        let mut updated = book.clone();
        updated.authors = Some(vec!["New Author".into()]);
        let mut sources = test_sources(root.path());
        let mut failed = DownloadTask::from_chapter("test.source", &book, &chapter);
        failed.status = DownloadStatus::Failed;
        failed.manga_snapshot = Some("0000000000000000".into());
        failed.chapter_snapshot = None;
        sources.tasks = vec![failed];
        sources.enqueue_chapter(&updated, &chapter);
        let task_id = identity_id("test.source", "m7", "c7");
        let retried = sources
            .tasks
            .iter()
            .find(|item| item.id == task_id)
            .unwrap();
        assert_ne!(
            retried.status,
            DownloadStatus::Failed,
            "failed chapter is requeued"
        );
        let rebuilt = retried
            .manga_for(root.path())
            .expect("re-join writes a readable snapshot");
        assert_eq!(rebuilt.authors, Some(vec!["New Author".into()]));
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
    fn cover_queue_tracks_only_the_visible_window() {
        let req = |key: &str| CoverReq {
            key: key.into(),
            source: "s".into(),
            url: format!("https://img/{key}"),
        };
        let keys = |queue: &VecDeque<CoverReq>| {
            queue.iter().map(|req| req.key.clone()).collect::<Vec<_>>()
        };
        let skip = HashSet::from(["in-flight".to_string(), "failed".to_string()]);
        let mut queue = VecDeque::new();
        plan_covers(
            &mut queue,
            vec![req("a"), req("b"), req("in-flight"), req("failed")],
            &skip,
        );
        assert_eq!(
            keys(&queue),
            ["a", "b"],
            "sent and failed covers never queue"
        );
        plan_covers(&mut queue, vec![req("a"), req("c"), req("d")], &skip);
        assert_eq!(
            keys(&queue),
            ["a", "c", "d"],
            "scrolled-away cells drop out"
        );
        let many: Vec<CoverReq> = (0..30).map(|i| req(&format!("k{i}"))).collect();
        plan_covers(&mut queue, many, &skip);
        assert_eq!(queue.len(), COVER_QUEUE_CAP);
        assert_eq!(queue.back().unwrap().key, "k29", "newest requests win");
        assert!(
            !queue.iter().any(|req| req.key == "a"),
            "oldest drops first"
        );
    }

    #[test]
    fn cover_key_is_stable_and_scoped() {
        assert_eq!(cover_key("s", "u"), cover_key("s", "u"));
        assert_ne!(cover_key("a", "u"), cover_key("b", "u"));
        assert_ne!(cover_key("s", "u1"), cover_key("s", "u2"));
        assert!(source_cover_path(Path::new("root"), &cover_key("s", "u")).is_ok());
    }

    #[test]
    fn source_cover_store_writes_atomically_and_reads_back() {
        let root = tempfile::tempdir().unwrap();
        let path = source_cover_path(root.path(), &cover_key("s", "u")).unwrap();
        let image = image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
            8,
            8,
            image::Rgba([1, 2, 3, 255]),
        ));
        let decoded = store_source_cover(&path, &image).unwrap();
        assert_eq!(decoded.size, [8, 8]);
        assert!(path.is_file());
        assert!(!path.with_extension("part").exists());
        assert!(image::load_from_memory(&std::fs::read(&path).unwrap()).is_ok());
    }

    #[test]
    fn cover_fetch_runs_end_to_end_and_caches_on_disk() {
        let root = tempfile::tempdir().unwrap();
        let (base, server) = serve_png(1);
        let engine =
            SourceEngine::load_wasm(source_manifest(), pages_wasm(&[]), EngineOptions::default())
                .unwrap();
        let url = format!("{base}/cover.png");
        let image = fetch_source_cover(root.path(), &engine, "test.source", &url).unwrap();
        assert_eq!(image.size, [4, 4]);
        server.join().unwrap();
        let path = source_cover_path(root.path(), &cover_key("test.source", &url)).unwrap();
        assert!(path.is_file());
        // The second fetch must come from disk: the server only answered once.

        let cached = fetch_source_cover(root.path(), &engine, "test.source", &url).unwrap();
        assert_eq!(cached.size, image.size);
    }

    fn detail_chapter(key: &str, number: f32) -> Chapter {
        Chapter {
            key: key.into(),
            title: Some(key.into()),
            chapter_number: Some(number),
            ..Default::default()
        }
    }

    fn queue_state(
        status: DownloadStatus,
        read_page: Option<usize>,
        read_at: Option<i64>,
    ) -> ChapterQueueState {
        ChapterQueueState {
            status,
            has_archive: status == DownloadStatus::Done,
            done: 10,
            total: 10,
            read_page,
            read_at,
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
    fn set_stream_page_records_the_read_timestamp() {
        let root = tempfile::tempdir().unwrap();
        let mut sources = test_sources(root.path());
        let mut item = task("source.a", "m", "c1");
        item.status = DownloadStatus::Running;
        item.pages_total = 30;
        sources.tasks.push(item);
        let id = identity_id("source.a", "m", "c1");
        sources.set_stream_page(&id, 7);
        let task = sources.tasks.iter().find(|task| task.id == id).unwrap();
        assert_eq!(task.read_page, Some(7));
        assert!(task.read_at.is_some());
    }
    // -----------------------------------------------------------------

    // Scheduled follow checks

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
    // -----------------------------------------------------------------

    // Follow covers on the shared source-covers cache

    // -----------------------------------------------------------------
    #[test]
    fn follow_cover_uses_source_cover_key_and_legacy_files_still_read() {
        let root = tempfile::tempdir().unwrap();
        // The same URL must resolve to the same `.img` file as the grid.
        let url = "https://img.example/cover/1.jpg";
        let key = cover_key("source.a", url);
        assert_eq!(key.len(), 16);
        let path = source_cover_path(root.path(), &key).unwrap();
        assert_eq!(path.parent().unwrap().file_name().unwrap(), "source-covers");
        // A pre-unification `follow-covers` file still paints — compat read

        // keeps working while the pipeline only writes to `source-covers/`.
        let mut sources = test_sources(root.path());
        let snapshot = "a1b2c3d4e5f60708".to_string();
        let legacy = follow_cover_path(root.path(), &snapshot).unwrap();
        std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        let png = {
            let img = image::RgbaImage::from_pixel(4, 6, image::Rgba([10, 20, 30, 255]));
            let mut out = std::io::Cursor::new(Vec::new());
            img.write_to(&mut out, image::ImageFormat::Png).unwrap();
            out.into_inner()
        };
        std::fs::write(&legacy, &png).unwrap();
        let texture = sources.cover_texture(&Context::default(), &snapshot);
        assert!(texture.is_some(), "legacy follow cover still renders");
        assert!(legacy.is_file(), "legacy file is left in place");
    }
    #[test]
    fn follow_cover_url_resolves_from_the_manga_snapshot_once() {
        let root = tempfile::tempdir().unwrap();
        let mut sources = test_sources(root.path());
        let mut book = manga("source.a", "m");
        book.cover = Some("https://img.example/c.jpg".into());
        sources.chapters = vec![Chapter {
            key: "c1".into(),
            ..Default::default()
        }];
        sources.toggle_follow(&book);
        let url = sources.follow_cover_url(0);
        assert_eq!(url.as_deref(), Some("https://img.example/c.jpg"));
        // Second call comes from the cache, not the file — deleting the

        // snapshot does not change the resolved value.
        let snapshot = sources.follows[0].snapshot.clone();
        std::fs::remove_file(manga_snapshot_path(root.path(), &snapshot).unwrap()).unwrap();
        assert_eq!(
            sources.follow_cover_url(0).as_deref(),
            Some("https://img.example/c.jpg")
        );
        // And the cache key matches what plan_covers would queue.
        assert_eq!(
            cover_key("source.a", "https://img.example/c.jpg"),
            cover_key(&sources.follows[0].source_id, &url.unwrap()),
        );
    }

    struct FakeEngine<F>(F);
    impl<F: Fn(&str) -> Vec<Manga> + Send + Sync> GlobalEngine for FakeEngine<F> {
        fn search(&self, query: &str) -> Result<Vec<Manga>, String> {
            Ok((self.0)(query))
        }
        fn chapters(&self, _manga: Manga) -> Result<Vec<Chapter>, String> {
            Ok(Vec::new())
        }
    }
    /// Test-side view of a `GlobalEvent::Search` the way the old struct read.
    fn search_event(
        event: &GlobalEvent,
    ) -> Option<(u64, &str, &Result<Vec<Manga>, String>)> {
        match event {
            GlobalEvent::Search {
                generation,
                source,
                result,
            } => Some((*generation, source.as_str(), result)),
            _ => None,
        }
    }

    #[test]
    fn global_worker_never_exceeds_the_concurrency_cap() {
        let running = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let (run, pk) = (running.clone(), peak.clone());
        let (cmd_tx, cmd_rx) = mpsc::channel::<GlobalCmd>();
        let (event_tx, event_rx) = mpsc::channel::<GlobalEvent>();
        let stale: Arc<std::sync::Mutex<HashSet<String>>> = Arc::new(std::sync::Mutex::new(HashSet::new()));
        let ctx = Context::default();
        let handle = thread::spawn(move || {
            global_search_run(
                cmd_rx,
                event_tx,
                move |source| {
                    if source == "err" {
                        return Err("boom".into());
                    }
                    let (run, pk) = (run.clone(), pk.clone());
                    Ok(Arc::new(FakeEngine(move |_: &str| {
                        let now = run.fetch_add(1, Ordering::SeqCst) + 1;
                        pk.fetch_max(now, Ordering::SeqCst);
                        thread::sleep(std::time::Duration::from_millis(30));
                        run.fetch_sub(1, Ordering::SeqCst);
                        vec![Manga::default()]
                    })) as Arc<dyn GlobalEngine>)
                },
                std::time::Duration::from_secs(5),
                stale,
                ctx,
            );
        });
        let sources: Vec<String> =
            (0..9).map(|i| format!("s{i}")).chain(["err".into()]).collect();
        let count = sources.len();
        cmd_tx
            .send(GlobalCmd::Search {
                generation: 1,
                query: "q".into(),
                sources,
            })
            .unwrap();
        drop(cmd_tx);
        handle.join().unwrap();
        assert!(peak.load(Ordering::SeqCst) <= GLOBAL_SEARCH_LIMIT);
        let events: Vec<GlobalEvent> = event_rx.iter().collect();
        assert_eq!(events.len(), count);
        let (.., result) = events
            .iter()
            .find_map(|event| {
                let (event_gen, source, result) = search_event(event)?;
                (source == "err" && event_gen == 1).then_some((event_gen, source, result))
            })
            .expect("load failures still produce an event");
        assert_eq!(result.as_ref().err().unwrap(), "boom");
    }

    #[test]
    fn global_worker_times_out_and_discards_the_late_result() {
        let (cmd_tx, cmd_rx) = mpsc::channel::<GlobalCmd>();
        let (event_tx, event_rx) = mpsc::channel::<GlobalEvent>();
        let stale: Arc<std::sync::Mutex<HashSet<String>>> = Arc::new(std::sync::Mutex::new(HashSet::new()));
        let ctx = Context::default();
        let handle = thread::spawn(move || {
            global_search_run(
                cmd_rx,
                event_tx,
                |source| {
                    if source == "hang" {
                        Ok(Arc::new(FakeEngine(|_: &str| {
                            thread::sleep(std::time::Duration::from_secs(2));
                            vec![Manga::default()]
                        })) as Arc<dyn GlobalEngine>)
                    } else {
                        Ok(Arc::new(FakeEngine(|_: &str| {
                            vec![Manga::default()]
                        })) as Arc<dyn GlobalEngine>)
                    }
                },
                std::time::Duration::from_millis(80),
                stale,
                ctx,
            );
        });
        cmd_tx
            .send(GlobalCmd::Search {
                generation: 1,
                query: "q".into(),
                sources: vec!["hang".into(), "fast".into()],
            })
            .unwrap();
        // Wait for the timeout + fast result while the hung thread still runs.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut events = Vec::new();
        while events.len() < 2 && std::time::Instant::now() < deadline {
            if let Ok(event) =
                event_rx.recv_timeout(std::time::Duration::from_millis(200))
            {
                events.push(event);
            }
        }
        let result = events
            .iter()
            .find_map(|event| {
                let (_, source, result) = search_event(event)?;
                (source == "hang").then_some(result)
            })
            .expect("hung source reports a timeout");
        assert_eq!(result.as_ref().err().unwrap(), "超时");
        // A new generation starts while `hang` is still sleeping: the worker
        // is not joining it, so the next search proceeds immediately.
        cmd_tx
            .send(GlobalCmd::Search {
                generation: 2,
                query: "q".into(),
                sources: vec!["fast".into()],
            })
            .unwrap();
        drop(cmd_tx);
        handle.join().unwrap();
        let events: Vec<GlobalEvent> = event_rx.iter().collect();
        assert!(events.iter().any(|event| {
            matches!(search_event(event), Some((2, "fast", result)) if result.is_ok())
        }));
    }

    #[test]
    fn global_worker_discards_stale_generation_events() {
        let root = tempfile::tempdir().unwrap();
        let mut sources = test_sources(root.path());
        sources.global_gen = 2;
        sources.global_groups = vec![GlobalGroup {
            source: "x".into(),
            state: GlobalState::Searching,
        }];
        // Same generation check poll() applies.
        let stale_event = GlobalEvent::Search {
            generation: 1,
            source: "x".into(),
            result: Ok(vec![Manga::default()]),
        };
        let (event_gen, source, _) = search_event(&stale_event).unwrap();
        assert_ne!(event_gen, sources.global_gen);
        if event_gen == sources.global_gen {
            if let Some(group) = sources
                .global_groups
                .iter_mut()
                .find(|group| group.source == source)
            {
                group.state = GlobalState::Done(vec![]);
            }
        }
        assert!(matches!(
            sources.global_groups[0].state,
            GlobalState::Searching
        ));
        // Same-source event at the current generation updates the group.
        let fresh = GlobalEvent::Search {
            generation: 2,
            source: "x".into(),
            result: Err("超时".into()),
        };
        let (event_gen, source, _) = search_event(&fresh).unwrap();
        if event_gen == sources.global_gen {
            if let Some(group) = sources
                .global_groups
                .iter_mut()
                .find(|group| group.source == source)
            {
                group.state = GlobalState::Failed("超时".into());
            }
        }
        assert!(matches!(
            &sources.global_groups[0].state,
            GlobalState::Failed(reason) if reason == "超时"
        ));
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

    /// One `task-*` staging dir with payload bytes.
    fn stage_task(root: &Path, id: &str, bytes: usize) {
        let dir = work_root(root).join(format!("task-{id}"));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("page-1.img"), vec![7u8; bytes]).unwrap();
    }
    #[test]
    fn storage_report_splits_active_and_finished_staging() {
        let root = tempfile::tempdir().unwrap();
        stage_task(root.path(), "aaaaaaaaaaaaaaaa", 100);
        stage_task(root.path(), "bbbbbbbbbbbbbbbb", 200);
        stage_task(root.path(), "cccccccccccccccc", 50);
        std::fs::create_dir_all(root.path().join("downloads")).unwrap();
        std::fs::write(root.path().join("downloads/done.zip"), vec![9u8; 30]).unwrap();
        std::fs::create_dir_all(root.path().join("source-covers")).unwrap();
        std::fs::write(root.path().join("source-covers/a.img"), vec![1u8; 20]).unwrap();

        let all: HashSet<String> = ["aaaaaaaaaaaaaaaa", "bbbbbbbbbbbbbbbb"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let finished: HashSet<String> = ["bbbbbbbbbbbbbbbb".to_string()].into_iter().collect();
        let report = source_storage(root.path(), &all, &finished);
        assert_eq!(report.staging_active, 100);
        // ccccc… is an orphan dir — finished bucket, cleanable.
        assert_eq!(report.staging_done, 250);
        assert_eq!(report.staging_done_count, 2);
        assert_eq!(report.downloads, 30);
        assert_eq!(report.covers, 20);
    }
    #[test]
    fn clean_finished_tasks_never_touches_active_or_queued() {
        let root = tempfile::tempdir().unwrap();
        stage_task(root.path(), "aaaaaaaaaaaaaaaa", 10);
        stage_task(root.path(), "bbbbbbbbbbbbbbbb", 10);
        stage_task(root.path(), "dddddddddddddddd", 10);
        let all: HashSet<String> = ["aaaaaaaaaaaaaaaa", "bbbbbbbbbbbbbbbb", "dddddddddddddddd"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let finished: HashSet<String> = ["bbbbbbbbbbbbbbbb".to_string()].into_iter().collect();

        assert_eq!(
            clean_finished_tasks(root.path(), &all, &finished).unwrap(),
            1
        );
        assert!(!work_root(root.path())
            .join("task-bbbbbbbbbbbbbbbb")
            .exists());
        // Queued/in-progress dirs and everything else are untouched.
        assert!(work_root(root.path())
            .join("task-aaaaaaaaaaaaaaaa")
            .exists());
        assert!(work_root(root.path())
            .join("task-dddddddddddddddd")
            .exists());
    }
    #[test]
    fn clean_finished_tasks_removes_orphan_dirs_but_not_snapshots() {
        let root = tempfile::tempdir().unwrap();
        stage_task(root.path(), "eeeeeeeeeeeeeeee", 10);
        let snapshots = snapshot_root(root.path());
        std::fs::create_dir_all(&snapshots).unwrap();
        std::fs::write(snapshots.join("a".repeat(16) + ".manga"), b"x").unwrap();
        let all = HashSet::new();
        let finished = HashSet::new();
        assert_eq!(
            clean_finished_tasks(root.path(), &all, &finished).unwrap(),
            1
        );
        assert!(snapshots.join("a".repeat(16) + ".manga").exists());
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

    #[test]
    fn bookmarked_online_chapter_reopens_its_stream_task() {
        let root = tempfile::tempdir().unwrap();
        let mut sources = test_sources(root.path());
        let mut download = task("source.a", "m1", "c1");
        download.id = "aaaaaaaaaaaaaaaa".into();
        sources.tasks.push(download);
        assert!(!sources.open_bookmarked("source.a", "m1", "missing"));
        assert!(sources.open_bookmarked("source.a", "m1", "c1"));
        let stream = sources.take_stream().unwrap();
        assert_eq!(stream.id, "aaaaaaaaaaaaaaaa");
        assert_eq!(stream.metadata.chapter_id, "c1");
    }

    fn numbered(key: &str, number: f32) -> Chapter {
        Chapter {
            key: key.into(),
            title: Some(format!("第 {number} 话")),
            chapter_number: Some(number),
            ..Default::default()
        }
    }

    fn update(source: &str, manga: &str, chapters: Vec<Chapter>) -> UpdateEvent {
        UpdateEvent {
            source: source.into(),
            manga: manga.into(),
            chapters: Some(Ok(chapters)),
            cover: None,
        }
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

    fn installed(id: &str, version: i64) -> InstalledSource {
        InstalledSource {
            manifest: SourceManifest {
                id: id.into(),
                name: id.into(),
                version,
                alt_names: Vec::new(),
                languages: vec!["zh".into()],
                urls: vec![format!("https://{id}.example")],
                content_rating: 0,
                listings: Vec::new(),
                allows_base_url_select: false,
                hides_filters_while_searching: false,
            },
            directory: PathBuf::from(id),
            wasm_path: PathBuf::from(id).join("main.wasm"),
            settings_path: None,
            filters_path: None,
            icon_path: None,
        }
    }

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

    #[test]
    fn uninstall_clears_settings_cookies_engines_and_test_state_only() {
        let root = tempfile::tempdir().unwrap();
        let mut sources = test_sources(root.path());
        let source = installed("source.a", 1);
        sources.installed = vec![source.clone()];
        // Per-source settings + web cookies tied to its host.
        sources.source_settings.insert(
            "source.a".into(),
            [("key".into(), SettingValue::Str("v".into()))].into_iter().collect(),
        );
        sources.web_cookies.insert(
            "https://source.a.example/login".into(),
            WebSiteCookies {
                cookies: vec![("s".into(), "v".into())],
                user_agent: None,
            },
        );
        sources.web_cookies.insert(
            "https://other.example".into(),
            WebSiteCookies {
                cookies: vec![("s".into(), "v".into())],
                user_agent: None,
            },
        );
        sources
            .repo_test_results
            .insert("source.a".into(), "可用 · 10 ms".into());
        sources.repo_testing.insert(
            "source.a".into(),
            std::time::Instant::now(),
        );
        sources.repo_test_queue.push_back("source.a".into());
        sources.follows.push(Follow {
            source_id: "source.a".into(),
            manga_id: "m".into(),
            title: "保留的追更".into(),
            ..Default::default()
        });
        sources.tasks.push(task("source.a", "m", "c1"));

        sources.ask_uninstall(source);
        assert!(sources.uninstall.is_some());
        sources.confirm_uninstall();
        assert!(sources.uninstall.is_none());
        assert!(sources.source_settings.get("source.a").is_none());
        assert!(
            sources.web_cookies.get("https://source.a.example/login").is_none(),
            "cookies tied to the source's hosts go with it"
        );
        assert!(sources.web_cookies.get("https://other.example").is_some());
        assert!(
            sources
                .stale_engines
                .lock()
                .unwrap()
                .contains("source.a"),
            "the engine is unloaded on every worker"
        );
        assert!(sources.repo_test_results.is_empty());
        assert!(sources.repo_testing.is_empty());
        assert!(sources.repo_test_queue.is_empty());
        // Downloads and follows survive the removal, marked 书源已卸载.
        assert_eq!(sources.follows.len(), 1);
        assert!(sources.follows[0].source_removed);
        assert_eq!(sources.tasks.len(), 1);
        // Settings and cookies were persisted, not just mutated in memory.
        assert!(sources.source_settings.is_empty());
        let saved: serde_json::Value = serde_json::from_slice(
            &std::fs::read(root.path().join("source-settings.json")).unwrap(),
        )
        .unwrap();
        assert!(!saved.to_string().contains("source.a"));
    }

    #[test]
    fn repo_tests_queue_at_four_and_timeout_reports_chinese_reason() {
        let root = tempfile::tempdir().unwrap();
        let mut sources = test_sources(root.path());
        // Four running → the fifth queues.
        for index in 0..4 {
            sources
                .repo_testing
                .insert(format!("s{index}"), std::time::Instant::now());
        }
        sources.test_source("s4");
        assert_eq!(sources.repo_testing.len(), 4);
        assert_eq!(sources.repo_test_queue.front().map(String::as_str), Some("s4"));
        // A finished test frees a slot and stores its result.
        let (tx, rx) = mpsc::channel();
        sources.test_tx = tx;
        sources.test_rx = rx;
        sources
            .repo_testing
            .insert("s4".into(), std::time::Instant::now());
        sources.repo_test_queue.clear();
        let send = sources.test_tx.clone();
        send.send(("s0".into(), Ok(820u64))).unwrap();
        sources.drain_repo_tests();
        assert_eq!(
            sources.repo_test_results.get("s0").map(String::as_str),
            Some("可用 · 820 ms")
        );
        // A wedged test is marked timed out after 15 s and dropped.
        sources.repo_testing.insert(
            "wedged".into(),
            std::time::Instant::now() - std::time::Duration::from_secs(20),
        );
        sources.pump_repo_tests();
        assert_eq!(
            sources.repo_test_results.get("wedged").map(String::as_str),
            Some("测试超时（15 秒）")
        );
    }

    #[test]
    fn update_badges_and_update_all_use_the_highest_repo_version() {
        let root = tempfile::tempdir().unwrap();
        let mut sources = test_sources(root.path());
        sources.installed = vec![installed("s1", 1), installed("s2", 5)];
        sources.repos.merge_index(
            "https://repo",
            &RepositoryIndex {
                name: None,
                website: None,
                index_url: "https://repo".into(),
                sources: vec![
                    RepositorySource {
                        id: "s1".into(),
                        name: "S1".into(),
                        version: 3,
                        download_url: "https://repo/s1.aix".into(),
                        icon_url: None,
                        languages: vec!["zh".into()],
                        content_rating: 0,
                        base_url: None,
                    },
                    RepositorySource {
                        id: "s2".into(),
                        name: "S2".into(),
                        version: 5,
                        download_url: "https://repo/s2.aix".into(),
                        icon_url: None,
                        languages: vec!["en".into()],
                        content_rating: 0,
                        base_url: None,
                    },
                ],
            },
            1,
        );
        assert_eq!(sources.updatable_count(), 1);
        assert_eq!(sources.updatable().get("s1"), Some(&3));
        assert!(sources.updatable().get("s2").is_none());
    }
}


impl Sources {
    /// Fresh-chapter items collected since the last drain — the shell turns
    /// them into system notifications (foregrounded frames only mark them).
    pub(crate) fn take_notify_updates(&mut self) -> Vec<crate::notify::FreshUpdate> {
        std::mem::take(&mut self.notify_pending)
    }
}

/// Online chapter read state for cloud sync — one record per task that has a
/// `read_at` (i.e. the user actually opened it).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OnlineReadState {
    pub source_id: String,
    pub manga_id: String,
    pub chapter_id: String,
    pub read_page: usize,
    pub total: usize,
    pub read_at: i64,
}

impl Sources {
    /// Every task's latest online read position — the cloud snapshot's
    /// `online` group.
    pub(crate) fn online_read_state(&self) -> Vec<OnlineReadState> {
        self.tasks
            .iter()
            .filter_map(|task| {
                Some(OnlineReadState {
                    source_id: task.source_id.clone(),
                    manga_id: task.manga_id.clone(),
                    chapter_id: task.chapter_id.clone(),
                    read_page: task.read_page?,
                    total: task.pages_total,
                    read_at: task.read_at?,
                })
            })
            .collect()
    }

    /// Applies remote online read state onto matching tasks; returns how
    /// many tasks were moved. `skip` excludes the work currently open in
    /// the reader so sync never yanks a live position.
    pub(crate) fn apply_online_state(
        &mut self,
        states: &[OnlineReadState],
        skip: Option<(&str, &str)>,
    ) -> usize {
        let mut changed = 0;
        for state in states {
            if skip.is_some_and(|(s, m)| s == state.source_id && m == state.manga_id) {
                continue;
            }
            let Some(task) = self.tasks.iter_mut().find(|task| {
                task.source_id == state.source_id
                    && task.manga_id == state.manga_id
                    && task.chapter_id == state.chapter_id
            }) else {
                continue;
            };
            if state.read_at > task.read_at.unwrap_or(0) {
                task.read_page = Some(state.read_page);
                task.read_at = Some(state.read_at);
                changed += 1;
            }
        }
        if changed > 0 {
            let _ = self.persist();
        }
        changed
    }

    /// Replaces the follows list wholesale after a cloud merge and persists.
    pub(crate) fn apply_synced_follows(&mut self, follows: Vec<Follow>) {
        self.follows = follows;
        self.save_follows();
    }

    /// `(id, version)` per installed source — the cloud snapshot's `sources`
    /// group, so another device can offer 一键安装.
    pub(crate) fn installed_source_meta(&self) -> Vec<(String, i64)> {
        self.installed
            .iter()
            .map(|source| (source.manifest.id.clone(), source.manifest.version))
            .collect()
    }

    /// Repo-list urls for the cloud `sources` group entries.
    pub(crate) fn repo_urls(&self) -> Vec<String> {
        self.repos.urls.clone()
    }

    /// Install one remote device already had — resolves the source id in the
    /// cached repository index and queues the normal install task. Returns
    /// true when the id was found.
    pub(crate) fn install_cloud_source(&mut self, id: &str) -> bool {
        let Some(repo) = &self.repository else {
            return false;
        };
        let Some(entry) = repo
            .sources
            .iter()
            .find(|source| &source.id == id)
            .cloned()
        else {
            return false;
        };
        self.submit(Task::Install(entry));
        true
    }
}
