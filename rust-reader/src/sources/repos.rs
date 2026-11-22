
use crate::follows::self;








use std::collections::HashSet;












use std::thread;


use yejian_sources::*;
use super::*;
use super::engine::Task;
use super::web::save_source_settings;
use super::web::url_host;

pub(super) const COMMUNITY_INDEX: &str = "https://aidoku-community.github.io/sources/index.min.json";

impl Sources {
    /// `source-repos` setting restored at startup.
    pub(crate) fn set_repos_setting(&mut self, json: &str) {
        self.repos.apply_urls(json);
    }
    /// `source-repos-checked-at` setting restored at startup (epoch day).
    pub(crate) fn set_repos_checked_day(&mut self, day: Option<i64>) {
        self.repos.checked_day = day;
    }
    /// Number of installed sources a cached index offers a newer build of.
    pub(crate) fn updatable_count(&self) -> usize {
        self.updatable().len()
    }
    pub(super)fn updatable(&self) -> std::collections::BTreeMap<String, i64> {
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
    pub(super)fn maybe_auto_repo_check(&mut self) {
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
    pub(super)fn apply_repo_all(&mut self, results: Vec<(String, Result<RepositoryIndex, String>)>) {
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
    pub(super)fn update_all(&mut self) {
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
    pub(super)fn test_all(&mut self) {
        for source in self.installed.clone() {
            self.test_source(&source.manifest.id);
        }
    }
    /// Starts queued tests while fewer than four run, marks timeouts.
    pub(super)fn pump_repo_tests(&mut self) {
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
    pub(super)fn drain_repo_tests(&mut self) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sources::test_support::*;

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
