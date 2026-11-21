//! 新章节通知 — dedup store, summary text, and the background check the iOS
//! `BGAppRefreshTask` handler runs. Everything here is UI-free: the handler
//! executes on a system thread with a hard deadline and only touches disk
//! state plus the source engine.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::anyhow;
use serde::{Deserialize, Serialize};
use yejian_platform::Notification;
use yejian_sources::{Chapter, Manga, SourceCatalog, SourceEngine};

use crate::follows::{self, Follow};
use crate::sources::manga_snapshot_path;

/// Follow key shared by dedup storage and notification payloads.
pub(crate) fn follow_key(source: &str, manga: &str) -> String {
    format!("{source}\u{1f}{manga}")
}

/// Payload `follows` — tapping opens the 我的追更 list.
pub(crate) const PAYLOAD_FOLLOWS: &str = "follows";

const FILE: &str = "notify-sent.json";
/// How many chapter keys stay recorded per follow — enough that a long
/// backlog can never re-notify, small enough to stay a few KB.
const SENT_CAP: usize = 128;
/// iOS gives a `BGAppRefreshTask` ~30 s; the check stops collecting at 25 s.
pub(crate) const BG_DEADLINE: Duration = Duration::from_secs(25);

#[derive(Default, Serialize, Deserialize)]
struct SentFile {
    /// follow_key → recently notified chapter keys (oldest first).
    sent: BTreeMap<String, Vec<String>>,
}

/// Persisted dedup: a chapter key already notified is never notified again,
/// even across restarts or a delete+re-add of the same work.
pub(crate) struct SentStore {
    sent: HashMap<String, HashSet<String>>,
    order: HashMap<String, VecDeque<String>>,
    path: PathBuf,
}

impl SentStore {
    pub(crate) fn load(root: &Path) -> Self {
        let path = root.join(FILE);
        let file: SentFile = std::fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        Self {
            sent: file
                .sent
                .iter()
                .map(|(key, list)| (key.clone(), list.iter().cloned().collect()))
                .collect(),
            order: file
                .sent
                .into_iter()
                .map(|(key, list)| (key, list.into()))
                .collect(),
            path,
        }
    }

    fn save(&self) -> anyhow::Result<()> {
        let file = SentFile {
            sent: self
                .order
                .iter()
                .map(|(key, order)| (key.clone(), order.iter().cloned().collect()))
                .collect(),
        };
        let bytes = serde_json::to_vec(&file)?;
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = self.path.with_extension("tmp");
        std::fs::write(&tmp, bytes)?;
        std::fs::rename(&tmp, &self.path).map_err(|error| {
            let _ = std::fs::remove_file(&tmp);
            anyhow!("写入 {} 失败：{error}", self.path.display())
        })
    }

    pub(crate) fn contains(&self, follow: &str, chapter: &str) -> bool {
        self.sent
            .get(follow)
            .is_some_and(|set| set.contains(chapter))
    }

    /// Records one key; the oldest beyond `SENT_CAP` drops off first.
    pub(crate) fn mark(&mut self, follow: &str, chapter: &str) {
        let set = self.sent.entry(follow.into()).or_default();
        if !set.insert(chapter.to_string()) {
            return;
        }
        let order = self.order.entry(follow.into()).or_default();
        order.push_back(chapter.to_string());
        while order.len() > SENT_CAP {
            if let Some(old) = order.pop_front() {
                set.remove(&old);
            }
        }
    }

    /// Records every key and persists in one atomic write.
    pub(crate) fn mark_all(&mut self, follow: &str, chapters: &[String]) {
        for chapter in chapters {
            self.mark(follow, chapter);
        }
        let _ = self.save();
    }
}

/// One follow's fresh chapters, ready to be turned into a notification.
pub(crate) struct FreshUpdate {
    pub source: String,
    pub manga: String,
    pub title: String,
    /// Fresh chapter keys that have not been notified before.
    pub added: Vec<String>,
}

/// One note per updated work — "《X》更新了 N 话" — plus a single rolled-up
/// "N 部追更有更新" when several works updated. The multi-work note carries
/// the `follows` payload so a tap lands on the list.
pub(crate) fn summarize(updates: &[FreshUpdate]) -> Vec<Notification> {
    if updates.is_empty() {
        return Vec::new();
    }
    if updates.len() == 1 {
        let update = &updates[0];
        return vec![Notification {
            id: 1,
            title: format!("《{}》更新了 {} 话", update.title, update.added.len()),
            body: update.title.clone(),
            payload: format!(
                "follow:{}\u{1f}{}",
                update.source, update.manga
            ),
        }];
    }
    let names = updates
        .iter()
        .take(3)
        .map(|update| format!("《{}》", update.title))
        .collect::<Vec<_>>()
        .join("、");
    let body = if updates.len() > 3 {
        format!("{names} 等")
    } else {
        names
    };
    vec![Notification {
        id: 1,
        title: format!("{} 部追更有更新", updates.len()),
        body,
        payload: PAYLOAD_FOLLOWS.into(),
    }]
}

/// Routes a tapped-notification payload. Returns the follow coordinates for
/// `follow:<source>\u{1f}<manga>` so the UI can open the detail page.
pub(crate) enum Route {
    Follow(String, String),
    Follows,
}

pub(crate) fn route(payload: &str) -> Option<Route> {
    if payload == PAYLOAD_FOLLOWS {
        return Some(Route::Follows);
    }
    let rest = payload.strip_prefix("follow:")?;
    let (source, manga) = rest.split_once('\u{1f}')?;
    Some(Route::Follow(source.to_string(), manga.to_string()))
}

/// Rebuilds a follow's `Manga` snapshot the way `Sources::follow_manga` does.
fn follow_manga(root: &Path, follow: &Follow) -> anyhow::Result<Manga> {
    let bytes = std::fs::read(manga_snapshot_path(root, &follow.snapshot)?)?;
    let mut manga = Manga::from_postcard_bytes(&bytes)
        .map_err(|error| anyhow!("漫画快照已损坏：{error}"))?;
    manga.source_key = follow.source_id.clone();
    Ok(manga)
}

/// Runs one chapter-list check per follow — same engine calls as the
/// in-app update worker, driven directly from disk state so it can run on
/// the background-refresh thread. Collects until `deadline`, diffs each
/// result against `known`/`fresh`/already-notified keys, marks and persists
/// what it notifies, and returns the notifications to post.
///
/// Never touches egui, worker channels, or in-memory app state: it reads
/// `follows.json` + manga snapshots, talks to the source engine, and writes
/// `notify-sent.json` — nothing else.
pub(crate) fn run_background_check(
    root: &Path,
    sent: &mut SentStore,
    deadline: Instant,
) -> Vec<Notification> {
    let Ok(follows) = follows::load(root) else {
        return Vec::new();
    };
    let jobs: Vec<Follow> = follows
        .into_iter()
        .filter(|follow| !follow.source_removed)
        .collect();
    if jobs.is_empty() {
        return Vec::new();
    }
    let (job_tx, job_rx) = mpsc::channel::<Follow>();
    let (done_tx, done_rx) = mpsc::channel::<(Follow, anyhow::Result<Vec<Chapter>>)>();
    let job_rx = std::sync::Arc::new(std::sync::Mutex::new(job_rx));
    for index in 0..4.min(jobs.len()) {
        let job_rx = job_rx.clone();
        let done_tx = done_tx.clone();
        let root = root.to_path_buf();
        thread::Builder::new()
            .name(format!("yejian-bg-check-{index}"))
            .spawn(move || {
                let catalog = SourceCatalog::new(root.join("sources"));
                let mut engines = HashMap::<String, SourceEngine>::new();
                while let Ok(follow) = job_rx.lock().expect("bg check job queue").recv() {
                    let result = (|| -> anyhow::Result<Vec<Chapter>> {
                        let manga = follow_manga(&root, &follow)?;
                        if !engines.contains_key(&follow.source_id) {
                            engines.insert(
                                follow.source_id.clone(),
                                SourceEngine::load(&catalog, &follow.source_id)?,
                            );
                        }
                        Ok(pollster::block_on(
                            engines[&follow.source_id].chapters_with(manga),
                        )?)
                    })();
                    if done_tx.send((follow, result)).is_err() {
                        return;
                    }
                }
            })
            .expect("Unable to start bg check worker");
    }
    drop(done_tx);
    for follow in jobs.iter().cloned() {
        let _ = job_tx.send(follow);
    }
    drop(job_tx);

    let mut updates = Vec::<FreshUpdate>::new();
    let total = jobs.len();
    for _ in 0..total {
        if Instant::now() >= deadline {
            break;
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        let Ok((follow, result)) = done_rx.recv_timeout(remaining) else {
            break;
        };
        let Ok(chapters) = result else {
            continue;
        };
        let key = follow_key(&follow.source_id, &follow.manga_id);
        let known: HashSet<&String> = follow.known.iter().chain(&follow.fresh).collect();
        let added: Vec<String> = chapters
            .iter()
            .map(|chapter| chapter.key.clone())
            .filter(|chapter| !known.contains(chapter) && !sent.contains(&key, chapter))
            .collect();
        if added.is_empty() {
            continue;
        }
        sent.mark_all(&key, &added);
        updates.push(FreshUpdate {
            source: follow.source_id,
            manga: follow.manga_id,
            title: follow.title,
            added,
        });
    }
    summarize(&updates)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    fn update(title: &str, added: usize) -> FreshUpdate {
        FreshUpdate {
            source: "src".into(),
            manga: "manga".into(),
            title: title.into(),
            added: (0..added).map(|i| format!("ch{i}")).collect(),
        }
    }

    #[test]
    fn summarize_single_work_names_it() {
        let notes = summarize(&[update("星降る王国", 3)]);
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].title, "《星降る王国》更新了 3 话");
        assert!(notes[0].payload.starts_with("follow:"));
    }

    #[test]
    fn summarize_multi_work_rolls_up() {
        let notes = summarize(&[
            update("一", 1),
            update("二", 2),
            update("三", 1),
            update("四", 5),
        ]);
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].title, "4 部追更有更新");
        assert!(notes[0].body.contains("等"));
        assert_eq!(notes[0].payload, PAYLOAD_FOLLOWS);
    }

    #[test]
    fn route_parses_follow_and_list_payloads() {
        match route("follow:src\u{1f}manga") {
            Some(Route::Follow(s, m)) => assert_eq!((s, m), ("src".into(), "manga".into())),
            _ => panic!("follow payload"),
        }
        assert!(matches!(route("follows"), Some(Route::Follows)));
        assert!(route("garbage").is_none());
        assert!(route("follow:no-separator").is_none());
    }

    #[test]
    fn sent_store_dedupes_and_persists() {
        let root = std::env::temp_dir().join(format!("yejian-notify-test-{}", follows::now()));
        let _ = std::fs::remove_dir_all(&root);
        let mut store = SentStore::load(&root);
        assert!(!store.contains("f", "c1"));
        store.mark_all("f", &["c1".into(), "c2".into()]);
        assert!(store.contains("f", "c1"));
        let reloaded = SentStore::load(&root);
        assert!(reloaded.contains("f", "c1") && reloaded.contains("f", "c2"));
        assert!(!reloaded.contains("f", "c3"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn sent_store_is_bounded() {
        let root = std::env::temp_dir().join(format!("yejian-notify-cap-{}", follows::now()));
        let _ = std::fs::remove_dir_all(&root);
        let mut store = SentStore::load(&root);
        for index in 0..(SENT_CAP + 10) {
            store.mark("f", &format!("c{index}"));
        }
        let _ = store.save();
        assert!(!store.contains("f", "c0"));
        assert!(store.contains("f", &format!("c{}", SENT_CAP + 9)));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn bg_check_on_empty_root_returns_nothing() {
        let root = std::env::temp_dir().join(format!("yejian-notify-bg-{}", follows::now()));
        let _ = std::fs::remove_dir_all(&root);
        let mut store = SentStore::load(&root);
        let notes = run_background_check(&root, &mut store, Instant::now() + BG_DEADLINE);
        assert!(notes.is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }
}
