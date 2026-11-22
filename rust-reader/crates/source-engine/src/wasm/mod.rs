//! Aidoku WebAssembly host implementation.
//!
//! Copied from `app/rust/src/aidoku_wasm` (Mangayomi v0.9.96, Apache-2.0).
//! The module layout, WASM ABI and postcard wire format are unchanged so real
//! Aidoku sources keep working. Flutter Rust Bridge was replaced with:
//!
//! * synchronous `net`/`print`/`partial_result` callbacks owned by the source
//!   worker thread,
//! * a native `reqwest` client (see [`crate::http`]),
//! * explicit "web verification required" errors for `js`/`webview` imports.
//!
//! Still intentionally limited (same as the original native engine): dynamic
//! `get_home`, dynamic `get_settings`, and `process_page_image` are not wired
//! up; static `settings.json`/`filters.json` remain the supported path.

#![allow(dead_code)]

pub(crate) mod host_canvas;
pub(crate) mod host_defaults;
pub(crate) mod host_env;
pub(crate) mod host_html;
pub(crate) mod host_js;
pub(crate) mod host_net;
pub(crate) mod host_std;
pub(crate) mod interpreter;
pub(crate) mod memory;
pub(crate) mod models;
pub(crate) mod postcard;
pub(crate) mod selector;
pub(crate) mod store;

pub(crate) use interpreter::HostState;
