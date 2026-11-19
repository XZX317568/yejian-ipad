//! Platform bridge shared by the Yejian Android and iOS shells.
//!
//! The UI crate owns every screen and every shell entry point. This crate only
//! answers four platform questions that winit/eframe cannot answer by itself:
//!
//! * open the system document picker and import the chosen archives,
//! * hand back an import batch after the copy finished,
//! * expose the app-private inbox and data directory,
//! * expose the current safe-area insets in physical pixels.
//!
//! The Android side lives in `platform/android` (Java `RustActivity` plus a
//! minimal Gradle project). The iOS side lives in `platform/ios` (an Xcode
//! project and a Swift document bridge). Both shells queue completed imports as
//! JSON strings; polling is deliberately simple so the caller can poll once per
//! frame from winit's event loop without any callback ordering.

use std::path::PathBuf;

/// One platform failure. Every variant is safe to show to a developer; the UI
/// layer should translate it into a user-facing message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlatformError {
    /// The current build target has no implementation for this operation.
    Unsupported(&'static str),
    /// The shell exists but is not ready yet (for example JNI is not attached).
    NotReady(&'static str),
    /// A JNI call failed. The message keeps the original Java error text.
    Jni(String),
    /// Reading or writing the app-private inbox failed.
    Io(String),
    /// The Java/Swift side returned JSON that does not match the contract.
    Protocol(String),
    /// The picker could not be presented, or one is already open.
    Picker(String),
}

impl std::fmt::Display for PlatformError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unsupported(what) => write!(f, "{what} is not available on this platform"),
            Self::NotReady(what) => write!(f, "{what} is not ready"),
            Self::Jni(message) => write!(f, "Android JNI error: {message}"),
            Self::Io(message) => write!(f, "I/O error: {message}"),
            Self::Protocol(message) => write!(f, "platform protocol error: {message}"),
            Self::Picker(message) => write!(f, "file picker error: {message}"),
        }
    }
}

impl std::error::Error for PlatformError {}

pub type Result<T> = std::result::Result<T, PlatformError>;

/// Safe-area insets in physical pixels.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Insets {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

/// Forced insets used by offscreen iPad layout checks (debug builds only).
#[cfg(debug_assertions)]
static INSETS_OVERRIDE: std::sync::Mutex<Option<Insets>> = std::sync::Mutex::new(None);

/// Force the safe-area insets instead of asking the shell.
///
/// Layout checks on a desktop GPU need the iPad insets, but those only exist
/// inside UIKit. Debug builds may pin them here; release builds ignore the
/// hook entirely, so shipped shells always read real values.
#[cfg(debug_assertions)]
pub fn set_insets_override(insets: Option<Insets>) {
    *INSETS_OVERRIDE
        .lock()
        .unwrap_or_else(|poison| poison.into_inner()) = insets;
}

#[cfg(debug_assertions)]
fn overridden_insets() -> Option<Insets> {
    *INSETS_OVERRIDE
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
}

/// Outcome of one picker request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportStatus {
    /// Every selected file was copied into the inbox.
    Completed,
    /// Some files were copied and some failed.
    Partial,
    /// The user left the picker without selecting a file.
    Canceled,
    /// Nothing was copied and at least one error was reported.
    Error,
}

/// Which picker produced an [`ImportBatch`].
///
/// Both pickers copy into the same app-private inbox and use the same JSON
/// contract. The kind travels with the batch so the caller can route a picked
/// archive to `import_files` and a picked backup to `Restore` without guessing
/// from the file extension.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum PickKind {
    /// Archive picker started by [`pick_comics`].
    #[default]
    Comics,
    /// Zip/JSON picker started by [`pick_backup`].
    Backup,
}

/// One archive copied into the app-private inbox.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportedFile {
    /// Display name after de-duplication (for example `Saga (2).cbz`).
    pub name: String,
    /// Absolute path inside the app sandbox.
    pub path: PathBuf,
}

/// One selected file that could not be copied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportFailure {
    pub name: String,
    pub reason: String,
}

/// Everything the UI needs after one picker interaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportBatch {
    /// The id returned by [`pick_comics`]. `0` means the batch came from the
    /// iOS `Documents/Inbox` scan at startup instead of a picker.
    pub request_id: i64,
    /// Picker that produced the batch. Startup `Documents/Inbox` batches are
    /// reported as [`PickKind::Comics`].
    pub kind: PickKind,
    pub status: ImportStatus,
    pub files: Vec<ImportedFile>,
    pub failed: Vec<ImportFailure>,
    /// Optional shell message, for example where a startup batch came from.
    pub message: Option<String>,
}

impl ImportBatch {
    /// True when at least one archive can be opened by the library importer.
    pub fn has_files(&self) -> bool {
        !self.files.is_empty()
    }

    /// Convenience for callers that only need the copied paths.
    pub fn paths(&self) -> impl Iterator<Item = &PathBuf> {
        self.files.iter().map(|file| &file.path)
    }
}

/// Prepare the shell once at startup. Safe to call more than once.
///
/// Android: creates `filesDir/inbox`. iOS: scans `Documents/Inbox`, creates the
/// Application Support inbox and installs the document-open delegate. On other
/// targets this is a no-op.
pub fn init() {
    #[cfg(target_os = "android")]
    android::init();
    #[cfg(target_os = "ios")]
    ios::init();
}

/// Open `ACTION_OPEN_DOCUMENT` (Android) or `UIDocumentPickerViewController`
/// (iOS) with multi-select enabled. Returns a request id used by
/// [`poll_import`].
///
/// The shell copies the selected content into the app-private inbox in the
/// background; the picker call itself returns immediately.
#[cfg(target_os = "android")]
pub fn pick_comics() -> Result<i64> {
    android::pick(PickKind::Comics)
}

/// Open the system document picker with multi-select enabled.
#[cfg(target_os = "ios")]
pub fn pick_comics() -> Result<i64> {
    ios::pick(PickKind::Comics)
}

/// Open the system document picker with multi-select enabled.
#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub fn pick_comics() -> Result<i64> {
    Err(PlatformError::Unsupported("comic file picker"))
}

/// Open the system picker restricted to `zip`/`json` so the user can choose a
/// library backup produced by the UI. Returns a request id used by
/// [`poll_import`]; the resulting batch has [`PickKind::Backup`].
///
/// One picker can run at a time on both platforms, so a backup pick while the
/// archive picker is still open fails with [`PlatformError::Picker`].
#[cfg(target_os = "android")]
pub fn pick_backup() -> Result<i64> {
    android::pick(PickKind::Backup)
}

/// Open the system picker restricted to `zip`/`json`.
#[cfg(target_os = "ios")]
pub fn pick_backup() -> Result<i64> {
    ios::pick(PickKind::Backup)
}

/// Open a `zip`/`json` picker.
#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub fn pick_backup() -> Result<i64> {
    Err(PlatformError::Unsupported("backup file picker"))
}

/// Return the next completed import batch, or `None` while the shell is still
/// copying. The queue is drained in order, so several picks in a row are safe.
#[cfg(target_os = "android")]
pub fn poll_import() -> Result<Option<ImportBatch>> {
    android::poll_import()
}

/// Return the next completed import batch, or `None` while the shell is still
/// copying. The queue is drained in order, so several picks in a row are safe.
#[cfg(target_os = "ios")]
pub fn poll_import() -> Result<Option<ImportBatch>> {
    ios::poll_import()
}

/// Return the next completed import batch, or `None`.
#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub fn poll_import() -> Result<Option<ImportBatch>> {
    Ok(None)
}

/// True while a picker is open or an import batch is still being copied.
///
/// The shell wakes the UI through [`set_event_waker`] when a batch is ready.
/// Callers that do not install a waker can poll at a low rate (250 ms) while
/// this returns `true` and return to idle polling afterwards.
#[cfg(target_os = "android")]
pub fn import_pending() -> Result<bool> {
    android::import_pending()
}

/// Always `false` on iOS: the Swift bridge wakes the UI directly and the poll
/// call is cheap, so no separate pending flag is needed.
#[cfg(target_os = "ios")]
pub fn import_pending() -> Result<bool> {
    Ok(false)
}

/// Always `false` without a mobile shell.
#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub fn import_pending() -> Result<bool> {
    Ok(false)
}

/// True when the system asked for "back" since the last call.
///
/// Android: a predictive-back gesture (API 33+) or the legacy back button
/// records the request in the shell and wakes the UI through
/// [`set_event_waker`]. The flag is drained by this call, which is a plain
/// atomic swap — no JNI round trip, safe to call every frame.
///
/// 3-button navigation on API 33+ still delivers `KEYCODE_BACK` to the native
/// input queue, and winit marks it handled, so it never reaches the activity.
/// Treat `egui::Key::BrowserBack` (and Escape) in the UI as the same request.
///
/// iOS and desktop have no hardware back; this always returns `false`.
#[cfg(target_os = "android")]
pub fn take_back_requested() -> bool {
    android::take_back_requested()
}

/// True when the system asked for "back"; always `false` without the Android
/// shell.
#[cfg(not(target_os = "android"))]
pub fn take_back_requested() -> bool {
    false
}

/// Ask the shell to close the app. Call it when back is pressed on the root
/// page; the shell never finishes on its own.
///
/// Android: `Activity.finish()` on the UI thread. iOS has no back button and no
/// supported programmatic exit, so it returns [`PlatformError::Unsupported`].
#[cfg(target_os = "android")]
pub fn finish() -> Result<()> {
    android::finish()
}

/// iOS has no supported programmatic exit; see App Store review guidance.
#[cfg(target_os = "ios")]
pub fn finish() -> Result<()> {
    Err(PlatformError::Unsupported("programmatic app exit"))
}

/// Only the mobile shells can close the app.
#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub fn finish() -> Result<()> {
    Err(PlatformError::Unsupported("programmatic app exit"))
}

/// Install the callback the shell runs when an import batch finished.
///
/// Android: `RustActivity` calls the exported
/// `Java_app_yejian_reader_RustActivity_nativeImportFinished` JNI function from
/// whichever thread finished the copy; pass a closure that calls
/// `egui::Context::request_repaint` and no frame has to poll while idle.
/// iOS: the Swift bridge dispatches the callback on the main queue.
///
/// Safe to call more than once; the latest callback replaces the previous one.
/// Without a waker the UI should fall back to the 250 ms
/// [`import_pending`] loop.
pub fn set_event_waker(waker: impl Fn() + Send + Sync + 'static) {
    #[cfg(target_os = "android")]
    android::set_event_waker(Box::new(waker));
    #[cfg(target_os = "ios")]
    ios::set_event_waker(Box::new(waker));
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    {
        let _ = waker;
    }
}

/// Hand one app-private file to the system share sheet ("save to Files",
/// "send to…") so the user can export a backup.
///
/// Android: `ACTION_SEND` with a read-only content URI from the bundled
/// `FileShareProvider`; the file is copied into `cacheDir/share` first, so the
/// source path may live anywhere in the app sandbox. No storage permission is
/// involved. iOS: `UIActivityViewController` with the file URL.
///
/// `path` must point at an existing file inside the app sandbox.
#[cfg(target_os = "android")]
pub fn share_file(path: &std::path::Path) -> Result<()> {
    android::share_file(path)
}

/// Hand one app-private file to `UIActivityViewController`.
#[cfg(target_os = "ios")]
pub fn share_file(path: &std::path::Path) -> Result<()> {
    ios::share_file(path)
}

/// Not available without a mobile shell.
#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub fn share_file(path: &std::path::Path) -> Result<()> {
    let _ = path;
    Err(PlatformError::Unsupported("system share sheet"))
}

/// "Save as": let the user pick a destination folder for the file.
///
/// Android: `ACTION_CREATE_DOCUMENT`, then the shell streams the file into the
/// returned URI — this is the reliable way to save a backup to Downloads/Drive.
/// iOS: the share sheet already contains "存储到文件", so this is the same as
/// [`share_file`].
#[cfg(target_os = "android")]
pub fn export_file(path: &std::path::Path) -> Result<()> {
    android::export_file(path)
}

/// Save through `UIActivityViewController` (contains "存储到文件").
#[cfg(target_os = "ios")]
pub fn export_file(path: &std::path::Path) -> Result<()> {
    ios::share_file(path)
}

/// Not available without a mobile shell.
#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub fn export_file(path: &std::path::Path) -> Result<()> {
    let _ = path;
    Err(PlatformError::Unsupported("save-as dialog"))
}

/// Absolute `filesDir/inbox` (Android) or Application Support `inbox` (iOS).
/// The directory is created if missing.
#[cfg(target_os = "android")]
pub fn inbox_dir() -> Result<PathBuf> {
    android::inbox_dir()
}

/// Absolute Application Support `inbox` path. The directory is created if
/// missing.
#[cfg(target_os = "ios")]
pub fn inbox_dir() -> Result<PathBuf> {
    ios::inbox_dir()
}
/// Absolute app-private inbox path.
#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub fn inbox_dir() -> Result<PathBuf> {
    Err(PlatformError::Unsupported("platform inbox directory"))
}

/// Absolute app-private data directory.
#[cfg(target_os = "android")]
pub fn app_data_dir() -> Result<PathBuf> {
    android::app_data_dir()
}

/// Absolute app-private data directory.
#[cfg(target_os = "ios")]
pub fn app_data_dir() -> Result<PathBuf> {
    ios::app_data_dir()
}
/// Absolute app-private data directory.
#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub fn app_data_dir() -> Result<PathBuf> {
    Err(PlatformError::Unsupported("platform app-data directory"))
}

/// Current safe-area insets in physical pixels. iOS must be called from the
/// main thread (the winit event loop already is).
#[cfg(target_os = "android")]
pub fn safe_insets() -> Result<Insets> {
    #[cfg(debug_assertions)]
    if let Some(insets) = overridden_insets() {
        return Ok(insets);
    }
    android::safe_insets()
}

/// Current safe-area insets in physical pixels.
#[cfg(target_os = "ios")]
pub fn safe_insets() -> Result<Insets> {
    #[cfg(debug_assertions)]
    if let Some(insets) = overridden_insets() {
        return Ok(insets);
    }
    ios::safe_insets()
}
/// Current safe-area insets in physical pixels.
#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub fn safe_insets() -> Result<Insets> {
    #[cfg(debug_assertions)]
    if let Some(insets) = overridden_insets() {
        return Ok(insets);
    }
    Ok(Insets::default())
}

/// Parse the JSON contract produced by `RustActivity.pollImport()` and
/// `yejian_ios_poll_import()`.
pub(crate) fn parse_import_json(json: &str) -> Result<ImportBatch> {
    let value: serde_json::Value = serde_json::from_str(json)
        .map_err(|error| PlatformError::Protocol(format!("invalid import JSON: {error}")))?;
    let object = value
        .as_object()
        .ok_or_else(|| PlatformError::Protocol("import payload is not an object".into()))?;

    let request_id = object
        .get("requestId")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(0);
    let kind = match object
        .get("kind")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("comics")
    {
        "backup" => PickKind::Backup,
        "comics" | "inbox" => PickKind::Comics,
        other => {
            return Err(PlatformError::Protocol(format!(
                "unknown import kind `{other}`"
            )))
        }
    };
    let status = match object
        .get("status")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("error")
    {
        "ok" => ImportStatus::Completed,
        "partial" => ImportStatus::Partial,
        "canceled" => ImportStatus::Canceled,
        "error" => ImportStatus::Error,
        other => {
            return Err(PlatformError::Protocol(format!(
                "unknown import status `{other}`"
            )))
        }
    };

    let files = object
        .get("files")
        .and_then(serde_json::Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    let path = item.get("path").and_then(serde_json::Value::as_str)?;
                    let name = item
                        .get("name")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_else(|| {
                            path.rsplit(['/', '\\']).next().unwrap_or("archive")
                        });
                    Some(ImportedFile {
                        name: name.to_owned(),
                        path: PathBuf::from(path),
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    let failed = object
        .get("failed")
        .and_then(serde_json::Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    let name = item
                        .get("name")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("archive");
                    let reason = item
                        .get("reason")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("copy failed");
                    Some(ImportFailure {
                        name: name.to_owned(),
                        reason: reason.to_owned(),
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    let message = object
        .get("message")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);

    Ok(ImportBatch {
        request_id,
        kind,
        status,
        files,
        failed,
        message,
    })
}

/// Parse `"left,top,right,bottom"` as physical pixels.
pub(crate) fn parse_insets_csv(csv: &str) -> Result<Insets> {
    let mut parts = csv.split(',');
    let mut next = || -> Result<i32> {
        let part = parts
            .next()
            .ok_or_else(|| PlatformError::Protocol("inset payload has fewer than 4 values".into()))?;
        part.trim()
            .parse::<i32>()
            .map_err(|error| PlatformError::Protocol(format!("invalid inset `{part}`: {error}")))
    };
    let insets = Insets {
        left: next()?,
        top: next()?,
        right: next()?,
        bottom: next()?,
    };
    if parts.next().is_some() {
        return Err(PlatformError::Protocol(
            "inset payload has more than 4 values".into(),
        ));
    }
    Ok(insets)
}

#[cfg(target_os = "android")]
mod android;
#[cfg(target_os = "ios")]
mod ios;
