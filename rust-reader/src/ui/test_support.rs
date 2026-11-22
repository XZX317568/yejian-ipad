






use crate::worker::Command;




#[cfg(test)]
use egui::*;



use std::path::PathBuf;




use super::*;


impl ReaderApp {
    pub fn import_files(&mut self, paths: Vec<PathBuf>) {
        if !paths.is_empty() {
            self.send(Command::Import(paths));
        }
    }
    /// Simulator snapshots: jump to a screen once the library is readable.
    #[cfg(debug_assertions)]
    pub fn request_snapshot(&mut self, name: String) {
        self.snapshot_request = Some(name);
    }
    #[cfg(debug_assertions)]
    pub fn prepare_source_acceptance(&mut self, mode: &str) -> anyhow::Result<()> {
        self.tab = Tab::Sources;
        self.screen = Screen::Library;
        self.sources.preview_browser(mode)
    }
    #[cfg(debug_assertions)]
    pub fn source_browser_report(&self) -> serde_json::Value {
        self.sources.browser_report()
    }
    #[cfg(debug_assertions)]
    pub fn source_browser_ready(&self) -> bool {
        self.sources.browser_ready("browse")
    }
    #[cfg(debug_assertions)]
    pub fn prepare_global_search_acceptance(&mut self) -> anyhow::Result<()> {
        self.tab = Tab::Sources;
        self.screen = Screen::Library;
        self.sources.preview_global_search()
    }
    #[cfg(debug_assertions)]
    pub fn global_search_done(&self) -> bool {
        let groups = &self.source_browser_report()["groups"];
        groups
            .as_array()
            .map(|list| {
                !list.is_empty()
                    && list
                        .iter()
                        .all(|group| group["state"] != "searching")
            })
            .unwrap_or(false)
    }
    #[cfg(debug_assertions)]
    pub fn seed_search_history(&mut self, history: Vec<String>) {
        self.sources.set_search_history(history);
        self.sources.clear_query();
    }
    #[cfg(debug_assertions)]
    pub fn seed_categories(&mut self, categories: Vec<String>) {
        self.user_categories = categories;
    }
    #[cfg(debug_assertions)]
    pub fn open_category_manage(&mut self) {
        self.category_manage = true;
    }
    #[cfg(debug_assertions)]
    pub fn category_order(&self) -> &[String] {
        &self.user_categories
    }
    #[cfg(debug_assertions)]
    pub fn pick_categories(&mut self, ids: Vec<String>) {
        self.selected = ids.into_iter().collect();
        self.category_pick = true;
    }
    #[cfg(debug_assertions)]
    pub fn first_book_id(&self) -> Option<String> {
        self.books.first().map(|book| book.id.clone())
    }
    #[cfg(debug_assertions)]
    pub fn share_ready_now(&self) -> bool {
        matches!(&self.screen, Screen::Reader(id) if self.share_enabled(id))
    }
    #[cfg(debug_assertions)]
    pub fn last_shared(&self) -> Option<&std::path::Path> {
        self.last_shared.as_deref()
    }
    #[cfg(debug_assertions)]
    pub fn bookmark_count(&self) -> usize {
        self.bookmarks.entries().len()
    }
    #[cfg(debug_assertions)]
    pub fn prepare_share_stream(&mut self) {
        if let Some(chapter) = self.sources.preview_share_stream() {
            self.open_stream(chapter);
        }
    }
    #[cfg(debug_assertions)]
    pub fn prepare_installed_source_acceptance(&mut self, id: &str) -> anyhow::Result<()> {
        self.tab = Tab::Sources;
        self.screen = Screen::Library;
        self.sources.preview_installed_source(id)
    }
    #[cfg(debug_assertions)]
    pub fn prepare_auto_follow_acceptance(&mut self) {
        self.tab = Tab::Sources;
        self.screen = Screen::Library;
        self.sources.preview_auto_download();
    }
    #[cfg(debug_assertions)]
    pub fn prepare_migrate_acceptance(&mut self, stage: &str) {
        self.tab = Tab::Sources;
        self.screen = Screen::Library;
        self.sources.preview_migrate(stage);
    }
    /// Acceptance seed: grouped local/online history plus seven days of
    /// reading stats so 阅读足迹 renders all groups and the stats card.
    #[cfg(debug_assertions)]
    pub fn prepare_history_acceptance(&mut self) {
        self.tab = Tab::History;
        self.screen = Screen::Library;
        let now_ms = crate::follows::now() as i64 * 1000;
        for (index, book) in self.books.iter_mut().enumerate() {
            book.last_read_at = Some(match index % 4 {
                0 => now_ms - 600_000,
                1 => now_ms - 90_000_000,
                2 => now_ms - 200_000_000,
                _ => now_ms - 2_600_000_000,
            });
        }
        self.sources.preview_history();
        self.stats.seed_capture(crate::follows::now() as f64);
    }
    /// Acceptance seed: the 我的书源 manage view with an update badge, a
    /// finished 测试 result and a repository index for the filter capture.
    #[cfg(debug_assertions)]
    pub fn prepare_sources_manage_acceptance(&mut self) {
        self.tab = Tab::Sources;
        self.screen = Screen::Library;
        self.sources.preview_sources_manage();
    }
    /// Acceptance seam: open the opaque uninstall confirm for a fake source.
    #[cfg(debug_assertions)]
    pub fn open_uninstall_acceptance(&mut self) {
        self.prepare_sources_manage_acceptance();
        if let Some(source) = self.sources.installed_source("local.manage-a") {
            self.sources.ask_uninstall(source);
        }
    }
    /// Acceptance teardown: remove preview follows/tasks before the app is
    /// dropped so they never leak into the real saved state.
    #[cfg(debug_assertions)]
    pub fn clear_acceptance_previews(&mut self) {
        self.sources.clear_acceptance_previews();
    }
    /// Acceptance seam: fake device-owner support/auth results on desktop.
    #[cfg(debug_assertions)]
    pub fn set_app_lock_fake(
        &mut self,
        supported: bool,
        auth: Option<std::result::Result<(), String>>,
    ) {
        yejian_platform::set_app_lock_fake(supported, auth);
    }
    #[cfg(debug_assertions)]
    pub fn debug_lock(&mut self) {
        self.applock.lock();
    }
    #[cfg(debug_assertions)]
    pub fn app_lock_locked(&self) -> bool {
        self.applock.locked()
    }
    #[cfg(debug_assertions)]
    pub fn app_lock_enabled(&self) -> bool {
        self.applock.enabled()
    }
    /// Acceptance seam: the 新章节通知 toggle state.
    #[cfg(debug_assertions)]
    pub fn notify_updates_enabled(&self) -> bool {
        self.notify_enabled
    }
    /// Acceptance seam: point the WebDAV form at a local fake server.
    #[cfg(debug_assertions)]
    pub fn prepare_webdav_acceptance(&mut self, url: String) {
        self.cloud_url = url;
        self.cloud_user = "acceptance".into();
        self.cloud_password = "pw".into();
        self.cloud_dir = "yejian".into();
        self.cloud.enabled = true;
        self.cloud.config.url = self.cloud_url.clone();
        self.cloud.config.user = self.cloud_user.clone();
        self.cloud.config.password = self.cloud_password.clone();
        self.cloud.config.dir = self.cloud_dir.clone();
    }
    /// Acceptance seam: kick a sync round with the current snapshot.
    #[cfg(debug_assertions)]
    pub fn cloud_sync_once(&mut self) {
        self.cloud.sync_now(self.cloud_snapshot());
    }
    /// Acceptance seam: the last round finished without an error.
    #[cfg(debug_assertions)]
    pub fn cloud_synced(&self) -> bool {
        self.cloud.synced_ok()
    }
    /// Acceptance seam: a (day, device) stats cell merged from the remote.
    #[cfg(debug_assertions)]
    pub fn stats_day_seconds(&self, day: &str, device: &str) -> Option<u64> {
        self.stats
            .export_days()
            .get(day)
            .and_then(|devices| devices.get(device))
            .map(|stats| stats.seconds)
    }
    /// Acceptance seam: Bangumi requests go to a local fake server.
    #[cfg(debug_assertions)]
    pub fn set_bangumi_endpoint(&mut self, ctx: Context, base: String) {
        self.trackers
            .replace_for_test(self.root.clone(), ctx, base);
    }
    #[cfg(debug_assertions)]
    pub fn tracker_command(&mut self, command: crate::trackers::TrackCommand) {
        self.trackers.submit(command);
    }
    #[cfg(debug_assertions)]
    pub fn tracker_report(&self, key: &str) -> serde_json::Value {
        let binding = self.trackers.binding(key);
        serde_json::json!({
            "bangumi": self
                .trackers
                .account(crate::trackers::TrackerKind::Bangumi)
                .map(|account| account.name.clone()),
            "bound": binding.is_some(),
            "tracker": binding.map(|entry| entry.tracker.clone()),
        })
    }
    pub fn ready(&self) -> bool {
        self.ready
    }
    pub fn book_count(&self) -> usize {
        self.books.len()
    }
    /// Shelf entries including synthesised online follows.
    pub fn visible_count(&self) -> usize {
        self.visible_books().len()
    }
    pub fn current_page(&self) -> usize {
        self.page
    }
    /// The 本章完 card is showing past the last page.
    pub fn chapter_end(&self) -> bool {
        self.chapter_end
    }
    /// Strip segments joined so far, for vertical-flow acceptance checks.
    pub fn strip_segments_len(&self) -> usize {
        self.strip_segments().len()
    }
    pub fn is_vertical(&self) -> bool {
        self.vertical
    }
    /// Which input blocker is active, for acceptance diagnostics.
    pub fn block_reason(&self) -> &'static str {
        if self.reader_options {
            "options"
        } else if self.chapter_dialog.is_some() {
            "chapters"
        } else if self.track_panel.is_some() {
            "track"
        } else if self.error.is_some() {
            "error"
        } else {
            "none"
        }
    }
    pub fn current_fraction(&self) -> f32 {
        if self.vertical {
            self.vertical_reader.fraction
        } else {
            0.
        }
    }
    pub fn position_saved(&self) -> bool {
        match (&self.saved_position, &self.persisted_position) {
            (None, _) => true,
            (Some((id, page, fraction)), Some((stored, stored_page, stored_fraction))) => {
                id == stored && page == stored_page && (*fraction - *stored_fraction).abs() < 0.0001
            }
            _ => false,
        }
    }
    pub fn current_book_id(&self) -> Option<&str> {
        match &self.screen {
            Screen::Reader(id) => Some(id),
            _ => None,
        }
    }
    pub fn shelf_book_count(&self) -> usize {
        self.visible_books().len()
    }
    pub fn screen_name(&self) -> &'static str {
        match self.screen {
            Screen::Library => "library",
            Screen::Detail(_) => "detail",
            Screen::Reader(_) => "reader",
        }
    }
    pub fn page_loaded(&self) -> bool {
        match &self.screen {
            Screen::Reader(id) => self.pages.contains_key(&(id.clone(), self.page)),
            _ => false,
        }
    }
    /// Leaves the reader and returns to the shelf, for bench setups.
    pub fn leave_reader(&mut self) {
        if matches!(self.screen, crate::ui::Screen::Reader(_)) {
            self.back(0., egui::Rect::NOTHING);
        }
    }
    /// Switches the global reading mode for bench/acceptance setups.
    pub fn set_mode(&mut self, mode: &str) {
        self.set_reading_mode(mode);
    }
    /// Opens `id` directly in the reader, for bench-driven measurements.
    pub fn open_book(&mut self, id: &str) {
        if let Some(book) = self.books.iter().find(|book| book.id == id).cloned() {
            self.open_reader(&book);
        }
    }
    /// Book id of the nth shelf entry, for bench-driven opening.
    pub fn shelf_book_id(&self, index: usize) -> Option<String> {
        self.visible_books().get(index).map(|book| book.id.clone())
    }
    pub fn covers_loaded(&self) -> bool {
        self.cover_pending.is_empty()
    }
    /// Height of the painted 3D shelf stage, for layout verification.
    pub fn shelf_stage_height(&self) -> f32 {
        self.shelf_stage.height()
    }
    /// Fraction of the stage width given to the 3D books, for layout
    /// verification.
    pub fn shelf_books_fraction(&self) -> f32 {
        self.shelf_books.width() / self.shelf_stage.width()
    }
    /// Index of the centred book in the current visible order.
    pub fn focus_index(&self) -> usize {
        (self.motion.focus.round() as i32).rem_euclid(self.visible_books().len().max(1) as i32)
            as usize
    }
    /// Persisted key of the active shelf sort, for menu verification.
    pub fn shelf_sort_key(&self) -> &'static str {
        self.shelf_sort.key()
    }
    /// Title of the centred book, for sort-order verification.
    pub fn focused_book_title(&self) -> String {
        let books = self.visible_books();
        books
            .get(self.focus_index())
            .map(|book| book.title.clone())
            .unwrap_or_default()
    }
}
