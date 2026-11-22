//! 云同步（WebDAV）— snapshot records, a pure three-way merge, the WebDAV
//! transport, and the background worker that ties them together.
//!
//! Data model: the remote file `<dir>/yejian-sync.json` is a [`Snapshot`] —
//! per-group maps of [`Entry`], each stamped `updated_at`. The local side
//! keeps `sync-state.json` with the last merged snapshot and the remote ETag
//! it was written under. `merge()` is a pure function; every group has
//! exhaustive unit tests and the transport is exercised against an in-process
//! fake WebDAV server.
//!
//! Secrets never enter the snapshot: tracker tokens live in trackers.json,
//! the WebDAV password lives in `webdav.json` (0600), and web cookies stay
//! in the source engine's jar — none of them are collected here.

use std::collections::BTreeMap;
#[cfg(test)]
use std::io::{BufRead, BufReader, Read, Write};
#[cfg(test)]
use std::net::TcpListener;
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::thread;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use yejian_core::{Library, ProgressRecord};
use yejian_sources::{send_isolated, NetRequestData, NetResponseData};

use crate::follows;

/// One mergeable record.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    /// Epoch seconds the value last changed. `0` counts as "no timestamp" —
    /// merge treats it as older than anything and prefers the local copy on
    /// a two-sided conflict.
    #[serde(default)]
    pub updated_at: i64,
    pub value: serde_json::Value,
}

impl Entry {
    pub fn new(updated_at: i64, value: serde_json::Value) -> Self {
        Self { updated_at, value }
    }
}

/// The synced state shared between devices. Every group is a flat map so
/// merge can run the same three-way rule for each key.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    /// Local-book progress — `book_id` → `ProgressRecord` JSON.
    #[serde(default)]
    pub progress: BTreeMap<String, Entry>,
    /// Online chapter read state — `source␟manga␟chapter` →
    /// `{read_page, total, read_at}`.
    #[serde(default)]
    pub online: BTreeMap<String, Entry>,
    /// Followed works — `source␟manga` → `Follow` JSON.
    #[serde(default)]
    pub follows: BTreeMap<String, Entry>,
    /// Bookmarks — `work␟chapter␟page` → `Bookmark` JSON.
    #[serde(default)]
    pub bookmarks: BTreeMap<String, Entry>,
    /// The shelf category list — single record `categories` → `[String]`.
    #[serde(default)]
    pub categories: BTreeMap<String, Entry>,
    /// Per-work reading prefs — reader-prefs key → `ReaderPrefs` JSON.
    #[serde(default)]
    pub reader_prefs: BTreeMap<String, Entry>,
    /// Reading statistics — `device␟YYYY-MM-DD` → `DayStats` JSON. Both
    /// sides changing the same (device, day) merges field-by-field `max`.
    #[serde(default)]
    pub stats: BTreeMap<String, Entry>,
    /// Hidden history rows — row key → `{hidden_at}`.
    #[serde(default)]
    pub history_hidden: BTreeMap<String, Entry>,
    /// Installed sources — source id → `{version, repos:[url..]}` so another
    /// device can offer 一键安装 from the same repositories.
    #[serde(default)]
    pub sources: BTreeMap<String, Entry>,
    /// Appearance subset — `theme`/`palette`/`font-scale`/`reading-direction`.
    #[serde(default)]
    pub appearance: BTreeMap<String, Entry>,
}

pub const REMOTE_FILE: &str = "yejian-sync.json";
const STATE_FILE: &str = "sync-state.json";
const WEBDAV_FILE: &str = "webdav.json";

// ---------------------------------------------------------------------------
// Three-way merge — pure function, unit-tested.
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct MergeReport {
    /// Records taken from the remote side (changed there, unchanged locally).
    pub remote_wins: usize,
    /// Records kept from the local side (changed locally, unchanged remotely).
    pub local_wins: usize,
    /// Records both sides changed; resolved by timestamp or stats max.
    pub conflicts: usize,
    /// Source ids installed remotely but missing locally (一键安装 prompt).
    pub remote_sources: Vec<String>,
}

fn merge_group(
    group: &str,
    base: &BTreeMap<String, Entry>,
    local: &BTreeMap<String, Entry>,
    remote: &BTreeMap<String, Entry>,
    report: &mut MergeReport,
) -> BTreeMap<String, Entry> {
    let mut merged = BTreeMap::new();
    for key in base.keys().chain(local.keys()).chain(remote.keys()) {
        if merged.contains_key(key) {
            continue;
        }
        let (base_e, local_e, remote_e) = (base.get(key), local.get(key), remote.get(key));
        let chosen = match (base_e, local_e, remote_e) {
            // New on one side only → keep that side.
            (None, Some(l), None) => Some(l.clone()),
            (None, None, Some(r)) => {
                report.remote_wins += 1;
                Some(r.clone())
            }
            (None, Some(l), Some(r)) => {
                // Same key created on both sides: treat as both-changed.
                report.conflicts += 1;
                Some(resolve_conflict(group, l, r))
            }
            // Untouched on both sides.
            (Some(b), Some(l), Some(r)) if l == b && r == b => Some(l.clone()),
            // Only remote changed (or local deleted while remote is as base —
            // handled below by the deletion arms).
            (Some(b), Some(l), Some(r)) if l == b => {
                report.remote_wins += 1;
                Some(r.clone())
            }
            (Some(b), Some(l), Some(r)) if r == b => {
                report.local_wins += 1;
                Some(l.clone())
            }
            // Local deleted, remote unchanged → deletion wins.
            (Some(b), None, Some(r)) if r == b => None,
            // Remote deleted, local unchanged → deletion wins.
            (Some(b), Some(l), None) if l == b => None,
            // One side deleted while the other changed → the change wins.
            (Some(_), None, Some(r)) => {
                report.conflicts += 1;
                Some(r.clone())
            }
            (Some(_), Some(l), None) => {
                report.conflicts += 1;
                Some(l.clone())
            }
            // Both changed → resolve.
            (Some(_), Some(l), Some(r)) => {
                report.conflicts += 1;
                Some(resolve_conflict(group, l, r))
            }
            // Impossible leftovers (no base/local/remote at all).
            _ => None,
        };
        if let Some(entry) = chosen {
            merged.insert(key.clone(), entry);
        }
    }
    merged
}

/// Both sides changed the same key. Stats merge per-field `max` (devices add
/// time independently); everything else takes the newer `updated_at`, and a
/// missing timestamp loses to the local copy.
fn resolve_conflict(group: &str, local: &Entry, remote: &Entry) -> Entry {
    if group == "stats" {
        let mut value = local.value.clone();
        if let (Some(local_obj), Some(remote_obj)) = (value.as_object_mut(), remote.value.as_object()) {
            for (field, remote_v) in remote_obj {
                let keep = match (local_obj.get(field), remote_v) {
                    (Some(local_v), remote_v) => {
                        local_v.as_u64().unwrap_or(0) >= remote_v.as_u64().unwrap_or(0)
                    }
                    (None, _) => false,
                };
                if !keep {
                    local_obj.insert(field.clone(), remote_v.clone());
                }
            }
        }
        return Entry::new(local.updated_at.max(remote.updated_at), value);
    }
    match (local.updated_at, remote.updated_at) {
        (l, r) if r > l => remote.clone(),
        (l, r) if l > r => local.clone(),
        // Equal or both missing → local wins, deterministically.
        _ => local.clone(),
    }
}

/// Three-way merge of every group. `base` is the last synced snapshot
/// (empty on first sync).
pub fn merge(base: &Snapshot, local: &Snapshot, remote: &Snapshot) -> (Snapshot, MergeReport) {
    let mut report = MergeReport::default();
    let merged = Snapshot {
        progress: merge_group("progress", &base.progress, &local.progress, &remote.progress, &mut report),
        online: merge_group("online", &base.online, &local.online, &remote.online, &mut report),
        follows: merge_group("follows", &base.follows, &local.follows, &remote.follows, &mut report),
        bookmarks: merge_group("bookmarks", &base.bookmarks, &local.bookmarks, &remote.bookmarks, &mut report),
        categories: merge_group("categories", &base.categories, &local.categories, &remote.categories, &mut report),
        reader_prefs: merge_group("reader_prefs", &base.reader_prefs, &local.reader_prefs, &remote.reader_prefs, &mut report),
        stats: merge_group("stats", &base.stats, &local.stats, &remote.stats, &mut report),
        history_hidden: merge_group("history_hidden", &base.history_hidden, &local.history_hidden, &remote.history_hidden, &mut report),
        sources: merge_group("sources", &base.sources, &local.sources, &remote.sources, &mut report),
        appearance: merge_group("appearance", &base.appearance, &local.appearance, &remote.appearance, &mut report),
    };
    // Sources installed on the other device but not here — the UI offers a
    // one-tap install; nothing is installed automatically.
    for id in merged.sources.keys() {
        if !local.sources.contains_key(id) {
            report.remote_sources.push(id.clone());
        }
    }
    (merged, report)
}

// ---------------------------------------------------------------------------
// WebDAV transport
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct DavConfig {
    /// e.g. `https://dav.jianguoyun.com/dav/` — server root, `dir` is joined
    /// under it.
    pub url: String,
    pub user: String,
    /// App password — kept in `webdav.json` (0600), never synced.
    pub password: String,
    /// Remote directory, default `yejian`.
    pub dir: String,
}

impl DavConfig {
    pub fn configured(&self) -> bool {
        !self.url.trim().is_empty() && !self.password.is_empty()
    }
}

/// Persisted WebDAV credentials — the same "tokens in one private file"
/// treatment trackers.json gets.
#[derive(Default, Serialize, Deserialize)]
struct DavSecrets {
    #[serde(default)]
    password: String,
}

fn secrets_path(root: &Path) -> PathBuf {
    root.join(WEBDAV_FILE)
}

pub fn load_password(root: &Path) -> String {
    std::fs::read(secrets_path(root))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<DavSecrets>(&bytes).ok())
        .map(|secrets| secrets.password)
        .unwrap_or_default()
}

pub fn save_password(root: &Path, password: &str) -> anyhow::Result<()> {
    let path = secrets_path(root);
    let part = path.with_extension("json.part");
    let bytes = serde_json::to_vec(&DavSecrets {
        password: password.into(),
    })?;
    #[cfg(unix)]
    {
        use std::io::Write as _;
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&part)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
    }
    #[cfg(not(unix))]
    {
        std::fs::write(&part, bytes)?;
    }
    std::fs::rename(&part, &path).map_err(|error| {
        let _ = std::fs::remove_file(&part);
        anyhow::anyhow!("写入 {} 失败：{error}", path.display())
    })
}

/// `https` only, except private/LAN hosts which may use `http` (a home NAS).
pub fn validate_url(url: &str) -> Result<(), String> {
    let url = url.trim();
    let rest = if let Some(rest) = url.strip_prefix("https://") {
        rest
    } else if let Some(rest) = url.strip_prefix("http://") {
        let host = rest
            .split(['/', ':', '?', '#'])
            .next()
            .unwrap_or("")
            .trim_end_matches(']')
            .trim_start_matches('[');
        let private = host == "localhost"
            || host == "::1"
            || host.ends_with(".local")
            || host.starts_with("127.")
            || host.starts_with("10.")
            || host.starts_with("192.168.")
            || host
                .strip_prefix("172.")
                .and_then(|second| second.split('.').next()?.parse::<u8>().ok())
                .is_some_and(|second| (16..=31).contains(&second));
        if private {
            return Ok(());
        }
        return Err("WebDAV 只允许 https 地址（局域网地址可用 http）".into());
    } else {
        return Err("服务器地址要以 https:// 开头".into());
    };
    if rest.split('/').next().unwrap_or("").is_empty() {
        return Err("服务器地址缺少主机名".into());
    }
    Ok(())
}

/// HTTP Basic `user:pass` — 20-line encoder so no base64 crate is needed.
fn basic_auth(user: &str, password: &str) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let bytes = format!("{user}:{password}").into_bytes();
    let mut out = String::with_capacity((bytes.len() + 2) / 3 * 4);
    for chunk in bytes.chunks(3) {
        let (a, b, c) = (
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        );
        out.push(TABLE[(a >> 2) as usize] as char);
        out.push(TABLE[(((a & 0b11) << 4) | (b >> 4)) as usize] as char);
        out.push(if chunk.len() > 1 {
            TABLE[(((b & 0b1111) << 2) | (c >> 6)) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[(c & 0b111111) as usize] as char
        } else {
            '='
        });
    }
    format!("Basic {out}")
}

/// Local merge state: the last merged snapshot + the ETag it was PUT under.
#[derive(Default, Serialize, Deserialize)]
pub(crate) struct SyncState {
    #[serde(default)]
    etag: Option<String>,
    #[serde(default)]
    snapshot: Snapshot,
}

pub(crate) fn load_state(root: &Path) -> SyncState {
    std::fs::read(root.join(STATE_FILE))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

fn save_state(root: &Path, state: &SyncState) -> anyhow::Result<()> {
    let path = root.join(STATE_FILE);
    let tmp = root.join(format!("{STATE_FILE}.tmp"));
    std::fs::write(&tmp, serde_json::to_vec(state)?)?;
    std::fs::rename(&tmp, &path).map_err(|error| {
        let _ = std::fs::remove_file(&tmp);
        anyhow::anyhow!("写入 {} 失败：{error}", path.display())
    })
}

#[derive(Debug)]
pub enum CloudError {
    /// 401 — wrong account or app password.
    Unauthorized,
    /// The server answered with an unexpected status.
    Http(u16, String),
    /// DNS/TLS/timeout before any HTTP status arrived.
    Transport(String),
    /// The remote file existed but was not a Snapshot JSON.
    Corrupt(String),
    /// PUT raced the remote copy three times.
    Conflict,
}

impl CloudError {
    pub fn describe(&self) -> String {
        match self {
            Self::Unauthorized => "同步失败：账号或密码错误（HTTP 401）".into(),
            Self::Http(code, body) => format!("同步失败：HTTP {code} {}", &body[..body.len().min(120)]),
            Self::Transport(error) => format!("同步失败：{error}"),
            Self::Corrupt(error) => format!("同步失败：云端数据损坏（{error}）"),
            Self::Conflict => "同步失败：云端一直被其他设备更新，稍后重试".into(),
        }
    }
}

const TIMEOUT_MS: u64 = 20_000;
/// PUT racing a remote writer: re-read and re-merge at most this many times.
const MAX_CONFLICT_RETRIES: usize = 3;

struct Dav {
    file_url: String,
    dir_url: String,
    auth: String,
}

impl Dav {
    fn new(config: &DavConfig) -> Self {
        let base = config.url.trim().trim_end_matches('/');
        let dir = config.dir.trim().trim_matches('/');
        let dir = if dir.is_empty() { "yejian" } else { dir };
        Self {
            file_url: format!("{base}/{dir}/{REMOTE_FILE}"),
            dir_url: format!("{base}/{dir}/"),
            auth: basic_auth(&config.user, &config.password),
        }
    }

    fn send(&self, method: &str, url: &str, headers: Vec<(String, String)>, body: Option<Vec<u8>>) -> Result<NetResponseData, CloudError> {
        let mut headers = headers;
        headers.push(("Authorization".into(), self.auth.clone()));
        let response = send_isolated(NetRequestData {
            method: method.into(),
            url: url.into(),
            headers,
            body,
            timeout_ms: TIMEOUT_MS,
        });
        if let Some(error) = response.error {
            return Err(CloudError::Transport(error));
        }
        if response.status_code == 401 {
            return Err(CloudError::Unauthorized);
        }
        Ok(response)
    }

    /// `MKCOL` on the sync directory; 201/301/302-created and 405
    /// (already exists) are both fine.
    fn ensure_dir(&self) -> Result<(), CloudError> {
        let response = self.send("MKCOL", &self.dir_url, vec![], None)?;
        match response.status_code {
            200..=299 | 301 | 302 | 405 => Ok(()),
            code => Err(CloudError::Http(code, String::from_utf8_lossy(&response.body).into_owned())),
        }
    }

    /// `GET` — `(body, ETag)`; `None` when the file does not exist yet.
    fn get(&self) -> Result<Option<(Vec<u8>, Option<String>)>, CloudError> {
        let response = self.send("GET", &self.file_url, vec![("Accept".into(), "application/json".into())], None)?;
        match response.status_code {
            200 => {
                let etag = response.header("etag").map(str::to_string);
                Ok(Some((response.body, etag)))
            }
            404 => Ok(None),
            code => Err(CloudError::Http(code, String::from_utf8_lossy(&response.body).into_owned())),
        }
    }

    /// `PUT` — `If-Match` when updating, `If-None-Match: *` on create.
    fn put(&self, body: &[u8], etag: Option<&str>, create: bool) -> Result<Option<String>, CloudError> {
        let condition = if create {
            ("If-None-Match".to_string(), "*".to_string())
        } else {
            ("If-Match".to_string(), etag.map(str::to_string).unwrap_or_else(|| "*".into()))
        };
        let response = self.send(
            "PUT",
            &self.file_url,
            vec![
                condition,
                ("Content-Type".into(), "application/json".into()),
            ],
            Some(body.to_vec()),
        )?;
        match response.status_code {
            200..=299 => Ok(Some(
                response
                    .header("etag")
                    .map(str::to_string)
                    .unwrap_or_default(),
            )),
            412 => Err(CloudError::Http(412, "precondition failed".into())),
            code => Err(CloudError::Http(code, String::from_utf8_lossy(&response.body).into_owned())),
        }
    }
}

/// What one finished sync produced.
#[derive(Debug)]
pub struct SyncOutcome {
    /// The merged snapshot now stored locally and remotely.
    pub merged: Snapshot,
    pub report: MergeReport,
}

/// One full sync round: GET remote, three-way merge, PUT with ETag, retry on
/// 412. Runs on the cloud worker thread. `local` already carries every group
/// except `progress`, which this fills from the local `Library` and applies
/// back through `Library::apply_progress` when the remote side wins.
pub fn sync_round(root: &Path, config: &DavConfig, mut local: Snapshot) -> Result<SyncOutcome, CloudError> {
    let dav = Dav::new(config);
    dav.ensure_dir()?;

    // Local progress comes straight from the library store — the cloud
    // worker opens its own `Library` connection exactly like sync.rs.
    let library = Library::open(root)
        .map_err(|error| CloudError::Transport(format!("打开书库失败：{error:#}")))?;
    let records = library
        .export_progress()
        .map_err(|error| CloudError::Transport(format!("读取阅读进度失败：{error:#}")))?;
    local.progress = records
        .iter()
        .map(|record| {
            (
                record.book_id.clone(),
                Entry::new(record.updated_at, serde_json::to_value(record).unwrap_or_default()),
            )
        })
        .collect();

    for attempt in 0..=MAX_CONFLICT_RETRIES {
        let remote_body = dav.get()?;
        let (remote, etag) = match remote_body {
            Some((bytes, etag)) => {
                let remote: Snapshot = serde_json::from_slice(&bytes).map_err(|error| {
                    CloudError::Corrupt(format!("{error}"))
                })?;
                (remote, etag)
            }
            None => (Snapshot::default(), None),
        };
        let state = load_state(root);
        let (merged, mut report) = merge(&state.snapshot, &local, &remote);
        let body = serde_json::to_vec(&merged)
            .map_err(|error| CloudError::Transport(format!("编码同步数据失败：{error}")))?;
        match dav.put(&body, etag.as_deref(), etag.is_none()) {
            Ok(new_etag) => {
                let _ = save_state(
                    root,
                    &SyncState {
                        etag: new_etag.filter(|etag| !etag.is_empty()).or(etag),
                        snapshot: merged.clone(),
                    },
                );
                // Apply remote-won progress through the library's own merge —
                // apply_progress keeps the newer timestamp per record anyway.
                // Progress entries the merge took from the remote side:
                // apply them through the library's own newer-wins merge and
                // report the ids it actually accepted.
                let remote_progress: Vec<ProgressRecord> = merged
                    .progress
                    .iter()
                    .filter(|(id, entry)| {
                        local
                            .progress
                            .get(*id)
                            .map(|local_entry| local_entry.value != entry.value)
                            .unwrap_or(true)
                    })
                    .filter_map(|(_, entry)| {
                        serde_json::from_value(entry.value.clone()).ok()
                    })
                    .collect();
                let applied = if remote_progress.is_empty() {
                    Vec::new()
                } else {
                    library
                        .apply_progress(&remote_progress)
                        .map_err(|error| {
                            CloudError::Transport(format!("写入阅读进度失败：{error:#}"))
                        })?
                        .applied
                };
                let _ = applied;
                report.remote_sources.sort();
                return Ok(SyncOutcome { merged, report });
            }
            Err(CloudError::Http(412, _)) if attempt < MAX_CONFLICT_RETRIES => {
                continue;
            }
            Err(CloudError::Http(412, _)) => return Err(CloudError::Conflict),
            Err(error) => return Err(error),
        }
    }
    unreachable!()
}

// ---------------------------------------------------------------------------
// Background worker — commands in, replies out, like Trackers.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct CloudConfig {
    pub url: String,
    pub user: String,
    pub password: String,
    pub dir: String,
    pub enabled: bool,
}

impl CloudConfig {
    fn dav(&self) -> DavConfig {
        DavConfig {
            url: self.url.clone(),
            user: self.user.clone(),
            password: self.password.clone(),
            dir: self.dir.clone(),
        }
    }
}

enum CloudCommand {
    /// Full snapshot (minus progress) collected on the UI thread.
    Sync { snapshot: Box<Snapshot>, config: Box<CloudConfig> },
}

enum CloudReply {
    Synced(Box<SyncOutcome>),
    Failed(String),
}

/// Owns the cloud sync thread. The UI collects a [`Snapshot`], sends
/// [`CloudCommand::Sync`], and applies [`SyncOutcome`] on a later frame.
pub struct CloudSync {
    tx: mpsc::Sender<CloudCommand>,
    rx: mpsc::Receiver<CloudReply>,
    /// Last finished sync time / failure, for the settings status line.
    pub last_sync_at: Option<u64>,
    pub last_error: Option<String>,
    /// The outcome waiting for the UI to apply remote-won groups.
    pending: Option<SyncOutcome>,
    busy: bool,
    /// Debounce: a change-triggered sync fires this far after the last change.
    debounce_at: Option<Instant>,
    debounced: Option<Box<Snapshot>>,
    pub config: CloudConfig,
    pub enabled: bool,
}

impl CloudSync {
    pub fn new(root: PathBuf) -> Self {
        let (tx, cmd_rx) = mpsc::channel::<CloudCommand>();
        let (reply_tx, rx) = mpsc::channel::<CloudReply>();
        crate::diagnostics::spawn_supervised(
            "yejian-cloud",
            true,
            || crate::diagnostics::report_recovery("云同步"),
            move || {
                while let Ok(command) = cmd_rx.recv() {
                    match command {
                        CloudCommand::Sync { snapshot, config } => {
                            let reply = match sync_round(&root, &config.dav(), *snapshot) {
                                Ok(outcome) => CloudReply::Synced(Box::new(outcome)),
                                Err(error) => CloudReply::Failed(error.describe()),
                            };
                            if reply_tx.send(reply).is_err() {
                                return;
                            }
                        }
                    }
                }
            },
        );
        Self {
            tx,
            rx,
            last_sync_at: None,
            last_error: None,
            pending: None,
            busy: false,
            debounce_at: None,
            debounced: None,
            config: CloudConfig {
                url: String::new(),
                user: String::new(),
                password: String::new(),
                dir: "yejian".into(),
                enabled: false,
            },
            enabled: false,
        }
    }

    /// Manual or scheduled sync. No-ops while disabled or mid-flight; the
    /// last snapshot wins while one is already queued.
    pub fn sync_now(&mut self, snapshot: Snapshot) {
        if !self.enabled || !self.config.dav().configured() {
            return;
        }
        self.debounced = Some(Box::new(snapshot));
        self.debounce_at = None;
        self.dispatch();
    }

    /// Change-triggered sync — coalesces into one run 30 s after the last
    /// change. The UI hands the freshest snapshot at fire time through
    /// `poll_debounce`.
    pub fn queue_debounced(&mut self) {
        if self.enabled && self.config.dav().configured() {
            self.debounce_at = Some(Instant::now() + Duration::from_secs(30));
        }
    }

    /// True when a debounced sync is due — the UI then calls `sync_now`.
    pub fn debounce_due(&self) -> bool {
        self.debounce_at.is_some_and(|at| Instant::now() >= at)
    }

    pub fn clear_debounce(&mut self) {
        self.debounce_at = None;
    }

    pub fn busy(&self) -> bool {
        self.busy
    }

    fn dispatch(&mut self) {
        if self.busy {
            return;
        }
        let Some(snapshot) = self.debounced.take() else {
            return;
        };
        if self
            .tx
            .send(CloudCommand::Sync {
                snapshot,
                config: Box::new(self.config.clone()),
            })
            .is_ok()
        {
            self.busy = true;
        }
    }

    /// Drains finished syncs; returns the outcome to apply, if any. Also
    /// dispatches the debounced snapshot once the debounce window passed.
    pub fn poll(&mut self) {
        while let Ok(reply) = self.rx.try_recv() {
            self.busy = false;
            match reply {
                CloudReply::Synced(outcome) => {
                    self.last_sync_at = Some(follows::now());
                    self.last_error = None;
                    self.pending = Some(*outcome);
                }
                CloudReply::Failed(error) => {
                    self.last_error = Some(error);
                }
            }
        }
        if self.debounce_due() {
            self.debounce_at = None;
            // The UI re-collects at fire time and calls sync_now.
        }
        self.dispatch();
    }

    /// The merged outcome the UI should now apply to its owners.
    pub fn take_outcome(&mut self) -> Option<SyncOutcome> {
        self.pending.take()
    }

    /// Status line under the settings form.
    pub fn status(&self) -> String {
        if self.busy {
            "正在同步…".into()
        } else if let Some(error) = &self.last_error {
            error.clone()
        } else if let Some(at) = self.last_sync_at {
            let when = chrono::DateTime::from_timestamp(at as i64, 0)
                .map(|stamp| stamp.with_timezone(&chrono::Local).format("%m-%d %H:%M").to_string())
                .unwrap_or_else(|| at.to_string());
            format!("上次同步 {when}")
        } else {
            "尚未同步".into()
        }
    }

    /// Acceptance seam: the last round finished without an error.
    #[cfg(debug_assertions)]
    pub fn synced_ok(&self) -> bool {
        !self.busy && self.last_sync_at.is_some() && self.last_error.is_none()
    }
}

/// Group key helpers shared between collect and apply.
pub fn follow_entry_key(source: &str, manga: &str) -> String {
    format!("{source}\u{1f}{manga}")
}

pub fn online_entry_key(source: &str, manga: &str, chapter: &str) -> String {
    format!("{source}\u{1f}{manga}\u{1f}{chapter}")
}

pub fn bookmark_entry_key(work: &str, chapter: &str, page: usize) -> String {
    format!("{work}\u{1f}{chapter}\u{1f}{page}")
}

pub fn stats_entry_key(device: &str, day: &str) -> String {
    format!("{device}\u{1f}{day}")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::sync::{Arc, Mutex};

    fn entry(at: i64, value: serde_json::Value) -> Entry {
        Entry::new(at, value)
    }

    fn v(n: i64) -> serde_json::Value {
        serde_json::json!(n)
    }

    #[test]
    fn merge_one_side_changes() {
        let base = Snapshot::default();
        let mut local = Snapshot::default();
        let mut remote = Snapshot::default();
        local.follows.insert("a".into(), entry(10, v(1)));
        remote.bookmarks.insert("b".into(), entry(20, v(2)));
        let (merged, report) = merge(&base, &local, &remote);
        assert_eq!(merged.follows["a"].value, v(1));
        assert_eq!(merged.bookmarks["b"].value, v(2));
        assert_eq!(report.remote_wins, 1);
        assert_eq!(report.local_wins, 0);
    }

    #[test]
    fn merge_remote_change_wins_when_local_unchanged() {
        let mut base = Snapshot::default();
        base.follows.insert("f".into(), entry(5, v(1)));
        let local = base.clone();
        let mut remote = Snapshot::default();
        remote.follows.insert("f".into(), entry(9, v(2)));
        let (merged, report) = merge(&base, &local, &remote);
        assert_eq!(merged.follows["f"].value, v(2));
        assert_eq!(report.remote_wins, 1);
    }

    #[test]
    fn merge_both_changed_takes_newer_timestamp() {
        let mut base = Snapshot::default();
        base.appearance
            .insert("theme".into(), entry(1, serde_json::json!("warm")));
        let mut local = base.clone();
        local.appearance.insert(
            "theme".into(),
            entry(3, serde_json::json!("sepia")),
        );
        let mut remote = base.clone();
        remote.appearance.insert(
            "theme".into(),
            entry(9, serde_json::json!("dark")),
        );
        let (merged, report) = merge(&base, &local, &remote);
        assert_eq!(merged.appearance["theme"].value, serde_json::json!("dark"));
        assert_eq!(report.conflicts, 1);
    }

    #[test]
    fn merge_untimestamped_conflict_prefers_local() {
        let mut base = Snapshot::default();
        base.appearance.insert("theme".into(), entry(0, v(0)));
        let mut local = base.clone();
        local
            .appearance
            .insert("theme".into(), entry(0, serde_json::json!("sepia")));
        let mut remote = base.clone();
        remote
            .appearance
            .insert("theme".into(), entry(0, serde_json::json!("dark")));
        let (merged, _) = merge(&base, &local, &remote);
        assert_eq!(merged.appearance["theme"].value, serde_json::json!("sepia"));
    }

    #[test]
    fn merge_local_delete_beats_base_copy() {
        let mut base = Snapshot::default();
        base.bookmarks.insert("b".into(), entry(5, v(1)));
        let local = Snapshot::default();
        let remote = base.clone();
        let (merged, _) = merge(&base, &local, &remote);
        assert!(!merged.bookmarks.contains_key("b"));
    }

    #[test]
    fn merge_remote_delete_beats_base_copy() {
        let mut base = Snapshot::default();
        base.follows.insert("f".into(), entry(5, v(1)));
        let local = base.clone();
        let remote = Snapshot::default();
        let (merged, _) = merge(&base, &local, &remote);
        assert!(!merged.follows.contains_key("f"));
    }

    #[test]
    fn merge_delete_vs_change_keeps_change() {
        let mut base = Snapshot::default();
        base.follows.insert("f".into(), entry(5, v(1)));
        let local = Snapshot::default();
        let mut remote = Snapshot::default();
        remote.follows.insert("f".into(), entry(9, v(2)));
        let (merged, report) = merge(&base, &local, &remote);
        assert_eq!(merged.follows["f"].value, v(2));
        assert_eq!(report.conflicts, 1);
    }

    #[test]
    fn merge_stats_take_field_max() {
        let day = |s: u64, p: u64, c: u64| {
            serde_json::json!({"seconds": s, "pages": p, "chapters": c})
        };
        let mut base = Snapshot::default();
        base.stats.insert("dev1\u{1f}2026-01-01".into(), entry(1, day(10, 5, 1)));
        let mut local = base.clone();
        local
            .stats
            .insert("dev1\u{1f}2026-01-01".into(), entry(2, day(50, 8, 1)));
        let mut remote = base.clone();
        remote
            .stats
            .insert("dev1\u{1f}2026-01-01".into(), entry(3, day(30, 20, 4)));
        remote
            .stats
            .insert("dev2\u{1f}2026-01-01".into(), entry(3, day(7, 7, 0)));
        let (merged, _) = merge(&base, &local, &remote);
        assert_eq!(
            merged.stats["dev1\u{1f}2026-01-01"].value,
            day(50, 20, 4),
            "each field keeps the larger side"
        );
        assert!(merged.stats.contains_key("dev2\u{1f}2026-01-01"));
    }

    #[test]
    fn merge_reports_remote_only_sources() {
        let mut remote = Snapshot::default();
        remote.sources.insert(
            "source.new".into(),
            entry(1, serde_json::json!({"version": 3})),
        );
        let (merged, report) = merge(&Snapshot::default(), &Snapshot::default(), &remote);
        assert!(merged.sources.contains_key("source.new"));
        assert_eq!(report.remote_sources, vec!["source.new".to_string()]);
    }

    #[test]
    fn validate_url_rules() {
        assert!(validate_url("https://dav.jianguoyun.com/dav/").is_ok());
        assert!(validate_url("http://dav.example.com/").is_err());
        assert!(validate_url("http://192.168.1.5:5005/").is_ok());
        assert!(validate_url("http://127.0.0.1:8000/dav").is_ok());
        assert!(validate_url("http://10.0.0.2/dav").is_ok());
        assert!(validate_url("http://172.16.0.3/dav").is_ok());
        assert!(validate_url("http://172.33.0.3/dav").is_err());
        assert!(validate_url("http://nas.local/dav").is_ok());
        assert!(validate_url("ftp://x").is_err());
        assert!(validate_url("https://").is_err());
    }

    #[test]
    fn basic_auth_encodes() {
        assert_eq!(basic_auth("user", "pass"), "Basic dXNlcjpwYXNz");
        assert_eq!(basic_auth("a", "b"), "Basic YTpi");
        assert_eq!(basic_auth("\u{fc}", "\u{df}"), "Basic w7w6w58=");
    }

    // -- fake stateful WebDAV server ------------------------------------------

    struct StateDav {
        url: String,
        file: Arc<Mutex<Option<(Vec<u8>, u64)>>>,
        puts: Arc<Mutex<Vec<(String, String)>>>,
        fail_next_put: Arc<Mutex<bool>>,
    }

    impl StateDav {
        fn start() -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let addr = listener.local_addr().unwrap();
            let file = Arc::new(Mutex::new(None::<(Vec<u8>, u64)>));
            let puts = Arc::new(Mutex::new(Vec::new()));
            let fail = Arc::new(Mutex::new(false));
            let (file2, puts2, fail2) = (file.clone(), puts.clone(), fail.clone());
            thread::spawn(move || {
                while let Ok((stream, _)) = listener.accept() {
                    let mut reader = BufReader::new(stream.try_clone().unwrap());
                    let mut request = String::new();
                    let _ = reader.read_line(&mut request);
                    let mut headers = Vec::new();
                    let mut line = String::new();
                    while reader.read_line(&mut line).is_ok() {
                        let trimmed = line.trim_end().to_string();
                        line.clear();
                        if trimmed.is_empty() {
                            break;
                        }
                        headers.push(trimmed);
                    }
                    let length: usize = headers
                        .iter()
                        .find_map(|h| {
                            h.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .map(|v| v.trim().parse().ok())
                                .flatten()
                        })
                        .unwrap_or(0);
                    let mut body = vec![0; length];
                    let _ = reader.read_exact(&mut body);
                    let header = |name: &str| -> Option<String> {
                        let lower = name.to_ascii_lowercase();
                        headers.iter().find_map(|h| {
                            h.to_ascii_lowercase()
                                .starts_with(&format!("{lower}:"))
                                .then(|| {
                                    h.split(':')
                                        .nth(1)
                                        .map(|v| v.trim().to_string())
                                        .unwrap_or_default()
                                })
                        })
                    };
                    let method = request.split(' ').next().unwrap_or("").to_string();
                    let mut stream = stream;
                    match method.as_str() {
                        "MKCOL" => {
                            let _ = write!(stream, "HTTP/1.1 201 Created\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                        }
                        "GET" => {
                            let file = file2.lock().unwrap();
                            if let Some((bytes, etag)) = file.as_ref() {
                                let _ = write!(
                                    stream,
                                    "HTTP/1.1 200 OK\r\nETag: \"{etag}\"\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                                    bytes.len()
                                );
                                let _ = stream.write_all(bytes);
                            } else {
                                let _ = write!(stream, "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                            }
                        }
                        "PUT" => {
                            let mut file = file2.lock().unwrap();
                            let if_none = header("If-None-Match");
                            let if_match = header("If-Match");
                            puts2.lock().unwrap().push((
                                if_none.clone().or(if_match.clone()).unwrap_or_default(),
                                String::from_utf8_lossy(&body).into_owned(),
                            ));
                            let conflict = if file.is_none() {
                                if_none.as_deref() != Some("*")
                            } else if fail2.lock().unwrap().clone() {
                                *fail2.lock().unwrap() = false;
                                true
                            } else {
                                let etag = file.as_ref().unwrap().1;
                                if_match.as_deref() != Some(&format!("\"{etag}\""))
                                    && if_match.as_deref() != Some("*")
                            };
                            if conflict {
                                let _ = write!(stream, "HTTP/1.1 412 Precondition Failed\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                            } else {
                                let etag = file.as_ref().map(|(_, e)| *e).unwrap_or(0) + 1;
                                *file = Some((body, etag));
                                let _ = write!(stream, "HTTP/1.1 201 Created\r\nETag: \"{etag}\"\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                            }
                        }
                        _ => {
                            let _ = write!(stream, "HTTP/1.1 405 Method Not Allowed\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                        }
                    }
                }
            });
            Self {
                url: format!("http://{addr}"),
                file,
                puts,
                fail_next_put: fail,
            }
        }

        fn config(&self, password: &str) -> DavConfig {
            DavConfig {
                url: self.url.clone(),
                user: "user".into(),
                password: password.into(),
                dir: "yejian".into(),
            }
        }
    }

    fn snapshot_with(
        key: &str,
        group: fn(&mut Snapshot) -> &mut BTreeMap<String, Entry>,
        at: i64,
        value: serde_json::Value,
    ) -> Snapshot {
        let mut snapshot = Snapshot::default();
        group(&mut snapshot).insert(key.into(), Entry::new(at, value));
        snapshot
    }

    #[test]
    fn sync_round_creates_then_merges_two_devices() {
        let dav = StateDav::start();
        let dir_a = tempfile::tempdir().unwrap();
        let dir_b = tempfile::tempdir().unwrap();
        Library::open(dir_a.path()).unwrap();
        Library::open(dir_b.path()).unwrap();

        let local_a = snapshot_with(
            "a\u{1f}m",
            |s| &mut s.follows,
            10,
            serde_json::json!({"title": "A 的追更"}),
        );
        let outcome_a = sync_round(dir_a.path(), &dav.config("pw"), local_a).unwrap();
        assert_eq!(outcome_a.merged.follows.len(), 1);
        assert_eq!(dav.puts.lock().unwrap()[0].0, "*");
        let etag = load_state(dir_a.path()).etag;
        assert_eq!(etag.as_deref(), Some("\"1\""));

        let local_b = snapshot_with(
            "b\u{1f}m",
            |s| &mut s.follows,
            11,
            serde_json::json!({"title": "B 的追更"}),
        );
        let outcome_b = sync_round(dir_b.path(), &dav.config("pw"), local_b).unwrap();
        assert_eq!(outcome_b.merged.follows.len(), 2);
        assert_eq!(outcome_b.report.remote_wins, 1);

        let local_a2 = snapshot_with(
            "a\u{1f}m",
            |s| &mut s.follows,
            10,
            serde_json::json!({"title": "A 的追更"}),
        );
        let outcome_a2 = sync_round(dir_a.path(), &dav.config("pw"), local_a2).unwrap();
        assert_eq!(outcome_a2.merged.follows.len(), 2);
    }

    #[test]
    fn sync_round_retries_after_412() {
        let dav = StateDav::start();
        let dir = tempfile::tempdir().unwrap();
        Library::open(dir.path()).unwrap();
        *dav.file.lock().unwrap() = Some((
            serde_json::to_vec(&Snapshot::default()).unwrap(),
            7,
        ));
        *dav.fail_next_put.lock().unwrap() = true;
        let outcome = sync_round(dir.path(), &dav.config("pw"), Snapshot::default()).unwrap();
        assert_eq!(dav.puts.lock().unwrap().len(), 2, "one 412 then retry");
        assert_eq!(outcome.report.remote_wins, 0);
    }

    #[test]
    fn sync_round_reports_401() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        thread::spawn(move || {
            while let Ok((mut stream, _)) = listener.accept() {
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                let _ = reader.read_line(&mut line);
                let mut dummy = String::new();
                loop {
                    dummy.clear();
                    let Ok(n) = reader.read_line(&mut dummy) else {
                        break;
                    };
                    if dummy.trim_end().is_empty() || n == 0 {
                        break;
                    }
                }
                let _ = write!(stream, "HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
            }
        });
        let dir = tempfile::tempdir().unwrap();
        Library::open(dir.path()).unwrap();
        let config = DavConfig {
            url: format!("http://{addr}"),
            user: "u".into(),
            password: "bad".into(),
            dir: "yejian".into(),
        };
        let error = sync_round(dir.path(), &config, Snapshot::default()).unwrap_err();
        assert!(matches!(error, CloudError::Unauthorized));
        assert!(error.describe().contains("401"));
    }

    #[test]
    fn secrets_file_is_written_and_loaded() {
        let dir = tempfile::tempdir().unwrap();
        save_password(dir.path(), "app-password").unwrap();
        assert_eq!(load_password(dir.path()), "app-password");
    }

    #[test]
    fn backup_carries_and_restores_app_state() {
        let source = tempfile::tempdir().unwrap();
        let library = Library::open(source.path()).unwrap();
        let app_state = serde_json::to_vec(&snapshot_with(
            "s\u{1f}m",
            |s| &mut s.follows,
            1,
            serde_json::json!({"title": "同步的追更"}),
        ))
        .unwrap();
        let backup = source.path().join("backup.yejian.zip");
        library
            .export_backup_with_extras(&backup, &[("app-state.json".into(), app_state.clone())])
            .unwrap();

        let target = tempfile::tempdir().unwrap();
        let restored = Library::open(target.path()).unwrap();
        let summary = restored.restore(&backup).unwrap();
        assert!(summary.app_state.is_some(), "native backup keeps app-state");

        // A backup without the extra file still restores cleanly.
        let backup_old = source.path().join("old.yejian.zip");
        library.export_backup(&backup_old).unwrap();
        let target_old = tempfile::tempdir().unwrap();
        let summary_old = Library::open(target_old.path())
            .unwrap()
            .restore(&backup_old)
            .unwrap();
        assert!(summary_old.app_state.is_none());
    }
}
