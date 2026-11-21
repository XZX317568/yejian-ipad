//! Native HTTP transport used by the WASM `net` host namespace.
//!
//! The old engine forwarded every request to a Dart callback. This module
//! executes the same requests with `reqwest::blocking` from the source's own
//! worker thread, preserving the Aidoku contract:
//!
//! * HTTP error statuses (4xx/5xx) are normal responses, not transport errors;
//! * response headers keep order and duplicates (e.g. multiple `Set-Cookie`);
//! * a failed request produces `error` plus status `0`, exactly like
//!   `NetRequest.responseError` in the Dart engine;
//! * response bodies are read as a stream with a 64 MiB cap, so one oversized
//!   page cannot exhaust memory;
//! * TLS uses Mozilla's webpki root store instead of
//!   `rustls-platform-verifier`, which on Android needs a Kotlin verifier
//!   class that a pure `NativeActivity` app does not ship. Certificate-chain
//!   and hostname verification stay enabled on every platform;
//! * cookies persist for the lifetime of one [`NativeHttp`] instance, which is
//!   one installed source.

use std::io::Read;
use std::sync::Arc;
use std::time::Duration;

use reqwest::Method;
use reqwest::blocking::{Client, RequestBuilder};
use reqwest::header::{HeaderName, HeaderValue};

/// One cookie jar for every source engine, so cookies from a platform web
/// view (Cloudflare clearance, web login) reach all later requests.
static JAR: once_cell::sync::Lazy<Arc<SharedJar>> =
    once_cell::sync::Lazy::new(|| Arc::new(SharedJar(std::sync::RwLock::new(Default::default()))));

/// A jar that can be emptied on logout while clients keep referencing it.
struct SharedJar(std::sync::RwLock<reqwest::cookie::Jar>);

impl reqwest::cookie::CookieStore for SharedJar {
    fn set_cookies(
        &self,
        headers: &mut dyn Iterator<Item = &HeaderValue>,
        url: &url::Url,
    ) {
        self.0.read().unwrap().set_cookies(headers, url);
    }

    fn cookies(&self, url: &url::Url) -> Option<HeaderValue> {
        self.0.read().unwrap().cookies(url)
    }
}

/// Expires every cookie sent to `url`, whether a server or a web view set
/// it; other sites keep theirs.
pub fn clear_site_cookies(url: &str) {
    use reqwest::cookie::CookieStore;
    let Ok(url) = url::Url::parse(url) else {
        return;
    };
    let host = url.host_str().unwrap_or_default().to_string();
    drop_site_user_agent_for(&host);
    let jar = JAR.0.read().unwrap();
    let Some(header) = jar.cookies(&url) else {
        return;
    };
    let header = String::from_utf8_lossy(header.as_bytes()).into_owned();
    for name in header.split(';').filter_map(|pair| pair.split('=').next()) {
        let name = name.trim();
        if name.is_empty() {
            continue;
        }
        let gone = format!("{name}=; Max-Age=0; Path=/");
        jar.add_cookie_str(&gone, &url);
        jar.add_cookie_str(&format!("{gone}; Secure"), &url);
        jar.add_cookie_str(&format!("{gone}; Domain={host}"), &url);
        jar.add_cookie_str(&format!("{gone}; Domain={host}; Secure"), &url);
    }
}

/// Origin of the last request a Cloudflare challenge blocked.
static CHALLENGE: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// Adds `name=value` cookies collected by a web view for `url`.
pub fn add_web_cookies(url: &str, cookies: &[(String, String)]) {
    let Ok(url) = url::Url::parse(url) else {
        return;
    };
    let secure = if url.scheme() == "https" { "; Secure" } else { "" };
    let jar = JAR.0.read().unwrap();
    for (name, value) in cookies {
        jar.add_cookie_str(&format!("{name}={value}; Path=/{secure}"), &url);
    }
}

/// Per-site User-Agent overrides keyed by lowercase host. Cloudflare-cleared
/// sessions only stay valid when the request agent matches the browser's
/// `navigator.userAgent`, so the paste-cookies flow can pin the browser UA
/// per site; `clear_site_cookies` removes the pin together with the cookies.
static SITE_AGENTS: std::sync::RwLock<Option<std::collections::HashMap<String, String>>> =
    std::sync::RwLock::new(None);

/// Records the browser UA to use for requests to `url`'s host. An empty
/// value removes the override.
pub fn set_site_user_agent(url: &str, user_agent: &str) {
    let Ok(url) = url::Url::parse(url) else {
        return;
    };
    let host = url.host_str().unwrap_or_default().to_ascii_lowercase();
    if host.is_empty() {
        return;
    }
    let mut guard = SITE_AGENTS.write().unwrap();
    let map = guard.get_or_insert_with(Default::default);
    if user_agent.trim().is_empty() {
        map.remove(&host);
    } else {
        map.insert(host, user_agent.trim().to_string());
    }
}

/// The pinned UA for `url`'s host, if the user imported one.
pub fn site_user_agent(url: &str) -> Option<String> {
    let host = url::Url::parse(url)
        .ok()
        .and_then(|url| url.host_str().map(str::to_ascii_lowercase))?;
    SITE_AGENTS.read().ok()?.as_ref()?.get(&host).cloned()
}

fn drop_site_user_agent_for(host: &str) {
    if let Ok(mut guard) = SITE_AGENTS.write()
        && let Some(map) = guard.as_mut()
    {
        let host = host.to_ascii_lowercase();
        map.remove(&host);
        // Subdomain cookies cleared for "example.com" should not keep a UA
        // pinned for "cdn.example.com" either; drop suffix matches.
        map.retain(|key, _| key != &host && !key.ends_with(&format!(".{host}")));
    }
}

/// Takes the origin that most recently needed browser verification.
pub fn take_web_challenge() -> Option<String> {
    CHALLENGE.lock().ok()?.take()
}

fn is_challenge(status: u16, headers: &[(String, String)], body: &[u8]) -> bool {
    if !matches!(status, 403 | 429 | 503) {
        return false;
    }
    let header = |name: &str| {
        headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.to_ascii_lowercase())
    };
    if header("cf-mitigated").is_some_and(|v| v.contains("challenge")) {
        return true;
    }
    let text = String::from_utf8_lossy(&body[..body.len().min(64 * 1024)]);
    header("server").is_some_and(|v| v.contains("cloudflare"))
        && (text.contains("challenge-platform") || text.contains("Just a moment"))
}

/// Default browser-like user agent used by the Mangayomi Dart stack for source
/// scraping. Sources receive the same value unless the caller overrides it.
pub const DEFAULT_USER_AGENT: &str = "Mozilla/5.0 (Linux; Android 13; 22081212UG Build/TKQ1.220829.002; wv) AppleWebKit/537.36 (KHTML, like Gecko) Version/4.0 Chrome/114.0.5735.131 Mobile Safari/537.36";

const DEFAULT_TIMEOUT_MS: u64 = 60_000;
const MAX_TIMEOUT_MS: u64 = 300_000;
/// Hard cap on one response body. Page images and chapter archives are read
/// into memory, so an unbounded stream could exhaust a whole device.
pub const MAX_RESPONSE_BODY_BYTES: usize = 64 * 1024 * 1024;

/// Builds the rustls client configuration used for every source request.
///
/// reqwest 0.13 selects `rustls-platform-verifier` by default. On Android that
/// verifier calls into a Kotlin `RootCertStoreProvider` that must be registered
/// by the embedding app; the pure native reader has no JVM glue, so the default
/// backend fails at the first HTTPS request. Supplying an explicit configuration
/// removes that dependency without weakening verification: the Mozilla webpki
/// roots validate the chain, and rustls still checks the server name.
fn build_tls_config() -> Result<rustls::ClientConfig, crate::SourceError> {
    let roots = rustls::RootCertStore {
        roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
    };
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let mut config = rustls::ClientConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13, &rustls::version::TLS12])
        .map_err(|e| crate::SourceError::Network(format!("TLS setup failed: {e}")))?
        .with_root_certificates(roots)
        .with_no_client_auth();
    // reqwest passes the config through unchanged, so ALPN has to be set here.
    config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    Ok(config)
}

/// One HTTP request emitted by a source through `net.send`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetRequestData {
    /// Upper-case method name: GET, POST, PUT, PATCH, HEAD or DELETE.
    pub method: String,
    pub url: String,
    /// Header pairs in insertion order; duplicate names keep the last value
    /// before reaching this layer (the WASM store is a map).
    pub headers: Vec<(String, String)>,
    pub body: Option<Vec<u8>>,
    pub timeout_ms: u64,
}

/// Result of one source HTTP request.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NetResponseData {
    /// `0` when `error` is set.
    pub status_code: u16,
    /// Response headers in wire order, duplicates preserved.
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    /// URL after redirects; used by `net.get_url` and as the HTML base URL.
    pub final_url: Option<String>,
    /// Transport-level failure only (DNS, TLS, timeout, invalid request).
    pub error: Option<String>,
}

impl NetResponseData {
    /// Case-insensitive first header lookup, matching `net.get_header`.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

/// A thin, cloneable wrapper around a `reqwest::blocking::Client`.
#[derive(Clone)]
pub struct NativeHttp {
    client: Arc<Client>,
    max_body_bytes: usize,
}

impl std::fmt::Debug for NativeHttp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NativeHttp").finish_non_exhaustive()
    }
}

impl NativeHttp {
    pub fn new() -> Result<Self, crate::SourceError> {
        Self::with_user_agent(DEFAULT_USER_AGENT)
    }

    pub fn with_user_agent(user_agent: &str) -> Result<Self, crate::SourceError> {
        Self::with_user_agent_and_limit(user_agent, MAX_RESPONSE_BODY_BYTES)
    }

    fn with_user_agent_and_limit(
        user_agent: &str,
        max_body_bytes: usize,
    ) -> Result<Self, crate::SourceError> {
        Self::build(user_agent, max_body_bytes, Some(JAR.clone()))
    }

    /// A client that shares every setting with the source transport —
    /// timeouts, redirects, the webpki TLS roots — except the cookie jar.
    /// Tracker traffic (AniList/Kitsu Bearer tokens) must never mix with
    /// scraped-site cookies, in either direction.
    fn build(
        user_agent: &str,
        max_body_bytes: usize,
        jar: Option<Arc<SharedJar>>,
    ) -> Result<Self, crate::SourceError> {
        let client = Client::builder()
            .user_agent(user_agent)
            .tls_backend_preconfigured(build_tls_config()?)
            .redirect(reqwest::redirect::Policy::limited(10))
            .connect_timeout(Duration::from_secs(20));
        let client = match jar {
            Some(jar) => client.cookie_provider(jar),
            None => client,
        }
        .build()
        .map_err(|e| crate::SourceError::Network(format!("HTTP client setup failed: {e}")))?;
        Ok(Self {
            client: Arc::new(client),
            max_body_bytes,
        })
    }

    /// Executes a source request. Never panics for bad source input.
    pub fn send(&self, request: NetRequestData) -> NetResponseData {
        match self.try_send(request) {
            Ok(response) => response,
            Err(message) => NetResponseData {
                error: Some(message),
                ..Default::default()
            },
        }
    }

    fn try_send(&self, request: NetRequestData) -> Result<NetResponseData, String> {
        let parsed = url::Url::parse(&request.url)
            .map_err(|e| format!("invalid request URL \"{}\": {e}", request.url))?;
        if !matches!(parsed.scheme(), "http" | "https") {
            return Err(format!(
                "unsupported URL scheme \"{}\" (only http/https are allowed)",
                parsed.scheme()
            ));
        }

        let method = match request.method.to_ascii_uppercase().as_str() {
            "GET" => Method::GET,
            "POST" => Method::POST,
            "PUT" => Method::PUT,
            "PATCH" => Method::PATCH,
            "HEAD" => Method::HEAD,
            "DELETE" => Method::DELETE,
            other => return Err(format!("unsupported HTTP method \"{other}\"")),
        };

        let mut builder: RequestBuilder = self.client.request(method, parsed.clone());
        for (name, value) in &request.headers {
            let header_name = HeaderName::from_bytes(name.as_bytes())
                .map_err(|e| format!("invalid request header name \"{name}\": {e}"))?;
            let header_value = HeaderValue::from_str(value)
                .map_err(|e| format!("invalid value for header \"{name}\": {e}"))?;
            builder = builder.header(header_name, header_value);
        }
        // A Cloudflare-cleared session only holds while the agent string
        // matches the browser that produced the cookies. The site pin wins
        // whenever the source did not set its own User-Agent header.
        let own_agent = request
            .headers
            .iter()
            .any(|(name, _)| name.eq_ignore_ascii_case("user-agent"));
        if !own_agent
            && let Some(host) = parsed.host_str()
            && let Some(agent) = SITE_AGENTS
                .read()
                .ok()
                .and_then(|guard| guard.as_ref()?.get(&host.to_ascii_lowercase()).cloned())
            && let Ok(value) = HeaderValue::from_str(&agent)
        {
            builder = builder.header(reqwest::header::USER_AGENT, value);
        }
        if let Some(body) = request.body {
            builder = builder.body(body);
        }

        let timeout_ms = if request.timeout_ms == 0 {
            DEFAULT_TIMEOUT_MS
        } else {
            request.timeout_ms.min(MAX_TIMEOUT_MS)
        };
        builder = builder.timeout(Duration::from_millis(timeout_ms));

        let response = builder.send().map_err(|e| describe_transport_error(&e))?;
        let status_code = response.status().as_u16();
        let final_url = Some(response.url().to_string());
        let headers: Vec<(String, String)> = response
            .headers()
            .iter()
            .map(|(name, value)| {
                (
                    name.as_str().to_string(),
                    String::from_utf8_lossy(value.as_bytes()).into_owned(),
                )
            })
            .collect();
        let mut reader = response.take(self.max_body_bytes as u64 + 1);
        let mut body = Vec::new();
        reader
            .read_to_end(&mut body)
            .map_err(|e| format!("reading response body failed: {e}"))?;
        if body.len() > self.max_body_bytes {
            return Err(format!(
                "response body exceeds the {} byte limit",
                self.max_body_bytes
            ));
        }

        if is_challenge(status_code, &headers, &body) {
            if let (Ok(mut slot), Some(url)) = (CHALLENGE.lock(), final_url.as_deref()) {
                if let Ok(url) = url::Url::parse(url) {
                    *slot = Some(url.origin().ascii_serialization());
                }
            }
        }

        Ok(NetResponseData {
            status_code,
            headers,
            body,
            final_url,
            error: None,
        })
    }
}

/// Sends one HTTPS request **without** the shared source cookie jar — the
/// channel tracker services (AniList, Kitsu) use so Bearer tokens and
/// scraped-site cookies never cross.
///
/// The client is built fresh per call: tracker traffic is infrequent (login,
/// a handful of sync calls per chapter), so pooling buys little while a
/// shared jar-free client would still need the same TLS setup anyway.
pub fn send_isolated(request: NetRequestData) -> NetResponseData {
    match NativeHttp::build(DEFAULT_USER_AGENT, MAX_RESPONSE_BODY_BYTES, None) {
        Ok(http) => http.send(request),
        Err(error) => NetResponseData {
            error: Some(error.to_string()),
            ..Default::default()
        },
    }
}

fn describe_transport_error(error: &reqwest::Error) -> String {
    if error.is_timeout() {
        format!("request timed out: {error}")
    } else if error.is_connect() {
        format!("connection failed: {error}")
    } else if error.is_redirect() {
        format!("redirect failed: {error}")
    } else if error.is_body() {
        format!("request body failed: {error}")
    } else {
        format!("request failed: {error}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    #[test]
    fn clearing_one_site_keeps_other_sites_cookies() {
        add_web_cookies("https://a.clear.test/", &[("s".into(), "1".into())]);
        add_web_cookies("https://b.clear.test/", &[("t".into(), "2".into())]);
        clear_site_cookies("https://a.clear.test/");
        let jar = JAR.0.read().unwrap();
        use reqwest::cookie::CookieStore;
        assert!(jar.cookies(&"https://a.clear.test/".parse().unwrap()).is_none());
        assert!(jar.cookies(&"https://b.clear.test/".parse().unwrap()).is_some());
    }

    #[test]
    fn detects_cloudflare_challenges_only() {
        let cf = vec![("Server".to_string(), "cloudflare".to_string())];
        assert!(is_challenge(403, &cf, b"<title>Just a moment...</title>"));
        assert!(is_challenge(503, &[("cf-mitigated".into(), "challenge".into())], b""));
        assert!(!is_challenge(200, &cf, b"Just a moment"));
        assert!(!is_challenge(404, &cf, b"not found"));
        assert!(!is_challenge(403, &cf, b"forbidden"));
    }

    #[test]
    fn preserves_status_duplicate_headers_and_body() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0u8; 4096];
            let read = stream.read(&mut buf).unwrap();
            let request = String::from_utf8_lossy(&buf[..read]);
            assert!(request.starts_with("GET /probe HTTP/1.1"));
            assert!(request.to_ascii_lowercase().contains("x-source: yejian"));
            stream
                .write_all(
                    b"HTTP/1.1 418 I'm a teapot\r\n\
                      Content-Type: text/plain\r\n\
                      Set-Cookie: a=1; Path=/\r\n\
                      Set-Cookie: b=2; Path=/\r\n\
                      Content-Length: 5\r\n\
                      Connection: close\r\n\r\n\
                      hello",
                )
                .unwrap();
        });

        let http = NativeHttp::new().unwrap();
        let response = http.send(NetRequestData {
            method: "GET".into(),
            url: format!("http://{addr}/probe"),
            headers: vec![("X-Source".into(), "yejian".into())],
            body: None,
            timeout_ms: 5_000,
        });
        server.join().unwrap();

        assert_eq!(response.status_code, 418);
        assert_eq!(response.error, None);
        assert_eq!(response.body, b"hello");
        assert_eq!(response.header("content-type"), Some("text/plain"));
        let cookies: Vec<_> = response
            .headers
            .iter()
            .filter(|(k, _)| k.eq_ignore_ascii_case("set-cookie"))
            .map(|(_, v)| v.as_str())
            .collect();
        assert_eq!(cookies, vec!["a=1; Path=/", "b=2; Path=/"]);
        let expected = format!("http://{addr}/probe");
        assert_eq!(response.final_url.as_deref(), Some(expected.as_str()));
    }

    #[test]
    fn transport_failure_sets_error_and_zero_status() {
        // Bind and immediately drop the listener to get a refused port.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);

        let http = NativeHttp::new().unwrap();
        let response = http.send(NetRequestData {
            method: "GET".into(),
            url: format!("http://{addr}/nope"),
            headers: Vec::new(),
            body: None,
            timeout_ms: 2_000,
        });
        assert_eq!(response.status_code, 0);
        assert!(response.error.is_some());
    }

    #[test]
    fn rejects_non_http_schemes() {
        let http = NativeHttp::new().unwrap();
        let response = http.send(NetRequestData {
            method: "GET".into(),
            url: "file:///etc/passwd".into(),
            headers: Vec::new(),
            body: None,
            timeout_ms: 1_000,
        });
        assert!(response.error.unwrap().contains("scheme"));
    }

    #[test]
    fn rejects_response_bodies_over_the_limit() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0u8; 4096];
            let _ = stream.read(&mut buf);
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\n\
                      Content-Length: 10\r\n\
                      Connection: close\r\n\r\n\
                      0123456789",
                )
                .unwrap();
        });

        let http = NativeHttp::with_user_agent_and_limit("test", 8).unwrap();
        let response = http.send(NetRequestData {
            method: "GET".into(),
            url: format!("http://{addr}/big"),
            headers: Vec::new(),
            body: None,
            timeout_ms: 5_000,
        });
        server.join().unwrap();

        assert_eq!(response.status_code, 0);
        assert!(
            response
                .error
                .as_deref()
                .is_some_and(|message| message.contains("limit"))
        );
    }

    #[test]
    fn tls_config_uses_the_webpki_root_store() {
        assert!(!webpki_roots::TLS_SERVER_ROOTS.is_empty());
        let config = build_tls_config().unwrap();
        assert_eq!(
            config.alpn_protocols,
            vec![b"h2".to_vec(), b"http/1.1".to_vec()]
        );
    }

    #[test]
    fn site_user_agent_overrides_requests_without_their_own() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let mut agents = Vec::new();
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().unwrap();
                let mut buf = [0u8; 4096];
                let read = stream.read(&mut buf).unwrap();
                let request = String::from_utf8_lossy(&buf[..read]).to_string();
                let line = request
                    .lines()
                    .find(|line| line.to_ascii_lowercase().starts_with("user-agent:"))
                    .unwrap_or("")
                    .to_string();
                agents.push(line);
                stream
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                    .unwrap();
            }
            agents
        });

        let url = format!("http://{addr}/ua");
        set_site_user_agent(&url, "BrowserUA/9.9");
        assert_eq!(site_user_agent(&url).as_deref(), Some("BrowserUA/9.9"));

        let http = NativeHttp::new().unwrap();
        // No explicit header → the pinned browser UA is sent.
        http.send(NetRequestData {
            method: "GET".into(),
            url: url.clone(),
            headers: vec![],
            body: None,
            timeout_ms: 5_000,
        });
        // An explicit source UA always wins over the site pin.
        http.send(NetRequestData {
            method: "GET".into(),
            url: url.clone(),
            headers: vec![("User-Agent".into(), "SourceUA/1".into())],
            body: None,
            timeout_ms: 5_000,
        });
        let agents = server.join().unwrap();
        assert!(agents[0].contains("BrowserUA/9.9"), "{}", agents[0]);
        assert!(agents[1].contains("SourceUA/1"), "{}", agents[1]);

        // Clearing the site's cookies drops the pin as well.
        clear_site_cookies(&url);
        assert_eq!(site_user_agent(&url), None);
    }

    #[test]
    fn isolated_channel_shares_nothing_with_the_source_jar() {
        add_web_cookies("http://isolated.test/", &[("cf".into(), "x".into())]);
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0u8; 8192];
            let read = stream.read(&mut buf).unwrap();
            let request = String::from_utf8_lossy(&buf[..read]).to_string();
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nSet-Cookie: tracker=1; Path=/\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok",
                )
                .unwrap();
            request
        });
        let response = send_isolated(NetRequestData {
            method: "GET".into(),
            url: format!("http://{addr}/api"),
            headers: vec![("Authorization".into(), "Bearer t".into())],
            body: None,
            timeout_ms: 5_000,
        });
        let request = server.join().unwrap();
        assert_eq!(response.status_code, 200);
        // No source cookies ride along; the tracker reply's Set-Cookie does
        // not land in the shared jar either.
        assert!(!request.to_ascii_lowercase().contains("cookie:"), "{request}");
        use reqwest::cookie::CookieStore;
        let jar = JAR.0.read().unwrap();
        // Other tests share 127.0.0.1 (cookies ignore ports), so only this
        // reply's cookie must be absent.
        let stored = jar
            .cookies(&format!("http://{addr}/api").parse().unwrap())
            .map(|header| header.to_str().unwrap_or_default().to_string())
            .unwrap_or_default();
        assert!(!stored.contains("tracker="), "{stored}");
    }
}
