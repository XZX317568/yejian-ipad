




use crate::sources::STREAM_PREFIX;
use crate::sources::StreamChapter;
use crate::worker::Command;




use egui::*;

use std::collections::HashMap;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use yejian_core::Book;

use yejian_core::ChapterMetadata;
use super::*;


impl ReaderApp {
    #[cfg(debug_assertions)]
    pub fn add_bookmark_here(&mut self) {
        if let Screen::Reader(id) = self.screen.clone() {
            self.bookmark_toggle(&id);
        }
    }
    #[cfg(debug_assertions)]
    pub fn open_bookmark_dialog(&mut self) {
        if let Screen::Reader(id) = self.screen.clone() {
            self.dialog_bookmarks = true;
            self.chapter_dialog = Some(id);
        }
    }
    pub(super) fn reader_book(&self, id: &str) -> Option<Book> {
        self.books
            .iter()
            .find(|b| b.id == id)
            .or_else(|| self.stream_for(id).map(|s| &s.book).filter(|b| b.id == id))
            .cloned()
    }
    pub(super) fn reading_metadata(&self, id: &str) -> Option<ChapterMetadata> {
        self.chapter(id)
            .cloned()
            .or_else(|| self.stream_for(id).map(|stream| stream.metadata.clone()))
            .or_else(|| {
                id.strip_prefix(STREAM_PREFIX)
                    .and_then(|task| self.sources.stream_metadata(task))
            })
    }
    /// The page's original bytes exist locally — share stays grey until then.
    pub(super) fn share_enabled(&self, id: &str) -> bool {
        if id.starts_with(STREAM_PREFIX) {
            self.sources.page_ready(id, self.page)
        } else {
            true
        }
    }
    /// Whether the currently shown page already has a bookmark.
    pub(super) fn bookmarked(&self, id: &str) -> bool {
        if self.private {
            return false;
        }
        let meta = self.reading_metadata(id);
        let work = crate::bookmarks::work_key(id, meta.as_ref());
        let chapter = crate::bookmarks::chapter_key(id, meta.as_ref());
        self.bookmarks.has(&work, &chapter, self.page)
    }
    pub(super) fn bookmark_toggle(&mut self, id: &str) {
        if self.private {
            self.pending_notice = Some("私密阅读时书签不可用".into());
            return;
        }
        let meta = self.reading_metadata(id);
        let work = crate::bookmarks::work_key(id, meta.as_ref());
        let chapter = crate::bookmarks::chapter_key(id, meta.as_ref());
        let title = meta
            .as_ref()
            .map(|meta| meta.chapter_title.clone())
            .unwrap_or_else(|| "本页".into());
        self.bookmarks.toggle(crate::bookmarks::Bookmark {
            work,
            chapter,
            page: self.page,
            title,
            created_at: crate::follows::now(),
        });
        if let Err(error) = self.bookmarks.save(&self.root) {
            self.error = Some(format!("书签保存失败：{error}"));
        }
    }
    /// Local book for a bookmark's work+chapter, if it was downloaded.
    pub(super) fn bookmark_book(&self, mark: &crate::bookmarks::Bookmark) -> Option<Book> {
        self.chapters
            .iter()
            .find(|link| {
                crate::bookmarks::work_key(&link.book_id, Some(&link.metadata))
                    == mark.work
                    && link.metadata.chapter_id == mark.chapter
            })
            .and_then(|link| {
                self.books
                    .iter()
                    .find(|book| book.id == link.book_id)
                    .cloned()
            })
            .or_else(|| {
                // Unlinked local books use the book id for both keys.
                (mark.work == mark.chapter)
                    .then(|| self.books.iter().find(|book| book.id == mark.work).cloned())
                    .flatten()
            })
    }
    pub(super) fn bookmark_jump(
        &mut self,
        mark: crate::bookmarks::Bookmark,
        current: &str,
        now: f64,
        viewport_w: f32,
    ) {
        if let Some(book) = self.bookmark_book(&mark) {
            let id = book.id.clone();
            self.open_reader(&book);
            self.chapter_dialog = None;
            let delta = mark.page as i32 - self.page as i32;
            if delta != 0 {
                self.next_page(&id, delta, now, viewport_w);
            }
            return;
        }
        // Online chapter with an existing download task: reopen the stream
        // and land on the bookmarked page once it is open.
        if let Some((source, manga)) = mark.work.split_once('\u{1f}')
            && self.sources.open_bookmarked(source, manga, &mark.chapter)
        {
            self.pending_bookmark_page = Some(mark.page);
            self.chapter_dialog = None;
            let _ = current;
        } else {
            self.error = Some("书签的章节还没下载，请先从详情页打开".into());
        }
    }
    pub(super) fn open_stream(&mut self, chapter: StreamChapter) {
        let id = format!("{STREAM_PREFIX}{}", chapter.id);
        let state = self.sources.stream_state(&chapter.id).unwrap_or_default();
        let wanted = if chapter.from_end {
            usize::MAX
        } else {
            self.pending_bookmark_page
                .take()
                .or(state.page)
                .unwrap_or(0)
        };
        let resume = (state.total == 0 && wanted > 0).then_some(wanted);
        let book = Book {
            id: id.clone(),
            title: format!(
                "{} · {}",
                chapter.metadata.manga_title, chapter.metadata.chapter_title
            ),
            author: String::new(),
            archive_path: PathBuf::new(),
            cover_path: None,
            page_count: state.total.max(1),
            current_page: wanted.min(state.total.saturating_sub(1)),
            page_fraction: 0.,
            updated_at: 0,
            added_at: 0,
            last_read_at: None,
            category: String::new(),
        };
        self.stream = Some(StreamBook {
            book: book.clone(),
            task: chapter.id,
            metadata: chapter.metadata,
            done: state.done,
            stopped: state.stopped,
            signature: state.signature,
            resume,
            waiting: HashSet::new(),
        });
        self.page_slide = motion::PageSlide::default();
        self.open_reader(&book);
    }
    /// Follows the online chapter's download, and hands the reader over to
    /// the library copy (same page) once it has been imported.
    /// The vertical strip's segments in order: the still-loaded previous
    /// chapter (kept for scrolling back), the current one, and the next one
    /// once its page count is known.
    pub(super) fn strip_segments(&self) -> Vec<(String, usize)> {
        let mut segments = Vec::with_capacity(3);
        if let Some(prev) = &self.vertical_prev {
            segments.push(prev.clone());
        }
        if let Screen::Reader(id) = &self.screen {
            let count = self
                .reader_book(id)
                .map(|book| book.page_count)
                .unwrap_or(0);
            segments.push((id.clone(), count));
        }
        if let Some(next) = &self.vertical_next {
            segments.push(next.clone());
        }
        segments
    }
    /// Resolves a global strip index to `(segment id, local page)`.
    pub(super) fn locate<'a>(segments: &'a [(String, usize)], global: usize) -> Option<(&'a str, usize)> {
        let mut base = 0;
        for (id, count) in segments {
            if global < base + count {
                return Some((id.as_str(), global - base));
            }
            base += count;
        }
        None
    }
    /// Rendered heights for all loaded strip pages, keyed by global index.
    pub(super) fn strip_heights(&self, segments: &[(String, usize)], width: f32) -> HashMap<usize, f32> {
        let mut heights = HashMap::new();
        let mut base = 0;
        for (id, count) in segments {
            for ((_, page), image) in self.pages.iter().filter(|((key, _), _)| key == id) {
                let size = image.shown_size(self.crop);
                heights.insert(base + *page, size.y / size.x * width);
            }
            base += count;
        }
        heights
    }
    /// An `(id, page)` key's global strip index, or `usize::MAX` when the
    /// page is not part of the strip any more.
    pub(super) fn global_page_index(&self, id: &str, page: usize) -> usize {
        let mut base = 0;
        for (seg_id, count) in self.strip_segments() {
            if seg_id == id {
                return if page < count { base + page } else { usize::MAX };
            }
            base += count;
        }
        usize::MAX
    }
    /// Whether an arriving page texture belongs in the reader's working set.
    pub(super) fn wants_page(&self, id: &str, page: usize) -> bool {
        if !self.vertical {
            return matches!(&self.screen, Screen::Reader(current) if current == id)
                && page.abs_diff(self.page) <= 2;
        }
        let global = self.global_page_index(id, page);
        global != usize::MAX && global.abs_diff(self.vertical_reader.page) <= 3
    }
    /// `(near, essential)` page keys: `near` is the keep window around the
    /// anchor across segments, `essential` the pages the current view may
    /// show, which eviction never removes.
    pub(super) fn page_keep_sets(&self) -> (HashSet<(String, usize)>, HashSet<(String, usize)>) {
        let mut near = HashSet::new();
        let mut essential = HashSet::new();
        let Screen::Reader(id) = &self.screen else {
            return (near, essential);
        };
        if self.vertical {
            let segments = self.strip_segments();
            let anchor = self.vertical_reader.page;
            for global in anchor.saturating_sub(2)..=anchor + 2 {
                if let Some((seg, local)) = Self::locate(&segments, global) {
                    near.insert((seg.to_string(), local));
                }
            }
            for global in anchor.saturating_sub(1)..=anchor + 1 {
                if let Some((seg, local)) = Self::locate(&segments, global) {
                    essential.insert((seg.to_string(), local));
                }
            }
        } else {
            for page in self.page.saturating_sub(2)..=self.page + 2 {
                near.insert((id.clone(), page));
            }
            essential.insert((id.clone(), self.page));
            if self.spread {
                let (base, second) = spread_pair(self.page, self.spread_offset);
                essential.insert((id.clone(), base));
                if let Some(second) = second {
                    essential.insert((id.clone(), second));
                }
            }
        }
        (near, essential)
    }
    /// Distance of a page key from the reader's anchor; used for eviction.
    pub(super) fn page_distance(&self, id: &str, page: usize) -> usize {
        if self.vertical {
            self.global_page_index(id, page)
                .abs_diff(self.vertical_reader.page)
        } else {
            page.abs_diff(self.page)
        }
    }
    /// Whether a chapter exists before/after `id` (library or online).
    pub(super) fn has_neighbor(&self, id: &str, delta: i32) -> bool {
        if let Some(metadata) = self.reading_metadata(id)
            && let Some(key) = self.sources.adjacent_online_key(&metadata, delta)
        {
            let local = self.chapters.iter().any(|link| {
                link.metadata.source_id == metadata.source_id
                    && link.metadata.manga_id == metadata.manga_id
                    && link.metadata.chapter_id == key
                    && self.books.iter().any(|book| book.id == link.book_id)
            });
            return local || self.sources.has_adjacent_online(&metadata, delta);
        }
        self.adjacent_chapter(id, delta).is_some()
    }
    /// Looks up the chapter that follows `id`, queuing its online download
    /// when `enqueue` is set (the anchor is within the last pages). The
    /// segment becomes `Ready` only once its page count is known.
    pub(super) fn peek_next(&mut self, id: &str, enqueue: bool) -> Peek {
        if let Some(metadata) = self.reading_metadata(id)
            && let Some(key) = self.sources.adjacent_online_key(&metadata, 1)
        {
            let local = self
                .chapters
                .iter()
                .find(|link| {
                    link.metadata.source_id == metadata.source_id
                        && link.metadata.manga_id == metadata.manga_id
                        && link.metadata.chapter_id == key
                })
                .and_then(|link| self.books.iter().find(|book| book.id == link.book_id))
                .cloned();
            if let Some(book) = local {
                return Peek::Ready(book.id, book.page_count, None);
            }
            if !self.sources.has_adjacent_online(&metadata, 1) {
                return Peek::Missing;
            }
            if enqueue
                && let Some((task, task_meta)) = self.sources.prepare_next(&metadata)
                && let Some(state) = self.sources.stream_state(&task)
                && state.total > 0
            {
                return Peek::Ready(
                    format!("{STREAM_PREFIX}{task}"),
                    state.total,
                    Some(task_meta),
                );
            }
            return Peek::Loading;
        }
        match self.adjacent_chapter(id, 1) {
            Some(book) => Peek::Ready(book.id, book.page_count, None),
            None => Peek::Missing,
        }
    }
    /// Stream bookkeeping for a `stream:` strip segment.
    pub(super) fn stream_segment_book(&self, seg_id: &str, metadata: &ChapterMetadata) -> Option<StreamBook> {
        let task = seg_id.strip_prefix(STREAM_PREFIX)?;
        let state = self.sources.stream_state(task).unwrap_or_default();
        Some(StreamBook {
            book: Book {
                id: seg_id.to_string(),
                title: format!("{} · {}", metadata.manga_title, metadata.chapter_title),
                author: String::new(),
                archive_path: PathBuf::new(),
                cover_path: None,
                page_count: state.total.max(1),
                current_page: 0,
                page_fraction: 0.,
                updated_at: 0,
                added_at: 0,
                last_read_at: None,
                category: String::new(),
            },
            task: task.to_string(),
            metadata: metadata.clone(),
            done: state.done,
            stopped: state.stopped,
            signature: state.signature,
            resume: None,
            waiting: HashSet::new(),
        })
    }
    /// The library book a downloaded task's metadata resolved to.
    pub(super) fn imported_book(&self, metadata: &ChapterMetadata) -> Option<Book> {
        self.chapters
            .iter()
            .find(|link| {
                link.metadata.source_id == metadata.source_id
                    && link.metadata.manga_id == metadata.manga_id
                    && link.metadata.chapter_id == metadata.chapter_id
            })
            .and_then(|link| self.books.iter().find(|book| book.id == link.book_id))
            .cloned()
    }
    /// Moves every page-scoped map entry from a `stream:` id to its imported
    /// library book id, keeping decoded textures across the handover.
    pub(super) fn remap_page_keys(&mut self, from: &str, to: &str) {
        for key in self
            .pages
            .keys()
            .filter(|(id, _)| id == from)
            .cloned()
            .collect::<Vec<_>>()
        {
            if let Some(texture) = self.pages.remove(&key) {
                self.pages.insert((to.to_string(), key.1), texture);
            }
        }
        for key in self
            .page_errors
            .keys()
            .filter(|(id, _)| id == from)
            .cloned()
            .collect::<Vec<_>>()
        {
            if let Some(error) = self.page_errors.remove(&key) {
                self.page_errors.insert((to.to_string(), key.1), error);
            }
        }
        for key in self
            .page_pending
            .iter()
            .filter(|(id, _)| id == from)
            .cloned()
            .collect::<Vec<_>>()
        {
            self.page_pending.remove(&key);
            self.page_pending.insert((to.to_string(), key.1));
        }
        for key in self
            .prefetch_blocked
            .iter()
            .filter(|(id, _)| id == from)
            .cloned()
            .collect::<Vec<_>>()
        {
            self.prefetch_blocked.remove(&key);
            self.prefetch_blocked.insert((to.to_string(), key.1));
        }
    }
    /// The vertical anchor entered the following segment: that chapter
    /// becomes the reader's book, keeping textures and the exact scroll
    /// offset. Returns the new current `(id, page count)`.
    pub(super) fn cross_vertical(&mut self, old_id: &str, old_count: usize) -> (String, usize) {
        let prev_count = self.vertical_prev.as_ref().map(|(_, c)| *c).unwrap_or(0);
        let Some((next_id, next_count)) = self.vertical_next.take() else {
            return (old_id.to_string(), old_count);
        };
        let global = self.vertical_reader.page;
        let local = global - prev_count - old_count;
        if let Some(task) = old_id.strip_prefix(STREAM_PREFIX) {
            if !self.private {
                self.sources
                    .set_stream_page(task, old_count.saturating_sub(1));
            }
        } else {
            self.record_position(old_id, old_count.saturating_sub(1), 0.);
        }
        self.vertical_prev = Some((old_id.to_string(), old_count));
        self.stream = self.next_stream.take();
        self.screen = Screen::Reader(next_id.clone());
        self.page = local;
        self.vertical_reader.page = global - prev_count;
        self.prefetch_blocked.clear();
        self.record_position(&next_id, local, self.vertical_reader.fraction);
        (next_id, next_count)
    }
    /// Saves the strip anchor's position for whichever chapter holds it.
    pub(super) fn record_anchor(&mut self, id: &str, local: usize) {
        let fraction = self.vertical_reader.fraction;
        if id.starts_with(STREAM_PREFIX)
            && !self.stream.as_ref().is_some_and(|s| s.book.id == id)
        {
            if !self.private {
                self.sources.set_stream_page(&id[STREAM_PREFIX.len()..], local);
            }
        } else {
            self.record_position(id, local, fraction);
        }
    }
    pub(super) fn update_stream(&mut self) {
        if let Some(stream) = self.stream.as_ref() {
            let id = stream.book.id.clone();
            if !matches!(&self.screen, Screen::Reader(current) if current == &id) {
                self.stream = None;
            } else if let Some(book) = self.imported_book(&stream.metadata) {
                self.remap_page_keys(&id, &book.id);
                self.stream = None;
                if self.vertical {
                    // In the strip only the id behind the segment changes;
                    // textures, offset and the strip itself stay put.
                    self.screen = Screen::Reader(book.id);
                } else {
                    let mut book = book;
                    book.current_page = self.page.min(book.page_count.saturating_sub(1));
                    book.page_fraction = 0.;
                    self.open_reader(&book);
                }
            } else {
                let task = stream.task.clone();
                if let Some(state) = self.sources.stream_state(&task) {
                    let Some(stream) = self.stream.as_mut() else {
                        return;
                    };
                    if state.total > 0 {
                        stream.book.page_count = state.total;
                    }
                    if state.signature != stream.signature {
                        stream.signature = state.signature.clone();
                        stream.waiting.clear();
                        self.pages.retain(|(key, _), _| key != &id);
                        self.page_errors.retain(|(key, _), _| key != &id);
                        self.page_pending.retain(|(key, _)| key != &id);
                        self.prefetch_blocked.retain(|(key, _)| key != &id);
                        self.page_generation += 1;
                        self.worker
                            .generation
                            .store(self.page_generation, Ordering::Relaxed);
                    }
                    let Some(stream) = self.stream.as_mut() else {
                        return;
                    };
                    if state.done != stream.done || state.stopped != stream.stopped {
                        stream.done = state.done;
                        stream.stopped = state.stopped;
                        stream.waiting.clear();
                    }
                    if let Some(wanted) = stream.resume.filter(|_| state.total > 0) {
                        stream.resume = None;
                        stream.book.current_page = wanted.min(state.total - 1);
                        stream.book.page_fraction = 0.;
                        let book = stream.book.clone();
                        self.open_reader(&book);
                        return;
                    }
                }
            }
        }
        // The following segment's stream hands over to its library copy the
        // same way once its task is imported.
        if let Some((stream_id, book)) = self.next_stream.as_ref().and_then(|next| {
            self.imported_book(&next.metadata)
                .map(|book| (next.book.id.clone(), book))
        }) {
            self.remap_page_keys(&stream_id, &book.id);
            if self
                .vertical_next
                .as_ref()
                .is_some_and(|(seg, _)| seg == &stream_id)
            {
                self.vertical_next = Some((book.id, book.page_count));
            }
            self.next_stream = None;
        }
        if let Some(next) = self.next_stream.as_mut()
            && let Some(state) = self.sources.stream_state(&next.task)
        {
            if state.total > 0 {
                next.book.page_count = state.total;
                if let Some(seg) = self.vertical_next.as_mut()
                    && seg.0 == next.book.id
                {
                    seg.1 = state.total;
                }
            }
            if state.signature != next.signature {
                next.signature = state.signature.clone();
                next.waiting.clear();
                let id = next.book.id.clone();
                self.pages.retain(|(key, _), _| key != &id);
                self.page_errors.retain(|(key, _), _| key != &id);
                self.page_pending.retain(|(key, _)| key != &id);
                self.prefetch_blocked.retain(|(key, _)| key != &id);
            }
            if state.done != next.done || state.stopped != next.stopped {
                next.done = state.done;
                next.stopped = state.stopped;
                next.waiting.clear();
            }
        }
        // A finished previous chapter swaps its stream id for the book id so
        // its still-loaded pages keep resolving.
        if let Some((prev_id, prev_count)) = self.vertical_prev.clone()
            && let Some(task) = prev_id.strip_prefix(STREAM_PREFIX)
            && let Some(metadata) = self.sources.stream_metadata(task)
            && let Some(book) = self.imported_book(&metadata)
        {
            self.remap_page_keys(&prev_id, &book.id);
            self.vertical_prev = Some((book.id, prev_count.min(book.page_count)));
        }
    }
    /// Opens the previous/next chapter in the source's order when that list
    /// is known (library copy if present, online otherwise), else the
    /// neighbouring library chapter.
    /// Display name of the next chapter (local or online) for the end card;
    /// `None` when this is the latest chapter.
    pub(super) fn next_chapter_name(&self, id: &str) -> Option<String> {
        if let Some(metadata) = self.reading_metadata(id) {
            if let Some(name) = self.sources.adjacent_online_name(&metadata, 1) {
                return Some(name);
            }
        }
        self.adjacent_chapter(id, 1)
            .map(|book| self.reading_title(&book))
    }
    pub(super) fn open_neighbor(&mut self, id: &str, delta: i32) -> bool {
        let moved = self.open_neighbor_inner(id, delta);
        if moved && delta > 0 {
            self.stats.chapter_finished(crate::follows::now() as f64);
        }
        moved
    }
    pub(super) fn open_neighbor_inner(&mut self, id: &str, delta: i32) -> bool {
        let start = |book: &mut Book| {
            book.current_page = if delta < 0 {
                book.page_count.saturating_sub(1)
            } else {
                0
            };
            book.page_fraction = 0.;
        };
        if let Some(metadata) = self.reading_metadata(id)
            && let Some(key) = self.sources.adjacent_online_key(&metadata, delta)
        {
            let local = self
                .chapters
                .iter()
                .find(|link| {
                    link.metadata.source_id == metadata.source_id
                        && link.metadata.manga_id == metadata.manga_id
                        && link.metadata.chapter_id == key
                })
                .and_then(|link| self.books.iter().find(|book| book.id == link.book_id))
                .cloned();
            if let Some(mut book) = local {
                start(&mut book);
                self.open_reader(&book);
                return true;
            }
            return self.sources.open_adjacent_online(&metadata, delta);
        }
        if let Some(mut adjacent) = self.adjacent_chapter(id, delta) {
            start(&mut adjacent);
            self.open_reader(&adjacent);
            return true;
        }
        false
    }
    pub(super) fn stream_for(&self, id: &str) -> Option<&StreamBook> {
        self.stream
            .as_ref()
            .filter(|stream| stream.book.id == id)
            .or(self
                .next_stream
                .as_ref()
                .filter(|stream| stream.book.id == id))
    }
    pub(super) fn stream_for_mut(&mut self, id: &str) -> Option<&mut StreamBook> {
        if self.stream.as_mut().is_some_and(|s| s.book.id == id) {
            return self.stream.as_mut();
        }
        self.next_stream.as_mut().filter(|s| s.book.id == id)
    }
    pub(super) fn stream_waiting_text(&self, id: &str, page: usize) -> Option<String> {
        let stream = self.stream_for(id)?;
        if !stream.waiting.contains(&page) {
            return None;
        }
        Some(if let Some(reason) = &stream.stopped {
            format!("第 {} 页还没下载
{reason}", page + 1)
        } else if stream.book.page_count > 1 || stream.done > 0 {
            format!(
                "正在下载第 {} 页…（{}/{}）",
                page + 1,
                stream.done,
                stream.book.page_count
            )
        } else {
            "正在获取章节页面…".into()
        })
    }
    pub(super) fn chapter(&self, id: &str) -> Option<&ChapterMetadata> {
        self.chapter_indices
            .get(id)
            .map(|&index| &self.chapters[index].metadata)
    }
    pub(super) fn record_page(&mut self, id: &str, page: usize) {
        let fraction = if self.vertical && self.vertical_reader.page == page {
            self.vertical_reader.fraction
        } else {
            0.
        };
        self.record_position(id, page, fraction);
    }
    pub(super) fn record_position(&mut self, id: &str, page: usize, fraction: f32) {
        if self
            .saved_position
            .as_ref()
            .is_some_and(|(key, p, f)| key == id && *p == page && (*f - fraction).abs() < 0.0001)
        {
            return;
        }
        self.saved_position = Some((id.to_string(), page, fraction));
        // Auto-sync: a non-private reader that reached the last page (or a
        // chapter seam) pushes the remote progress forward. `chapter_done`
        // itself enforces the auto flag and the monotone floor.
        if !self.private {
            let last = self
                .reader_book(id)
                .map(|book| book.page_count.saturating_sub(1))
                .unwrap_or(usize::MAX);
            if page >= last {
                if let Some(metadata) = self.reading_metadata(id) {
                    let key = crate::trackers::binding_key(
                        Some((&metadata.source_id, &metadata.manga_id)),
                        None,
                    );
                    let chapter = metadata
                        .chapter_number
                        .filter(|n| n.is_finite() && *n >= 1.)
                        .map(|n| n.floor() as i64)
                        .or_else(|| crate::trackers::chapter_number(&metadata.chapter_title));
                    if let Some(chapter) = chapter {
                        self.trackers.chapter_done(&key, chapter);
                    }
                } else {
                    let key = crate::trackers::binding_key(None, Some(id));
                    self.trackers.chapter_done(&key, 0);
                }
            }
        }
        if let Some(stream) = self.stream.as_mut().filter(|stream| stream.book.id == id) {
            stream.book.current_page = page;
            stream.book.page_fraction = fraction;
            if stream.resume.is_none() && !self.private {
                let task = stream.task.clone();
                self.sources.set_stream_page(&task, page);
            }
            return;
        }
        self.send(Command::Progress {
            id: id.into(),
            page,
            fraction,
            private: self.private,
        });
        if !self.private {
            if let Some(b) = self.books.iter_mut().find(|b| b.id == id) {
                b.current_page = page;
                b.page_fraction = fraction;
                b.last_read_at = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .ok()
                    .map(|d| d.as_millis() as i64);
            }
        }
    }
    pub(super) fn flush_position(&mut self) {
        if let Screen::Reader(id) = self.screen.clone() {
            if self.pages.contains_key(&(id.clone(), self.page)) {
                self.record_page(&id, self.page);
            }
        }
    }
    pub(super) fn ask_cover(&mut self, ctx: &Context, id: &str) {
        self.visible_covers.insert(id.into());
        self.cover_clock += 1;
        self.cover_usage.insert(id.into(), self.cover_clock);
        if let Some((source, manga)) = Self::online_parts(id) {
            // Online entries have no library archive; their cover comes from
            // the followed snapshot through the source cover cache.
            if !self.covers.contains_key(id)
                && let Some(texture) = self.sources.work_cover(ctx, source, manga)
            {
                self.covers.insert(id.into(), texture);
            }
            return;
        }
        if !self.covers.contains_key(id)
            && !self.cover_failed.contains(id)
            && self.cover_pending.insert(id.into())
        {
            self.send(Command::Image {
                id: id.into(),
                page: 0,
                cover: true,
                generation: 0,
                target: 0,
            });
        }
    }
    pub(super) fn trim_covers(&mut self) {
        let mut bytes: usize = self.covers.values().map(|texture| texture.size()[0] * texture.size()[1] * 4).sum();
        crate::perf::set_cover_bytes(bytes as u64);
        while bytes > 64 * 1024 * 1024 {
            let candidate = self.covers.keys()
                .filter(|id| !self.visible_covers.contains(*id)
                    && !self.flight.as_ref().is_some_and(|flight| &flight.id == *id))
                .min_by_key(|id| self.cover_usage.get(*id).copied().unwrap_or(0))
                .cloned();
            let Some(id) = candidate else { break };
            if let Some(texture) = self.covers.remove(&id) {
                bytes -= texture.size()[0] * texture.size()[1] * 4;
            }
            self.cover_usage.remove(&id);
        }
    }
    /// Decode target width for page requests: the viewport's physical slot
    /// width (halved for two-page spreads), clamped to [1024, 3072]. Pages
    /// under 2x this width stay untouched so GPU linear filtering handles them.
    pub(super) fn page_target_width(&self) -> u32 {
        let factor = if self.spread && !self.vertical { 0.5 } else { 1. };
        (self.page_target_px * factor).clamp(1024., 3072.) as u32
    }
    /// Zoom past 1.5x asks for a 3072-wide decode of the current page; the
    /// arriving texture swaps in through the normal image path.
    pub(super) fn ask_hires(&mut self, id: &str) {
        let key = (id.to_string(), self.page);
        let already = self
            .pages
            .get(&key)
            .is_some_and(|texture| texture.size.x >= 3072.);
        if !already && self.hires_requested.insert(key) {
            self.send(Command::Image {
                id: id.into(),
                page: self.page,
                cover: false,
                generation: self.page_generation,
                target: 3072,
            });
        }
    }
    pub(super) fn ask_page(&mut self, id: &str, page: usize) {
        let key = (id.to_string(), page);
        if page != self.page && self.prefetch_blocked.contains(&key) {
            return;
        }
        if self
            .stream_for(id)
            .is_some_and(|stream| stream.waiting.contains(&page))
        {
            return;
        }
        if !self.pages.contains_key(&key)
            && !self.page_errors.contains_key(&key)
            && self.page_pending.insert(key)
        {
            self.send(Command::Image {
                id: id.into(),
                page,
                cover: false,
                generation: self.page_generation,
                target: self.page_target_width(),
            });
        }
    }
    pub(super) fn trim_pages(&mut self) {
        let (near, essential) = self.page_keep_sets();
        self.pages
            .retain(|key, _| near.contains(key) || essential.contains(key));
        // Page textures also have a byte budget on top of the page window;
        // essential pages never evict.
        let budget: u64 = crate::perf::page_budget();
        while self
            .pages
            .values()
            .map(|page| page.size.x as u64 * page.size.y as u64 * 4)
            .sum::<u64>()
            > budget
        {
            let Some(key) = self
                .pages
                .keys()
                .filter(|key| !essential.contains(*key))
                .max_by_key(|(id, page)| self.page_distance(id, *page))
                .cloned()
            else {
                break;
            };
            self.pages.remove(&key);
            self.prefetch_blocked.insert(key);
        }
        crate::perf::set_page_bytes(
            self.pages
                .values()
                .map(|page| page.size.x as u64 * page.size.y as u64 * 4)
                .sum::<u64>(),
        );
    }
    /// The effective reader settings of one book: a saved per-work entry
    /// wins, then the source's viewer hint, then the global defaults.
    pub(super) fn apply_reader_prefs(&mut self, book: &Book) {
        let metadata = self.reading_metadata(&book.id);
        let key = crate::reader_prefs::key_for(metadata.as_ref(), &book.id);
        let mut prefs = self.reader_prefs.get(&key).cloned().unwrap_or_else(|| {
            let mut prefs = self.reader_globals.clone();
            if let Some(viewer) = metadata.and_then(|metadata| {
                self.sources
                    .viewer_hint(&metadata.source_id, &metadata.manga_id)
            }) {
                match viewer {
                    yejian_sources::Viewer::RightToLeft => {
                        prefs.mode = crate::reader_prefs::Mode::Paged;
                        prefs.rtl = true;
                    }
                    yejian_sources::Viewer::LeftToRight => {
                        prefs.mode = crate::reader_prefs::Mode::Paged;
                        prefs.rtl = false;
                    }
                    yejian_sources::Viewer::Vertical | yejian_sources::Viewer::Webtoon => {
                        prefs.mode = crate::reader_prefs::Mode::Vertical;
                    }
                    yejian_sources::Viewer::Unknown => {}
                }
            }
            prefs
        });
        prefs.brightness = prefs.brightness();
        self.vertical = prefs.mode == crate::reader_prefs::Mode::Vertical;
        self.spread = prefs.mode == crate::reader_prefs::Mode::Spread;
        self.rtl = prefs.rtl;
        self.crop = prefs.crop;
        self.fit_width = prefs.fit_width;
        self.brightness = prefs.brightness;
        self.spread_offset = prefs.spread_offset;
    }
    pub(super) fn current_prefs(&self) -> crate::reader_prefs::ReaderPrefs {
        crate::reader_prefs::ReaderPrefs {
            mode: if self.vertical {
                crate::reader_prefs::Mode::Vertical
            } else if self.spread {
                crate::reader_prefs::Mode::Spread
            } else {
                crate::reader_prefs::Mode::Paged
            },
            rtl: self.rtl,
            crop: self.crop,
            fit_width: self.fit_width,
            brightness: self.brightness,
            spread_offset: self.spread_offset,
        }
    }
    /// Changes made inside the reader always land on the current work.
    pub(super) fn save_reader_prefs(&mut self) {
        let Screen::Reader(id) = self.screen.clone() else {
            return;
        };
        let key = crate::reader_prefs::key_for(self.reading_metadata(&id).as_ref(), &id);
        self.reader_prefs.insert(key, self.current_prefs());
        if let Err(error) = crate::reader_prefs::save(&self.root, &self.reader_prefs) {
            self.error = Some(format!("阅读设置保存失败：{error:#}"));
        }
    }
    /// The "default for every work" action copies the live settings into the
    /// global keys works without their own entry resolve from.
    pub(super) fn save_reader_globals(&mut self) {
        self.reader_globals = self.current_prefs();
        self.setting(
            "reading-mode",
            match self.reader_globals.mode {
                crate::reader_prefs::Mode::Vertical => "vertical",
                crate::reader_prefs::Mode::Spread => "spread",
                crate::reader_prefs::Mode::Paged => "paged",
            }
            .into(),
        );
        self.setting(
            "reading-direction",
            if self.rtl { "rtl" } else { "ltr" }.into(),
        );
        self.setting("reader-crop", self.crop.to_string());
        self.setting("reader-fit", self.fit_width.to_string());
        self.setting("reader-brightness", self.brightness.to_string());
        self.setting("reader-spread-offset", self.spread_offset.to_string());
    }
    pub(super) fn open_reader(&mut self, book: &Book) {
        self.flush_position();
        self.apply_reader_prefs(book);
        if !matches!(self.screen, Screen::Reader(_)) {
            self.reader_return = self.screen.clone();
        }
        self.page = book.current_page.min(book.page_count.saturating_sub(1));
        self.chapter_end = false;
        self.keep_awake(self.keep_awake_setting);
        self.screen = Screen::Reader(book.id.clone());
        self.vertical_reader.reset(self.page, book.page_fraction);
        self.vertical_prev = None;
        self.vertical_next = None;
        self.next_stream = None;
        self.saved_position = None;
        self.chapter_dialog = None;
        self.reader_options = false;
        self.pending_reader_tap = None;
        self.pages
            .retain(|(id, page), _| id == &book.id && page.abs_diff(self.page) <= 2);
        self.zoom = 1.;
        self.pan = Vec2::ZERO;
        self.controls = true;
        self.controls_anim = None;
        self.page_slide = motion::PageSlide::default();
        self.page_slide.page = self.page;
        self.zoom_anim.cancel();
        self.page_generation += 1;
        self.worker
            .generation
            .store(self.page_generation, Ordering::Relaxed);
        self.page_pending.clear();
        self.prefetch_blocked.clear();
        self.page_errors.clear();
        self.ask_page(&book.id, self.page);
        if self.pages.contains_key(&(book.id.clone(), self.page)) {
            self.record_page(&book.id, self.page);
        }
    }
    pub(super) fn next_page(&mut self, id: &str, delta: i32, now: f64, viewport_w: f32) {
        self.pending_reader_tap = None;
        let old_page = self.page;
        let Some(book) = self.reader_book(id) else {
            return;
        };
        let target = self.page as i64 + delta as i64;
        let streaming_unknown = self
            .stream
            .as_ref()
            .is_some_and(|stream| stream.book.id == id && stream.done == 0 && book.page_count <= 1);
        if self.chapter_end {
            // End card: forward continues into the next chapter, back
            // returns to the real last page. Progress never leaves [0, count).
            self.chapter_end = false;
            if delta > 0 {
                if self.open_neighbor(id, 1) {
                    return;
                }
                self.chapter_end = true; // 已是最新一章 — stay on the card.
            }
            return;
        }
        if target >= book.page_count as i64 && !streaming_unknown {
            // Paged mode shows the 本章完 card first; the vertical strip
            // crosses chapters on its own separator path.
            if !self.vertical {
                self.chapter_end = true;
                return;
            }
            if self.open_neighbor(id, 1) {
                return;
            }
        }
        if target < 0 && !streaming_unknown {
            if self.open_neighbor(id, -1) {
                return;
            }
        }
        let page = (self.page as i64 + delta as i64)
            .clamp(0, book.page_count.saturating_sub(1) as i64) as usize;
        if page == self.page {
            return;
        }
        self.page = page;
        self.stats.page_turned(crate::follows::now() as f64);
        let base = self.vertical_prev.as_ref().map(|(_, c)| *c).unwrap_or(0);
        self.vertical_reader.reset(base + page, 0.);
        self.pan = Vec2::ZERO;
        self.page_generation += 1;
        self.worker
            .generation
            .store(self.page_generation, Ordering::Relaxed);
        self.page_pending.clear();
        self.prefetch_blocked.clear();
        self.pages
            .retain(|(key, p), _| key == id && p.abs_diff(page) <= 2);
        self.page_errors
            .retain(|(key, p), _| key == id && p.abs_diff(page) <= 2);
        self.ask_page(id, page);
        if self.pages.contains_key(&(id.to_string(), page)) {
            self.record_page(id, page);
        }
        // Tap, button and keyboard turns ease the incoming page in from a
        // small strip offset (instant under reduced motion); a committed
        // finger drag re-arms the turn with the real drag offset instead.
        if matches!(&self.screen, Screen::Reader(current) if current == id)
            && !self.vertical
            && self.zoom <= 1.
        {
            let dir_sign = if (delta > 0) ^ self.rtl { 1. } else { -1. };
            let start_offset = dir_sign * viewport_w * 0.24;
            self.page_slide.page = page;
            self.slide_turn_gap = start_offset;
            self.page_slide
                .animate_turn(old_page, start_offset, now, self.reduced);
        }
    }
    pub(super) fn reader_input_blocked(&self) -> bool {
        self.reader_options
            || self.chapter_dialog.is_some()
            || self.track_panel.is_some()
            || self.error.is_some()
    }
    pub(super) fn set_reading_mode(&mut self, mode: &str) {
        if self.vertical == (mode == "vertical") && self.spread == (mode == "spread") {
            return;
        }
        self.flush_position();
        self.vertical = mode == "vertical";
        self.spread = mode == "spread";
        self.vertical_reader.reset(self.page, 0.);
        self.vertical_prev = None;
        self.vertical_next = None;
        self.next_stream = None;
        self.zoom = 1.;
        self.pan = Vec2::ZERO;
        self.page_slide.cancel();
        self.zoom_anim.cancel();
        self.pending_reader_tap = None;
        if matches!(self.screen, Screen::Reader(_)) {
            self.save_reader_prefs();
        } else {
            self.reader_globals.mode = if self.vertical {
                crate::reader_prefs::Mode::Vertical
            } else if self.spread {
                crate::reader_prefs::Mode::Spread
            } else {
                crate::reader_prefs::Mode::Paged
            };
            self.setting("reading-mode", mode.into());
        }
    }
    /// Transition-driven keep-awake calls (platform no-ops on desktop).
    pub(super) fn keep_awake(&mut self, on: bool) {
        if self.awake == on {
            return;
        }
        self.awake = on;
        yejian_platform::set_keep_awake(on);
        #[cfg(test)]
        self.awake_log.push(on);
    }
    pub(super) fn reader_tap(
        &mut self,
        id: &str,
        relative: f32,
        step: i32,
        now: f64,
        viewport_w: f32,
    ) {
        self.stats.activity(crate::follows::now() as f64);
        if self.vertical || self.zoom > 1. || (0.27..=0.73).contains(&relative) {
            let current = self
                .controls_anim
                .map(|t| t.value(now))
                .unwrap_or(if self.controls { 1. } else { 0. });
            self.controls = !self.controls;
            self.controls_anim = Some(motion::TimedTween::new(
                now,
                if self.reduced { 0. } else { 0.18 },
                current,
                if self.controls { 1. } else { 0. },
            ));
        } else {
            match self.tap_zones.as_str() {
                "next" => self.next_page(id, step, now, viewport_w),
                "off" => {}
                _ => {
                    if relative < 0.27 {
                        self.next_page(id, if self.rtl { step } else { -step }, now, viewport_w);
                    } else {
                        self.next_page(id, if self.rtl { -step } else { step }, now, viewport_w);
                    }
                }
            }
        }
    }
    /// "第 X 话 · 标题" for the vertical chapter separator.
    pub(super) fn chapter_separator_label(&self, seg: &str) -> String {
        let metadata = self
            .chapter(seg)
            .or_else(|| self.stream_for(seg).map(|stream| &stream.metadata));
        metadata
            .map(|meta| {
                meta.chapter_number
                    .map(|n| {
                        let number = format!("第 {n} 话");
                        if meta.chapter_title.starts_with(&number) {
                            meta.chapter_title.clone()
                        } else {
                            format!("{number} · {}", meta.chapter_title)
                        }
                    })
                    .unwrap_or_else(|| meta.chapter_title.clone())
            })
            .unwrap_or_else(|| seg.into())
    }
    /// "21:34 · 85%", or just the time when no battery reading exists.
    pub(super) fn status_text(time: &str, battery: Option<f32>) -> String {
        match battery {
            Some(level) => format!("{time} · {:.0}%", level.clamp(0., 1.) * 100.),
            None => time.into(),
        }
    }
    /// Battery charge 0.0–1.0, refreshed at most once per minute.
    pub(super) fn cached_battery(&mut self) -> Option<f32> {
        let stale = self
            .battery_at
            .map(|at| at.elapsed().as_secs() >= 60)
            .unwrap_or(true);
        if stale {
            self.battery = yejian_platform::battery_level();
            self.battery_at = Some(std::time::Instant::now());
        }
        self.battery
    }
    /// The 本章完 card shown after the last page (one per chapter).
    pub(super) fn chapter_end_card(&mut self, ui: &mut Ui, content: Rect, id: &str) {
        let palette = crate::design::palette(true);
        let width = (content.width() - 48.).min(340.);
        let card = Rect::from_center_size(content.center(), vec2(width, 220.));
        ui.painter().rect_filled(card, 14., palette.soft);
        ui.painter().rect_stroke(
            card,
            14.,
            Stroke::new(1., palette.line),
            StrokeKind::Inside,
        );
        ui.painter().text(
            pos2(card.center().x, card.top() + 48.),
            Align2::CENTER_CENTER,
            "本章完",
            FontId::proportional(24.),
            palette.ink,
        );
        match self.next_chapter_name(id) {
            Some(name) => {
                ui.painter().text(
                    pos2(card.center().x, card.top() + 92.),
                    Align2::CENTER_CENTER,
                    name,
                    FontId::proportional(13.),
                    palette.muted,
                );
                let button = Rect::from_center_size(
                    pos2(card.center().x, card.bottom() - 48.),
                    vec2(160., 44.),
                );
                self.hitboxes.insert("chapter-end-continue".into(), button);
                let response =
                    ui.interact(button, Id::unique("chapter-end-continue"), Sense::click());
                response.widget_info(|| WidgetInfo::labeled(Role::Button, true, "继续"));
                let fill = if response.hovered() {
                    palette.primary.gamma_multiply(0.88)
                } else {
                    palette.primary
                };
                ui.painter().rect_filled(button, 10., fill);
                ui.painter().text(
                    button.center(),
                    Align2::CENTER_CENTER,
                    "继续",
                    FontId::proportional(15.),
                    palette.on_primary,
                );
                if response.clicked() {
                    self.next_page(
                        id,
                        1,
                        ui.ctx().input(|i| i.time),
                        content.width(),
                    );
                }
            }
            None => {
                ui.painter().text(
                    pos2(card.center().x, card.bottom() - 48.),
                    Align2::CENTER_CENTER,
                    "已是最新一章",
                    FontId::proportional(14.),
                    palette.muted,
                );
            }
        }
    }
    pub(super) fn reader_ui(&mut self, ui: &mut Ui, area: Rect, id: &str, now: f64) {
        let Some(book) = self.reader_book(id) else {
            return;
        };
        let ctx = ui.ctx().clone();
        ui.painter()
            .rect_filled(area, 0., Color32::from_rgb(18, 17, 16));
        self.ask_page(id, self.page);
        let step: i32 = if self.spread && !self.vertical && area.width() > 700. {
            if self.spread_offset && self.page == 0 {
                1
            } else {
                2
            }
        } else {
            1
        };
        let (pair_base, _) = spread_pair(self.page, self.spread_offset && step != 1);
        if pair_base + 1 < book.page_count {
            self.ask_page(id, pair_base + 1);
        }
        if pair_base != self.page {
            self.ask_page(id, pair_base);
        }
        // `page_viewport` never moves when the chrome toggles — all page
        // scaling, centering, panning and the dim overlay use it, so showing
        // or hiding the bars never resizes the comic. `canvas_hit` is the
        // gesture surface only, and still yields to the visible chrome.
        let page_viewport = area.shrink2(vec2(8., 0.));
        // The page always fits inside the band between the chrome bars.
        let page_fit = area.shrink2(vec2(8., 64.));
        let canvas_hit = area.shrink2(vec2(8., if self.controls { 64. } else { 0. }));
        let input_blocked = self.reader_input_blocked();
        let gesture = ui.interact(
            canvas_hit,
            Id::unique("reader-canvas"),
            if input_blocked {
                Sense::hover()
            } else {
                Sense::click_and_drag()
            },
        );
        self.hitboxes.insert("reader-canvas".into(), canvas_hit);
        let fit_single = self.fit_width && !self.vertical && step == 1;
        let page_swipe = !self.vertical && self.zoom <= 1. && !fit_single;
        let zoom = if input_blocked {
            1.
        } else {
            ui.input(|i| i.zoom_delta())
        };
        if zoom != 1. {
            self.zoom_anim.cancel();
            self.page_slide.cancel();
            self.zoom = (self.zoom * zoom).clamp(1., 5.);
            self.pending_reader_tap = None;
            if self.zoom == 1. {
                self.pan = Vec2::ZERO;
            }
        }
        if let Some((zoom, [px, py])) = self.zoom_anim.sample(now) {
            self.zoom = zoom;
            self.pan = vec2(px, py);
        }
        if gesture.drag_started() {
            self.reader_drag = 0.;
            self.reader_drag_y = 0.;
            self.vertical_reader.stop();
            self.pending_reader_tap = None;
            if page_swipe {
                self.page_slide.begin_drag();
                // Warm the neighbours so a committed swipe lands on a
                // decoded page instead of a placeholder.
                if self.page + (step as usize) < book.page_count {
                    self.ask_page(id, self.page + step as usize);
                }
                if self.page >= step as usize {
                    self.ask_page(id, self.page - step as usize);
                }
            }
        }
        if gesture.dragged() {
            let delta = ui.input(|i| i.pointer.delta());
            self.reader_drag += delta.x;
            self.reader_drag_y += delta.y;
            if page_swipe {
                let can_prev = self.page > 0 || self.has_neighbor(id, -1);
                let can_next = self.page + (step as usize) < book.page_count
                    || self.has_neighbor(id, 1);
                self.page_slide.drag(
                    delta.x,
                    can_prev,
                    can_next,
                    self.rtl,
                    page_viewport.width(),
                );
            }
        }
        if gesture.dragged() && (self.zoom > 1. || fit_single) {
            let delta = ui.input(|i| i.pointer.delta());
            self.zoom_anim.cancel();
            self.pan += if self.vertical {
                vec2(delta.x, 0.)
            } else if self.zoom > 1. {
                delta
            } else {
                vec2(0., delta.y)
            };
        }
        if gesture.drag_stopped() && self.zoom <= 1. && !self.vertical {
            let distance = self.reader_drag;
            let distance_y = self.reader_drag_y;
            self.reader_drag = 0.;
            self.reader_drag_y = 0.;
            if page_swipe {
                if distance.abs() > 45. {
                    let old_page = self.page;
                    let old_book = id.to_owned();
                    let drag_off = self.page_slide.current_offset(now);
                    self.next_page(
                        id,
                        if (distance < 0.) ^ self.rtl {
                            step
                        } else {
                            -step
                        },
                        now,
                        page_viewport.width(),
                    );
                    if self.page != old_page
                        && matches!(&self.screen, Screen::Reader(current) if current == &old_book)
                    {
                        // The neighbour followed the finger one strip width
                        // away; it now settles from where the drag left it.
                        let sign = if (distance < 0.) ^ self.rtl { 1. } else { -1. };
                        let start_offset = drag_off
                            + if drag_off < 0. {
                                page_viewport.width()
                            } else {
                                -page_viewport.width()
                            };
                        self.page_slide.page = self.page;
                        self.slide_turn_gap = sign * page_viewport.width();
                        self.page_slide
                            .animate_turn(old_page, start_offset, now, self.reduced);
                    } else {
                        self.page_slide.rebound(now, self.reduced);
                    }
                } else {
                    self.page_slide.rebound(now, self.reduced);
                }
            } else if distance.abs() > 45. {
                self.next_page(
                    id,
                    if (distance < 0.) ^ self.rtl {
                        step
                    } else {
                        -step
                    },
                    now,
                    page_viewport.width(),
                );
            } else if fit_single && distance_y.abs() > 45. {
                // Once the fitted page cannot pan any further, a vertical
                // swipe turns like the usual next/previous controls.
                if distance_y < 0. && self.pan.y <= -self.reader_fit_limit + 1. {
                    self.next_page(id, step, now, page_viewport.width());
                } else if distance_y > 0. && self.pan.y >= self.reader_fit_limit - 1. {
                    self.next_page(id, -step, now, page_viewport.width());
                }
            }
        }
        if input_blocked {
            self.pending_reader_tap = None;
            self.vertical_reader.stop();
        } else {
            // Wait for the same double-click interval as egui. Otherwise the
            // first tap turns a page before the second can request zoom.
            let tapped = gesture
                .clicked()
                .then(|| gesture.interact_pointer_pos())
                .flatten();
            let double_tap = tapped.is_some_and(|pos| {
                self.pending_reader_tap.is_some_and(|(deadline, first)| {
                    now <= deadline
                        && first.distance(pos)
                            <= ctx.options(|options| options.input_options.max_click_dist)
                })
            });
            if double_tap {
                self.pending_reader_tap = None;
                if let Some(pos) = tapped {
                    if self.zoom <= 1.01 {
                        // Focal zoom: the tapped point stays put as the page
                        // doubles, then the target pan is clamped to the 2x
                        // travel limits so nothing lands off-canvas.
                        let target_zoom = 2.0_f32;
                        let mut target_pan =
                            (page_viewport.center() - pos) * (target_zoom - 1.);
                        if let Some(texture) = self.pages.get(&(id.into(), self.page)) {
                            let size = texture.shown_size(self.crop);
                            let scale = (page_viewport.width() / size.x)
                                .min(page_viewport.height() / size.y);
                            let limit_2x = ((size * scale * target_zoom
                                - page_viewport.size())
                                * 0.5)
                                .max(Vec2::ZERO);
                            target_pan.x = target_pan.x.clamp(-limit_2x.x, limit_2x.x);
                            target_pan.y = target_pan.y.clamp(-limit_2x.y, limit_2x.y);
                        }
                        self.zoom_anim.start(
                            self.zoom,
                            target_zoom,
                            [self.pan.x, self.pan.y],
                            [target_pan.x, target_pan.y],
                            now,
                            self.reduced,
                        );
                    } else {
                        self.zoom_anim.start(
                            self.zoom,
                            1.0,
                            [self.pan.x, self.pan.y],
                            [0., 0.],
                            now,
                            self.reduced,
                        );
                    }
                }
            }
            if let Some((deadline, pos)) = self.pending_reader_tap {
                if now >= deadline && !ctx.input(|i| i.pointer.primary_down()) {
                    self.pending_reader_tap = None;
                    self.reader_tap(
                        id,
                        (pos.x - canvas_hit.left()) / canvas_hit.width(),
                        step,
                        now,
                        page_viewport.width(),
                    );
                } else {
                    ctx.request_repaint_after(std::time::Duration::from_secs_f64(
                        (deadline - now).max(0.01),
                    ));
                }
            }
            if !double_tap && let Some(pos) = tapped {
                if let Some((_, first)) = self.pending_reader_tap.take() {
                    self.reader_tap(
                        id,
                        (first.x - canvas_hit.left()) / canvas_hit.width(),
                        step,
                        now,
                        page_viewport.width(),
                    );
                }
                let delay = ctx.options(|options| options.input_options.max_double_click_delay);
                self.pending_reader_tap = Some((now + delay, pos));
                ctx.request_repaint_after(std::time::Duration::from_secs_f64(delay));
            }
        }
        if !input_blocked && !ctx.egui_wants_keyboard_input() {
            let delta = ctx.input(|i| {
                if !self.vertical && (i.key_pressed(Key::PageDown) || i.key_pressed(Key::Space)) {
                    step
                } else if !self.vertical && i.key_pressed(Key::PageUp) {
                    -step
                } else if i.key_pressed(Key::ArrowRight) {
                    if self.rtl { -step } else { step }
                } else if i.key_pressed(Key::ArrowLeft) {
                    if self.rtl { step } else { -step }
                } else {
                    0
                }
            });
            if delta != 0 {
                self.next_page(id, delta, now, page_viewport.width());
            }
        }
        if !matches!(&self.screen,Screen::Reader(current) if current==id) {
            return;
        }
        if !self.vertical && self.zoom > 1.5 {
            self.ask_hires(id);
        }
        self.rendered_page_nodes = 0;
        let slide_x = if self.vertical || self.zoom > 1. {
            0.
        } else {
            self.page_slide.current_offset(now)
        };
        self.page_slide.tick(now);
        if self.chapter_end {
            self.chapter_end_card(ui, page_fit, id);
        } else if self.vertical {
            self.vertical_ui(ui, page_viewport, id, book.page_count, now, &gesture);
        } else if let Some(error) = self.page_errors.get(&(id.into(), self.page)).cloned() {
            ui.put(
                Rect::from_center_size(
                    page_viewport.center(),
                    vec2(page_viewport.width() - 40., 120.),
                ),
                Label::new(
                    RichText::new(format!("这一页暂时无法打开\n{error}")).color(Color32::WHITE),
                )
                .wrap(),
            );
            if self.button(
                ui,
                "retry-page",
                Rect::from_center_size(page_viewport.center() + vec2(0., 90.), vec2(144., 44.)),
                "重试这一页",
                false,
            ) {
                self.page_errors.remove(&(id.into(), self.page));
                self.ask_page(id, self.page);
            }
        } else if let Some(texture) = self.pages.get(&(id.into(), self.page)) {
            let pair = step == 2;
            let (base, partner) = if pair {
                spread_pair(self.page, self.spread_offset)
            } else {
                (self.page, None)
            };
            let first_on_right = pair && self.rtl;
            let available = if pair {
                Rect::from_min_size(
                    page_fit.min
                        + vec2(
                            if first_on_right {
                                page_fit.width() / 2.
                            } else {
                                0.
                            },
                            0.,
                        ),
                    vec2(page_fit.width() / 2., page_fit.height()),
                )
            } else {
                page_fit
            };
            let painter = ui.painter().with_clip_rect(page_viewport);
            let crop = self.crop;
            let slot = |texture: &PageTexture, center: Pos2, pan: Vec2, zoom: f32| {
                let size = texture.shown_size(crop);
                let scale = (available.width() / size.x).min(available.height() / size.y);
                let target = Rect::from_center_size(center + pan, size * scale * zoom);
                texture.paint(&painter, target, crop);
                target
            };
            if base == self.page {
                let size = texture.shown_size(self.crop);
                let scale = if fit_single {
                    available.width() / size.x
                } else {
                    (available.width() / size.x).min(available.height() / size.y)
                };
                let size = size * scale * self.zoom;
                let limit = ((size - available.size()) * 0.5).max(Vec2::ZERO);
                self.reader_fit_limit = if pair { 0. } else { limit.y };
                self.pan.x = self.pan.x.clamp(-limit.x, limit.x);
                self.pan.y = self.pan.y.clamp(-limit.y, limit.y);
                let image_rect = Rect::from_center_size(
                    available.center() + self.pan + vec2(slide_x, 0.),
                    size,
                );
                self.hitboxes.insert("reader-image".into(), image_rect);
                texture.paint(&painter, image_rect, self.crop);
                self.rendered_page_nodes += 1;
            } else {
                self.reader_fit_limit = 0.;
                if let Some(first) = self.pages.get(&(id.into(), base)) {
                    slot(
                        first,
                        available.center() + vec2(slide_x, 0.),
                        self.pan,
                        self.zoom,
                    );
                    self.rendered_page_nodes += 1;
                } else {
                    painter.text(
                        available.center(),
                        Align2::CENTER_CENTER,
                        "正在打开这一页…",
                        FontId::proportional(15.),
                        Color32::LIGHT_GRAY,
                    );
                }
            }
            if let Some(partner) = partner {
                let second_center = available.center()
                    + vec2(
                        if first_on_right {
                            -available.width()
                        } else {
                            available.width()
                        },
                        0.,
                    )
                    + vec2(slide_x, 0.);
                if let Some(texture) = self.pages.get(&(id.into(), partner)) {
                    let target = slot(texture, second_center, self.pan, self.zoom);
                    if partner == self.page {
                        self.hitboxes.insert("reader-image".into(), target);
                    }
                    self.rendered_page_nodes += 1;
                } else if partner < book.page_count {
                    if let Some(error) = self
                        .page_errors
                        .get(&(id.to_string(), partner))
                        .cloned()
                    {
                        ui.put(
                            Rect::from_center_size(
                                second_center,
                                vec2(available.width() - 28., 130.),
                            ),
                            Label::new(
                                RichText::new(format!("第 {} 页无法打开\n{error}", partner + 1))
                                    .color(Color32::WHITE),
                            )
                            .wrap(),
                        );
                        if self.button(
                            ui,
                            "retry-spread-page",
                            Rect::from_center_size(second_center + vec2(0., 94.), vec2(144., 44.)),
                            "重试这一页",
                            false,
                        ) {
                            self.page_errors.remove(&(id.to_string(), partner));
                            self.prefetch_blocked.remove(&(id.to_string(), partner));
                            self.ask_page(id, partner);
                        }
                    } else {
                        ui.painter().text(
                            second_center,
                            Align2::CENTER_CENTER,
                            "正在打开下一页…",
                            FontId::proportional(15.),
                            Color32::LIGHT_GRAY,
                        );
                    }
                }
            }
            // A committed turn slides the outgoing page away one strip gap
            // behind the incoming one (single-page turns only).
            if step == 1
                && let Some((from_page, _)) = self.page_slide.outgoing(now)
                && let Some(outgoing) = self.pages.get(&(id.into(), from_page))
            {
                slot(
                    outgoing,
                    available.center() + vec2(slide_x - self.slide_turn_gap, 0.),
                    self.pan,
                    self.zoom,
                );
                self.rendered_page_nodes += 1;
            }
            // While the finger holds, the page the drag reveals follows at
            // exactly one strip width away.
            if step == 1 && gesture.dragged() && slide_x.abs() > 2. {
                let reveals_prev = if self.rtl { slide_x < 0. } else { slide_x > 0. };
                let neighbor = if reveals_prev {
                    self.page.checked_sub(1)
                } else {
                    (self.page + 1 < book.page_count).then_some(self.page + 1)
                };
                let neighbor_side = if slide_x > 0. { -1. } else { 1. };
                if let Some(neighbor) = neighbor
                    && let Some(texture) = self.pages.get(&(id.into(), neighbor))
                {
                    slot(
                        texture,
                        available.center()
                            + vec2(slide_x + neighbor_side * page_viewport.width(), 0.),
                        self.pan,
                        self.zoom,
                    );
                    self.rendered_page_nodes += 1;
                }
            }
        } else {
            let text = self
                .stream_waiting_text(id, self.page)
                .unwrap_or_else(|| "正在打开这一页…".into());
            ui.put(
                Rect::from_center_size(
                    page_viewport.center(),
                    vec2(page_viewport.width() - 40., 120.),
                ),
                Label::new(RichText::new(text).size(15.).color(Color32::LIGHT_GRAY)).wrap(),
            );
        }
        // Dragging past the chapter's first or last page surfaces a quiet
        // boundary hint at the edge being pulled open.
        if !self.vertical
            && step == 1
            && gesture.dragged()
            && slide_x.abs() > 28.
        {
            let reveals_prev = if self.rtl { slide_x < 0. } else { slide_x > 0. };
            let past_edge = if reveals_prev {
                self.page == 0
            } else {
                self.page + 1 >= book.page_count
            };
            if past_edge {
                let hint = if self.has_neighbor(id, if reveals_prev { -1 } else { 1 }) {
                    if reveals_prev {
                        "松手进入上一章"
                    } else {
                        "松手进入下一章"
                    }
                } else if reveals_prev {
                    "已是第一页"
                } else {
                    "已是最后一页"
                };
                let galley = ui.painter().layout(
                    hint.into(),
                    FontId::proportional(13.),
                    Color32::WHITE,
                    page_viewport.width() - 40.,
                );
                let pill_size = vec2(galley.size().x + 30., galley.size().y + 18.);
                let pill = Rect::from_center_size(
                    pos2(
                        if slide_x > 0. {
                            page_viewport.left() + pill_size.x / 2. + 10.
                        } else {
                            page_viewport.right() - pill_size.x / 2. - 10.
                        },
                        page_viewport.center().y,
                    ),
                    pill_size,
                );
                crate::design::soft_shadow(ui.painter(), pill, pill_size.y / 2., 1., true);
                ui.painter()
                    .rect_filled(pill, pill_size.y / 2., Color32::from_black_alpha(215));
                ui.painter().galley(
                    pos2(pill.left() + 15., pill.center().y - galley.size().y / 2.),
                    galley,
                    Color32::WHITE,
                );
            }
        }
        if self.brightness < 1. {
            ui.painter().rect_filled(
                page_viewport,
                0.,
                Color32::from_black_alpha(((1. - self.brightness) * 255.).round() as u8),
            );
        }
        if !matches!(&self.screen, Screen::Reader(current) if current == id) {
            return;
        }
        if let Some((task, reason)) = self
            .stream
            .as_ref()
            .filter(|stream| stream.book.id == id)
            .and_then(|stream| stream.stopped.clone().map(|reason| (stream.task.clone(), reason)))
        {
            let banner = Rect::from_center_size(
                pos2(page_viewport.center().x, page_viewport.bottom() - 60.),
                vec2((page_viewport.width() - 24.).min(420.), 96.),
            );
            ui.painter().rect_filled(banner, 12., Color32::from_black_alpha(200));
            ui.put(
                Rect::from_min_size(banner.min + vec2(12., 8.), vec2(banner.width() - 24., 36.)),
                Label::new(RichText::new(format!("下载中断：{reason}")).color(Color32::WHITE)).truncate(),
            );
            if self.button(
                ui,
                "stream-resume",
                Rect::from_center_size(pos2(banner.center().x, banner.bottom() - 30.), vec2(160., 44.)),
                "继续下载",
                true,
            ) {
                self.sources.resume_stream(&task);
            }
        }
        // The chrome fades and slides over a fixed page viewport, so it can
        // never make the comic jump. Interactive controls only register
        // while `controls` is on; a fading ghost carries them out.
        let chrome_t = self
            .controls_anim
            .map(|t| t.value(now))
            .unwrap_or(if self.controls { 1. } else { 0. });
        if self.controls_anim.is_some_and(|t| t.done(now)) {
            self.controls_anim = None;
        }
        if chrome_t > 0.001 {
            let palette = crate::design::palette(true);
            let (ink, muted) = (palette.ink, palette.muted);
            ui.style_mut().visuals = crate::design::visuals(true);
            let bar_fill = palette.surface.gamma_multiply(chrome_t);
            let hairline = Stroke::new(1., Color32::from_white_alpha((30. * chrome_t) as u8));
            let top_shift = -56. * (1. - chrome_t);
            let bottom_shift = 68. * (1. - chrome_t);
            let top_bar = Rect::from_min_max(
                area.min + vec2(0., top_shift),
                pos2(area.right(), area.top() + 56. + top_shift),
            );
            let bottom_bar = Rect::from_min_max(
                pos2(area.left(), area.bottom() - 68. + bottom_shift),
                pos2(area.right(), area.bottom() + bottom_shift),
            );
            ui.painter().rect_filled(top_bar, 0., bar_fill);
            ui.painter()
                .line_segment([top_bar.left_bottom(), top_bar.right_bottom()], hairline);
            ui.painter().rect_filled(bottom_bar, 0., bar_fill);
            ui.painter()
                .line_segment([bottom_bar.left_top(), bottom_bar.right_top()], hairline);
            if self.controls {
                if self.icon(
                    ui,
                    "reader-back",
                    Rect::from_min_size(area.min + vec2(8., 8. + top_shift), vec2(44., 44.)),
                    "back",
                    match (&self.reader_return, self.tab) {
                        (Screen::Library, Tab::History) => "返回阅读足迹",
                        (Screen::Library, Tab::Sources) => "返回书源",
                        (Screen::Library, _) => "返回书架",
                        _ => "返回详情",
                    },
                ) {
                    self.back(now, area);
                }
                let title_width = (area.width() - 56. - 216.).max(0.);
                let mut job = egui::text::LayoutJob::simple(
                    self.reading_title(&book),
                    FontId::proportional(15.),
                    ink,
                    title_width,
                );
                job.wrap.max_rows = 1;
                let galley = ui.painter().layout_job(job);
                ui.painter()
                    .with_clip_rect(Rect::from_min_max(
                        area.min + vec2(56., top_shift),
                        pos2(area.right() - 216., area.top() + 56. + top_shift),
                    ))
                    .galley(area.min + vec2(56., 20. + top_shift), galley, ink);
                if self.icon(
                    ui,
                    "reader-chapters",
                    Rect::from_min_size(
                        pos2(area.right() - 104., area.top() + 8. + top_shift),
                        vec2(44., 44.),
                    ),
                    "chapters",
                    if self.chapter(id).is_some() {
                        "章节与书签"
                    } else {
                        "书签"
                    },
                ) {
                    self.dialog_bookmarks = self.chapter(id).is_none();
                    self.chapter_dialog = Some(id.to_string());
                }
                let bookmarked = self.bookmarked(id);
                if self.icon(
                    ui,
                    "reader-bookmark",
                    Rect::from_min_size(
                        pos2(area.right() - 156., area.top() + 8. + top_shift),
                        vec2(44., 44.),
                    ),
                    if bookmarked { "bookmark-on" } else { "bookmark" },
                    if bookmarked { "取消书签" } else { "加书签" },
                ) {
                    self.bookmark_toggle(id);
                }
                let share_rect = Rect::from_min_size(
                    pos2(area.right() - 208., area.top() + 8. + top_shift),
                    vec2(44., 44.),
                );
                if self.share_enabled(id) {
                    if self.icon(ui, "reader-share", share_rect, "share", "分享本页") {
                        self.send(Command::SharePage {
                            id: id.to_string(),
                            page: self.page,
                        });
                    }
                } else {
                    self.hitboxes.insert("reader-share".into(), share_rect);
                    // Disabled until the page's original bytes exist.
                    let p = ui.painter();
                    let c = share_rect.center();
                    let stroke = Stroke::new(1.7, Color32::from_gray(140));
                    p.line_segment([c + vec2(-7., -1.), c + vec2(-7., 7.)], stroke);
                    p.line_segment([c + vec2(-7., 7.), c + vec2(7., 7.)], stroke);
                    p.line_segment([c + vec2(7., 7.), c + vec2(7., -1.)], stroke);
                    p.line_segment([c + vec2(0., 4.), c + vec2(0., -7.)], stroke);
                    p.line_segment([c + vec2(0., -7.), c + vec2(-4., -3.)], stroke);
                    p.line_segment([c + vec2(0., -7.), c + vec2(4., -3.)], stroke);
                }
                if self.icon(
                    ui,
                    "reader-settings",
                    Rect::from_min_size(
                        pos2(area.right() - 52., area.top() + 8. + top_shift),
                        vec2(44., 44.),
                    ),
                    "settings",
                    "阅读设置",
                ) {
                    self.reader_options = true;
                    self.reader_options_at = Some(now);
                    self.vertical_reader.stop();
                    self.pending_reader_tap = None;
                }
                if self.icon(
                    ui,
                    "reader-prev",
                    Rect::from_min_size(
                        pos2(area.left() + 10., bottom_bar.top() + 12.),
                        vec2(44., 44.),
                    ),
                    "prev",
                    "上一页",
                ) {
                    self.next_page(id, -step, now, page_viewport.width());
                }
                if self.icon(
                    ui,
                    "reader-next",
                    Rect::from_min_size(
                        pos2(area.right() - 54., bottom_bar.top() + 12.),
                        vec2(44., 44.),
                    ),
                    "next",
                    "下一页",
                ) {
                    self.next_page(id, step, now, page_viewport.width());
                }
                ui.painter().text(
                    pos2(area.center().x, bottom_bar.top() + 2.),
                    Align2::CENTER_TOP,
                    format!(
                        "{} / {}{}",
                        self.page + 1,
                        book.page_count,
                        if self.private { " · 私密阅读" } else { "" }
                    ),
                    FontId::monospace(12.),
                    muted,
                );
                let mut page_f32 = self.page as f32 + 1.;
                let slider_rect = Rect::from_min_size(
                    pos2(area.left() + 62., bottom_bar.top() + 18.),
                    vec2((area.width() - 124.).max(80.), 44.),
                );
                let out = ui
                    .scope_builder(UiBuilder::new().max_rect(slider_rect), |ui| {
                        crate::design::slider(
                            ui,
                            "reader-page-slider",
                            &mut page_f32,
                            1.0..=(book.page_count.max(1) as f32),
                            slider_rect.width(),
                            self.rtl,
                            true,
                        )
                    })
                    .inner;
                self.hitboxes.insert("reader-page-slider".into(), out.rect);
                if out.changed {
                    let target = (page_f32.round() as usize).saturating_sub(1);
                    self.next_page(
                        id,
                        target as i32 - self.page as i32,
                        now,
                        page_viewport.width(),
                    );
                    self.page_slide.cancel();
                }
                if out.dragging {
                    let text =
                        format!("第 {} / {} 页", page_f32.round() as usize, book.page_count);
                    let galley =
                        ui.painter().layout_no_wrap(text, FontId::proportional(13.), ink);
                    let bubble_size = vec2(galley.size().x + 26., galley.size().y + 14.);
                    let thumb_x = out.thumb_x.clamp(
                        area.left() + bubble_size.x / 2. + 6.,
                        area.right() - bubble_size.x / 2. - 6.,
                    );
                    let bubble = Rect::from_center_size(
                        pos2(thumb_x, bottom_bar.top() - 24.),
                        bubble_size,
                    );
                    crate::design::soft_shadow(ui.painter(), bubble, bubble_size.y / 2., 1.4, true);
                    ui.painter()
                        .rect_filled(bubble, bubble_size.y / 2., palette.surface);
                    ui.painter().rect_stroke(
                        bubble,
                        bubble_size.y / 2.,
                        Stroke::new(1., palette.line),
                        StrokeKind::Inside,
                    );
                    ui.painter()
                        .galley(bubble.center() - galley.size() / 2., galley, ink);
                }
            } else {
                // A fading ghost keeps the outgoing chrome sliding away;
                // nothing here is interactive any more.
                let mut ghost = ui.painter().clone();
                ghost.multiply_opacity(chrome_t);
                let icon_color = Color32::from_gray(232);
                crate::design::draw_icon(
                    &ghost,
                    pos2(area.left() + 30., area.top() + 30. + top_shift),
                    "back",
                    icon_color,
                    false,
                );
                crate::design::draw_icon(
                    &ghost,
                    pos2(area.right() - 82., area.top() + 30. + top_shift),
                    "chapters",
                    icon_color,
                    false,
                );
                crate::design::draw_icon(
                    &ghost,
                    pos2(area.right() - 134., area.top() + 30. + top_shift),
                    "bookmark",
                    icon_color,
                    false,
                );
                crate::design::draw_icon(
                    &ghost,
                    pos2(area.right() - 186., area.top() + 30. + top_shift),
                    "share",
                    icon_color,
                    false,
                );
                crate::design::draw_icon(
                    &ghost,
                    pos2(area.right() - 30., area.top() + 30. + top_shift),
                    "settings",
                    icon_color,
                    false,
                );
                ghost.text(
                    area.min + vec2(56., 20. + top_shift),
                    Align2::LEFT_TOP,
                    self.reading_title(&book),
                    FontId::proportional(15.),
                    ink,
                );
                crate::design::draw_icon(
                    &ghost,
                    pos2(area.left() + 32., bottom_bar.top() + 34.),
                    "prev",
                    icon_color,
                    false,
                );
                crate::design::draw_icon(
                    &ghost,
                    pos2(area.right() - 32., bottom_bar.top() + 34.),
                    "next",
                    icon_color,
                    false,
                );
                ghost.text(
                    pos2(area.center().x, bottom_bar.top() + 2.),
                    Align2::CENTER_TOP,
                    format!("{} / {}", self.page + 1, book.page_count),
                    FontId::monospace(12.),
                    muted,
                );
            }
        } else if self.reader_status {
            // Bottom-right 时间 · 电量 pill while the controls are hidden;
            // repaint once at each minute boundary, battery polled ≤1/min.
            let now = chrono::Local::now();
            let text = Self::status_text(&now.format("%H:%M").to_string(), self.cached_battery());
            let palette = crate::design::palette(true);
            let galley = ui.painter().layout_no_wrap(
                text,
                FontId::proportional(12.),
                Color32::from_white_alpha(230),
            );
            let size = galley.size() + vec2(20., 12.);
            let rect = Rect::from_min_size(
                pos2(area.right() - 10. - size.x, area.bottom() - 10. - size.y),
                size,
            );
            ui.painter()
                .rect_filled(rect, size.y / 2., Color32::from_black_alpha(120));
            ui.painter().galley(rect.center() - galley.size() / 2., galley, palette.muted);
            let wait = 61 - chrono::Timelike::second(&now) as u64;
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_secs(wait));
        }
    }
    pub(super) fn vertical_ui(
        &mut self,
        ui: &mut Ui,
        content: Rect,
        id: &str,
        count: usize,
        now: f64,
        gesture: &Response,
    ) {
        let input_blocked = self.reader_input_blocked();
        let scroll = if input_blocked {
            0.
        } else {
            ui.input(|i| i.smooth_scroll_delta.y)
        };
        // Scrolling/dragging is user input: it keeps the reading clock live.
        if scroll.abs() > 0.1 || gesture.dragged() {
            self.stats.activity(crate::follows::now() as f64);
        }
        self.vertical_reader.scroll(-scroll);
        if gesture.dragged() {
            self.vertical_reader
                .scroll(-ui.input(|i| i.pointer.delta().y));
        }
        if gesture.drag_stopped() {
            self.vertical_reader
                .release(-ui.input(|i| i.pointer.velocity().y), now, self.reduced);
        }
        if !input_blocked
            && !ui.ctx().egui_wants_keyboard_input()
            && ui.input(|i| i.key_pressed(Key::PageDown) || i.key_pressed(Key::Space))
        {
            self.vertical_reader.scroll(content.height() * 0.85);
        }
        if !input_blocked
            && !ui.ctx().egui_wants_keyboard_input()
            && ui.input(|i| i.key_pressed(Key::PageUp))
        {
            self.vertical_reader.scroll(-content.height() * 0.85);
        }
        if self.vertical_reader.tick(now) {
            ui.ctx().request_repaint();
        }
        let width = content.width() * self.zoom;
        self.pan.x = self.pan.x.clamp(
            -(width - content.width()) / 2.,
            (width - content.width()) / 2.,
        );
        // The strip is the current chapter plus at most one following
        // segment; global indices run across both. The next chapter is
        // queued once the anchor reaches its last pages, and joins the
        // strip as soon as its page count is known — until then a thin
        // band below the last page stands in for it.
        let prev_count = self.vertical_prev.as_ref().map(|(_, c)| *c).unwrap_or(0);
        let near_end = self.vertical_reader.page.saturating_sub(prev_count) + 3 >= count;
        let peek = self.peek_next(id, near_end);
        let loading_next = matches!(peek, Peek::Loading);
        match peek {
            Peek::Ready(seg_id, seg_count, seg_meta) => {
                if self
                    .vertical_next
                    .as_ref()
                    .is_none_or(|(old, _)| old != &seg_id)
                {
                    self.next_stream =
                        seg_meta.and_then(|meta| self.stream_segment_book(&seg_id, &meta));
                }
                self.vertical_next = Some((seg_id, seg_count));
            }
            Peek::Loading => {}
            Peek::Missing => {
                self.vertical_next = None;
                self.next_stream = None;
            }
        }
        let footer = if loading_next { 56. } else { 0. };
        let mut segments = self.strip_segments();
        let mut total: usize = segments.iter().map(|(_, c)| *c).sum();
        let mut heights = self.strip_heights(&segments, width);
        self.vertical_reader.settle(total, content.height() - footer, |g| {
            heights.get(&g).copied()
        });
        // The anchor entered the following segment: that chapter becomes
        // the reader's book, keeping the exact scroll offset.
        let cur_id = if self.vertical_reader.page >= prev_count + count
            && self.vertical_next.is_some()
        {
            let (new_id, _) = self.cross_vertical(id, count);
            segments = self.strip_segments();
            total = segments.iter().map(|(_, c)| *c).sum();
            heights = self.strip_heights(&segments, width);
            new_id
        } else {
            id.to_string()
        };
        let g = self.vertical_reader.page;
        if let Some((seg, local)) = Self::locate(&segments, g)
            && seg == cur_id
            && self.page != local
        {
            self.page = local;
            self.prefetch_blocked.clear();
        }
        self.trim_pages();
        for global in g.saturating_sub(1)..=(g + 2).min(total.saturating_sub(1)) {
            if let Some((seg, local)) = Self::locate(&segments, global) {
                self.ask_page(seg, local);
            }
        }
        let height = heights.get(&g).copied().unwrap_or(content.height());
        let mut top = content.top() - self.vertical_reader.top_offset(height);
        let start = g.saturating_sub(1);
        if start < g {
            top -= heights.get(&start).copied().unwrap_or(content.height());
        }
        let painter = ui.painter().with_clip_rect(content);
        for global in start..=(g + 2).min(total.saturating_sub(1)) {
            let Some((seg, local)) = Self::locate(&segments, global) else {
                continue;
            };
            let height = heights.get(&global).copied().unwrap_or(content.height());
            let target = Rect::from_min_size(
                pos2(content.center().x - width / 2. + self.pan.x, top),
                vec2(width, height),
            );
            top += height;
            if !target.intersects(content) {
                continue;
            }
            self.hitboxes
                .insert(format!("vertical-page-{global}"), target);
            // Chapter separator: a strip on the boundary between the
            // previous chapter's last page and this segment's first page.
            // Purely visual — `top`/`heights`/anchors are untouched.
            if local == 0 && global > 0 {
                let seam = target.top().clamp(content.top(), content.bottom());
                painter.line_segment(
                    [pos2(content.left(), seam), pos2(content.right(), seam)],
                    Stroke::new(1., Color32::from_white_alpha(40)),
                );
                // Compact floating pill on the seam — ellipsized to at most
                // 70% of the strip width. Purely visual; top/heights/anchors
                // are untouched.
                let mut job = egui::text::LayoutJob::simple(
                    self.chapter_separator_label(&seg),
                    FontId::proportional(12.),
                    Color32::from_white_alpha(235),
                    content.width() * 0.7 - 20.,
                );
                job.wrap.max_rows = 1;
                let galley = painter.layout_job(job);
                let band = Rect::from_center_size(
                    pos2(content.center().x, seam),
                    galley.size() + vec2(20., 10.),
                );
                painter.rect_filled(band, 10., Color32::from_black_alpha(150));
                painter.galley(band.center() - galley.size() / 2., galley, Color32::WHITE);
                self.hitboxes
                    .insert(format!("vertical-separator-{global}"), band);
            }
            if let Some(texture) = self.pages.get(&(seg.to_string(), local)) {
                texture.paint(&painter, target, self.crop);
                self.rendered_page_nodes += 1;
            } else if let Some(error) = self
                .page_errors
                .get(&(seg.to_string(), local))
                .cloned()
            {
                let center = target.intersect(content).center();
                ui.put(
                    Rect::from_center_size(center, vec2(content.width() - 40., 120.)),
                    Label::new(
                        RichText::new(format!("第 {} 页无法打开\n{error}", local + 1))
                            .color(Color32::WHITE),
                    )
                    .wrap(),
                );
                if self.button(
                    ui,
                    &format!("retry-page-{global}"),
                    Rect::from_center_size(center + vec2(0., 82.), vec2(144., 44.)),
                    "重试这一页",
                    false,
                ) {
                    self.page_errors.remove(&(seg.to_string(), local));
                    self.ask_page(seg, local);
                }
            } else {
                let text = self
                    .stream_waiting_text(seg, local)
                    .unwrap_or_else(|| "正在打开这一页…".into());
                crate::perf::note_blank_frame();
                painter.text(
                    target.intersect(content).center(),
                    Align2::CENTER_CENTER,
                    text,
                    FontId::proportional(15.),
                    Color32::LIGHT_GRAY,
                );
            }
        }
        if let Some((seg, local)) = Self::locate(&segments, g)
            && self.pages.contains_key(&(seg.to_string(), local))
            && (now - self.last_position_save >= 0.25
                || (!gesture.dragged() && !self.vertical_reader.active() && scroll.abs() < 0.1))
        {
            self.record_anchor(seg, local);
            self.last_position_save = now;
        }
        if loading_next
            && let Some(last) = self
                .hitboxes
                .get(&format!("vertical-page-{}", total.saturating_sub(1)))
                .copied()
        {
            let band = Rect::from_min_size(
                pos2(
                    content.center().x - (content.width() - 24.).min(420.) / 2.,
                    last.bottom() + 8.,
                ),
                vec2((content.width() - 24.).min(420.), 40.),
            );
            if band.intersects(content) {
                ui.painter().rect_filled(band, 10., Color32::from_black_alpha(150));
                ui.painter().text(
                    band.center(),
                    Align2::CENTER_CENTER,
                    "正在加载下一章…",
                    FontId::proportional(14.),
                    Color32::LIGHT_GRAY,
                );
            }
        }
        let first_id = segments
            .first()
            .map(|(seg, _)| seg.as_str())
            .unwrap_or(cur_id.as_str());
        if g == 0 && self.has_neighbor(first_id, -1) {
            let button = Rect::from_center_size(
                pos2(content.center().x, content.top() + 30.),
                vec2((content.width() - 24.).min(300.), 44.),
            );
            if self.button(ui, "reader-prev-chapter", button, "上一章", true) {
                self.open_neighbor(first_id, -1);
            }
        }
    }
}
pub(super) struct PageTexture {
    pub(super) tiles: Vec<TextureHandle>,
    pub(super) size: Vec2,
    /// Drawn content inside the full image, normalized to 0..1.
    pub(super) content: Rect,
}
impl PageTexture {
    /// Pixels the layout should measure when white/black margins are cropped.
    pub(super) fn shown_size(&self, crop: bool) -> Vec2 {
        if crop {
            vec2(
                self.size.x * self.content.width(),
                self.size.y * self.content.height(),
            )
        } else {
            self.size
        }
    }
    /// Paints the page; with `crop` only the content sub-rect is drawn,
    /// stretched to fill `rect`. Tiles still slice vertically.
    pub(super) fn paint(&self, painter: &Painter, rect: Rect, crop: bool) {
        let content = if crop {
            self.content
        } else {
            Rect::from_min_max(Pos2::ZERO, pos2(1., 1.))
        };
        let mut top = 0.;
        for tile in &self.tiles {
            let tile_height = tile.size()[1] as f32 / self.size.y;
            let (tile_top, tile_bottom) = (top, top + tile_height);
            top = tile_bottom;
            let (visible_top, visible_bottom) = (
                tile_top.max(content.top()),
                tile_bottom.min(content.bottom()),
            );
            if visible_bottom <= visible_top {
                continue;
            }
            let uv = Rect::from_min_max(
                pos2(content.left(), (visible_top - tile_top) / tile_height),
                pos2(content.right(), (visible_bottom - tile_top) / tile_height),
            );
            let target = Rect::from_min_size(
                pos2(
                    rect.left(),
                    rect.top()
                        + (visible_top - content.top()) / content.height() * rect.height(),
                ),
                vec2(
                    rect.width(),
                    (visible_bottom - visible_top) / content.height() * rect.height(),
                ),
            );
            if target.intersects(painter.clip_rect()) {
                painter.image(tile.id(), target, uv, Color32::WHITE);
            }
        }
    }
}
/// What follows the current chapter in the vertical strip.
pub(super) enum Peek {
    /// No next chapter.
    Missing,
    /// A next chapter exists but its page count is not known yet.
    Loading,
    /// `(segment id, page count, stream metadata when it is an online task)`.
    Ready(String, usize, Option<ChapterMetadata>),
}
/// The pair shown in two-page mode: with `spread_offset` page 0 stands
/// alone and the following spreads start on odd pages.
pub(super) fn spread_pair(page: usize, spread_offset: bool) -> (usize, Option<usize>) {
    if spread_offset && page == 0 {
        (0, None)
    } else if spread_offset && page % 2 == 0 {
        (page - 1, Some(page))
    } else {
        (page, Some(page + 1))
    }
}
/// A chapter read while its download is still running. Its position lives
/// only in memory until the finished chapter joins the library.
pub(super) struct StreamBook {
    pub(super) book: Book,
    pub(super) task: String,
    pub(super) metadata: ChapterMetadata,
    pub(super) done: usize,
    pub(super) stopped: Option<String>,
    pub(super) signature: Option<String>,
    /// Page to restore once the page count is known (`usize::MAX`: last).
    pub(super) resume: Option<usize>,
    pub(super) waiting: HashSet<usize>,
}
