//! Android bridge. Calls the static methods on
//! `app.yejian.reader.RustActivity` through the `ndk-context` JVM handle that
//! `android-activity` (and therefore winit) initialises before `android_main`.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use jni::objects::{GlobalRef, JClass, JObject, JString, JValue};
use jni::{JNIEnv, JavaVM};

use crate::{
    parse_import_json, parse_insets_csv, ImportBatch, Insets, PickKind, PlatformError, Result,
};

impl From<jni::errors::Error> for PlatformError {
    fn from(error: jni::errors::Error) -> Self {
        PlatformError::Jni(error.to_string())
    }
}

pub(crate) fn init() {
    // Creating the inbox here means the UI can show a usable path before the
    // first picker call. A failure is surfaced later by `inbox_dir`.
    // A cold-start ACTION_VIEW may already have queued a batch.
    QUEUED.store(true, Ordering::Release);
    let _ = inbox_dir();
}

static WAKER: Mutex<Option<Arc<dyn Fn() + Send + Sync>>> = Mutex::new(None);
/// Set by the JNI callback and the first successful `set_event_waker`. When a
/// waker is installed, a `false` flag proves the Java queue is empty, so the UI
/// can call `poll_import()` every frame without paying a JNI round trip.
static QUEUED: AtomicBool = AtomicBool::new(false);
static WAKER_SET: AtomicBool = AtomicBool::new(false);
/// Linux-style "back requested by the system shell". Drained by the UI.
static BACK_PENDING: AtomicBool = AtomicBool::new(false);
/// Safety net for the fast path: if the JNI wake callback were ever missing,
/// an idle repaint still checks the Java queue every few seconds. Repaints only
/// happen while the UI is drawing anyway, so this never creates a tick.
static LAST_POLL_MS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();

fn now_ms() -> u64 {
    START.get_or_init(Instant::now).elapsed().as_millis() as u64
}

fn notify_waker() {
    let waker = WAKER
        .lock()
        .ok()
        .and_then(|guard| guard.as_ref().map(Arc::clone));
    let Some(waker) = waker else {
        return;
    };
    // A panic in the UI callback must not unwind into the JVM.
    let _ = catch_unwind(AssertUnwindSafe(|| waker()));
}

/// JNI entry point called by `RustActivity.enqueueResult` once a batch is
/// queued. Waking the event loop here means the UI never polls while idle.
///
/// # Safety
/// Called by the JVM with the activity's `jclass`; the arguments are unused.
#[unsafe(no_mangle)]
pub extern "system" fn Java_app_yejian_reader_RustActivity_nativeImportFinished(
    _env: JNIEnv<'_>,
    _class: JClass<'_>,
) {
    QUEUED.store(true, Ordering::Release);
    notify_waker();
}

/// JNI entry point called by `RustActivity` when the user presses or gestures
/// back. Only records the request: the UI decides whether to navigate or exit.
///
/// # Safety
/// Called by the JVM with the activity's `jclass`; the arguments are unused.
#[unsafe(no_mangle)]
pub extern "system" fn Java_app_yejian_reader_RustActivity_nativeBackPressed(
    _env: JNIEnv<'_>,
    _class: JClass<'_>,
) {
    BACK_PENDING.store(true, Ordering::Release);
    notify_waker();
}

pub(crate) fn take_back_requested() -> bool {
    BACK_PENDING.swap(false, Ordering::AcqRel)
}

pub(crate) fn finish() -> Result<()> {
    with_activity(|env, class| {
        let value = env
            .call_static_method(class, "finishApp", "()V", &[])
            .map_err(|error| PlatformError::Jni(format!("finishApp: {error}")))?;
        value
            .v()
            .map_err(|error| PlatformError::Jni(format!("finishApp result: {error}")))
    })
}

pub(crate) fn set_event_waker(waker: Box<dyn Fn() + Send + Sync>) {
    if let Ok(mut guard) = WAKER.lock() {
        *guard = Some(Arc::from(waker));
    }
    WAKER_SET.store(true, Ordering::Release);
}

fn with_activity<T>(
    f: impl FnOnce(&mut JNIEnv<'_>, &GlobalRef) -> Result<T>,
) -> Result<T> {
    let context = catch_unwind(ndk_context::android_context).map_err(|_| {
        PlatformError::NotReady("Android JNI context (android-activity has not started)")
    })?;

    // SAFETY: `ndk-context` hands back the JavaVM owned by android-activity.
    let vm = unsafe { JavaVM::from_raw(context.vm().cast()) }
        .map_err(|error| PlatformError::Jni(format!("JavaVM::from_raw: {error}")))?;
    // A permanent attachment avoids an attach/detach round trip on every frame.
    // The local frame then releases the class/string locals created below.
    let mut env = vm
        .attach_current_thread_permanently()
        .map_err(|error| PlatformError::Jni(format!("attach_current_thread: {error}")))?;

    env.with_local_frame(32, |env| {
        // SAFETY: `ndk-context` owns this global reference. `forget` below
        // keeps `JObject::drop` from deleting a reference we do not own.
        let activity = unsafe { JObject::from_raw(context.context().cast()) };
        // `FindClass` on a native thread uses the system class loader, which
        // cannot see app classes. Ask the activity for its class instead and
        // keep a global reference for the static-method call.
        let class = env.get_object_class(&activity);
        std::mem::forget(activity);

        let class = class?;
        let class_object: &JObject<'_> = class.as_ref();
        let class = env.new_global_ref(class_object)?;
        f(env, &class)
    })
}

fn call_static_string(
    env: &mut JNIEnv<'_>,
    class: &GlobalRef,
    method: &str,
) -> Result<String> {
    let value = env
        .call_static_method(class, method, "()Ljava/lang/String;", &[])
        .map_err(|error| PlatformError::Jni(format!("{method}: {error}")))?;
    let object = value
        .l()
        .map_err(|error| PlatformError::Jni(format!("{method} result: {error}")))?;
    if object.is_null() {
        return Ok(String::new());
    }
    let string = JString::from(object);
    let text: String = env
        .get_string(&string)
        .map_err(|error| PlatformError::Jni(format!("{method} text: {error}")))?
        .into();
    Ok(text)
}

pub(crate) fn pick(kind: PickKind) -> Result<i64> {
    // `RustActivity.pickComics(int mode)`: 0 = archive picker, 1 = zip/json.
    let mode = match kind {
        PickKind::Comics => 0,
        PickKind::Backup => 1,
    };
    with_activity(|env, class| {
        let value = env
            .call_static_method(class, "pickComics", "(I)J", &[JValue::Int(mode)])
            .map_err(|error| PlatformError::Jni(format!("pickComics: {error}")))?;
        let request_id = value
            .j()
            .map_err(|error| PlatformError::Jni(format!("pickComics result: {error}")))?;
        if request_id < 0 {
            Err(PlatformError::Picker(
                "a picker request is already running or the activity is not ready".into(),
            ))
        } else {
            // A batch will arrive later; keep the fast path checking the queue.
            QUEUED.store(true, Ordering::Release);
            Ok(request_id)
        }
    })
}

pub(crate) fn import_pending() -> Result<bool> {
    with_activity(|env, class| {
        let value = env
            .call_static_method(class, "isImporting", "()Z", &[])
            .map_err(|error| PlatformError::Jni(format!("isImporting: {error}")))?;
        value
            .z()
            .map_err(|error| PlatformError::Jni(format!("isImporting result: {error}")))
    })
}

pub(crate) fn share_file(path: &std::path::Path) -> Result<()> {
    call_file_action("shareFile", "share", path)
}

pub(crate) fn export_file(path: &std::path::Path) -> Result<()> {
    call_file_action("exportFile", "save", path)
}

fn call_file_action(method: &str, verb: &str, path: &std::path::Path) -> Result<()> {
    let value = path
        .to_str()
        .ok_or_else(|| PlatformError::Io(format!("path is not valid UTF-8: {path:?}")))?;
    with_activity(|env, class| {
        let argument = env
            .new_string(value)
            .map_err(|error| PlatformError::Jni(format!("{method} argument: {error}")))?;
        let result = env
            .call_static_method(
                class,
                method,
                "(Ljava/lang/String;)Z",
                &[JValue::Object(&JObject::from(argument))],
            )
            .map_err(|error| PlatformError::Jni(format!("{method}: {error}")))?;
        let started = result
            .z()
            .map_err(|error| PlatformError::Jni(format!("{method} result: {error}")))?;
        if started {
            Ok(())
        } else {
            Err(PlatformError::Io(format!(
                "the Java shell could not {verb} {value}"
            )))
        }
    })
}

pub(crate) fn poll_import() -> Result<Option<ImportBatch>> {
    // Fast path: with a waker installed, "not queued" is authoritative, so an
    // idle frame costs one atomic load instead of a JNI call.
    // Consume the wake before checking Java. A result enqueued while
    // pollImport is returning an empty string must leave its wake pending.
    let queued = QUEUED.swap(false, Ordering::AcqRel);
    if WAKER_SET.load(Ordering::Acquire) && !queued {
        if now_ms().saturating_sub(LAST_POLL_MS.load(Ordering::Relaxed)) < 3000 {
            return Ok(None);
        }
    }
    with_activity(|env, class| {
        let text = call_static_string(env, class, "pollImport")?;
        LAST_POLL_MS.store(now_ms(), Ordering::Relaxed);
        if text.is_empty() {
            Ok(None)
        } else {
            QUEUED.store(true, Ordering::Release);
            parse_import_json(&text).map(Some)
        }
    })
}

pub(crate) fn inbox_dir() -> Result<PathBuf> {
    with_activity(|env, class| {
        let text = call_static_string(env, class, "inboxDir")?;
        if text.is_empty() {
            Err(PlatformError::NotReady("filesDir/inbox"))
        } else {
            Ok(PathBuf::from(text))
        }
    })
}

pub(crate) fn app_data_dir() -> Result<PathBuf> {
    with_activity(|env, class| {
        let text = call_static_string(env, class, "appDataDir")?;
        if text.is_empty() {
            Err(PlatformError::NotReady("filesDir"))
        } else {
            Ok(PathBuf::from(text))
        }
    })
}

pub(crate) fn safe_insets() -> Result<Insets> {
    with_activity(|env, class| {
        let text = call_static_string(env, class, "safeInsetsPx")?;
        if text.is_empty() {
            Err(PlatformError::NotReady("window insets"))
        } else {
            parse_insets_csv(&text)
        }
    })
}
