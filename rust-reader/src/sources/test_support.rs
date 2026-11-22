
use crate::follows::self;
use crate::follows::Follow;

#[cfg(test)]
use crate::source_repos::Lang;
#[cfg(test)]
use crate::source_repos::SourceRepos;

#[cfg(test)]
use egui::*;


use std::collections::HashMap;
#[cfg(test)]
use std::collections::HashSet;
#[cfg(test)]
use std::collections::VecDeque;
#[cfg(test)]
use std::io::Read;
#[cfg(test)]
use std::io::Write;
#[cfg(test)]
use std::path::Path;
#[cfg(test)]
use std::path::PathBuf;
#[cfg(test)]
use std::sync::Arc;

#[cfg(test)]
use std::sync::atomic::AtomicU8;
#[cfg(test)]
use std::sync::atomic::AtomicU64;


#[cfg(test)]
use std::sync::mpsc;
#[cfg(test)]
use std::thread;


use yejian_sources::*;
use super::*;
use super::repos::COMMUNITY_INDEX;
#[cfg(test)]
use super::downloads::ChapterQueueState;
use super::downloads::DownloadStatus;
use super::tasks::DownloadTask;
use super::migrate::MigrateConfirm;
use super::migrate::MigrateGroup;
use super::migrate::MigrateOutcome;
#[cfg(test)]
use super::ui_browse::ResultsState;
#[cfg(test)]
use super::downloads::STOP_NONE;
use super::engine::Task;
#[cfg(test)]
use super::updates::UpdateEvent;
use super::ui_detail::chapter_list;
#[cfg(test)]
use super::downloads::work_root;

#[cfg(test)]
pub(super) fn manga(source: &str, key: &str) -> Manga {
    Manga {
        source_key: source.into(),
        key: key.into(),
        title: key.into(),
        ..Default::default()
    }
}

#[cfg(test)]
pub(super) fn url_page(url: &str) -> Page {
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

#[cfg(test)]
pub(super) fn png() -> Vec<u8> {
    colored_png([36, 82, 94, 255])
}

#[cfg(test)]
pub(super) fn colored_png(pixel: [u8; 4]) -> Vec<u8> {
    let image = image::RgbaImage::from_pixel(4, 4, image::Rgba(pixel));
    let mut out = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(image)
        .write_to(&mut out, image::ImageFormat::Png)
        .unwrap();
    out.into_inner()
}

#[cfg(test)]
pub(super) fn source_manifest() -> SourceManifest {
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

#[cfg(test)]
pub(super) fn leb_u32(mut value: u32, out: &mut Vec<u8>) {
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

#[cfg(test)]
pub(super) fn wasm_section(id: u8, payload: &[u8], out: &mut Vec<u8>) {
    out.push(id);
    leb_u32(payload.len() as u32, out);
    out.extend_from_slice(payload);
}

#[cfg(test)]
pub(super) fn pages_wasm(urls: &[String]) -> Vec<u8> {
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

#[cfg(test)]
pub(super) fn serve_png(count: usize) -> (String, thread::JoinHandle<()>) {
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

#[cfg(test)]
pub(super) fn task(source: &str, manga_id: &str, chapter: &str) -> DownloadTask {
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

#[cfg(test)]
pub(super) fn test_sources(root: &Path) -> Sources {
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
        cover_loaded_at: Default::default(),
        reduced: false,
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

#[cfg(test)]
pub(super) fn detail_chapter(key: &str, number: f32) -> Chapter {
    Chapter {
        key: key.into(),
        title: Some(key.into()),
        chapter_number: Some(number),
        ..Default::default()
    }
}

#[cfg(test)]
pub(super) fn queue_state(
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

/// One `task-*` staging dir with payload bytes.
#[cfg(test)]
pub(super) fn stage_task(root: &Path, id: &str, bytes: usize) {
    let dir = work_root(root).join(format!("task-{id}"));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("page-1.img"), vec![7u8; bytes]).unwrap();
}

#[cfg(test)]
pub(super) fn numbered(key: &str, number: f32) -> Chapter {
    Chapter {
        key: key.into(),
        title: Some(format!("第 {number} 话")),
        chapter_number: Some(number),
        ..Default::default()
    }
}

#[cfg(test)]
pub(super) fn update(source: &str, manga: &str, chapters: Vec<Chapter>) -> UpdateEvent {
    UpdateEvent {
        source: source.into(),
        manga: manga.into(),
        chapters: Some(Ok(chapters)),
        cover: None,
    }
}

#[cfg(test)]
pub(super) fn installed(id: &str, version: i64) -> InstalledSource {
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
                include_bytes!("../../crates/source-engine/tests/fixtures/browser/source.json")
                    .as_slice(),
            ),
            (
                "filters.json",
                include_bytes!("../../crates/source-engine/tests/fixtures/browser/filters.json")
                    .as_slice(),
            ),
            (
                "main.wasm",
                include_bytes!("../../crates/source-engine/tests/fixtures/browser/main.wasm")
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
        pub(super) const PREVIEW_SOURCES: [&str; 5] = [
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
    #[cfg(test)]
    pub(crate) fn seed_online_detail(&mut self, manga: Manga, chapters: Vec<Chapter>) {
        self.selected_source = manga.source_key.clone();
        self.selected_manga = Some(manga);
        self.chapters = chapters;
    }
    #[cfg(test)]
    pub(crate) fn force_pages(&mut self, id: &str, total: usize, done: usize) {
        if let Some(task) = self.tasks.iter_mut().find(|task| task.id == id) {
            task.pages_total = total;
            task.pages_done = done;
        }
    }
}

