






use egui::*;



use std::collections::HashSet;



use std::path::Path;
use std::path::PathBuf;






use std::sync::mpsc;




use super::*;
use super::downloads::DownloadStatus;
use super::downloads::remove_task_cache;
use super::downloads::work_root;

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

pub(super) enum StorageAction {
    Report,
    CleanStaging,
    ClearCovers,
}

/// Task ids snapshotted on the UI thread: `all` knows every queued task,
/// `finished` the Done/Failed subset whose staging is safe to remove.
pub(super) struct StorageCmd {
    pub(super) action: StorageAction,
    pub(super) all: HashSet<String>,
    pub(super) finished: HashSet<String>,
}

pub(super) enum StorageEvent {
    Report(SourceStorage),
    /// `source-covers/` was wiped; the in-memory caches must go too.
    CoversCleared,
    Failed(String),
}

pub(super) fn tree_size(path: &Path) -> u64 {
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

pub(super) fn source_storage(root: &Path, all: &HashSet<String>, finished: &HashSet<String>) -> SourceStorage {
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
pub(super) fn clean_finished_tasks(
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
pub(super) fn storage_worker_run(
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

impl Sources {
    /// Task-id snapshot the storage worker classifies staging with:
    /// `all` covers every known task, `finished` the Done/Failed subset.
    pub(super)fn storage_snapshot(&self) -> (HashSet<String>, HashSet<String>) {
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sources::test_support::*;
    use crate::sources::downloads::snapshot_root;

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
}
