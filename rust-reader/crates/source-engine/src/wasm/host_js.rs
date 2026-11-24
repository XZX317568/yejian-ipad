//! `js` / `webview` WASM host imports.
//!
//! Synchronous JavaScript runs on a sandboxed QuickJS context ([`JsRuntime`])
//! matching the upstream Aidoku contract: `context_eval`/`context_get` read a
//! UTF-8 string from WASM memory and store the `ToString`-coerced result as a
//! new `Bytes` descriptor. Error codes follow
//! `aidoku-rs/crates/test-runner/src/imports/js.rs`:
//!
//! * `-1` MissingResult — JS exception, timeout, or context creation failure;
//! * `-2` InvalidContext — descriptor is not a live JS context;
//! * `-3` InvalidString — negative/out-of-bounds pointer or non-UTF-8 input.
//!
//! Only *unsupported* calls (`context_eval_async`, every `webview_*`) raise
//! [`HostState::js_requested`]; a real JS error is reported to the source as
//! `-1` and never escalates into a web-verification prompt.

use wasmi::{Caller, Linker};

use super::HostState;
use super::js_runtime::JsRuntime;
use super::memory;
use super::store::{MAX_JS_CONTEXTS, StoredValue};

/// Upstream `Result::MissingResult`.
const MISSING_RESULT: i32 = -1;
/// Upstream `Result::InvalidContext`.
const INVALID_CONTEXT: i32 = -2;
/// Upstream `Result::InvalidString`.
const INVALID_STRING: i32 = -3;

fn unsupported(caller: &mut Caller<'_, HostState>) -> i32 {
    caller.data_mut().js_requested = true;
    -1
}

/// Reads a UTF-8 argument; mirrors upstream `read_string` where a bad
/// pointer/length or invalid UTF-8 is `InvalidString`.
fn read_arg(caller: &Caller<'_, HostState>, ptr: i32, len: i32) -> Result<String, i32> {
    if ptr < 0 || len < 0 {
        return Err(INVALID_STRING);
    }
    let Some(mem) = caller.data().memory else {
        return Err(INVALID_STRING);
    };
    memory::read_string(mem, caller, ptr, len).map_err(|_| INVALID_STRING)
}

fn js_context<'a>(caller: &'a Caller<'_, HostState>, desc: i32) -> Result<&'a JsRuntime, i32> {
    match caller.data().store.get(desc) {
        Some(StoredValue::JsContext(rt)) => Ok(rt),
        _ => Err(INVALID_CONTEXT),
    }
}

pub(crate) fn link(linker: &mut Linker<HostState>) -> anyhow::Result<()> {
    linker.func_wrap(
        "js",
        "context_create",
        |mut caller: Caller<'_, HostState>| -> i32 {
            // Cap live contexts; over-limit is a plain MissingResult, never a
            // WebView request.
            if caller.data().store.js_context_count() >= MAX_JS_CONTEXTS {
                return MISSING_RESULT;
            }
            match JsRuntime::new() {
                Ok(rt) => caller
                    .data_mut()
                    .store
                    .store(StoredValue::JsContext(Box::new(rt))),
                // No verification flag: a host-side failure is not the source
                // asking for a WebView.
                Err(e) => {
                    eprintln!("js.context_create failed: {e:#}");
                    MISSING_RESULT
                }
            }
        },
    )?;

    linker.func_wrap(
        "js",
        "context_eval",
        |mut caller: Caller<'_, HostState>, desc: i32, ptr: i32, len: i32| -> i32 {
            let source = match read_arg(&caller, ptr, len) {
                Ok(s) => s,
                Err(code) => return code,
            };
            let out = match js_context(&caller, desc).and_then(|rt| {
                rt.eval(&source).map_err(|_| MISSING_RESULT)
            }) {
                Ok(s) => s,
                Err(code) => return code,
            };
            caller
                .data_mut()
                .store
                .store(StoredValue::Bytes(out.into_bytes()))
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
        |mut caller: Caller<'_, HostState>, desc: i32, ptr: i32, len: i32| -> i32 {
            let name = match read_arg(&caller, ptr, len) {
                Ok(s) => s,
                Err(code) => return code,
            };
            let out = match js_context(&caller, desc)
                .and_then(|rt| rt.get(&name).map_err(|_| MISSING_RESULT))
            {
                Ok(s) => s,
                Err(code) => return code,
            };
            caller
                .data_mut()
                .store
                .store(StoredValue::Bytes(out.into_bytes()))
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

#[cfg(test)]
mod tests {
    //! End-to-end ABI checks: a tiny WAT module exposes thin wrappers over the
    //! `js`/`std` imports so tests exercise them through wasmi, exactly like a
    //! real source would.
    use std::sync::Arc;

    use wasmi::{Engine, Instance, Linker, Module, Store, Val};

    use super::*;
    use crate::http::NetResponseData;
    use crate::wasm::interpreter::RequestHandler;
    use crate::wasm::store::GlobalStore;

    /// `copy` writes a descriptor's bytes at offset 0 and returns its length.
    const WAT: &str = r#"(module
        (import "js" "context_create" (func $create (result i32)))
        (import "js" "context_eval" (func $eval (param i32 i32 i32) (result i32)))
        (import "js" "context_eval_async" (func $aeval (param i32 i32 i32) (result i32)))
        (import "js" "context_get" (func $get (param i32 i32 i32) (result i32)))
        (import "std" "destroy" (func $destroy (param i32)))
        (import "std" "buffer_len" (func $len (param i32) (result i32)))
        (import "std" "read_buffer" (func $read (param i32 i32 i32) (result i32)))
        (memory (export "memory") 1)
        (func (export "create") (result i32) (call $create))
        (func (export "eval") (param i32 i32 i32) (result i32)
            local.get 0 local.get 1 local.get 2 (call $eval))
        (func (export "aeval") (param i32 i32 i32) (result i32)
            local.get 0 local.get 1 local.get 2 (call $aeval))
        (func (export "get") (param i32 i32 i32) (result i32)
            local.get 0 local.get 1 local.get 2 (call $get))
        (func (export "destroy") (param i32) (result i32)
            local.get 0 (call $destroy) i32.const 0)
        (func (export "copy") (param i32) (result i32)
            (local $n i32)
            local.get 0 (call $len) local.set $n
            (if (i32.gt_s (local.get $n) (i32.const 0))
                (then local.get 0 i32.const 0 local.get $n (call $read) drop))
            local.get $n))"#;

    const BUF: i32 = 16;

    struct Fx {
        store: Store<HostState>,
        instance: Instance,
        memory: wasmi::Memory,
    }

    impl Fx {
        fn new() -> Self {
            let engine = Engine::default();
            let module = Module::new(&engine, wat::parse_str(WAT).unwrap().as_slice()).unwrap();
            let state = HostState {
                store: GlobalStore::new(),
                memory: None,
                source_key: "test.source".into(),
                print_handler: None,
                partial_result_handler: None,
                request_handler: Arc::new(|_| NetResponseData::default()) as RequestHandler,
                js_requested: false,
                challenge_suspected: false,
            };
            let mut store = Store::new(&engine, state);
            let mut linker = Linker::new(&engine);
            link(&mut linker).unwrap();
            crate::wasm::host_std::link(&mut linker).unwrap();
            let instance = linker.instantiate_and_start(&mut store, &module).unwrap();
            let memory = instance.get_memory(&store, "memory").unwrap();
            store.data_mut().memory = Some(memory);
            Self { store, instance, memory }
        }

        fn call(&mut self, name: &str, args: &[i32]) -> i32 {
            let params: Vec<Val> = args.iter().map(|a| Val::I32(*a)).collect();
            let mut out = [Val::I32(0)];
            self.instance
                .get_func(&self.store, name)
                .unwrap()
                .call(&mut self.store, &params, &mut out)
                .unwrap();
            match out[0] {
                Val::I32(v) => v,
                _ => panic!("{name} returned a non-i32"),
            }
        }

        fn put_str(&mut self, s: &str) -> (i32, i32) {
            self.memory.write(&mut self.store, BUF as usize, s.as_bytes()).unwrap();
            (BUF, s.len() as i32)
        }

        fn copy(&mut self, desc: i32) -> Option<String> {
            let n = self.call("copy", &[desc]);
            if n < 0 {
                return None;
            }
            let mut buf = vec![0u8; n as usize];
            self.memory.read(&self.store, 0, &mut buf).unwrap();
            Some(String::from_utf8(buf).unwrap())
        }
    }

    #[test]
    fn eval_round_trips_utf8_result() {
        let mut fx = Fx::new();
        let ctx = fx.call("create", &[]);
        assert!(ctx > 0);
        let (p, l) = fx.put_str("JSON.stringify({name:'海贼王'})");
        let desc = fx.call("eval", &[ctx, p, l]);
        assert!(desc > 0, "eval returned {desc}");
        assert_eq!(fx.copy(desc).as_deref(), Some(r#"{"name":"海贼王"}"#));
        // `get` reads globals without double-encoding strings.
        let (p, l) = fx.put_str("'世界'");
        let desc = fx.call("eval", &[ctx, p, l]);
        let (p, l) = fx.put_str("missing");
        let undef = fx.call("get", &[ctx, p, l]);
        assert_eq!(fx.copy(desc).as_deref(), Some("世界"));
        assert_eq!(fx.copy(undef).as_deref(), Some("undefined"));
        assert!(!fx.store.data().js_requested);
    }

    #[test]
    fn destroyed_context_returns_invalid_context() {
        let mut fx = Fx::new();
        let ctx = fx.call("create", &[]);
        fx.call("destroy", &[ctx]);
        let (p, l) = fx.put_str("1+1");
        assert_eq!(fx.call("eval", &[ctx, p, l]), -2);
        assert_eq!(fx.call("get", &[ctx, p, l]), -2);
    }

    #[test]
    fn context_cap_returns_missing_then_frees_slot() {
        let mut fx = Fx::new();
        let mut descs = Vec::new();
        for _ in 0..MAX_JS_CONTEXTS {
            let d = fx.call("create", &[]);
            assert!(d > 0, "create returned {d}");
            descs.push(d);
        }
        // The fifth context is MissingResult and never a verification request.
        assert_eq!(fx.call("create", &[]), -1);
        assert!(!fx.store.data().js_requested);
        // `std.destroy` releases the slot so the next create succeeds.
        fx.call("destroy", &[descs[0]]);
        let d = fx.call("create", &[]);
        assert!(d > 0, "create after destroy returned {d}");
        // The new context is isolated and surviving ones still work.
        let (p, l) = fx.put_str("var only_second = 9; 1+1");
        assert!(fx.call("eval", &[descs[1], p, l]) > 0);
        let (p, l) = fx.put_str("only_second");
        let got = fx.call("get", &[d, p, l]);
        assert_eq!(fx.copy(got).as_deref(), Some("undefined"));
    }

    #[test]
    fn invalid_input_returns_invalid_string() {
        let mut fx = Fx::new();
        let ctx = fx.call("create", &[]);
        assert_eq!(fx.call("eval", &[ctx, -1, 4]), -3);
        assert_eq!(fx.call("eval", &[ctx, 0, -4]), -3);
        assert_eq!(fx.call("eval", &[ctx, 65530, 100]), -3);
        fx.memory.write(&mut fx.store, BUF as usize, &[0xFF, 0xFE]).unwrap();
        assert_eq!(fx.call("eval", &[ctx, BUF, 2]), -3);
    }

    #[test]
    fn js_failure_returns_missing_result_without_verification() {
        let mut fx = Fx::new();
        let ctx = fx.call("create", &[]);
        let (p, l) = fx.put_str("throw new Error('x')");
        assert_eq!(fx.call("eval", &[ctx, p, l]), -1);
        assert!(!fx.store.data().js_requested);
        let (p, l) = fx.put_str("while(true){}");
        assert_eq!(fx.call("eval", &[ctx, p, l]), -1);
        assert!(!fx.store.data().js_requested);
        let (p, l) = fx.put_str("1+1");
        assert!(fx.call("eval", &[ctx, p, l]) > 0);
    }

    #[test]
    fn async_eval_stays_unsupported() {
        let mut fx = Fx::new();
        let ctx = fx.call("create", &[]);
        let (p, l) = fx.put_str("1+1");
        assert_eq!(fx.call("aeval", &[ctx, p, l]), -1);
        assert!(fx.store.data().js_requested);
    }
}
