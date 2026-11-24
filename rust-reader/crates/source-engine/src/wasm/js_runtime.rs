//! Sandboxed QuickJS runtime backing the `js` WASM host imports.
//!
//! Backs the `js` WASM host imports via [`super::host_js`].
//!
//! `rquickjs` is built with the `parallel` feature: `Runtime`/`Context` are
//! `Send`/`Sync` because all entry points guard the underlying `JSRuntime`
//! with an internal lock (`Context::with` takes that lock once per call). That
//! lets the context live inside the `SourceEngine` worker thread with no
//! unsafe impl on our side.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};
use rquickjs::{Coerced, Context, Ctx, FromJs, Runtime};

/// Max memory the QuickJS allocator may use for one context.
const MEMORY_LIMIT: usize = 32 * 1024 * 1024;
/// Max JS stack size (QuickJS default is 256KiB; Aidoku sources recurse more).
const STACK_LIMIT: usize = 512 * 1024;
/// Wall-clock budget for a single `eval`/`get` call. Enforced via QuickJS's
/// interrupt handler: cooperative, not a preemptive hard limit — the handler
/// is only polled inside JS bytecode loops, property access and calls, so
/// pure-C builtins (e.g. a huge `JSON.stringify`) may overshoot it.
const CALL_BUDGET: Duration = Duration::from_millis(200);
/// Cap on JS exception text so errors stay readable in the UI.
const ERROR_TEXT_LIMIT: usize = 1024;

/// One QuickJS runtime + context pair. Memory and stack sizes are hard
/// allocator limits; the call budget is cooperative (see [`CALL_BUDGET`]).
///
/// Field order matters: `context` is declared before `_runtime` so the context
/// is dropped before the runtime that owns it.
pub(crate) struct JsRuntime {
    context: Context,
    /// Held only for ownership/drop; all calls go through `context.with`.
    _runtime: Runtime,
    /// Deadline of the currently running call. Refreshed before every
    /// `eval`/`get`; the interrupt handler interrupts once it passes.
    deadline: Arc<Mutex<Instant>>,
}

impl JsRuntime {
    /// Creates a limited context. Returns `Err` (never panics) when the
    /// runtime or context cannot be built.
    pub(crate) fn new() -> Result<Self> {
        let runtime = Runtime::new().map_err(|e| anyhow!("quickjs runtime init: {e}"))?;
        runtime.set_memory_limit(MEMORY_LIMIT);
        runtime.set_max_stack_size(STACK_LIMIT);
        let deadline = Arc::new(Mutex::new(Instant::now()));
        let handler_deadline = Arc::clone(&deadline);
        runtime.set_interrupt_handler(Some(Box::new(move || {
            match handler_deadline.lock() {
                Ok(d) => Instant::now() >= *d,
                // A poisoned deadline means something already went wrong:
                // stop the script rather than letting it run unbounded.
                Err(_) => true,
            }
        })));
        let context =
            Context::full(&runtime).map_err(|e| anyhow!("quickjs context init: {e}"))?;
        Ok(Self {
            context,
            _runtime: runtime,
            deadline,
        })
    }

    /// Evaluates `source` and returns the completion value coerced with JS
    /// `ToString` semantics — the Aidoku `js.context_eval` contract, where the
    /// caller wraps results in `JSON.stringify` itself when it wants JSON.
    pub(crate) fn eval(&self, source: &str) -> Result<String> {
        self.context.with(|ctx| {
            // Arm inside `with` so the deadline refresh happens under the
            // runtime lock: a racing call can never re-arm while this one runs.
            self.arm();
            match ctx.eval::<Coerced<String>, _>(source) {
                Ok(s) => Ok(s.0),
                Err(e) => Err(self.describe(&ctx, e)),
            }
        })
    }

    /// Reads a global property and returns it coerced to a string. This is a
    /// plain property lookup, not `eval`, so property names cannot smuggle
    /// code (`a.b` reads the global literally named `a.b`).
    pub(crate) fn get(&self, name: &str) -> Result<String> {
        self.context.with(|ctx| {
            self.arm();
            match ctx.globals().get::<_, Coerced<String>>(name) {
                Ok(s) => Ok(s.0),
                Err(e) => Err(self.describe(&ctx, e)),
            }
        })
    }

    /// Refreshes the deadline for the next call.
    fn arm(&self) {
        if let Ok(mut d) = self.deadline.lock() {
            *d = Instant::now() + CALL_BUDGET;
        }
    }

    /// Turns an `rquickjs` failure into a bounded error message. Drains the
    /// pending JS exception via `ctx.catch()` so the context is reusable.
    fn describe(&self, ctx: &Ctx<'_>, err: rquickjs::Error) -> anyhow::Error {
        let timed_out = self
            .deadline
            .lock()
            .map(|d| Instant::now() >= *d)
            .unwrap_or(true);
        let mut text = match err {
            rquickjs::Error::Exception => {
                let exception = ctx.catch();
                match Coerced::<String>::from_js(ctx, exception) {
                    Ok(s) => s.0,
                    Err(_) => {
                        // Stringifying the exception threw again (e.g. a
                        // hostile `toString`); drain that second exception so
                        // the context stays usable.
                        let _ = ctx.catch();
                        "JavaScript exception".to_string()
                    }
                }
            }
            other => other.to_string(),
        };
        if text.chars().count() > ERROR_TEXT_LIMIT {
            text = text.chars().take(ERROR_TEXT_LIMIT).collect();
        }
        if timed_out {
            anyhow!("js call exceeded the {}ms budget: {text}", CALL_BUDGET.as_millis())
        } else {
            anyhow!("js error: {text}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eval_returns_json_string_unchecked() {
        let rt = JsRuntime::new().unwrap();
        let out = rt.eval("JSON.stringify([{name:'海贼王',value:1}])").unwrap();
        assert_eq!(out, r#"[{"name":"海贼王","value":1}]"#);
    }

    #[test]
    fn context_persists_and_stays_isolated() {
        let a = JsRuntime::new().unwrap();
        let b = JsRuntime::new().unwrap();
        a.eval("var kept = 41;").unwrap();
        assert_eq!(a.eval("kept + 1").unwrap(), "42");
        assert_eq!(b.eval("typeof kept").unwrap(), "undefined");
    }

    #[test]
    fn get_reads_global_without_eval() {
        let rt = JsRuntime::new().unwrap();
        rt.eval("var cfg = {v:1}; var num = 7;").unwrap();
        assert_eq!(rt.get("num").unwrap(), "7");
        assert_eq!(rt.get("cfg").unwrap(), "[object Object]");
        // Literal property name, never parsed as code.
        assert_eq!(rt.get("num;kept=1").unwrap(), "undefined");
        assert_eq!(rt.get("missing").unwrap(), "undefined");
    }

    #[test]
    fn infinite_loop_times_out_then_recovers() {
        let rt = JsRuntime::new().unwrap();
        let err = rt.eval("while(true){}").unwrap_err().to_string();
        assert!(err.contains("budget") || err.contains("interrupt"), "{err}");
        // The next call gets a fresh deadline and a reusable context.
        assert_eq!(rt.eval("1 + 1").unwrap(), "2");
    }

    #[test]
    fn syntax_and_throw_are_errors_not_panics() {
        let rt = JsRuntime::new().unwrap();
        assert!(rt.eval("function {").is_err());
        let err = rt.eval("throw new Error('boom')").unwrap_err().to_string();
        assert!(err.contains("boom"), "{err}");
    }

    #[test]
    fn exception_to_string_failure_is_drained() {
        let rt = JsRuntime::new().unwrap();
        assert!(rt.eval("throw {toString(){throw 1}}").is_err());
        // The secondary exception must not poison later evaluations.
        assert_eq!(rt.eval("2").unwrap(), "2");
    }

    #[test]
    fn oversized_allocation_fails_then_recovers() {
        let rt = JsRuntime::new().unwrap();
        // ~64MiB string > the 32MiB memory limit: an allocation error, not a
        // panic, and the runtime stays usable afterwards.
        assert!(rt.eval("'x'.repeat(1 << 26)").is_err());
        assert!(rt.eval("new ArrayBuffer(1 << 26)").is_err());
        assert_eq!(rt.eval("2").unwrap(), "2");
    }

    #[test]
    fn infinite_getter_times_out_then_recovers() {
        let rt = JsRuntime::new().unwrap();
        // `get` runs the property getter; the cooperative interrupt handler
        // bounds it like any eval.
        rt.eval("Object.defineProperty(globalThis, 'bad', {get(){while(true){}}});")
            .unwrap();
        assert!(rt.get("bad").is_err());
        assert_eq!(rt.eval("1 + 1").unwrap(), "2");
    }

    #[test]
    fn exception_with_infinite_to_string_times_out() {
        let rt = JsRuntime::new().unwrap();
        // Stringifying the thrown object runs its `toString` under the same
        // deadline: the loop must not hang the error path either.
        assert!(rt.eval("throw {toString(){while(true){}}}").is_err());
        assert_eq!(rt.eval("2").unwrap(), "2");
    }
}
