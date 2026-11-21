//! Progress tracking against AniList (GraphQL) and Kitsu (JSON:API).
//!
//! Two hard rules shape the whole module:
//!
//! * **Isolation** — every request goes through
//!   [`yejian_sources::send_isolated`], a transport without the source
//!   cookie jar. Bearer tokens never leak into scraped-site traffic and
//!   site cookies never pollute tracker calls.
//! * **Monotone progress** — an automatic sync can only move a remote entry
//!   forward. A remote progress higher than the local chapter number is
//!   never written down, so a second device is never rolled back.
//!
//! Accounts, the auto-sync switch and per-book bindings persist in
//! `trackers.json` inside the data directory (atomic `.part` + rename).

use std::path::{Path, PathBuf};
use std::sync::mpsc;

use serde::{Deserialize, Serialize};
use yejian_sources::{NetRequestData, send_isolated};

const STORE_FILE: &str = "trackers.json";

/// Public OAuth client ids from the predecessor app — these identify the
/// *application* to AniList, never a user account.
fn anilist_client_id() -> &'static str {
    // Public desktop client id; the loopback redirect belongs to it. Mobile
    // flows share the paste-token path, which needs no registered redirect.
    "13587"
}

const ANILIST_GRAPHQL: &str = "https://graphql.anilist.co";
const ANILIST_AUTHORIZE: &str = "https://anilist.co/api/v2/oauth/authorize";
const KITSU_TOKEN: &str = "https://kitsu.app/api/oauth/token";
const KITSU_EDGE: &str = "https://kitsu.app/api/edge";
const KITSU_ALGOLIA: &str = "https://AWQO5J657S-dsn.algolia.net/1/indexes/production_media/query";
const KITSU_ALGOLIA_APP: &str = "AWQO5J657S";
/// Kitsu's public application credentials (as published for third-party
/// clients); still no user data.
const KITSU_CLIENT_ID: &str =
    "dd031b32d2f56c990b1425efe6c42ad847e7fe3ab46bf1299f05ecd856bdb7dd";
const KITSU_CLIENT_SECRET: &str =
    "54d7307928f63414defd96399fc31ba847961ceaecef3a5fd93144e960c0e151";

/// Seconds the desktop loopback OAuth listener waits for the redirect.
const OAUTH_LISTEN_TIMEOUT_SECS: u64 = 300;
/// Fixed loopback port AniList's public desktop client redirects to.
pub const OAUTH_LOOPBACK_PORT: u16 = 43824;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum TrackerKind {
    #[serde(rename = "anilist")]
    Anilist,
    #[serde(rename = "kitsu")]
    Kitsu,
}

impl TrackerKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Anilist => "AniList",
            Self::Kitsu => "Kitsu",
        }
    }
}

/// Reading status mirrored to a tracker entry.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub enum TrackStatus {
    #[default]
    #[serde(rename = "current")]
    Reading,
    #[serde(rename = "completed")]
    Completed,
    #[serde(rename = "planned")]
    Planned,
    #[serde(rename = "paused")]
    OnHold,
    #[serde(rename = "dropped")]
    Dropped,
    #[serde(rename = "repeating")]
    ReReading,
}

impl TrackStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Reading => "在读",
            Self::Completed => "已完成",
            Self::Planned => "计划读",
            Self::OnHold => "搁置",
            Self::Dropped => "弃坑",
            Self::ReReading => "重读",
        }
    }

    fn anilist(self) -> &'static str {
        match self {
            Self::Reading => "CURRENT",
            Self::Completed => "COMPLETED",
            Self::Planned => "PLANNING",
            Self::OnHold => "PAUSED",
            Self::Dropped => "DROPPED",
            Self::ReReading => "REPEATING",
        }
    }

    fn kitsu(self) -> &'static str {
        match self {
            Self::Reading => "current",
            Self::Completed => "completed",
            Self::Planned => "planned",
            Self::OnHold => "on_hold",
            Self::Dropped => "dropped",
            Self::ReReading => "current", // Kitsu has no re-reading flag.
        }
    }

    fn from_anilist(value: &str) -> Self {
        match value {
            "COMPLETED" => Self::Completed,
            "PLANNING" => Self::Planned,
            "PAUSED" => Self::OnHold,
            "DROPPED" => Self::Dropped,
            "REPEATING" => Self::ReReading,
            _ => Self::Reading,
        }
    }

    fn from_kitsu(value: &str) -> Self {
        match value {
            "completed" => Self::Completed,
            "planned" => Self::Planned,
            "on_hold" => Self::OnHold,
            "dropped" => Self::Dropped,
            _ => Self::Reading,
        }
    }
}

/// One remote media entry as shown in the bind picker.
#[derive(Clone, Debug, PartialEq)]
pub struct TrackSearch {
    /// Remote media id (AniList media id / Kitsu manga id).
    pub media_id: String,
    pub title: String,
    pub cover: String,
    pub total_chapters: i64,
    pub status: String,
}

/// The entry a bound book currently has on the tracker.
#[derive(Clone, Debug, PartialEq)]
pub struct TrackEntry {
    /// Remote library-entry id (`None` until first save for Kitsu).
    pub library_id: Option<String>,
    pub media_id: String,
    pub title: String,
    pub status: TrackStatus,
    pub progress: i64,
    pub score: i64,
    pub total_chapters: i64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Account {
    pub user_id: String,
    pub name: String,
    pub avatar: String,
    pub access_token: String,
    /// Unix-millis expiry from the OAuth response; `0` means unknown.
    #[serde(default)]
    pub expires_at: i64,
}

impl Account {
    fn expired(&self) -> bool {
        self.expires_at > 0
            && now_millis() >= self.expires_at
    }
}

/// A book-to-remote-entry binding persisted in `trackers.json`.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Binding {
    /// `anilist` / `kitsu`.
    pub tracker: String,
    pub media_id: String,
    #[serde(default)]
    pub library_id: Option<String>,
    pub title: String,
    /// Highest chapter number we pushed, so auto-sync never regresses.
    #[serde(default)]
    pub progress: i64,
    #[serde(default)]
    pub status: TrackStatus,
}

#[derive(Default, Serialize, Deserialize)]
struct Store {
    #[serde(default)]
    auto_sync: bool,
    #[serde(default)]
    anilist: Option<Account>,
    #[serde(default)]
    kitsu: Option<Account>,
    /// `source:{id}\x1f{manga}` or `book:{id}` → binding.
    #[serde(default)]
    bindings: std::collections::BTreeMap<String, Binding>,
}

fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis() as i64)
}

/// The binding key for an online manga, or a local imported book.
pub fn binding_key(source: Option<(&str, &str)>, book: Option<&str>) -> String {
    match (source, book) {
        (Some((source, manga)), _) => format!("source:{source}\x1f{manga}"),
        (None, Some(book)) => format!("book:{book}"),
        _ => String::new(),
    }
}

/// First positive chapter integer in a title like "第 12 话" or "Ch.34".
/// Returns `None` for titles without a plain number — decimals/extras stay
/// unsynced rather than guessed.
pub fn chapter_number(title: &str) -> Option<i64> {
    let mut digits = String::new();
    for ch in title.chars().chain(std::iter::once(' ')) {
        if ch.is_ascii_digit() {
            digits.push(ch);
            continue;
        }
        if digits.is_empty() {
            continue;
        }
        if let Ok(number) = digits.parse::<i64>()
            && number > 0
        {
            return Some(number);
        }
        digits.clear();
    }
    None
}

/// URL-encoded POST body (`application/x-www-form-urlencoded`).
fn form_body(pairs: &[(&str, &str)]) -> String {
    pairs
        .iter()
        .map(|(key, value)| format!("{}={}", url_encode(key), url_encode(value)))
        .collect::<Vec<_>>()
        .join("&")
}

pub fn url_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

fn post(url: &str, token: Option<&str>, content_type: &str, body: Vec<u8>) -> Result<String, String> {
    let mut headers = vec![
        ("Content-Type".into(), content_type.to_string()),
        ("Accept".into(), "application/json".into()),
    ];
    if let Some(token) = token {
        headers.push(("Authorization".into(), format!("Bearer {token}")));
    }
    let response = send_isolated(NetRequestData {
        method: "POST".into(),
        url: url.into(),
        headers,
        body: Some(body),
        timeout_ms: 30_000,
    });
    if let Some(error) = response.error {
        return Err(error);
    }
    let text = String::from_utf8_lossy(&response.body).into_owned();
    if !(200..300).contains(&response.status_code) {
        return Err(format!("HTTP {}：{}", response.status_code, &text[..text.len().min(160)]));
    }
    Ok(text)
}

fn get(url: &str, token: &str) -> Result<String, String> {
    let response = send_isolated(NetRequestData {
        method: "GET".into(),
        url: url.into(),
        headers: vec![
            ("Accept".into(), "application/vnd.api+json".into()),
            ("Authorization".into(), format!("Bearer {token}")),
        ],
        body: None,
        timeout_ms: 30_000,
    });
    if let Some(error) = response.error {
        return Err(error);
    }
    let text = String::from_utf8_lossy(&response.body).into_owned();
    if !(200..300).contains(&response.status_code) {
        return Err(format!("HTTP {}：{}", response.status_code, &text[..text.len().min(160)]));
    }
    Ok(text)
}

fn graphql(base: &str, token: &str, query: &str, variables: serde_json::Value) -> Result<serde_json::Value, String> {
    let body = serde_json::json!({ "query": query, "variables": variables });
    let text = post(
        base,
        Some(token),
        "application/json",
        serde_json::to_vec(&body).unwrap_or_default(),
    )?;
    let value: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("响应不是 JSON：{e}"))?;
    if let Some(errors) = value.get("errors").and_then(|v| v.as_array())
        && let Some(first) = errors.first()
    {
        let message = first
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("AniList 返回错误");
        return Err(message.to_string());
    }
    value
        .get("data")
        .cloned()
        .ok_or_else(|| "AniList 没有返回数据".to_string())
}

// ---------------------------------------------------------------------------
// AniList
// ---------------------------------------------------------------------------

/// The browser URL the user authorizes at; `code` comes back through the
/// loopback listener or a pasted token.
pub fn anilist_authorize_url(redirect: &str, state: &str) -> String {
    format!(
        "{ANILIST_AUTHORIZE}?client_id={}&redirect_uri={}&response_type=code&state={}",
        anilist_client_id(),
        url_encode(redirect),
        url_encode(state),
    )
}

/// A per-attempt value the loopback callback must echo back.
pub fn oauth_state() -> String {
    use std::hash::{BuildHasher, Hasher};
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or_default();
    let mut parts = [0u64; 2];
    for (index, part) in parts.iter_mut().enumerate() {
        let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
        hasher.write_u128(nanos);
        hasher.write_usize(index);
        hasher.write_u32(std::process::id());
        *part = hasher.finish();
    }
    format!("{:016x}{:016x}", parts[0], parts[1])
}

/// Desktop loopback redirect AniList's public client is registered with.
pub fn anilist_loopback_redirect() -> String {
    format!("http://127.0.0.1:{OAUTH_LOOPBACK_PORT}/success?code=1337")
}

/// Waits (bounded) on `127.0.0.1:{port}` for the OAuth redirect and returns
/// the `code` parameter. Desktop flows call this; mobile uses the paste-token
/// path instead, so the UI never reaches it there.
pub fn wait_oauth_code(
    port: u16,
    timeout: std::time::Duration,
    state: &str,
) -> Result<String, String> {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind(("127.0.0.1", port))
        .map_err(|e| format!("无法监听本地回环端口 {port}：{e}"))?;
    listener
        .set_nonblocking(true)
        .map_err(|e| format!("监听配置失败：{e}"))?;
    let deadline = std::time::Instant::now() + timeout;
    loop {
        match listener.accept() {
            Ok((mut stream, _)) => {
                let mut buffer = [0u8; 4096];
                let read = stream.read(&mut buffer).unwrap_or(0);
                let request = String::from_utf8_lossy(&buffer[..read]).to_string();
                let path = request
                    .lines()
                    .next()
                    .and_then(|line| line.split_whitespace().nth(1))
                    .unwrap_or("/");
                let param = |name: &str| {
                    path.split(['?', '&'])
                        .filter_map(|part| part.strip_prefix(name))
                        .last()
                        .map(percent_decode)
                };
                // The registered redirect already carries `code=1337`; the
                // real code is the last one appended by the provider.
                let code = param("code=");
                if param("state=").as_deref() != Some(state) {
                    let html = "<html><body><h2>授权请求无效，请回到应用重试。</h2></body></html>";
                    let body = format!(
                        "HTTP/1.1 400 Bad Request\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{html}",
                        html.len()
                    );
                    let _ = stream.write_all(body.as_bytes());
                    continue;
                }
                let page = if code.is_some() {
                    "已收到授权，请回到应用继续。"
                } else {
                    "没有读到授权码，请回到应用重试。"
                };
                let html = format!("<html><body><h2>{page}</h2></body></html>");
                let body = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{html}",
                    html.len()
                );
                let _ = stream.write_all(body.as_bytes());
                return code.ok_or_else(|| "回调中没有授权码".to_string());
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                if std::time::Instant::now() >= deadline {
                    return Err("等待授权超时".to_string());
                }
                std::thread::sleep(std::time::Duration::from_millis(120));
            }
            Err(error) => return Err(format!("本地监听失败：{error}")),
        }
    }
}

fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            if let Ok(byte) = u8::from_str_radix(&value[index + 1..index + 3], 16) {
                out.push(byte);
                index += 3;
                continue;
            }
        }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Exchanges an authorization code for an access token.
fn anilist_exchange_code(base: &str, code: &str, redirect: &str) -> Result<Account, String> {
    // The token endpoint lives under the same host as the API root in
    // production; tests point `base` at a loopback mock.
    let body = serde_json::json!({
        "grant_type": "authorization_code",
        "client_id": anilist_client_id(),
        "client_secret": "",
        "redirect_uri": redirect,
        "code": code,
    });
    let text = post(base, None, "application/json", serde_json::to_vec(&body).unwrap_or_default())?;
    let value: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("令牌响应不是 JSON：{e}"))?;
    let token = value
        .get("access_token")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "AniList 没有返回访问令牌".to_string())?
        .to_string();
    let expires_in = value
        .get("expires_in")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    Ok(Account {
        access_token: token,
        expires_at: now_millis() + expires_in * 1000,
        ..Default::default()
    })
}

/// `Viewer` for a token; also finishes an `Account` started by code exchange.
fn anilist_viewer(graphql_url: &str, token: &str) -> Result<Account, String> {
    let data = graphql(
        graphql_url,
        token,
        "query { Viewer { id name avatar { large } mediaListOptions { scoreFormat } } }",
        serde_json::json!({}),
    )?;
    let viewer = &data["Viewer"];
    Ok(Account {
        user_id: viewer["id"].as_i64().map(|v| v.to_string()).unwrap_or_default(),
        name: viewer["name"].as_str().unwrap_or_default().to_string(),
        avatar: viewer["avatar"]["large"].as_str().unwrap_or_default().to_string(),
        access_token: token.to_string(),
        ..Default::default()
    })
}

fn anilist_search(graphql_url: &str, token: &str, query: &str) -> Result<Vec<TrackSearch>, String> {
    let document = "query ($q: String) { Page(perPage: 20) { media(search: $q, type: MANGA, format_not_in: [NOVEL]) { id title { userPreferred } coverImage { large } status chapters } } }";
    let data = graphql(
        graphql_url,
        token,
        document,
        serde_json::json!({ "q": query }),
    )?;
    let media = data["Page"]["media"].as_array().cloned().unwrap_or_default();
    Ok(media
        .iter()
        .filter_map(|item| {
            Some(TrackSearch {
                media_id: item["id"].as_i64()?.to_string(),
                title: item["title"]["userPreferred"].as_str().unwrap_or("").to_string(),
                cover: item["coverImage"]["large"].as_str().unwrap_or("").to_string(),
                total_chapters: item["chapters"].as_i64().unwrap_or(0),
                status: item["status"].as_str().unwrap_or("").to_string(),
            })
        })
        .collect())
}

fn anilist_entry(graphql_url: &str, token: &str, user_id: &str, media_id: &str)
    -> Result<Option<TrackEntry>, String>
{
    let document = "query ($user: Int!, $media: Int!) { Page { mediaList(userId: $user, type: MANGA, mediaId: $media) { id status scoreRaw: score(format: POINT_100) progress media { id title { userPreferred } chapters } } } }";
    let data = graphql(
        graphql_url,
        token,
        document,
        serde_json::json!({
            "user": user_id.parse::<i64>().unwrap_or(0),
            "media": media_id.parse::<i64>().unwrap_or(0),
        }),
    )?;
    let list = data["Page"]["mediaList"].as_array().cloned().unwrap_or_default();
    Ok(list.first().map(|entry| TrackEntry {
        library_id: entry["id"].as_i64().map(|v| v.to_string()),
        media_id: media_id.to_string(),
        title: entry["media"]["title"]["userPreferred"].as_str().unwrap_or("").to_string(),
        status: TrackStatus::from_anilist(entry["status"].as_str().unwrap_or("CURRENT")),
        progress: entry["progress"].as_i64().unwrap_or(0),
        score: entry["scoreRaw"].as_i64().unwrap_or(0),
        total_chapters: entry["media"]["chapters"].as_i64().unwrap_or(0),
    }))
}

fn anilist_save(
    graphql_url: &str,
    token: &str,
    entry: &Binding,
    status: TrackStatus,
    progress: i64,
    score: i64,
) -> Result<TrackEntry, String> {
    let media_id = entry.media_id.parse::<i64>().unwrap_or(0);
    let (document, variables) = match &entry.library_id {
        Some(library_id) => (
            "mutation ($id: Int!, $progress: Int!, $status: MediaListStatus, $score: Int) { SaveMediaListEntry(id: $id, progress: $progress, status: $status, scoreRaw: $score) { id status progress } }",
            serde_json::json!({
                "id": library_id.parse::<i64>().unwrap_or(0),
                "progress": progress,
                "status": status.anilist(),
                "score": if score > 0 { serde_json::json!(score) } else { serde_json::Value::Null },
            }),
        ),
        None => (
            "mutation ($mediaId: Int!, $progress: Int!, $status: MediaListStatus) { SaveMediaListEntry(mediaId: $mediaId, progress: $progress, status: $status) { id status progress } }",
            serde_json::json!({
                "mediaId": media_id,
                "progress": progress,
                "status": status.anilist(),
            }),
        ),
    };
    let data = graphql(graphql_url, token, document, variables)?;
    let saved = &data["SaveMediaListEntry"];
    Ok(TrackEntry {
        library_id: saved["id"].as_i64().map(|v| v.to_string()),
        media_id: entry.media_id.clone(),
        title: entry.title.clone(),
        status,
        progress,
        score,
        total_chapters: 0,
    })
}

// ---------------------------------------------------------------------------
// Kitsu
// ---------------------------------------------------------------------------

fn kitsu_login(token_url: &str, edge: &str, user: &str, password: &str) -> Result<Account, String> {
    let body = form_body(&[
        ("grant_type", "password"),
        ("username", user),
        ("password", password),
        ("client_id", KITSU_CLIENT_ID),
        ("client_secret", KITSU_CLIENT_SECRET),
    ]);
    let text = post(
        token_url,
        None,
        "application/x-www-form-urlencoded",
        body.into_bytes(),
    )?;
    let value: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("登录响应不是 JSON：{e}"))?;
    let token = value["access_token"]
        .as_str()
        .ok_or_else(|| "Kitsu 没有返回访问令牌".to_string())?
        .to_string();
    let expires_in = value["expires_in"].as_i64().unwrap_or(0);
    let mut account = Account {
        access_token: token,
        expires_at: now_millis() + expires_in * 1000,
        ..Default::default()
    };
    let me = get(&format!("{edge}/users?filter[self]=true"), &account.access_token)?;
    let data: serde_json::Value =
        serde_json::from_str(&me).map_err(|e| format!("用户响应不是 JSON：{e}"))?;
    let first = &data["data"][0];
    account.user_id = first["id"].as_str().unwrap_or_default().to_string();
    account.name = first["attributes"]["name"]
        .as_str()
        .or_else(|| first["attributes"]["slug"].as_str())
        .unwrap_or_default()
        .to_string();
    account.avatar = first["attributes"]["avatar"]["original"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    Ok(account)
}

fn kitsu_search(edge: &str, algolia: &str, token: &str, query: &str) -> Result<Vec<TrackSearch>, String> {
    // Algolia key first (same flow the Dart client used), then the query.
    let key_text = get(&format!("{edge}/algolia-keys/media/"), token)?;
    let key: serde_json::Value =
        serde_json::from_str(&key_text).map_err(|e| format!("搜索密钥响应不是 JSON：{e}"))?;
    let api_key = key["media"]["key"]
        .as_str()
        .ok_or_else(|| "Kitsu 没有返回搜索密钥".to_string())?;
    let params = format!(
        "query={}&facetFilters=%5B%22kind%3Amanga%22%5D&attributesToRetrieve=%5B%22synopsis%22%2C%22canonicalTitle%22%2C%22chapterCount%22%2C%22posterImage%22%2C%22subtype%22%2C%22id%22%5D",
        url_encode(query)
    );
    let response = send_isolated(NetRequestData {
        method: "POST".into(),
        url: algolia.into(),
        headers: vec![
            ("Content-Type".into(), "application/json".into()),
            ("X-Algolia-Application-Id".into(), KITSU_ALGOLIA_APP.into()),
            ("X-Algolia-API-Key".into(), api_key.to_string()),
        ],
        body: Some(serde_json::to_vec(&serde_json::json!({ "params": params })).unwrap_or_default()),
        timeout_ms: 30_000,
    });
    if let Some(error) = response.error {
        return Err(error);
    }
    let text = String::from_utf8_lossy(&response.body).into_owned();
    let hits: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("搜索结果不是 JSON：{e}"))?;
    Ok(hits["hits"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter(|hit| hit["subtype"].as_str() != Some("novel"))
        .filter_map(|hit| {
            Some(TrackSearch {
                media_id: hit["id"].as_i64().map(|v| v.to_string())
                    .or_else(|| hit["id"].as_str().map(str::to_string))?,
                title: hit["canonicalTitle"].as_str().unwrap_or("").to_string(),
                cover: hit["posterImage"]["original"].as_str().unwrap_or("").to_string(),
                total_chapters: hit["chapterCount"].as_i64().unwrap_or(0),
                status: hit["subtype"].as_str().unwrap_or("").to_string(),
            })
        })
        .collect())
}

fn kitsu_entry(edge: &str, token: &str, user_id: &str, media_id: &str)
    -> Result<Option<TrackEntry>, String>
{
    let url = format!(
        "{edge}/library-entries?filter[manga_id]={}&filter[user_id]={}&include=manga",
        url_encode(media_id),
        url_encode(user_id),
    );
    let text = get(&url, token)?;
    let value: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("条目响应不是 JSON：{e}"))?;
    let data = value["data"].as_array().cloned().unwrap_or_default();
    let Some(entry) = data.first() else {
        return Ok(None);
    };
    let attributes = &entry["attributes"];
    let included = value["included"].as_array().cloned().unwrap_or_default();
    let media = included.first().cloned().unwrap_or_default();
    Ok(Some(TrackEntry {
        library_id: entry["id"].as_str().map(str::to_string),
        media_id: media_id.to_string(),
        title: media["attributes"]["canonicalTitle"].as_str().unwrap_or("").to_string(),
        status: TrackStatus::from_kitsu(attributes["status"].as_str().unwrap_or("current")),
        progress: attributes["progress"].as_i64().unwrap_or(0),
        score: attributes["ratingTwenty"].as_i64().unwrap_or(0) / 2,
        total_chapters: media["attributes"]["chapterCount"].as_i64().unwrap_or(0),
    }))
}

fn kitsu_save(
    edge: &str,
    token: &str,
    user_id: &str,
    binding: &Binding,
    status: TrackStatus,
    progress: i64,
) -> Result<TrackEntry, String> {
    let is_new = binding.library_id.is_none();
    let payload = if is_new {
        serde_json::json!({
            "data": {
                "type": "libraryEntries",
                "attributes": { "status": status.kitsu(), "progress": progress },
                "relationships": {
                    "user": { "data": { "id": user_id, "type": "users" } },
                    "media": { "data": { "id": binding.media_id, "type": "manga" } },
                },
            }
        })
    } else {
        serde_json::json!({
            "data": {
                "type": "libraryEntries",
                "id": binding.library_id,
                "attributes": { "status": status.kitsu(), "progress": progress },
            }
        })
    };
    let (method, url) = if is_new {
        ("POST", format!("{edge}/library-entries"))
    } else {
        ("PATCH", format!("{edge}/library-entries/{}", binding.library_id.as_deref().unwrap_or("0")))
    };
    let response = send_isolated(NetRequestData {
        method: method.into(),
        url,
        headers: vec![
            ("Content-Type".into(), "application/vnd.api+json".into()),
            ("Authorization".into(), format!("Bearer {token}")),
        ],
        body: Some(serde_json::to_vec(&payload).unwrap_or_default()),
        timeout_ms: 30_000,
    });
    if let Some(error) = response.error {
        return Err(error);
    }
    let text = String::from_utf8_lossy(&response.body).into_owned();
    if !(200..300).contains(&response.status_code) {
        return Err(format!("HTTP {}：{}", response.status_code, &text[..text.len().min(160)]));
    }
    let mut library_id = binding.library_id.clone();
    if is_new {
        let value: serde_json::Value = serde_json::from_str(&text).unwrap_or_default();
        library_id = value["data"]["id"].as_str().map(str::to_string);
    }
    Ok(TrackEntry {
        library_id,
        media_id: binding.media_id.clone(),
        title: binding.title.clone(),
        status,
        progress,
        score: 0,
        total_chapters: 0,
    })
}

// ---------------------------------------------------------------------------
// Persistent tracker store + background commands
// ---------------------------------------------------------------------------

/// One unit of work for the tracker worker thread; the UI stays on egui.
pub enum TrackCommand {
    /// Finish a desktop OAuth flow: open the authorize page, wait on the
    /// loopback port, exchange the code, fetch the viewer.
    AnilistOAuth,
    /// All platforms: the user pasted an AniList access token.
    AnilistToken(String),
    /// Kitsu username + password grant.
    KitsuLogin(String, String),
    Logout(TrackerKind),
    SetAutoSync(bool),
    /// Search remote entries for the bind picker.
    Search { tracker: TrackerKind, key: String, query: String },
    /// Bind `key` to a picked media and (optionally) push progress now.
    Bind { tracker: TrackerKind, key: String, pick: TrackSearch, progress: i64 },
    Unbind(String),
    /// Fetch the remote entry for the bound key.
    Refresh(String),
    /// Set status/progress on the bound entry (manual panel edits).
    Update { key: String, status: TrackStatus, progress: i64 },
    /// Auto-sync after a chapter finished; guarded by private/auto flags.
    ChapterDone { key: String, chapter: i64 },
}

/// Results the UI consumes; strings are already user-readable.
/// Payloads stay attached even where the UI only reloads state today — a
/// later panel uses `key`/`title`/`progress` without another event shape.
#[allow(dead_code)]
#[derive(Debug)]
pub enum TrackEvent {
    Account { tracker: TrackerKind, name: String },
    LoggedOut(TrackerKind),
    AutoSync(bool),
    SearchResults { key: String, tracker: TrackerKind, list: Vec<TrackSearch> },
    /// Sent after every command, including skipped and no-op ones.
    Done,
    Bound { key: String, title: String },
    Unbound(String),
    Entry { key: String, entry: Option<TrackEntry> },
    Synced { key: String, progress: i64 },
    Notice(String),
    Failed(String),
}

/// URL overrides so tests point the clients at `127.0.0.1` mocks.
#[derive(Clone)]
struct Endpoints {
    graphql: String,
    anilist_token: String,
    kitsu_token: String,
    kitsu_edge: String,
    kitsu_algolia: String,
}

impl Default for Endpoints {
    fn default() -> Self {
        Self {
            graphql: ANILIST_GRAPHQL.into(),
            anilist_token: "https://anilist.co/api/v2/oauth/token".into(),
            kitsu_token: KITSU_TOKEN.into(),
            kitsu_edge: KITSU_EDGE.into(),
            kitsu_algolia: KITSU_ALGOLIA.into(),
        }
    }
}

/// Persistent tracker state plus the worker queue.
pub struct Trackers {
    root: PathBuf,
    store: Store,
    tx: mpsc::Sender<(u64, TrackCommand)>,
    rx: mpsc::Receiver<TrackEvent>,
    request: std::sync::Arc<std::sync::atomic::AtomicU64>,
    /// Latest result per binding key for the detail panel.
    pub entries: std::collections::HashMap<String, Option<TrackEntry>>,
    /// Latest search results per binding key, with the tracker they came from.
    pub searches: std::collections::HashMap<String, (TrackerKind, Vec<TrackSearch>)>,
    /// Commands queued but not yet finished by the worker.
    pending: usize,
    /// In-flight flag for the panel spinner.
    pub busy: bool,
    /// Last failure text, shown beside the settings block.
    pub error: Option<String>,
    /// Soft messages the app shell shows in its notice bar — sync results
    /// never open an error dialog.
    pub notices: Vec<String>,
}

impl Trackers {
    pub fn new(root: PathBuf, ctx: egui::Context) -> Self {
        Self::with_endpoints(root, ctx, Endpoints::default())
    }

    fn with_endpoints(root: PathBuf, ctx: egui::Context, endpoints: Endpoints) -> Self {
        let store: Store = std::fs::read(root.join(STORE_FILE))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        let (tx, cmd_rx) = mpsc::channel::<(u64, TrackCommand)>();
        let (event_tx, rx) = mpsc::channel::<TrackEvent>();
        let request = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
        let worker_request = request.clone();
        let worker_root = root.clone();
        std::thread::Builder::new()
            .name("yejian-trackers".into())
            .spawn(move || {
                while let Ok((id, command)) = cmd_rx.recv() {
                    if id != worker_request.load(std::sync::atomic::Ordering::Relaxed)
                        && matches!(command, TrackCommand::Search { .. } | TrackCommand::Refresh(_))
                    {
                        let _ = event_tx.send(TrackEvent::Done);
                        ctx.request_repaint();
                        continue;
                    }
                    let store_path = worker_root.join(STORE_FILE);
                    let events = run_command(command, &store_path, &endpoints);
                    for event in events {
                        let _ = event_tx.send(event);
                    }
                    let _ = event_tx.send(TrackEvent::Done);
                    ctx.request_repaint();
                }
            })
            .expect("unable to start tracker worker");
        Self {
            root,
            store,
            tx,
            rx,
            request,
            entries: Default::default(),
            searches: Default::default(),
            pending: 0,
            busy: false,
            error: None,
            notices: Vec::new(),
        }
    }

    pub fn account(&self, tracker: TrackerKind) -> Option<&Account> {
        match tracker {
            TrackerKind::Anilist => self.store.anilist.as_ref(),
            TrackerKind::Kitsu => self.store.kitsu.as_ref(),
        }
    }

    pub fn auto_sync(&self) -> bool {
        self.store.auto_sync
    }

    pub fn binding(&self, key: &str) -> Option<&Binding> {
        self.store.bindings.get(key)
    }

    /// True when a new command may be queued (never two OAuth dialogs at
    /// once).
    pub fn idle(&self) -> bool {
        !self.busy
    }

    pub fn submit(&mut self, command: TrackCommand) {
        self.request.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let id = self.request.load(std::sync::atomic::Ordering::Relaxed);
        if self.tx.send((id, command)).is_err() {
            self.error = Some("同步服务不可用".into());
            return;
        }
        self.pending += 1;
        self.busy = true;
    }

    /// Called by the reader when a chapter finished. The private-mode gate
    /// lives in the UI; here we additionally honour the auto switch and the
    /// monotone rule.
    pub fn chapter_done(&mut self, key: &str, chapter: i64) {
        if !self.store.auto_sync || chapter <= 0 {
            return;
        }
        let Some(binding) = self.store.bindings.get(key) else {
            return;
        };
        if chapter <= binding.progress {
            return;
        }
        self.submit(TrackCommand::ChapterDone {
            key: key.to_string(),
            chapter,
        });
    }

    pub fn poll(&mut self) {
        while let Ok(event) = self.rx.try_recv() {
            match event {
                TrackEvent::Account { tracker, name } => {
                    self.error = None;
                    // The worker already persisted the account.
                    self.reload_store();
                    let _ = (tracker, name);
                }
                TrackEvent::LoggedOut(_) | TrackEvent::AutoSync(_)
                | TrackEvent::Unbound(_) => self.reload_store(),
                TrackEvent::SearchResults { key, tracker, list } => {
                    self.searches.insert(key, (tracker, list));
                }
                TrackEvent::Done => {
                    self.pending = self.pending.saturating_sub(1);
                    self.busy = self.pending > 0;
                }
                TrackEvent::Bound { .. } | TrackEvent::Synced { .. } => self.reload_store(),
                TrackEvent::Entry { key, entry } => {
                    self.entries.insert(key, entry);
                }
                TrackEvent::Notice(message) => self.notices.push(message),
                TrackEvent::Failed(message) => self.error = Some(message),
            }
        }
    }

    fn reload_store(&mut self) {
        if let Ok(bytes) = std::fs::read(self.root.join(STORE_FILE))
            && let Ok(store) = serde_json::from_slice::<Store>(&bytes)
        {
            self.store = store;
        }
    }
}

/// Executes one command on the worker thread; account/store mutations are
/// persisted atomically before the UI is told.
fn run_command(command: TrackCommand, store_path: &Path, endpoints: &Endpoints) -> Vec<TrackEvent> {
    let mut store: Store = std::fs::read(store_path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default();
    let save = |store: &Store| -> Result<(), String> {
        if let Some(parent) = store_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let part = store_path.with_extension("json.part");
        write_private(&part, &serde_json::to_vec_pretty(store).unwrap_or_default())
            .map_err(|e| format!("写入 trackers.json 失败：{e}"))?;
        std::fs::rename(&part, store_path).map_err(|e| format!("保存 trackers.json 失败：{e}"))
    };
    let fail = |message: String| vec![TrackEvent::Failed(message)];

    match command {
        TrackCommand::AnilistOAuth => {
            #[cfg(any(target_os = "android", target_os = "ios"))]
            return fail("移动端请使用「粘贴令牌」登录".into());
            #[cfg(not(any(target_os = "android", target_os = "ios")))]
            {
                let redirect = anilist_loopback_redirect();
                let state = oauth_state();
                let url = anilist_authorize_url(&redirect, &state);
                if let Err(error) = yejian_platform::open_external_url(&url) {
                    return fail(format!("无法打开授权页：{error}"));
                }
                let code = match wait_oauth_code(
                    OAUTH_LOOPBACK_PORT,
                    std::time::Duration::from_secs(OAUTH_LISTEN_TIMEOUT_SECS),
                    &state,
                ) {
                    Ok(code) => code,
                    Err(error) => return fail(format!("等待 AniList 授权失败：{error}")),
                };
                let mut account = match anilist_exchange_code(
                    &endpoints.anilist_token,
                    &code,
                    &redirect,
                ) {
                    Ok(account) => account,
                    Err(error) => return fail(format!("AniList 令牌交换失败：{error}")),
                };
                match anilist_viewer(&endpoints.graphql, &account.access_token) {
                    Ok(viewer) => {
                        account.user_id = viewer.user_id;
                        account.name = viewer.name.clone();
                        account.avatar = viewer.avatar;
                    }
                    Err(error) => return fail(format!("读取 AniList 账号失败：{error}")),
                }
                store.anilist = Some(account.clone());
                if let Err(error) = save(&store) {
                    return fail(error);
                }
                vec![TrackEvent::Account {
                    tracker: TrackerKind::Anilist,
                    name: account.name,
                }]
            }
        }
        TrackCommand::AnilistToken(token) => {
            let token = token.trim().to_string();
            if token.is_empty() {
                return fail("请粘贴 AniList 访问令牌".into());
            }
            match anilist_viewer(&endpoints.graphql, &token) {
                Ok(account) => {
                    let name = account.name.clone();
                    store.anilist = Some(account);
                    if let Err(error) = save(&store) {
                        return fail(error);
                    }
                    vec![TrackEvent::Account {
                        tracker: TrackerKind::Anilist,
                        name,
                    }]
                }
                Err(error) => fail(format!("AniList 令牌无效：{error}")),
            }
        }
        TrackCommand::KitsuLogin(user, password) => {
            match kitsu_login(&endpoints.kitsu_token, &endpoints.kitsu_edge, &user, &password) {
                Ok(account) => {
                    let name = account.name.clone();
                    store.kitsu = Some(account);
                    if let Err(error) = save(&store) {
                        return fail(error);
                    }
                    vec![TrackEvent::Account {
                        tracker: TrackerKind::Kitsu,
                        name,
                    }]
                }
                Err(error) => fail(format!("Kitsu 登录失败：{error}")),
            }
        }
        TrackCommand::Logout(tracker) => {
            match tracker {
                TrackerKind::Anilist => store.anilist = None,
                TrackerKind::Kitsu => store.kitsu = None,
            }
            store
                .bindings
                .retain(|_, binding| binding.tracker != tracker_label(tracker));
            if let Err(error) = save(&store) {
                return fail(error);
            }
            vec![TrackEvent::LoggedOut(tracker)]
        }
        TrackCommand::SetAutoSync(on) => {
            store.auto_sync = on;
            if let Err(error) = save(&store) {
                return fail(error);
            }
            vec![TrackEvent::AutoSync(on)]
        }
        TrackCommand::Search { tracker, key, query } => {
            let account = match account_for(&store, tracker) {
                Some(account) => account,
                None => return fail(format!("请先登录 {}", tracker.label())),
            };
            let result = match tracker {
                TrackerKind::Anilist => {
                    anilist_search(&endpoints.graphql, &account.access_token, &query)
                }
                TrackerKind::Kitsu => kitsu_search(
                    &endpoints.kitsu_edge,
                    &endpoints.kitsu_algolia,
                    &account.access_token,
                    &query,
                ),
            };
            match result {
                Ok(list) => vec![TrackEvent::SearchResults { key, tracker, list }],
                Err(error) => fail(format!("{} 搜索失败：{error}", tracker.label())),
            }
        }
        TrackCommand::Bind { tracker, key, pick, progress } => {
            let Some(account) = account_for(&store, tracker).cloned() else {
                return fail(format!("请先登录 {}", tracker.label()));
            };
            // Adopt an entry the reader already has instead of overwriting it.
            let existing = match tracker {
                TrackerKind::Anilist => anilist_entry(
                    &endpoints.graphql,
                    &account.access_token,
                    &account.user_id,
                    &pick.media_id,
                ),
                TrackerKind::Kitsu => kitsu_entry(
                    &endpoints.kitsu_edge,
                    &account.access_token,
                    &account.user_id,
                    &pick.media_id,
                ),
            };
            let existing = match existing {
                Ok(existing) => existing,
                Err(error) => return fail(format!("读取远端条目失败：{error}")),
            };
            let mut binding = Binding {
                tracker: tracker_label(tracker).to_string(),
                media_id: pick.media_id.clone(),
                library_id: None,
                title: pick.title.clone(),
                progress: 0,
                status: TrackStatus::Reading,
            };
            if let Some(entry) = &existing {
                binding.library_id = entry.library_id.clone();
                binding.progress = entry.progress;
                binding.status = entry.status;
            }
            let remote = binding.progress;
            store.bindings.insert(key.clone(), binding);
            let mut all = vec![TrackEvent::Bound {
                key: key.clone(),
                title: pick.title,
            }];
            if existing.is_none() || progress > remote {
                // Create the entry, or move it forward to local progress.
                all.extend(push_progress(&mut store, &key, progress.max(remote), &save, endpoints));
            } else {
                if let Err(error) = save(&store) {
                    return fail(error);
                }
                all.push(TrackEvent::Entry { key, entry: existing });
            }
            all
        }
        TrackCommand::Unbind(key) => {
            store.bindings.remove(&key);
            if let Err(error) = save(&store) {
                return fail(error);
            }
            vec![TrackEvent::Unbound(key)]
        }
        TrackCommand::Refresh(key) => {
            let Some(binding) = store.bindings.get(&key).cloned() else {
                return vec![TrackEvent::Entry { key, entry: None }];
            };
            let Some(account) = account_for(&store, tracker_kind(&binding.tracker)) else {
                return fail("跟踪账号已退出，请重新登录".into());
            };
            let entry = match tracker_kind(&binding.tracker) {
                TrackerKind::Anilist => anilist_entry(
                    &endpoints.graphql,
                    &account.access_token,
                    &account.user_id,
                    &binding.media_id,
                ),
                TrackerKind::Kitsu => kitsu_entry(
                    &endpoints.kitsu_edge,
                    &account.access_token,
                    &account.user_id,
                    &binding.media_id,
                ),
            };
            match entry {
                Ok(entry) => {
                    if let Some(entry) = &entry
                        && let Some(binding) = store.bindings.get_mut(&key)
                    {
                        binding.library_id = entry.library_id.clone();
                        binding.progress = binding.progress.max(entry.progress);
                        let _ = save(&store);
                    }
                    vec![TrackEvent::Entry { key, entry }]
                }
                Err(error) => fail(format!("读取进度失败：{error}")),
            }
        }
        TrackCommand::Update { key, status, progress } => {
            if let Some(binding) = store.bindings.get_mut(&key) {
                binding.status = status;
            } else {
                return fail("这部作品还没有绑定跟踪条目".into());
            }
            let mut events = push_progress(&mut store, &key, progress, &save, endpoints);
            if events.is_empty() {
                events.push(TrackEvent::Notice("已保存".into()));
            }
            events
        }
        TrackCommand::ChapterDone { key, chapter } => {
            let binding = store.bindings.get(&key).cloned();
            let Some(binding) = binding else {
                return Vec::new();
            };
            if chapter <= binding.progress {
                return Vec::new();
            }
            // Remote can be ahead (another device); never push backwards.
            match remote_progress(&store, &binding, endpoints) {
                Ok(remote) if remote > chapter => Vec::new(),
                Ok(_) => push_progress(&mut store, &key, chapter, &save, endpoints),
                Err(error) => vec![TrackEvent::Notice(format!("同步进度失败：{error}"))],
            }
        }
    }
}

/// Writes a file only the current user can read: it holds access tokens.
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        file.write_all(bytes)?;
        file.sync_all()
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, bytes)
    }
}

fn tracker_label(tracker: TrackerKind) -> &'static str {
    match tracker {
        TrackerKind::Anilist => "anilist",
        TrackerKind::Kitsu => "kitsu",
    }
}

fn tracker_kind(value: &str) -> TrackerKind {
    match value {
        "kitsu" => TrackerKind::Kitsu,
        _ => TrackerKind::Anilist,
    }
}

fn account_for<'a>(store: &'a Store, tracker: TrackerKind) -> Option<&'a Account> {
    match tracker {
        TrackerKind::Anilist => store.anilist.as_ref(),
        TrackerKind::Kitsu => store.kitsu.as_ref(),
    }
    .filter(|account| !account.expired())
}

/// Reads the remote entry's progress — the floor a sync may never go below.
fn remote_progress(store: &Store, binding: &Binding, endpoints: &Endpoints) -> Result<i64, String> {
    let Some(account) = account_for(store, tracker_kind(&binding.tracker)) else {
        return Err("跟踪账号未登录".into());
    };
    let entry = match tracker_kind(&binding.tracker) {
        TrackerKind::Anilist => anilist_entry(
            &endpoints.graphql,
            &account.access_token,
            &account.user_id,
            &binding.media_id,
        ),
        TrackerKind::Kitsu => kitsu_entry(
            &endpoints.kitsu_edge,
            &account.access_token,
            &account.user_id,
            &binding.media_id,
        ),
    }?;
    Ok(entry.map(|entry| entry.progress).unwrap_or(0))
}

/// Writes `progress` to the bound entry, creating it when needed; the local
/// binding records `max(remote, pushed)` afterwards.
fn push_progress(
    store: &mut Store,
    key: &str,
    progress: i64,
    save: &dyn Fn(&Store) -> Result<(), String>,
    endpoints: &Endpoints,
) -> Vec<TrackEvent> {
    let Some(binding) = store.bindings.get(key).cloned() else {
        return vec![TrackEvent::Failed("这部作品还没有绑定跟踪条目".into())];
    };
    let tracker = tracker_kind(&binding.tracker);
    let Some(account) = account_for(store, tracker).cloned() else {
        return vec![TrackEvent::Failed(format!("请先登录 {}", tracker.label()))];
    };
    let status = if binding.status == TrackStatus::Completed {
        TrackStatus::Completed
    } else {
        binding.status
    };
    let result = match tracker {
        TrackerKind::Anilist => anilist_save(
            &endpoints.graphql,
            &account.access_token,
            &binding,
            status,
            progress,
            0,
        ),
        TrackerKind::Kitsu => kitsu_save(
            &endpoints.kitsu_edge,
            &account.access_token,
            &account.user_id,
            &binding,
            status,
            progress,
        ),
    };
    match result {
        Ok(entry) => {
            if let Some(binding) = store.bindings.get_mut(key) {
                binding.library_id = entry.library_id.clone().or(binding.library_id.clone());
                binding.progress = binding.progress.max(progress);
            }
            if let Err(error) = save(store) {
                return vec![TrackEvent::Failed(error)];
            }
            vec![
                TrackEvent::Synced { key: key.to_string(), progress },
                TrackEvent::Notice(format!("已同步到 {} · 第 {progress} 章", tracker.label())),
            ]
        }
        Err(error) => vec![TrackEvent::Notice(format!("同步进度失败：{error}"))],
    }
}

/// Paste text → `(name, value)` cookie pairs. Accepts every paste shape a
/// user realistically copies from a browser:
///
/// * `name=value; a=b` header contents,
/// * a full `Cookie: name=value; ...` line,
/// * Netscape `cookies.txt` lines,
/// * a JSON array of `{ "name": ..., "value": ... }` objects.
pub fn parse_cookie_paste(text: &str) -> Vec<(String, String)> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    if trimmed.starts_with('[')
        && let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed)
        && let Some(items) = value.as_array()
    {
        return items
            .iter()
            .filter_map(|item| {
                Some((
                    item.get("name")?.as_str()?.to_string(),
                    item.get("value")?.as_str()?.to_string(),
                ))
            })
            .collect();
    }
    let mut cookies = Vec::new();
    for line in trimmed.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        // Netscape cookies.txt: 7 tab-separated fields, cookie is fields 6,7.
        let tabs: Vec<&str> = line.split('\t').collect();
        if tabs.len() == 7 {
            cookies.push((tabs[5].to_string(), tabs[6].to_string()));
            continue;
        }
        let line = line
            .strip_prefix("Cookie:")
            .or_else(|| line.strip_prefix("cookie:"))
            .unwrap_or(line)
            .trim();
        for pair in line.split(';') {
            let Some((name, value)) = pair.split_once('=') else {
                continue;
            };
            let name = name.trim();
            if name.is_empty() {
                continue;
            }
            cookies.push((name.to_string(), value.trim().to_string()));
        }
    }
    cookies
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    fn serve(mut handler: impl FnMut(&str, &str) -> (u16, String) + Send + 'static) -> (String, std::thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = std::thread::spawn(move || loop {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            let mut buffer = vec![0u8; 64 * 1024];
            let mut request = Vec::new();
            let read = stream.read(&mut buffer).unwrap_or(0);
            request.extend_from_slice(&buffer[..read]);
            // Read the rest of the body for POSTs with bigger payloads.
            let text = String::from_utf8_lossy(&request).to_string();
            if let Some(length) = text
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .map(str::to_string)
                })
                .and_then(|v| v.trim().parse::<usize>().ok())
            {
                let header_end = text.find("\r\n\r\n").map(|i| i + 4).unwrap_or(text.len());
                let mut have = request.len().saturating_sub(header_end);
                while have < length {
                    let read = stream.read(&mut buffer).unwrap_or(0);
                    if read == 0 {
                        break;
                    }
                    request.extend_from_slice(&buffer[..read]);
                    have += read;
                }
            }
            let text = String::from_utf8_lossy(&request).to_string();
            let mut parts = text.splitn(2, "\r\n\r\n");
            let head = parts.next().unwrap_or("");
            let body = parts.next().unwrap_or("").to_string();
            let (status, reply) = handler(head, &body);
            let response = format!(
                "HTTP/1.1 {status} OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",
                reply.len()
            );
            let _ = stream.write_all(response.as_bytes());
        });
        (format!("http://{addr}"), handle)
    }

    #[test]
    fn cookie_paste_accepts_all_common_formats() {
        assert_eq!(
            parse_cookie_paste("cf_clearance=abc; session=xyz"),
            vec![("cf_clearance".into(), "abc".into()), ("session".into(), "xyz".into())]
        );
        assert_eq!(
            parse_cookie_paste("Cookie: a=1; b=2\nOther: ignored"),
            vec![("a".into(), "1".into()), ("b".into(), "2".into())]
        );
        assert_eq!(
            parse_cookie_paste(".example.com\tTRUE\t/\tTRUE\t0\tsid\t42"),
            vec![("sid".into(), "42".into())]
        );
        assert_eq!(
            parse_cookie_paste(r#"[{"name":"n","value":"v"},{"name":"m","value":"w"}]"#),
            vec![("n".into(), "v".into()), ("m".into(), "w".into())]
        );
        assert!(parse_cookie_paste("").is_empty());
        assert!(parse_cookie_paste("no-equals-here").is_empty());
    }

    #[test]
    fn chapter_number_takes_the_first_positive_integer() {
        assert_eq!(chapter_number("第 12 话"), Some(12));
        assert_eq!(chapter_number("Ch.34"), Some(34));
        assert_eq!(chapter_number("外传"), None);
        assert_eq!(chapter_number("第 0 话"), None);
        assert_eq!(chapter_number("Vol 3 Ch 7"), Some(3));
    }

    #[test]
    fn binding_keys_match_the_spec() {
        assert_eq!(
            binding_key(Some(("src.a", "manga-1")), None),
            "source:src.a\x1fmanga-1"
        );
        assert_eq!(binding_key(None, Some("book-7")), "book:book-7");
    }

    #[test]
    fn anilist_viewer_search_and_save_roundtrip() {
        let (base, server) = serve(|head, body| {
            if body.contains("Viewer") {
                return (
                    200,
                    r#"{"data":{"Viewer":{"id":42,"name":"测试者","avatar":{"large":"http://img"},"mediaListOptions":{"scoreFormat":"POINT_10"}}}}"#.into(),
                );
            }
            if body.contains("media(search") {
                return (
                    200,
                    r#"{"data":{"Page":{"media":[{"id":99,"title":{"userPreferred":"作品A"},"coverImage":{"large":"http://c"},"status":"RELEASING","chapters":120}]}}}"#.into(),
                );
            }
            if body.contains("mediaList") {
                return (
                    200,
                    r#"{"data":{"Page":{"mediaList":[{"id":7,"status":"CURRENT","scoreRaw":70,"progress":5,"media":{"id":99,"title":{"userPreferred":"作品A"},"chapters":120}}]}}}"#.into(),
                );
            }
            if body.contains("SaveMediaListEntry") {
                assert!(body.contains("\"progress\":9") || body.contains("\"progress\": 9"));
                return (
                    200,
                    r#"{"data":{"SaveMediaListEntry":{"id":7,"status":"CURRENT","progress":9}}}"#.into(),
                );
            }
            (404, format!("{{\"unexpected\":\"{}\"}}", head.lines().next().unwrap_or("")))
        });
        let account = anilist_viewer(&base, "token").unwrap();
        assert_eq!(account.user_id, "42");
        assert_eq!(account.name, "测试者");
        let hits = anilist_search(&base, "token", "作品").unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].media_id, "99");
        let entry = anilist_entry(&base, "token", "42", "99").unwrap().unwrap();
        assert_eq!(entry.progress, 5);
        let binding = Binding {
            tracker: "anilist".into(),
            media_id: "99".into(),
            library_id: Some("7".into()),
            title: "作品A".into(),
            progress: 5,
            status: TrackStatus::Reading,
        };
        let saved = anilist_save(&base, "token", &binding, TrackStatus::Reading, 9, 0).unwrap();
        assert_eq!(saved.progress, 9);
        drop(server);
    }

    #[test]
    fn kitsu_login_search_entry_and_save_roundtrip() {
        let requests = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = requests.clone();
        let (base, server) = serve(move |head, body| {
            let line = head.lines().next().unwrap_or("").to_string();
            seen.lock().unwrap().push(line.clone());
            if line.contains("/oauth/token") {
                assert!(body.contains("grant_type=password"));
                return (200, r#"{"access_token":"kitsu-t","expires_in":3600}"#.into());
            }
            if line.contains("filter[self]") {
                return (
                    200,
                    r#"{"data":[{"id":"77","type":"users","attributes":{"name":"凯特","ratingSystem":"regular","avatar":{"original":"http://a"}}}]}"#.into(),
                );
            }
            if line.contains("algolia-keys") {
                return (200, r#"{"media":{"key":"algolia-k"}}"#.into());
            }
            if line.starts_with("POST") && line.contains("/algolia") {
                return (
                    200,
                    r#"{"hits":[{"id":123,"canonicalTitle":"K 作品","synopsis":"x","chapterCount":40,"posterImage":{"original":"http://p"},"subtype":"manga"}]}"#.into(),
                );
            }
            if line.contains("library-entries") && line.starts_with("GET") {
                return (
                    200,
                    r#"{"data":[{"id":"500","type":"libraryEntries","attributes":{"status":"current","progress":4}}],"included":[{"attributes":{"canonicalTitle":"K 作品","chapterCount":40}}]}"#.into(),
                );
            }
            if line.contains("library-entries") && line.starts_with("PATCH") {
                return (200, r#"{"data":{"id":"500"}}"#.into());
            }
            if line.contains("library-entries") && line.starts_with("POST") {
                return (200, r#"{"data":{"id":"501"}}"#.into());
            }
            (404, "{}".into())
        });
        let account = kitsu_login(
            &format!("{base}/oauth/token"),
            &format!("{base}/edge"),
            "u",
            "p",
        )
        .unwrap();
        assert_eq!(account.user_id, "77");
        let hits = kitsu_search(
            &format!("{base}/edge"),
            &format!("{base}/algolia"),
            "kitsu-t",
            "K",
        )
        .unwrap();
        assert_eq!(hits[0].media_id, "123");
        let entry = kitsu_entry(&format!("{base}/edge"), "kitsu-t", "77", "123")
            .unwrap()
            .unwrap();
        assert_eq!(entry.progress, 4);
        assert_eq!(entry.library_id.as_deref(), Some("500"));
        let binding = Binding {
            tracker: "kitsu".into(),
            media_id: "123".into(),
            library_id: Some("500".into()),
            title: "K 作品".into(),
            progress: 4,
            status: TrackStatus::Reading,
        };
        let saved = kitsu_save(&format!("{base}/edge"), "kitsu-t", "77", &binding, TrackStatus::Reading, 10).unwrap();
        assert_eq!(saved.progress, 10);
        let log = requests.lock().unwrap().clone();
        assert!(log.iter().any(|line| line.starts_with("PATCH")));
        drop(server);
    }

    #[test]
    fn remote_progress_never_moves_backwards() {
        let (base, _server) = serve(|_head, _body| {
            (
                200,
                r#"{"data":{"Page":{"mediaList":[{"id":7,"status":"CURRENT","scoreRaw":0,"progress":80,"media":{"id":99,"title":{"userPreferred":"A"},"chapters":120}}]}}}"#.into(),
            )
        });
        let endpoints = Endpoints {
            graphql: base,
            ..Default::default()
        };
        let mut store = Store::default();
        store.auto_sync = true;
        store.anilist = Some(Account {
            user_id: "42".into(),
            name: "u".into(),
            access_token: "t".into(),
            ..Default::default()
        });
        store.bindings.insert(
            "book:x".into(),
            Binding {
                tracker: "anilist".into(),
                media_id: "99".into(),
                library_id: Some("7".into()),
                title: "A".into(),
                progress: 10,
                status: TrackStatus::Reading,
            },
        );
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join(STORE_FILE);
        std::fs::write(&path, serde_json::to_vec(&store).unwrap()).unwrap();
        // Chapter 9 read locally while the remote is at 80 → nothing is sent.
        let events = run_command(
            TrackCommand::ChapterDone { key: "book:x".into(), chapter: 9 },
            &path,
            &endpoints,
        );
        assert!(events.is_empty(), "{events:?} would regress the remote");
    }

    #[test]
    fn oauth_loopback_collects_the_code() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let handle = std::thread::spawn(move || {
            wait_oauth_code(port, std::time::Duration::from_secs(5), "S1")
        });
        std::thread::sleep(std::time::Duration::from_millis(150));
        // An unsolicited callback without the attempt's state is ignored.
        let mut forged = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
        forged
            .write_all(b"GET /success?code=1337&code=EVIL HTTP/1.1\r\nHost: x\r\n\r\n")
            .unwrap();
        let mut reply = String::new();
        let _ = forged.read_to_string(&mut reply);
        assert!(reply.starts_with("HTTP/1.1 400"), "{reply}");
        let mut stream = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
        stream
            .write_all(b"GET /success?code=1337&code=REAL&state=S1 HTTP/1.1\r\nHost: x\r\n\r\n")
            .unwrap();
        let code = handle.join().unwrap().unwrap();
        assert_eq!(code, "REAL");
    }

    #[test]
    fn binding_adopts_an_existing_remote_entry() {
        let saves = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let seen = saves.clone();
        let (base, _server) = serve(move |_head, body| {
            if body.contains("SaveMediaListEntry") {
                seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                return (200, r#"{"data":{"SaveMediaListEntry":{"id":7,"status":"COMPLETED","progress":0}}}"#.into());
            }
            (
                200,
                r#"{"data":{"Page":{"mediaList":[{"id":7,"status":"COMPLETED","scoreRaw":0,"progress":80,"media":{"id":99,"title":{"userPreferred":"A"},"chapters":80}}]}}}"#.into(),
            )
        });
        let endpoints = Endpoints {
            graphql: base,
            ..Default::default()
        };
        let mut store = Store::default();
        store.anilist = Some(Account {
            user_id: "42".into(),
            name: "u".into(),
            access_token: "t".into(),
            ..Default::default()
        });
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join(STORE_FILE);
        std::fs::write(&path, serde_json::to_vec(&store).unwrap()).unwrap();
        let pick = TrackSearch {
            media_id: "99".into(),
            title: "A".into(),
            cover: String::new(),
            total_chapters: 80,
            status: String::new(),
        };
        run_command(
            TrackCommand::Bind { tracker: TrackerKind::Anilist, key: "book:x".into(), pick, progress: 0 },
            &path,
            &endpoints,
        );
        assert_eq!(saves.load(std::sync::atomic::Ordering::SeqCst), 0);
        let saved: Store = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let binding = &saved.bindings["book:x"];
        assert_eq!(binding.progress, 80);
        assert_eq!(binding.status, TrackStatus::Completed);
        assert_eq!(binding.library_id.as_deref(), Some("7"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o077, 0, "token file is readable by others");
        }
    }
}
