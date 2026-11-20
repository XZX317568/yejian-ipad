use crate::{
    design,
    follows::{self, Follow},
    source_filters::FilterEditor,
};
use anyhow::Context as _;
use egui::*;
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashSet, VecDeque},
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
        .frame_when_inactive(false)
        .min_size(vec2(44., 44.))
        .wrap()
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
        painter.rect_filled(rect, 8., palette.accent_soft);
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
}
enum Reply {
    Installed {
        list: Vec<InstalledSource>,
        updated: Option<String>,
    },
    Repository(RepositoryIndex),
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
}

#[derive(Default, Clone)]
struct BrowseSession {
    query: String,
    filters: Vec<FilterValue>,
    listing: Option<String>,
}

fn chapter_name(chapter: &Chapter) -> String {
    chapter.title.clone().filter(|title| !title.trim().is_empty()).unwrap_or_else(|| {
        chapter.chapter_number
            .map(|number| format!("第 {number} 话"))
            .unwrap_or_else(|| chapter.key.clone())
    })
}

/// Use one comparable volume group for chapters without explicit volumes,
/// matching the library's reading order (including decimal extra chapters).
fn chapter_list(chapters: &[Chapter], query: &str, newest_first: bool) -> Vec<Chapter> {
    let first_volume = chapters.iter()
        .filter_map(|chapter| chapter.volume_number.filter(|number| number.is_finite()))
        .min_by(f32::total_cmp)
        .unwrap_or(0.);
    let needle = query.trim().to_lowercase();
    let mut visible: Vec<_> = chapters.iter().filter_map(|chapter| {
        let name = chapter_name(chapter);
        let number = chapter.chapter_number.filter(|number| number.is_finite());
        if !needle.is_empty() && !name.to_lowercase().contains(&needle)
            && !number.is_some_and(|number| number.to_string().contains(&needle))
        {
            return None;
        }
        let volume = chapter.volume_number
            .filter(|number| number.is_finite()).unwrap_or(first_volume);
        Some((chapter, name, volume, number))
    }).collect();
    visible.sort_by(|left, right| {
        left.2.total_cmp(&right.2).then_with(|| {
            match (left.3, right.3) {
                (Some(left), Some(right)) => left.total_cmp(&right),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => std::cmp::Ordering::Equal,
            }
        }).then_with(|| yejian_core::natural_cmp(&left.1, &right.1))
          .then_with(|| left.0.key.cmp(&right.0.key))
    });
    if newest_first {
        visible.reverse();
    }
    visible.into_iter().map(|(chapter, _, _, _)| chapter.clone()).collect()
}

/// One finished chapter ready for `Command::ImportChapter`.
///
/// `id` is the queue task id: after `import` and `link_chapter` both succeed
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
            chapter_number: chapter.chapter_number.map(f64::from),
            volume_number: chapter.volume_number.map(f64::from),
            chapter_url: chapter.url.clone(),
            chapter_language: chapter.language.clone(),
            manga_snapshot: None,
            chapter_snapshot: Some(chapter.to_postcard_bytes()),
            read_page: None,
            status: DownloadStatus::Queued,
            error: None,
            pages_done: 0,
            pages_total: 0,
            pages_signature: None,
            cbz: None,
            linked: false,
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
///
/// Every entry keeps its own `source_key`, and results are only accepted for
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

fn manga_snapshot_path(root: &Path, key: &str) -> anyhow::Result<PathBuf> {
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
                    if failed.load(Ordering::SeqCst)
                        || stop.load(Ordering::SeqCst) != STOP_NONE
                    {
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
                        drop(reader.decode().with_context(|| {
                            format!("第 {number} 页图片不完整或无法解码")
                        })?);
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
    checking: usize,
    auto_checked: bool,
    up_tx: mpsc::Sender<Vec<(String, Manga)>>,
    up_rx: mpsc::Receiver<UpdateEvent>,
    /// Decoded follow covers by snapshot key; `None` means not on disk yet.
    covers: std::collections::HashMap<String, Option<TextureHandle>>,
    settings_open: bool,
    settings_changed: bool,
    /// User choices per source, persisted in `source-settings.json`.
    source_settings: SettingsStore,
    /// Sources whose loaded engines must restart to read changed settings.
    stale_engines: Arc<std::sync::Mutex<HashSet<String>>>,
    /// Origin a Cloudflare challenge blocked, until the user verifies.
    challenge: Option<String>,
    /// Source and optional login key of the open in-app browser page.
    web_purpose: Option<(String, Option<String>)>,
    login_user: String,
    login_pass: String,
    /// Cookies from verification/login pages by URL, in `web-cookies.json`.
    web_cookies: std::collections::BTreeMap<String, Vec<(String, String)>>,
}

const WEB_COOKIES_FILE: &str = "web-cookies.json";

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
fn setting_row(ui: &mut Ui, item: &SettingItem, current: Option<SettingValue>) -> Option<SettingValue> {
    let mut changed = None;
    let label = |ui: &mut Ui| {
        ui.add(Label::new(RichText::new(&item.title).strong()).wrap());
    };
    match &item.kind {
        SettingKind::Login { .. } => {}
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
                    if ui.selectable_label(selected == Some(value.as_str()), title).clicked() {
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
                    if ui.selectable_label(selected == index as i32, title).clicked() {
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
                    let shown = if item.key == "languages" { language_label(title).to_string() } else { title.clone() };
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
                if ui.add_enabled(value > *min, Button::new("−").min_size(vec2(40., 36.))).clicked() {
                    next = Some((value - step).max(*min));
                }
                ui.label(if integral { format!("{}", value as i64) } else { format!("{value:.2}") });
                if ui.add_enabled(value < *max, Button::new("+").min_size(vec2(40., 36.))).clicked() {
                    next = Some((value + step).min(*max));
                }
                changed = next.map(|v| {
                    if integral { SettingValue::Int(v as i32) } else { SettingValue::Float(v as f32) }
                });
            });
        }
    }
    changed
}

fn url_host(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host = authority.rsplit_once('@').map_or(authority, |(_, host)| host);
    host.split(':').next().unwrap_or("").to_ascii_lowercase()
}

fn follow_cover_path(root: &Path, snapshot: &str) -> anyhow::Result<PathBuf> {
    anyhow::ensure!(valid_task_id(snapshot), "非法的封面 id：{snapshot:?}");
    Ok(root.join("follow-covers").join(format!("{snapshot}.img")))
}

/// Downloads a follow's cover once, through the source's image request hook.
fn fetch_follow_cover(root: &Path, engine: &SourceEngine, manga: &Manga) -> anyhow::Result<()> {
    let Some(url) = manga.cover.as_deref().filter(|url| !url.is_empty()) else {
        return Ok(());
    };
    let path = follow_cover_path(root, &manga_snapshot_key(&manga.source_key, &manga.key))?;
    if path.is_file() {
        return Ok(());
    }
    let handler = engine.request_handler();
    let bytes = match pollster::block_on(engine.image_request(url, None))? {
        Some(req) => request(&handler, req.url, req.headers, req.body)?,
        None => request(&handler, url.to_string(), vec![], None)?,
    };
    image::load_from_memory(&bytes)?;
    std::fs::create_dir_all(path.parent().unwrap())?;
    let part = path.with_extension("part");
    std::fs::write(&part, &bytes)?;
    std::fs::rename(&part, &path)?;
    Ok(())
}

/// Result of checking one followed manga for new chapters.
struct UpdateEvent {
    source: String,
    manga: String,
    /// `None`: only the cover arrived, after the batch's chapter results.
    chapters: Option<Result<Vec<String>, String>>,
}

impl Sources {
    #[cfg(debug_assertions)]
    pub(crate) fn preview_community(&mut self) {
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

    #[cfg(debug_assertions)]
    pub(crate) fn browser_report(&self) -> serde_json::Value {
        serde_json::json!({
            "source":self.selected_source,"loaded":self.metadata_loaded,
            "filter_count":self.filters.len(),"listing_count":self.listings.len(),
            "filter_editor":self.filter_editor.is_some(),"busy":self.busy,
            "results":self.results.entries.iter().map(|manga|manga.title.as_str()).collect::<Vec<_>>(),
            "applied":self.applied_filters,
        })
    }

    #[cfg(debug_assertions)]
    pub(crate) fn preview_installed_source(&mut self, id: &str) -> anyhow::Result<()> {
        self.installed = SourceCatalog::new(self.root.join("sources")).list()?;
        anyhow::ensure!(
            self.installed.iter().any(|source| source.manifest.id == id),
            "验收书源未安装：{id}"
        );
        self.preview_mode = Some("filters".into());
        self.select_source(id);
        Ok(())
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
        let web_cookies: std::collections::BTreeMap<String, Vec<(String, String)>> =
            std::fs::read(root.join(WEB_COOKIES_FILE))
                .ok()
                .and_then(|bytes| serde_json::from_slice(&bytes).ok())
                .unwrap_or_default();
        for (url, cookies) in &web_cookies {
            add_web_cookies(url, cookies);
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
                        && !matches!(&task, Task::Install(_))
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
                            Task::Login { source, key, credentials } => {
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
                                        let ok = pollster::block_on(engine.web_login(&key, cookies))?;
                                        ("网页登录".to_string(), ok)
                                    }
                                };
                                Reply::Login { key, user, ok, web }
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
                        let chapters = (|| -> anyhow::Result<Vec<String>> {
                            if !engines.contains_key(&source) {
                                engines.insert(source.clone(), SourceEngine::load(&catalog, &source)?);
                            }
                            let list =
                                pollster::block_on(engines[&source].chapters_with(manga.clone()))?;
                            Ok(list.into_iter().map(|chapter| chapter.key).collect())
                        })()
                        .map_err(|error| format!("{error:#}"));
                        let event = UpdateEvent { source, manga: manga.key, chapters: Some(chapters) };
                        if up_event_tx.send(event).is_err() {
                            return;
                        }
                        up_ctx.request_repaint();
                    }
                    // Covers come last so a slow image host never delays
                    // new-chapter results; a missing cover is not an error.
                    for (source, manga) in batch {
                        let Some(engine) = engines.get(&source) else { continue };
                        if fetch_follow_cover(&up_root, engine, &manga).is_ok() {
                            let event = UpdateEvent { source, manga: manga.key, chapters: None };
                            if up_event_tx.send(event).is_err() {
                                return;
                            }
                            up_ctx.request_repaint();
                        }
                    }
                }
            })
            .expect("Unable to start update worker");

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
            chapters_newest_first: false,
            open_chapter: None,
            library_chapters: Default::default(),
            selected_source: String::new(),
            selected_manga: None,
            url: String::new(),
            query: String::new(),
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
            checking: 0,
            auto_checked: false,
            up_tx,
            up_rx,
            covers: Default::default(),
            settings_open: false,
            settings_changed: false,
            source_settings,
            stale_engines,
            challenge: None,
            web_purpose: None,
            login_user: String::new(),
            login_pass: String::new(),
            web_cookies,
        };
        let mut sources = sources;
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
        self.follows.iter().filter(|follow| !follow.fresh.is_empty()).count()
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
        let keys: Vec<String> = self.chapters.iter().map(|chapter| chapter.key.clone()).collect();
        follow.mark_seen(&keys);
        follow.checked_at = Some(follows::now());
        self.follows.insert(0, follow);
        self.status = "已加入追更，有新章节会在书源页顶部提示".into();
        self.save_follows();
        // Fetches the cover right away; the chapter list is already known.
        self.check_follows(&[0]);
    }

    fn follow_manga(&self, follow: &Follow) -> anyhow::Result<Manga> {
        let bytes = std::fs::read(manga_snapshot_path(&self.root, &follow.snapshot)?)?;
        let mut manga = Manga::from_postcard_bytes(&bytes)
            .map_err(|error| anyhow::anyhow!("漫画快照已损坏：{error}"))?;
        manga.source_key = follow.source_id.clone();
        Ok(manga)
    }

    fn open_follow(&mut self, index: usize) {
        let Some(follow) = self.follows.get(index).cloned() else {
            return;
        };
        if !self.installed.iter().any(|source| source.manifest.id == follow.source_id) {
            self.status = format!("书源 {} 未安装，无法打开《{}》", follow.source_id, follow.title);
            return;
        }
        let manga = match self.follow_manga(&follow) {
            Ok(manga) => manga,
            Err(error) => {
                self.status = format!("无法打开《{}》：{error:#}", follow.title);
                return;
            }
        };
        if self.selected_source == follow.source_id {
            self.open_manga(manga);
        } else {
            self.select_source(&follow.source_id);
            self.pending_open = Some(manga);
        }
    }

    fn cover_texture(&mut self, ctx: &Context, snapshot: &str) -> Option<TextureHandle> {
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

    /// Asks every followed manga's source for its current chapter list.
    pub fn check_updates(&mut self) {
        if self.checking > 0 {
            return;
        }
        let all: Vec<usize> = (0..self.follows.len()).collect();
        self.check_follows(&all);
    }

    fn check_follows(&mut self, indices: &[usize]) {
        let mut batch = Vec::new();
        for follow in indices.iter().filter_map(|&index| self.follows.get(index)) {
            match self.follow_manga(follow) {
                Ok(manga) => batch.push((follow.source_id.clone(), manga)),
                Err(error) => self.status = format!("《{}》无法检查更新：{error:#}", follow.title),
            }
        }
        if batch.is_empty() {
            return;
        }
        let count = batch.len();
        if self.up_tx.send(batch).is_ok() {
            self.checking += count;
        } else {
            self.status = "更新检查线程已停止，请重新打开应用".into();
        }
    }

    fn handle_update(&mut self, event: UpdateEvent) {
        let Some(chapters) = event.chapters else {
            if let Some(index) = self.follow_index(&event.source, &event.manga) {
                let snapshot = self.follows[index].snapshot.clone();
                if matches!(self.covers.get(&snapshot), Some(None)) {
                    self.covers.remove(&snapshot);
                }
            }
            return;
        };
        self.checking = self.checking.saturating_sub(1);
        if let Some(index) = self.follow_index(&event.source, &event.manga) {
            let follow = &mut self.follows[index];
            match chapters {
                Ok(keys) => {
                    follow.apply_check(&keys, follows::now());
                }
                Err(error) => {
                    follow.error = Some(error);
                    follow.checked_at = Some(follows::now());
                }
            }
        }
        if self.checking == 0 {
            let fresh = self.fresh_follows();
            self.status = if fresh > 0 {
                format!("{fresh} 部追更有新章节")
            } else {
                "追更已是最新".into()
            };
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
                quiet_action(if checking { "正在检查更新…" } else { "检查更新" }),
            )
            .clicked()
        {
            self.check_updates();
        }
        let mut order: Vec<usize> = (0..self.follows.len()).collect();
        order.sort_by_key(|&index| std::cmp::Reverse(self.follows[index].fresh.len()));
        let mut open = None;
        for index in order {
            let follow = &self.follows[index];
            let source = self
                .installed
                .iter()
                .find(|source| source.manifest.id == follow.source_id)
                .map_or_else(|| follow.source_id.clone(), |source| source.manifest.name.clone());
            let detail = if let Some(error) = &follow.error {
                format!("{source} · 检查失败：{error}")
            } else if !follow.fresh.is_empty() {
                format!("{source} · {} 话更新", follow.fresh.len())
            } else {
                format!("{source} · 共 {} 话", follow.total)
            };
            let snapshot = follow.snapshot.clone();
            let top = ui.cursor().top();
            let left = ui.cursor().left();
            if source_item(ui, ("follow", index), &follow.title, &detail, true, true) {
                open = Some(index);
            }
            let bottom = ui.min_rect().bottom();
            if let Some(texture) = self.cover_texture(ui.ctx(), &snapshot) {
                let cover = Rect::from_center_size(
                    pos2(left + 18., (top + bottom) / 2.),
                    vec2(40., 54.),
                );
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
        ui.add_space(20.);
    }

    /// Pops a chapter the user started reading online.
    pub fn take_stream(&mut self) -> Option<StreamChapter> {
        self.stream_request.take()
    }

    pub fn stream_state(&self, id: &str) -> Option<StreamState> {
        let task = self.tasks.iter().find(|task| task.id == id)?;
        let stopped = match task.status {
            DownloadStatus::Failed => Some(
                task.error.clone().unwrap_or_else(|| "下载失败".into()),
            ),
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
        let queued = self.tasks.iter().any(|task| {
            task.id == id && task.status != DownloadStatus::Failed
        });
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
        self.stream_request = Some(StreamChapter { id, metadata, from_end });
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
    fn adjacent_online(&self, source: &str, manga: &str, chapter: &str, delta: i32) -> Option<Chapter> {
        let selected = self.selected_manga.as_ref()?;
        if selected.key != manga || self.source_for(selected) != source {
            return None;
        }
        let chapters = chapter_list(&self.chapters, "", false);
        let index = chapters.iter().position(|item| item.key == chapter)? as i64 + delta as i64;
        usize::try_from(index).ok().and_then(|index| chapters.get(index).cloned())
    }

    /// Chapter key of the neighbour in the source's reading order, so the
    /// reader does not skip a chapter that is not in the library yet.
    pub fn adjacent_online_key(&self, metadata: &ChapterMetadata, delta: i32) -> Option<String> {
        self.adjacent_online(&metadata.source_id, &metadata.manga_id, &metadata.chapter_id, delta)
            .map(|chapter| chapter.key)
    }

    pub fn has_adjacent_online(&self, metadata: &ChapterMetadata, delta: i32) -> bool {
        self.adjacent_online(&metadata.source_id, &metadata.manga_id, &metadata.chapter_id, delta)
            .is_some_and(|chapter| !chapter.locked)
    }

    /// Queues the chapter after `metadata` for download without opening it,
    /// so the reader can keep scrolling straight into it. Returns the stream
    /// task id and its metadata once the chapter is tracked; `None` when the
    /// chapter is unknown, locked, already imported, or cannot be queued
    /// here (its manga detail is not loaded).
    pub fn prepare_next(&mut self, metadata: &ChapterMetadata) -> Option<(String, ChapterMetadata)> {
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
        self.tasks.iter().find(|task| task.id == id).map(|task| task.metadata())
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
                    manga_snapshot_path(&self.root, &manga_snapshot_key(source_id, manga_id)).ok()?;
                let bytes = std::fs::read(path).ok()?;
                Manga::from_postcard_bytes(&bytes).ok().map(|manga| manga.viewer)
            });
        viewer.filter(|viewer| *viewer != Viewer::Unknown)
    }

    /// Opens the previous/next chapter of `metadata`, from the library when it
    /// is already there and online otherwise. Returns false when unknown.
    pub fn open_adjacent_online(&mut self, metadata: &ChapterMetadata, delta: i32) -> bool {
        let Some(chapter) = self.adjacent_online(
            &metadata.source_id, &metadata.manga_id, &metadata.chapter_id, delta,
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
        self.library_chapters = links.iter().map(|link| {
            let metadata = &link.metadata;
            (identity_id(&metadata.source_id, &metadata.manga_id, &metadata.chapter_id),
                metadata.clone())
        }).collect();
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
                self.status.push_str(&format!("；队列状态保存失败：{error:#}"));
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
            self.status.push_str(&format!("；队列状态保存失败：{persist_error:#}"));
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
    /// New search from the text field: page one of this source and query.
    fn start_search(&mut self) {
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
        !chapter.locked && (self.chapter_state(source, manga, &chapter.key).is_none()
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
            if !matches!(enqueue_task(&mut self.tasks, task), EnqueueOutcome::AlreadyDownloaded) {
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
                        _ => format!("加载失败：{error}"),
                    };
                    self.failed = completed;
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
                    self.repository = Some(index);
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
                            self.follows[index].mark_seen(&keys);
                            self.follows[index].title = manga.title.clone();
                            self.save_follows();
                        }
                        self.selected_manga = Some(manga);
                        self.chapters = list;
                        self.status.clear();
                    }
                }
            }
        }
        while let Ok(event) = self.dl_rx.try_recv() {
            self.handle_download_event(event);
        }
        while let Ok(event) = self.up_rx.try_recv() {
            self.handle_update(event);
        }
        if !self.auto_checked && !self.installed.is_empty() {
            self.auto_checked = true;
            self.check_updates();
        }
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
                let name = self
                    .installed
                    .iter()
                    .find(|source| source.manifest.id == self.selected_source)
                    .map(|source| source.manifest.name.as_str())
                    .unwrap_or("");
                ui.add(Label::new(RichText::new(name).strong().color(palette.ink)).wrap());
                if !self.selected_source.is_empty()
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
                if let Some(site) = site.filter(|_| cfg!(target_os = "ios")) {
                    if ui.add(quiet_action("网页验证")).clicked() {
                        self.open_web(&site, None);
                    }
                }
            });
            if self.selected_source.is_empty() {
                return;
            }
            ui.add_space(2.);
            let input = ui.add(
                TextEdit::singleline(&mut self.query)
                    .hint_text("在此书源搜索漫画")
                    .desired_width(ui.available_width()),
            );
            let enter = input.lost_focus() && ui.input(|input| input.key_pressed(Key::Enter));
            ui.horizontal_wrapped(|ui| {
                if ui
                    .add_enabled(
                        self.metadata_loaded,
                        design::primary("搜索", ui.visuals().dark_mode),
                    )
                    .clicked()
                    || enter && self.metadata_loaded
                {
                    self.start_search();
                }
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
            });
            if self.filters_hidden() {
                ui.label("此书源的关键词搜索不使用筛选；清空关键词后可筛选。");
            } else if self.metadata_loaded && self.filters.is_empty() && self.filter_error.is_none()
            {
                ui.label("此书源未提供筛选选项。");
            }
            if let Some(error) = self.filter_error.clone() {
                disclosure(ui, "部分筛选未能加载 · 查看原因", false, |ui| {
                    ui.add(Label::new(error).wrap());
                });
            }
            if let Some(error) = self.listing_error.clone() {
                disclosure(ui, "部分列表未能加载 · 查看原因", false, |ui| {
                    ui.add(Label::new(error).wrap());
                });
            }
            if (self.filter_error.is_some() || self.listing_error.is_some())
                && ui
                    .add_enabled(!self.busy, quiet_action("重新加载书源选项"))
                    .clicked()
            {
                self.submit(Task::Metadata(self.selected_source.clone()));
            }
            if self.listings.len() > 4 {
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
            } else {
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
            if self.listing.is_some() && !self.applied_filters.is_empty() {
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

    /// Opens `url` in the in-app browser for verification (`login` = None)
    /// or a source's web login.
    fn open_web(&mut self, url: &str, login: Option<String>) {
        match yejian_platform::open_web(url, DEFAULT_USER_AGENT) {
            Ok(()) => self.web_purpose = Some((self.selected_source.clone(), login)),
            Err(error) => self.status = format!("无法打开网页：{error:?}"),
        }
    }

    fn finish_web(&mut self, result: yejian_platform::WebResult) {
        let Some((source, login)) = self.web_purpose.take() else {
            return;
        };
        let Some(cookies) = result.cookies else {
            self.status = "已取消网页验证".into();
            return;
        };
        add_web_cookies(&result.url, &cookies);
        self.web_cookies.insert(result.url.clone(), cookies.clone());
        if let Err(error) = self.save_web_cookies() {
            self.status = format!("网页验证结果保存失败：{error:#}");
        }
        if let Some(key) = login {
            self.submit(Task::Login { source, key, credentials: Err(cookies) });
            return;
        }
        self.challenge = None;
        self.status = "网页验证完成，正在重新加载".into();
        if self.selected_source == source && !source.is_empty() {
            self.select_source("");
            self.select_source(&source);
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
        forget.extend(self.web_cookies.keys().filter(|url| matches(&url_host(url))).cloned());
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
            if ui.add_enabled(!self.busy, quiet_action("打开网页登录")).clicked() {
                if let Some(url) = url {
                    self.open_web(url, Some(item.key.clone()));
                }
            }
            return;
        }
        ui.add(TextEdit::singleline(&mut self.login_user).hint_text("账号").desired_width(ui.available_width()));
        ui.add(
            TextEdit::singleline(&mut self.login_pass)
                .password(true)
                .hint_text("密码")
                .desired_width(ui.available_width()),
        );
        let ready = !self.busy && !self.login_user.is_empty() && !self.login_pass.is_empty();
        if ui.add_enabled(ready, design::primary("登录", ui.visuals().dark_mode)).clicked() {
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
                if self.busy || !self.status.is_empty() {
                    ui.add(Label::new(RichText::new(&self.status).color(design::palette(ui.visuals().dark_mode).muted)).wrap());
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
                    let current = setting_value(&source, &item.key).or(item.default.clone());
                    if let Some(value) = setting_row(ui, &item, current) {
                        self.change_setting(&item.key, value);
                    }
                }
            });
    }

    fn installed_ui(&mut self, ui: &mut Ui) {
        design::section(ui, "我的书源");
        for source in self.installed.clone() {
            let languages = source
                .manifest
                .languages
                .iter()
                .map(|lang| language_label(lang))
                .collect::<Vec<_>>()
                .join(" / ");
            let metadata = if languages.is_empty() {
                format!("v{}", source.manifest.version)
            } else {
                format!("{languages} · v{}", source.manifest.version)
            };
            if source_item(
                ui,
                ("installed", &source.manifest.id),
                &source.manifest.name,
                &metadata,
                true,
                true,
            ) {
                self.select_source(&source.manifest.id);
            }
        }
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
            ui.horizontal(|ui| {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let clicked = ui.add_enabled(!self.busy && !installed, action()).clicked();
                    ui.allocate_ui_with_layout(
                        vec2(ui.available_width(), 0.),
                        Layout::top_down(Align::Min),
                        title,
                    );
                    clicked
                })
                .inner
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
            let needle = self.repository_search.trim().to_lowercase();
            ui.add_space(8.);
            let sources: Vec<&RepositorySource> = repo
                .sources
                .iter()
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

    pub fn ui(&mut self, ui: &mut Ui) {
        self.poll();
        if let Some(origin) = self.challenge.clone() {
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("书源被 Cloudflare 拦截，需要网页验证").strong());
                if ui.add(design::primary("网页验证", ui.visuals().dark_mode)).clicked() {
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
        ScrollArea::vertical()
            .id_salt((
                "source-content",
                &self.selected_source,
                self.results.revision,
                self.selected_manga.as_ref().map(|manga| &manga.key),
            ))
            .show(ui, |ui| {
                // Never let one long row stretch the page beyond the viewport; rows
                // truncate or wrap inside this width instead.
                ui.set_max_width(ui.available_width());
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
                    if !self.results.entries.is_empty() {
                        let label = self
                            .listing
                            .as_ref()
                            .and_then(|id| self.listings.iter().find(|listing| listing.id == *id))
                            .map(|listing| listing.name.clone())
                            .unwrap_or_else(|| "搜索结果".into());
                        design::section(ui, &format!("{label} · {}", self.results.entries.len()));
                    }
                    for manga in self.results.entries.clone() {
                        let metadata = manga
                            .authors
                            .as_ref()
                            .filter(|values| !values.is_empty())
                            .map(|values| values.join(" / "))
                            .or_else(|| {
                                manga.tags.as_ref().map(|values| {
                                    values
                                        .iter()
                                        .take(3)
                                        .cloned()
                                        .collect::<Vec<_>>()
                                        .join(" · ")
                                })
                            })
                            .unwrap_or_default();
                        if source_item(
                            ui,
                            ("manga", &manga.key),
                            &manga.title,
                            &metadata,
                            false,
                            !self.busy,
                        ) {
                            self.open_manga(manga);
                        }
                    }
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
                } else {
                    if ui.add(quiet_action("返回搜索结果")).clicked() {
                        self.selected_manga = None;
                        self.chapters.clear();
                        return;
                    }
                    let manga = self.selected_manga.clone().unwrap();
                    let source = if manga.source_key.is_empty() {
                        self.selected_source.clone()
                    } else {
                        manga.source_key.clone()
                    };
                    ui.add_space(12.);
                    ui.add(Label::new(RichText::new(&manga.title).heading().strong()).wrap());
                    let followed = self.follow_index(&source, &manga.key).is_some();
                    if if followed {
                        ui.add(quiet_action("已追更 · 取消追更")).clicked()
                    } else {
                        ui.add(design::primary("追更", ui.visuals().dark_mode)).clicked()
                    } {
                        self.toggle_follow(&manga);
                    }
                    if let Some(description) = &manga.description {
                        ui.add(Label::new(description).wrap());
                    }
                    ui.add_space(12.);
                    let states: std::collections::HashMap<_, _> = self.tasks.iter()
                        .filter(|task| task.source_id == source && task.manga_id == manga.key)
                        .map(|task| (task.chapter_id.clone(), ChapterQueueState {
                            status: task.status,
                            has_archive: task.cbz.is_some(),
                        }))
                        .collect();
                    let missing = self
                        .chapters
                        .iter()
                        .filter(|chapter| {
                            !chapter.locked
                                && !self.library_chapters.contains_key(
                                    &identity_id(&source, &manga.key, &chapter.key),
                                )
                                && states.get(&chapter.key).is_none_or(|task| {
                                    task.status == DownloadStatus::Failed
                                })
                        })
                        .count();
                    if ui
                        .add_enabled(
                            missing > 0 && !self.busy,
                            design::primary(
                                &format!("加入未下载章节（{missing}）"),
                                ui.visuals().dark_mode,
                            ),
                        )
                        .clicked()
                    {
                        self.enqueue_missing(&manga);
                    }
                    let queue: Vec<DownloadTask> = self
                        .tasks
                        .iter()
                        .filter(|task| task.source_id == source && task.manga_id == manga.key
                            && (task.status != DownloadStatus::Done || !task.linked))
                        .cloned()
                        .collect();
                    if !queue.is_empty() {
                        ui.add_space(8.);
                        disclosure(ui, &format!("本书下载任务（{}）", queue.len()), false, |ui| {
                            for task in &queue {
                                self.download_row(ui, task);
                            }
                        });
                    }
                    ui.add_space(12.);
                    design::section(ui, &format!("章节 · {}", self.chapters.len()));
                    ui.add(
                        TextEdit::singleline(&mut self.chapter_query)
                            .hint_text("查找章节名称或编号")
                            .desired_width(ui.available_width()),
                    );
                    ui.horizontal_wrapped(|ui| {
                        ui.selectable_value(&mut self.chapters_newest_first, false, "从头开始");
                        ui.selectable_value(&mut self.chapters_newest_first, true, "最新在前");
                        if !self.chapter_query.is_empty()
                            && ui.add(quiet_action("清除查找")).clicked()
                        {
                            self.chapter_query.clear();
                        }
                    });
                    let chapters = chapter_list(
                        &self.chapters, &self.chapter_query, self.chapters_newest_first,
                    );
                    if chapters.is_empty() {
                        ui.label(if self.chapters.is_empty() {
                            "书源尚未提供章节。"
                        } else {
                            "没有匹配的章节，试试其他名称或编号。"
                        });
                    }
                    for chapter in chapters {
                        let name = chapter_name(&chapter);
                        let task = states.get(&chapter.key);
                        let id = identity_id(&source, &manga.key, &chapter.key);
                        let downloaded = self.library_chapters.get(&id).cloned();
                        let readable = downloaded.is_some();
                        let can_import = !readable && task.is_some_and(|task| {
                            task.status == DownloadStatus::Done
                                && task.has_archive
                        }) && !self.import_attempts.contains(&id)
                            && !self.ready.iter().any(|queued| queued == &id);
                        let failed = task.is_some_and(|task| task.status == DownloadStatus::Failed);
                        let downloading = task.is_some_and(|task| task.status != DownloadStatus::Done);
                        let enabled = readable || can_import || !chapter.locked;
                        let action = if readable { "阅读" }
                            else if can_import { "加入书架" }
                            else if chapter.locked { "暂不可阅读" }
                            else if downloading && !failed { "边下边读" }
                            else { "在线阅读" };
                        let label = format!("{name}    {action}");
                        ui.push_id(("chapter", &chapter.key), |ui| {
                            if ui
                                .add_enabled(
                                    enabled,
                                    Button::new(label)
                                        .frame_when_inactive(false)
                                        .wrap()
                                        .min_size(vec2(ui.available_width(), 44.)),
                                )
                                .clicked()
                            {
                                if let Some(metadata) = downloaded {
                                    self.open_chapter = Some(metadata);
                                } else if can_import {
                                    self.add_downloaded_to_library(&id);
                                } else {
                                    self.read_online(&manga, &chapter, false);
                                }
                            }
                            if !readable && !can_import && !chapter.locked
                                && (task.is_none() || failed)
                                && ui.add_enabled(!self.busy, quiet_action(
                                    if failed { "重试下载" } else { "仅下载" },
                                )).clicked()
                            {
                                self.enqueue_chapter(&manga, &chapter);
                            }
                            if readable {
                                ui.label(RichText::new("已在书架，可离线阅读")
                                    .color(design::palette(ui.visuals().dark_mode).muted));
                            } else if let Some(task) = task {
                                let state = if task.status == DownloadStatus::Done { "待加入书架" }
                                    else { task.status.label() };
                                ui.label(RichText::new(state)
                                    .color(design::palette(ui.visuals().dark_mode).muted));
                            }
                            ui.separator();
                        });
                    }
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
                if !self.status.is_empty() {
                    ui.add_space(12.);
                    ui.add(Label::new(&self.status).wrap());
                }
                if self.busy {
                    ui.spinner();
                }
                if let Some(task) = self.failed.clone() {
                    if ui.add(quiet_action("重试")).clicked() {
                        self.submit(task);
                    }
                }
            });
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
            if let Some(req) = modified {
                request(&handler, req.url, req.headers, req.body)
            } else {
                request(&handler, url.clone(), vec![], None)
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
///
/// Kept separate from the worker thread so tests can execute the real page
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
    fn test_sources(root: &Path) -> Sources {
        let (dl_tx, dl_cmd_rx) = mpsc::channel();
        // Keep the command receiver alive so `pump()` can actually dispatch the
        // next task in tests. The task itself is exercised directly.
        std::mem::forget(dl_cmd_rx);
        Sources {
            root: root.to_path_buf(),
            tx: mpsc::channel().0,
            rx: mpsc::channel().1,
            installed: Vec::new(),
            repository: None,
            results: ResultsState::default(),
            chapters: Vec::new(),
            chapter_query: String::new(),
            chapters_newest_first: false,
            open_chapter: None,
            library_chapters: Default::default(),
            selected_source: String::new(),
            selected_manga: None,
            url: String::new(),
            query: String::new(),
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
            checking: 0,
            auto_checked: true,
            up_tx: mpsc::channel().0,
            up_rx: mpsc::channel().1,
            covers: Default::default(),
            settings_open: false,
            settings_changed: false,
            source_settings: Default::default(),
            stale_engines: Default::default(),
            challenge: None,
            web_purpose: None,
            login_user: String::new(),
            login_pass: String::new(),
            web_cookies: Default::default(),
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
        assert_eq!(sources.tasks[0].id, stream.id, "read chapter downloads first");
        assert_eq!(sources.preempted.as_deref(), Some(other.id.as_str()));
        assert_eq!(sources.dl_stop.load(Ordering::SeqCst), STOP_PAUSE);
        assert!(
            sources.tasks.iter().any(|task| task.chapter_id == "c2"),
            "next chapter in reading order is prefetched"
        );
        assert!(!sources.tasks.iter().any(|task| task.chapter_id == "c3"));
        assert_eq!(sources.stream_state(&stream.id).unwrap().stopped, None);

        sources.handle_download_event(DlEvent::Aborted { id: other.id.clone(), how: STOP_PAUSE });
        let requeued = sources.tasks.iter().find(|task| task.id == other.id).unwrap();
        assert_eq!(requeued.status, DownloadStatus::Queued, "no manual continue needed");
        assert!(sources.preempted.is_none());
        assert_eq!(sources.running.as_deref(), Some(stream.id.as_str()));

        assert!(sources.has_adjacent_online(&stream.metadata, 1));
        assert!(!sources.has_adjacent_online(&stream.metadata, -1));
        assert_eq!(sources.adjacent_online_key(&stream.metadata, 1).as_deref(), Some("c2"));

        assert!(sources.open_adjacent_online(&sources.tasks[0].metadata(), 0));
        let next = sources.tasks.iter().find(|task| task.chapter_id == "c2").unwrap().metadata();
        assert!(sources.open_adjacent_online(&next, -1));
        assert!(sources.take_stream().unwrap().from_end, "backwards starts on the last page");
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
        sources.handle_download_event(DlEvent::Failed { id: other.id.clone(), error: "timeout".into() });
        let other = sources.tasks.iter().find(|task| task.id == other.id).unwrap();
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
        let chapter = |key: &str| Chapter { key: key.into(), ..Default::default() };
        sources.chapters = vec![chapter("c1"), chapter("c2")];
        sources.toggle_follow(&book);
        assert_eq!(sources.follows.len(), 1);
        assert_eq!(follows::load(root.path()).unwrap().len(), 1, "saved immediately");
        assert_eq!(sources.follow_manga(&sources.follows[0]).unwrap().key, "m");

        sources.checking = 1;
        sources.handle_update(UpdateEvent {
            source: "source.a".into(),
            manga: "m".into(),
            chapters: Some(Ok(vec!["c1".into(), "c2".into(), "c3".into()])),
        });
        assert_eq!(sources.checking, 0);
        assert_eq!(sources.fresh_follows(), 1);
        assert_eq!(follows::load(root.path()).unwrap()[0].fresh, vec!["c3".to_string()]);

        sources.request_id = 7;
        sources.current_request.store(7, Ordering::Relaxed);
        let (tx, rx) = mpsc::channel();
        sources.rx = rx;
        tx.send((7, Ok(Reply::Chapters(
            "source.a".into(),
            book.clone(),
            vec![chapter("c1"), chapter("c2"), chapter("c3")],
        ))))
        .unwrap();
        sources.poll();
        assert_eq!(sources.fresh_follows(), 0, "opening the manga marks chapters seen");

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
        assert!(stream_page(root.path(), &id, 1).unwrap().is_none(), "partial page is not ready");
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
        assert_eq!(saved.iter().find(|task| task.id == item.id).unwrap().read_page, Some(7));
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
            ordered.iter().map(|chapter| chapter.key.as_str()).collect::<Vec<_>>(),
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
        let link = ChapterLink { book_id: "book".into(), metadata: item.metadata() };
        sources.set_library_chapters(&[link]);
        assert!(sources.tasks.is_empty(), "restore does not need download-tasks.json");
        assert_eq!(sources.chapter_state("source.a", "m", "c").as_deref(), Some("已在书架"));
        assert!(!sources.chapter_needs_download("source.a", "m", &item.chapter()));
        sources.read_downloaded(&item);
        assert_eq!(sources.take_open_chapter(), Some(item.metadata()));
        assert!(sources.take_open_chapter().is_none(), "consume one request once");
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
            sources.chapters = vec![Chapter { key: "1".into(), ..Default::default() }];
            sources.retry_task(&original);
            assert_eq!(sources.tasks.len(), 1, "must not enqueue the current foreign manga");
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
        sources.chapters = [10., 2., 2.5, 1.].into_iter().map(|number| Chapter {
            key: number.to_string(),
            chapter_number: Some(number),
            ..Default::default()
        }).collect();
        let mut failed = DownloadTask::from_chapter(
            "source.a", &manga("source.a", "m"), &sources.chapters[1],
        );
        failed.status = DownloadStatus::Failed;
        sources.tasks.push(failed);
        sources.enqueue_missing(&manga("source.a", "m"));
        assert_eq!(sources.tasks.len(), 4);
        assert!(sources.tasks.iter().all(|task| matches!(
            task.status, DownloadStatus::Running | DownloadStatus::Queued
        )));
        assert!(sources.status.contains("4"), "retry count should match action");
        // A failed task keeps its existing queue position; newly added tasks
        // are queued from the first chapter rather than the source's newest.
        assert_eq!(
            sources.tasks.iter().skip(1).map(|task| task.chapter_id.as_str()).collect::<Vec<_>>(),
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
            fetched.load(Ordering::SeqCst), 1,
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
        let pages: Vec<_> = (0..8)
            .map(|index| url_page(&format!("p{index}")))
            .collect();
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
            first_batch.iter().all(|url| url.starts_with('p')
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
        let pages: Vec<_> = (0..6)
            .map(|index| url_page(&format!("p{index}")))
            .collect();
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
        sources.chapters = vec![chapter, Chapter { key: "new".into(), ..Default::default() }];
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
}
