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
static JAR: once_cell::sync::Lazy<Arc<reqwest::cookie::Jar>> =
    once_cell::sync::Lazy::new(|| Arc::new(reqwest::cookie::Jar::default()));

/// Origin of the last request a Cloudflare challenge blocked.
static CHALLENGE: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// Adds `name=value` cookies collected by a web view for `url`.
pub fn add_web_cookies(url: &str, cookies: &[(String, String)]) {
    let Ok(url) = url::Url::parse(url) else {
        return;
    };
    for (name, value) in cookies {
        JAR.add_cookie_str(&format!("{name}={value}; Path=/"), &url);
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
    /// Upper-case method name: GET, POST, PUT, HEAD or DELETE.
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
        let client = Client::builder()
            .user_agent(user_agent)
            .tls_backend_preconfigured(build_tls_config()?)
            .cookie_provider(JAR.clone())
            .redirect(reqwest::redirect::Policy::limited(10))
            .connect_timeout(Duration::from_secs(20))
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
            "HEAD" => Method::HEAD,
            "DELETE" => Method::DELETE,
            other => return Err(format!("unsupported HTTP method \"{other}\"")),
        };

        let mut builder: RequestBuilder = self.client.request(method, parsed);
        for (name, value) in &request.headers {
            let header_name = HeaderName::from_bytes(name.as_bytes())
                .map_err(|e| format!("invalid request header name \"{name}\": {e}"))?;
            let header_value = HeaderValue::from_str(value)
                .map_err(|e| format!("invalid value for header \"{name}\": {e}"))?;
            builder = builder.header(header_name, header_value);
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
}
