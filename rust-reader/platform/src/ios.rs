//! iOS bridge. Calls the Swift functions exported with `@_cdecl` in
//! `platform/ios/Yejian/DocumentBridge.swift`.
//!
//! Every function here must be called from the main thread. The winit event
//! loop on iOS already runs there, so the UI can call these directly.

use std::ffi::{CStr, CString};
use std::os::raw::c_char;
use std::path::PathBuf;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::Mutex;

use crate::{
    parse_import_json, parse_insets_csv, ImportBatch, Insets, PickKind, PlatformError, Result,
};

extern "C" {
    fn yejian_ios_init();
    fn yejian_ios_pick_documents(mode: i64) -> i64;
    fn yejian_ios_poll_import() -> *mut c_char;
    fn yejian_ios_free_string(pointer: *mut c_char);
    fn yejian_ios_inbox_dir() -> *mut c_char;
    fn yejian_ios_app_data_dir() -> *mut c_char;
    fn yejian_ios_safe_insets() -> *mut c_char;
    fn yejian_ios_share_file(path: *const c_char) -> i64;
    fn yejian_ios_set_waker(callback: extern "C" fn());
    fn yejian_ios_open_web(url: *const c_char, user_agent: *const c_char) -> i64;
    fn yejian_ios_poll_web() -> *mut c_char;
}

static WAKER: Mutex<Option<Box<dyn Fn() + Send + Sync>>> = Mutex::new(None);

/// Swift calls this on the main queue after a batch is queued.
extern "C" fn waker_trampoline() {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        if let Ok(guard) = WAKER.lock() {
            if let Some(waker) = guard.as_ref() {
                waker();
            }
        }
    }));
}

fn take_string(pointer: *mut c_char) -> Option<String> {
    if pointer.is_null() {
        return None;
    }
    // SAFETY: the Swift bridge returns `strdup`ed C strings and only frees them
    // through this exact function.
    let value = unsafe { CStr::from_ptr(pointer) }
        .to_string_lossy()
        .into_owned();
    unsafe { yejian_ios_free_string(pointer) };
    Some(value)
}

pub(crate) fn init() {
    unsafe { yejian_ios_init() };
}

pub(crate) fn pick(kind: PickKind) -> Result<i64> {
    let mode = match kind {
        PickKind::Comics => 0,
        PickKind::Backup => 1,
    };
    let request_id = unsafe { yejian_ios_pick_documents(mode) };
    if request_id < 0 {
        Err(PlatformError::Picker(
            "no view controller is available to present the document picker".into(),
        ))
    } else {
        Ok(request_id)
    }
}

pub(crate) fn set_event_waker(waker: Box<dyn Fn() + Send + Sync>) {
    if let Ok(mut guard) = WAKER.lock() {
        *guard = Some(waker);
    }
    unsafe { yejian_ios_set_waker(waker_trampoline) };
}

pub(crate) fn share_file(path: &std::path::Path) -> Result<()> {
    let text = path
        .to_str()
        .ok_or_else(|| PlatformError::Io(format!("path is not valid UTF-8: {path:?}")))?;
    let c_path = CString::new(text)
        .map_err(|_| PlatformError::Io("path contains a NUL byte".into()))?;
    let status = unsafe { yejian_ios_share_file(c_path.as_ptr()) };
    match status {
        0 => Ok(()),
        -2 => Err(PlatformError::Io(format!("{text} does not exist"))),
        other => Err(PlatformError::NotReady(
            if other == -1 {
                "UIKit presenter for the share sheet"
            } else {
                "system share sheet"
            },
        )),
    }
}

pub(crate) fn poll_import() -> Result<Option<ImportBatch>> {
    match take_string(unsafe { yejian_ios_poll_import() }) {
        Some(text) if !text.is_empty() => parse_import_json(&text).map(Some),
        _ => Ok(None),
    }
}

pub(crate) fn inbox_dir() -> Result<PathBuf> {
    match take_string(unsafe { yejian_ios_inbox_dir() }) {
        Some(text) if !text.is_empty() => Ok(PathBuf::from(text)),
        _ => Err(PlatformError::NotReady("Application Support/inbox")),
    }
}

pub(crate) fn app_data_dir() -> Result<PathBuf> {
    match take_string(unsafe { yejian_ios_app_data_dir() }) {
        Some(text) if !text.is_empty() => Ok(PathBuf::from(text)),
        _ => Err(PlatformError::NotReady("Application Support")),
    }
}

pub(crate) fn safe_insets() -> Result<Insets> {
    match take_string(unsafe { yejian_ios_safe_insets() }) {
        Some(text) if !text.is_empty() => parse_insets_csv(&text),
        _ => Err(PlatformError::NotReady("UIKit safe-area insets")),
    }
}

pub(crate) fn open_web(url: &str, user_agent: &str) -> Result<()> {
    let url = CString::new(url).map_err(|_| PlatformError::Io("URL contains a NUL byte".into()))?;
    let agent = CString::new(user_agent)
        .map_err(|_| PlatformError::Io("user agent contains a NUL byte".into()))?;
    match unsafe { yejian_ios_open_web(url.as_ptr(), agent.as_ptr()) } {
        0 => Ok(()),
        _ => Err(PlatformError::NotReady("UIKit presenter for the in-app browser")),
    }
}

pub(crate) fn poll_web() -> Result<Option<crate::WebResult>> {
    match take_string(unsafe { yejian_ios_poll_web() }) {
        Some(text) if !text.is_empty() => crate::parse_web_json(&text).map(Some),
        _ => Ok(None),
    }
}
