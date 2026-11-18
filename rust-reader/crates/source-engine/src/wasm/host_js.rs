//! `js` / `webview` WASM host imports.
//!
//! The native engine has no JavaScript runtime or web view. Every import keeps
//! its original signature so a source module still instantiates, but execution
//! fails and raises [`HostState::js_requested`]. The interpreter turns a failed
//! or empty call after that flag into `SourceError::WebVerificationRequired`,
//! so the UI can offer an explicit verification flow instead of showing an
//! empty catalogue.

use wasmi::{Caller, Linker};

use super::HostState;
use super::store::StoredValue;

fn unsupported(caller: &mut Caller<'_, HostState>) -> i32 {
    caller.data_mut().js_requested = true;
    -1
}

pub(crate) fn link(linker: &mut Linker<HostState>) -> anyhow::Result<()> {
    linker.func_wrap(
        "js",
        "context_create",
        |mut caller: Caller<'_, HostState>| -> i32 {
            caller.data_mut().store.store(StoredValue::JsPlaceholder)
        },
    )?;

    linker.func_wrap(
        "js",
        "context_eval",
        |mut caller: Caller<'_, HostState>, _desc: i32, _ptr: i32, _len: i32| -> i32 {
            unsupported(&mut caller)
        },
    )?;

    linker.func_wrap(
        "js",
        "context_eval_async",
        |mut caller: Caller<'_, HostState>, _desc: i32, _ptr: i32, _len: i32| -> i32 {
            unsupported(&mut caller)
        },
    )?;

    linker.func_wrap(
        "js",
        "context_get",
        |mut caller: Caller<'_, HostState>, _desc: i32, _ptr: i32, _len: i32| -> i32 {
            unsupported(&mut caller)
        },
    )?;

    linker.func_wrap(
        "js",
        "webview_create",
        |mut caller: Caller<'_, HostState>| -> i32 {
            caller.data_mut().store.store(StoredValue::WebviewContext)
        },
    )?;

    linker.func_wrap(
        "js",
        "webview_set_rule_list",
        |_caller: Caller<'_, HostState>, _desc: i32, _ptr: i32, _len: i32| -> i32 { 0 },
    )?;

    linker.func_wrap(
        "js",
        "webview_load",
        |mut caller: Caller<'_, HostState>, _desc: i32, _ptr: i32, _len: i32| -> i32 {
            unsupported(&mut caller)
        },
    )?;

    linker.func_wrap(
        "js",
        "webview_load_html",
        |mut caller: Caller<'_, HostState>,
         _desc: i32,
         _html_ptr: i32,
         _html_len: i32,
         _base_ptr: i32,
         _base_len: i32|
         -> i32 { unsupported(&mut caller) },
    )?;

    linker.func_wrap(
        "js",
        "webview_wait_for_load",
        |mut caller: Caller<'_, HostState>, _desc: i32| -> i32 {
            caller.data_mut().js_requested = true;
            0
        },
    )?;

    linker.func_wrap(
        "js",
        "webview_eval",
        |mut caller: Caller<'_, HostState>, _desc: i32, _ptr: i32, _len: i32| -> i32 {
            unsupported(&mut caller)
        },
    )?;

    linker.func_wrap(
        "js",
        "webview_eval_async",
        |mut caller: Caller<'_, HostState>, _desc: i32, _ptr: i32, _len: i32| -> i32 {
            unsupported(&mut caller)
        },
    )?;

    linker.func_wrap(
        "js",
        "webview_add_user_script",
        |_caller: Caller<'_, HostState>, _desc: i32, _ptr: i32, _len: i32, _time: i32| -> i32 { 0 },
    )?;

    linker.func_wrap(
        "js",
        "webview_get_cookies",
        |mut caller: Caller<'_, HostState>, _desc: i32| -> i32 {
            caller.data_mut().js_requested = true;
            let mut w = super::postcard::PostcardWriter::new();
            w.write_u64(0);
            caller
                .data_mut()
                .store
                .store(StoredValue::Bytes(w.into_bytes()))
        },
    )?;

    linker.func_wrap(
        "js",
        "webview_delete_cookie",
        |mut caller: Caller<'_, HostState>,
         _desc: i32,
         _name_ptr: i32,
         _name_len: i32,
         _url_ptr: i32,
         _url_len: i32|
         -> i32 {
            caller.data_mut().js_requested = true;
            0
        },
    )?;

    Ok(())
}
