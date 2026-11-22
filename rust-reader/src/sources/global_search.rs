






use egui::*;



use std::collections::HashSet;
use std::collections::VecDeque;




use std::sync::Arc;





use std::sync::mpsc;
use std::thread;


use yejian_sources::*;
use super::*;

/// How many sources may search at once in the global scope.
pub(super) const GLOBAL_SEARCH_LIMIT: usize = 4;

/// One source's slice of a global search; longer means "超时".
pub(super) const GLOBAL_SEARCH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

/// A global search request; retries reuse the generation of their run.
pub(super) enum GlobalCmd {
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
pub(super) enum GlobalEvent {
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
pub(super) trait GlobalEngine: Send + Sync {
    fn search(&self, query: &str) -> Result<Vec<Manga>, String>;
    fn chapters(&self, manga: Manga) -> Result<Vec<Chapter>, String>;
}

pub(super) struct RealGlobalEngine(pub(super) Arc<SourceEngine>);

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
pub(super) fn global_search_run(
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
pub(super) struct GlobalGroup {
    pub(super) source: String,
    pub(super) state: GlobalState,
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
    /// True while the search field targets every installed source.
    pub(super)fn global_mode(&self) -> bool {
        self.search_scope == "global"
    }
    /// Short Chinese reason for a global-search failure, plus whether the
    /// existing web-verification flow applies.
    pub(super)fn global_error_label(reason: &str) -> (&'static str, bool) {
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
    pub(super)fn global_search(&mut self) {
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
    pub(super)fn retry_global(&mut self, source: &str) {
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
    /// Group header: jump into that source and run the normal search.
    pub(super)fn dive_into(&mut self, source: &str) {
        self.select_source(source);
        self.search_scope = "current".into();
        self.scope_dirty = Some("current".into());
        self.start_search();
    }
    /// A cover from another source opens under that source, like 我的追更.
    pub(super)fn open_global_manga(&mut self, manga: Manga) {
        if manga.source_key == self.selected_source {
            self.open_manga(manga);
        } else {
            self.select_source(&manga.source_key.clone());
            self.pending_open = Some(manga);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sources::test_support::*;
    use std::sync::atomic::AtomicUsize;

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
}
