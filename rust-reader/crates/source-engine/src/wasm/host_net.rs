//! `net` WASM host imports.
//!
//! Requests are executed synchronously on the source's worker thread by the
//! handler in [`crate::http`] (or a platform-provided replacement). The host
//! keeps the exact Aidoku semantics:
//!
//! * request method, URL, headers, body and timeout come from the source;
//! * `send` returns `0` for any HTTP response (including 4xx/5xx) and `-3` for a
//!   transport failure, with the failure text available to the UI;
//! * `get_url` reports the effective URL after redirects;
//! * `html` uses that effective URL as the document base URL.

use std::time::Duration;

use wasmi::{Caller, Linker};

use crate::http::{NetRequestData, NetResponseData};

use super::HostState;
use super::memory;
use super::store::StoredValue;

#[derive(Debug, Clone, Copy)]
pub(crate) enum NetMethod {
    Get,
    Post,
    Put,
    Head,
    Delete,
}

impl NetMethod {
    fn from_value(v: i32) -> Self {
        match v {
            1 => Self::Post,
            2 => Self::Put,
            3 => Self::Head,
            4 => Self::Delete,
            _ => Self::Get,
        }
    }

    fn as_str(&self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Post => "POST",
            Self::Put => "PUT",
            Self::Head => "HEAD",
            Self::Delete => "DELETE",
        }
    }
}

pub(crate) struct NetRequestState {
    pub(crate) method: NetMethod,
    pub(crate) url: Option<String>,
    pub(crate) headers: Vec<(String, String)>,
    pub(crate) body: Option<Vec<u8>>,
    pub(crate) timeout: Duration,
    pub(crate) response: Option<NetResponseData>,
    pub(crate) effective_url: Option<String>,
}

impl NetRequestState {
    fn new(method: NetMethod) -> Self {
        Self {
            method,
            url: None,
            headers: Vec::new(),
            body: None,
            timeout: Duration::from_secs(60),
            response: None,
            effective_url: None,
        }
    }

    fn set_header(&mut self, key: String, value: String) {
        self.headers
            .retain(|(existing, _)| !existing.eq_ignore_ascii_case(&key));
        self.headers.push((key, value));
    }

    fn base_url(&self) -> Option<String> {
        self.effective_url.clone().or_else(|| self.url.clone())
    }
}

pub(crate) fn link(linker: &mut Linker<HostState>) -> anyhow::Result<()> {
    linker.func_wrap(
        "net",
        "init",
        |mut caller: Caller<'_, HostState>, method: i32| -> i32 {
            let req = NetRequestState::new(NetMethod::from_value(method));
            caller.data_mut().store.store(StoredValue::NetRequest(req))
        },
    )?;

    linker.func_wrap(
        "net",
        "set_url",
        |mut caller: Caller<'_, HostState>, desc: i32, ptr: i32, len: i32| -> i32 {
            let url = match memory::read_string(caller.data().memory.unwrap(), &caller, ptr, len) {
                Ok(s) => s,
                Err(_) => return -4,
            };
            match caller.data_mut().store.get_mut(desc) {
                Some(StoredValue::NetRequest(req)) => {
                    req.url = Some(url);
                    req.effective_url = None;
                    0
                }
                _ => -1,
            }
        },
    )?;

    linker.func_wrap(
        "net",
        "set_header",
        |mut caller: Caller<'_, HostState>,
         desc: i32,
         kptr: i32,
         klen: i32,
         vptr: i32,
         vlen: i32|
         -> i32 {
            let mem = caller.data().memory.unwrap();
            let key = match memory::read_string(mem, &caller, kptr, klen) {
                Ok(s) => s,
                Err(_) => return -2,
            };
            let value = match memory::read_string(mem, &caller, vptr, vlen) {
                Ok(s) => s,
                Err(_) => return -2,
            };
            match caller.data_mut().store.get_mut(desc) {
                Some(StoredValue::NetRequest(req)) => {
                    req.set_header(key, value);
                    0
                }
                _ => -1,
            }
        },
    )?;

    linker.func_wrap(
        "net",
        "set_body",
        |mut caller: Caller<'_, HostState>, desc: i32, ptr: i32, len: i32| -> i32 {
            let bytes = match memory::read_bytes(caller.data().memory.unwrap(), &caller, ptr, len) {
                Ok(b) => b,
                Err(_) => return -2,
            };
            match caller.data_mut().store.get_mut(desc) {
                Some(StoredValue::NetRequest(req)) => {
                    req.body = Some(bytes);
                    0
                }
                _ => -1,
            }
        },
    )?;

    linker.func_wrap(
        "net",
        "set_timeout",
        |mut caller: Caller<'_, HostState>, desc: i32, seconds: f64| -> i32 {
            match caller.data_mut().store.get_mut(desc) {
                Some(StoredValue::NetRequest(req)) => {
                    req.timeout = Duration::from_millis((seconds * 1000.0) as u64);
                    0
                }
                _ => -1,
            }
        },
    )?;

    // Sources execute one request at a time on their own worker thread, so the
    // old Dart-side rate limiter has no work to schedule here.
    linker.func_wrap(
        "net",
        "set_rate_limit",
        |_: Caller<'_, HostState>, _a: i32, _b: i32, _c: i32| {},
    )?;

    linker.func_wrap(
        "net",
        "send",
        |mut caller: Caller<'_, HostState>, desc: i32| -> i32 { send_one(&mut caller, desc) },
    )?;

    linker.func_wrap(
        "net",
        "send_all",
        |mut caller: Caller<'_, HostState>, ptr: i32, len: i32| -> i32 {
            let mem = caller.data().memory.unwrap();
            let count = (len as usize).min(1024);
            let mut descriptors = Vec::with_capacity(count);
            for i in 0..count {
                match memory::read_i32(mem, &caller, ptr + (i as i32) * 4) {
                    Ok(d) => descriptors.push(d),
                    Err(_) => return -1,
                }
            }
            for d in descriptors {
                let code = send_one(&mut caller, d);
                if code != 0 {
                    return code;
                }
            }
            0
        },
    )?;

    linker.func_wrap(
        "net",
        "data_len",
        |caller: Caller<'_, HostState>, desc: i32| -> i32 {
            match caller.data().store.get(desc) {
                Some(StoredValue::NetRequest(req)) => req
                    .response
                    .as_ref()
                    .map(|r| r.body.len() as i32)
                    .unwrap_or(-1),
                _ => -1,
            }
        },
    )?;

    linker.func_wrap(
        "net",
        "read_data",
        |mut caller: Caller<'_, HostState>, desc: i32, buf: i32, size: i32| -> i32 {
            let bytes = match caller.data().store.get(desc) {
                Some(StoredValue::NetRequest(req)) => match &req.response {
                    Some(r) => r.body.clone(),
                    None => return -1,
                },
                _ => return -1,
            };
            if size < 0 {
                return -1;
            }
            let size = (size as usize).min(bytes.len());
            let mem = caller.data().memory.unwrap();
            match memory::write_bytes(mem, &mut caller, buf, &bytes[..size]) {
                Ok(()) => 0,
                Err(_) => -3,
            }
        },
    )?;

    linker.func_wrap(
        "net",
        "get_image",
        |mut caller: Caller<'_, HostState>, desc: i32| -> i32 {
            let bytes = match caller.data().store.get(desc) {
                Some(StoredValue::NetRequest(req)) => match &req.response {
                    Some(r) => r.body.clone(),
                    None => return -5,
                },
                _ => return -5,
            };
            caller.data_mut().store.store(StoredValue::Bytes(bytes))
        },
    )?;

    linker.func_wrap(
        "net",
        "get_status_code",
        |caller: Caller<'_, HostState>, desc: i32| -> i32 {
            match caller.data().store.get(desc) {
                Some(StoredValue::NetRequest(req)) => req
                    .response
                    .as_ref()
                    .map(|r| r.status_code as i32)
                    .unwrap_or(-1),
                _ => -1,
            }
        },
    )?;

    linker.func_wrap(
        "net",
        "get_url",
        |mut caller: Caller<'_, HostState>, desc: i32| -> i32 {
            let url = match caller.data().store.get(desc) {
                Some(StoredValue::NetRequest(req)) => req.base_url(),
                _ => None,
            };
            match url {
                Some(u) => caller
                    .data_mut()
                    .store
                    .store(StoredValue::Bytes(u.into_bytes())),
                None => -5,
            }
        },
    )?;

    linker.func_wrap(
        "net",
        "get_header",
        |mut caller: Caller<'_, HostState>, desc: i32, kptr: i32, klen: i32| -> i32 {
            let key = match memory::read_string(caller.data().memory.unwrap(), &caller, kptr, klen)
            {
                Ok(s) => s,
                Err(_) => return -5,
            };
            let value = match caller.data().store.get(desc) {
                Some(StoredValue::NetRequest(req)) => req
                    .response
                    .as_ref()
                    .and_then(|r| r.header(&key))
                    .map(str::to_string),
                _ => None,
            };
            match value {
                Some(v) => caller
                    .data_mut()
                    .store
                    .store(StoredValue::Bytes(v.into_bytes())),
                None => -5,
            }
        },
    )?;

    linker.func_wrap(
        "net",
        "html",
        |mut caller: Caller<'_, HostState>, desc: i32| -> i32 {
            let (bytes, base_url) = match caller.data().store.get(desc) {
                Some(StoredValue::NetRequest(req)) => (
                    req.response.as_ref().map(|r| r.body.clone()),
                    req.base_url(),
                ),
                _ => (None, None),
            };
            let bytes = match bytes {
                Some(b) => b,
                None => return -5,
            };
            let text = String::from_utf8_lossy(&bytes).to_string();
            super::host_html::store_document(&mut caller, &text, base_url)
        },
    )?;

    Ok(())
}

fn send_one(caller: &mut Caller<'_, HostState>, desc: i32) -> i32 {
    let (method, url, headers, body, timeout) = match caller.data().store.get(desc) {
        Some(StoredValue::NetRequest(req)) => {
            let url = match &req.url {
                Some(u) => u.clone(),
                None => return -9,
            };
            (
                req.method,
                url,
                req.headers.clone(),
                req.body.clone(),
                req.timeout,
            )
        }
        _ => return -1,
    };

    let request_data = NetRequestData {
        method: method.as_str().to_string(),
        url,
        headers,
        body,
        timeout_ms: timeout.as_millis() as u64,
    };

    let handler = caller.data().request_handler.clone();
    let response = handler(request_data);

    if looks_like_verification_challenge(&response) {
        caller.data_mut().challenge_suspected = true;
    }

    match caller.data_mut().store.get_mut(desc) {
        Some(StoredValue::NetRequest(req)) => {
            let is_err = response.error.is_some();
            req.effective_url = response.final_url.clone();
            req.response = Some(response);
            if is_err { -3 } else { 0 }
        }
        _ => -1,
    }
}

/// Conservative anti-bot detection: only flags responses that are already
/// HTTP failures and carry a well-known challenge marker. The engine never
/// turns a successful response into a verification error.
fn looks_like_verification_challenge(response: &NetResponseData) -> bool {
    if !matches!(response.status_code, 403 | 429 | 503) {
        return false;
    }
    let head = &response.body[..response.body.len().min(8192)];
    let text = String::from_utf8_lossy(head).to_ascii_lowercase();
    text.contains("cf-chl")
        || text.contains("__cf_chl")
        || text.contains("challenge-platform")
        || text.contains("just a moment")
        || text.contains("attention required")
        || text.contains("ddos-guard")
}
