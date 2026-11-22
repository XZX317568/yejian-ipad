//! Online manga the user follows, with the chapters already seen so new
//! ones can be counted after an update check.

use serde::{Deserialize, Serialize};
use std::{collections::HashSet, path::Path};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Follow {
    pub source_id: String,
    pub manga_id: String,
    pub title: String,
    /// Key of the stored `Manga` snapshot used to reopen and check it.
    pub snapshot: String,
    pub known: Vec<String>,
    /// Chapters found by a check that the user has not opened yet.
    #[serde(default)]
    pub fresh: Vec<String>,
    #[serde(default)]
    pub total: usize,
    #[serde(default)]
    pub checked_at: Option<u64>,
    #[serde(default)]
    pub error: Option<String>,
}

impl Follow {
    /// Records a fresh chapter list; returns how many chapters are new.
    pub fn apply_check(&mut self, keys: &[String], now: u64) -> usize {
        let seen: HashSet<&String> = self.known.iter().chain(&self.fresh).collect();
        let added: Vec<String> = keys.iter().filter(|key| !seen.contains(key)).cloned().collect();
        let count = added.len();
        self.fresh.extend(added);
        self.total = keys.len();
        self.checked_at = Some(now);
        self.error = None;
        count
    }

    /// The user opened the chapter list: everything listed is now known.
    pub fn mark_seen(&mut self, keys: &[String]) {
        self.known = keys.to_vec();
        self.fresh.clear();
        self.total = keys.len();
        self.error = None;
    }
}

#[derive(Default, Serialize, Deserialize)]
struct Store {
    follows: Vec<Follow>,
}

const FILE: &str = "follows.json";

pub fn load(root: &Path) -> anyhow::Result<Vec<Follow>> {
    let path = root.join(FILE);
    match std::fs::read(&path) {
        Ok(bytes) => Ok(serde_json::from_slice::<Store>(&bytes)
            .map_err(|error| anyhow::anyhow!("追更列表损坏：{error}"))?
            .follows),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(anyhow::anyhow!("读取 {} 失败：{error}", path.display())),
    }
}

pub fn save(root: &Path, follows: &[Follow]) -> anyhow::Result<()> {
    let bytes = serde_json::to_vec(&Store { follows: follows.to_vec() })?;
    std::fs::create_dir_all(root)?;
    let path = root.join(FILE);
    let tmp = root.join(format!("{FILE}.tmp"));
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, &path).map_err(|error| {
        let _ = std::fs::remove_file(&tmp);
        anyhow::anyhow!("写入 {} 失败：{error}", path.display())
    })
}

pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    #[test]
    fn new_chapters_accumulate_until_seen() {
        let mut follow = Follow::default();
        follow.mark_seen(&keys(&["1", "2"]));
        assert_eq!(follow.apply_check(&keys(&["1", "2", "3"]), 10), 1);
        assert_eq!(follow.apply_check(&keys(&["1", "2", "3", "4"]), 20), 1);
        assert_eq!(follow.fresh, keys(&["3", "4"]));
        assert_eq!((follow.total, follow.checked_at), (4, Some(20)));
        follow.mark_seen(&keys(&["1", "2", "3", "4"]));
        assert!(follow.fresh.is_empty());
        assert_eq!(follow.apply_check(&keys(&["1", "2", "3", "4"]), 30), 0);
    }

    #[test]
    fn follows_survive_a_restart() {
        let root = tempfile::tempdir().unwrap();
        assert!(load(root.path()).unwrap().is_empty());
        let follow = Follow {
            source_id: "s".into(),
            manga_id: "m".into(),
            title: "T".into(),
            known: keys(&["1"]),
            ..Default::default()
        };
        save(root.path(), std::slice::from_ref(&follow)).unwrap();
        assert_eq!(load(root.path()).unwrap(), vec![follow]);
        std::fs::write(root.path().join(FILE), b"{").unwrap();
        assert!(load(root.path()).is_err());
    }
}
