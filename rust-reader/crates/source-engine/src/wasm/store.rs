//! Descriptor-based object store shared across all WASM host imports, mirroring
//! `lib/eval/aidoku/src/store/global_store.dart`.

use std::collections::HashMap;
use std::sync::Arc;

use image::RgbaImage;
use parking_lot::Mutex;
use scraper::Html;

use super::host_net::NetRequestState;
use super::js_runtime::JsRuntime;

/// A parsed HTML document. Uses `Arc<Mutex<..>>` (not `Rc<RefCell<..>>`) even
/// though it's only ever touched from one thread at a time, because the
/// whole `AidokuInterpreter` (and everything reachable from its
/// [`GlobalStore`]) has to be `Send`: the interpreter is created on, and then
/// moved to, the dedicated source worker thread.
pub(crate) struct HtmlDoc {
    pub html: Mutex<Html>,
    pub base_url: Option<String>,
}

unsafe impl Send for HtmlDoc {}
unsafe impl Sync for HtmlDoc {}

/// A reference to a single node inside a [`HtmlDoc`]'s tree. `scraper::ElementRef`
/// can't be stored directly (it borrows from the tree), so we keep the owning
/// document + a stable `NodeId` and re-derive the live reference on demand.
#[derive(Clone)]
pub(crate) struct HtmlNode {
    pub doc: Arc<HtmlDoc>,
    pub id: ego_tree::NodeId,
}

pub(crate) enum StoredValue {
    Bytes(Vec<u8>),
    NetRequest(NetRequestState),
    HtmlDocRoot(Arc<HtmlDoc>),
    HtmlNode(HtmlNode),
    HtmlNodeList(Vec<HtmlNode>),
    CanvasContext(RgbaImage),
    Image(RgbaImage),
    /// A live QuickJS runtime+context created by `js.context_create`.
    JsContext(Box<JsRuntime>),
    WebviewContext,
}

/// Max simultaneously live `js.context_create` contexts. Each [`JsRuntime`]
/// may allocate up to 32MiB, so 4 caps JS heap at ~128MiB per interpreter.
/// `remove`/`clear` below drop the `JsRuntime` and free the slot.
pub(crate) const MAX_JS_CONTEXTS: usize = 4;

/// Mirrors `GlobalStore` in Dart: a simple incrementing-id map of host-owned
/// objects referenced by the WASM module via opaque `i32` descriptors.
#[derive(Default)]
pub(crate) struct GlobalStore {
    storage: HashMap<i32, StoredValue>,
    next: i32,
}

impl GlobalStore {
    pub fn new() -> Self {
        Self {
            storage: HashMap::new(),
            next: 1,
        }
    }

    pub fn store(&mut self, value: StoredValue) -> i32 {
        let desc = self.next;
        self.storage.insert(desc, value);
        self.next += 1;
        desc
    }

    /// Count of live `StoredValue::JsContext` entries, checked by
    /// `js.context_create` against [`MAX_JS_CONTEXTS`].
    pub fn js_context_count(&self) -> usize {
        self.storage
            .values()
            .filter(|v| matches!(v, StoredValue::JsContext(_)))
            .count()
    }

    pub fn get(&self, desc: i32) -> Option<&StoredValue> {
        self.storage.get(&desc)
    }

    pub fn get_mut(&mut self, desc: i32) -> Option<&mut StoredValue> {
        self.storage.get_mut(&desc)
    }

    pub fn set(&mut self, desc: i32, value: StoredValue) {
        self.storage.insert(desc, value);
    }

    pub fn remove(&mut self, desc: i32) {
        self.storage.remove(&desc);
        if self.storage.is_empty() {
            self.next = 1;
        }
    }

    pub fn clear(&mut self) {
        self.storage.clear();
        self.next = 1;
    }

    /// Convenience used by `std.buffer_len` / `std.read_buffer`: any stored
    /// value that can be viewed as a byte buffer (raw bytes, or a postcard
    /// payload produced by another import).
    pub fn as_bytes(&self, desc: i32) -> Option<Vec<u8>> {
        match self.storage.get(&desc)? {
            StoredValue::Bytes(b) => Some(b.clone()),
            _ => None,
        }
    }
}
