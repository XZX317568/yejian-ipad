//! Cross-platform logging, crash records, abnormal-exit detection,
//! diagnostics export and supervised long-running threads.
use std::{
    backtrace::Backtrace,
    collections::VecDeque,
    fs::{self, File, OpenOptions},
    io::{self, Write},
    panic::AssertUnwindSafe,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock, RwLock},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

/// In-memory log lines kept for crash dumps and diagnostics.
const RING_LINES: usize = 300;
/// `logs/yejian.log` rotates past this size; one rotated file is kept.
const LOG_MAX_BYTES: u64 = 1024 * 1024;
const LOG_FILE: &str = "yejian.log";
const MARKER: &str = "running.marker";
const CRASH_PREFIX: &str = "crash-";
/// Mirrors the iOS/Android build number reported to stores.
const BUILD: &str = "20";

pub fn version_line() -> String {
    format!("yejian {} (build {BUILD})", env!("CARGO_PKG_VERSION"))
}

fn now_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or_default()
}

struct Ring(VecDeque<String>);
impl Ring {
    fn push(&mut self, line: String) {
        self.0.push_back(line);
        while self.0.len() > RING_LINES {
            self.0.pop_front();
        }
    }
    fn lines(&self) -> Vec<String> {
        self.0.iter().cloned().collect()
    }
}

struct Rolling {
    path: PathBuf,
    file: File,
    bytes: u64,
}
impl Rolling {
    fn open(path: &Path) -> io::Result<Self> {
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        let bytes = file.metadata().map(|m| m.len()).unwrap_or(0);
        Ok(Self {
            path: path.to_path_buf(),
            file,
            bytes,
        })
    }
    fn rotated(&self) -> PathBuf {
        self.path.with_file_name(format!("{LOG_FILE}.1"))
    }
    fn write_line(&mut self, line: &str) {
        if self.bytes + line.len() as u64 > LOG_MAX_BYTES {
            let rotated = self.rotated();
            let _ = fs::remove_file(&rotated);
            let _ = fs::rename(&self.path, &rotated);
            if let Ok(file) = OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(&self.path)
            {
                self.file = file;
                self.bytes = 0;
            }
        }
        if self.file.write_all(line.as_bytes()).is_ok() {
            self.bytes += line.len() as u64;
        }
    }
}

struct Shared {
    ring: Ring,
    log: Option<Rolling>,
    /// iOS keeps its pre-framework startup log (`Documents/yejian-startup.log`).
    startup: Option<File>,
}

struct YejianLogger {
    shared: Mutex<Shared>,
}
impl log::Log for YejianLogger {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        metadata.level() <= log::Level::Warn
            || (metadata.level() <= log::Level::Debug
                && ["yejian", "eframe", "egui_wgpu", "winit", "source_engine"]
                    .iter()
                    .any(|prefix| metadata.target().starts_with(prefix)))
    }
    fn log(&self, record: &log::Record<'_>) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let line = format!(
            "{} {}: {}\n",
            record.level(),
            record.target(),
            record.args()
        );
        let _ = std::io::stderr().write_all(line.as_bytes());
        let mut shared = self
            .shared
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        shared.ring.push(line.clone());
        if let Some(log) = shared.log.as_mut() {
            log.write_line(&line);
        }
        if let Some(startup) = shared.startup.as_mut() {
            let _ = startup.write_all(line.as_bytes());
        }
    }
    fn flush(&self) {
        let mut shared = self
            .shared
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(log) = shared.log.as_mut() {
            let _ = log.file.flush();
        }
        if let Some(startup) = shared.startup.as_mut() {
            let _ = startup.flush();
        }
    }
}

static LOGGER: YejianLogger = YejianLogger {
    shared: Mutex::new(Shared {
        ring: Ring(VecDeque::new()),
        log: None,
        startup: None,
    }),
};
static ROOT: RwLock<Option<PathBuf>> = RwLock::new(None);
static EXIT_NOTICE: Mutex<Option<ExitNotice>> = Mutex::new(None);
static INIT: OnceLock<()> = OnceLock::new();

/// Installs the logger and panic hook exactly once, on every platform.
pub fn init() {
    INIT.get_or_init(|| {
        #[cfg(target_os = "ios")]
        {
            // Preserve the original iOS startup log beside the rolling log.
            let file = std::env::var_os("HOME").and_then(|home| {
                let documents = PathBuf::from(home).join("Documents");
                fs::create_dir_all(&documents).ok()?;
                OpenOptions::new()
                    .create(true)
                    .write(true)
                    .truncate(true)
                    .open(documents.join("yejian-startup.log"))
                    .ok()
            });
            LOGGER
                .shared
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .startup = file;
        }
        if log::set_logger(&LOGGER).is_ok() {
            log::set_max_level(log::LevelFilter::Debug);
        }
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let thread = thread::current();
            let name = thread.name().unwrap_or("unnamed").to_string();
            let message = info
                .payload()
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .or_else(|| info.payload().downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "unknown panic".into());
            let location = info
                .location()
                .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
                .unwrap_or_else(|| "unknown".into());
            let backtrace = Backtrace::capture().to_string();
            log::error!("panic on {name}: {message} at {location}");
            write_crash(&name, &message, &location, &backtrace);
            log::logger().flush();
            previous(info);
        }));
    });
}

/// Points the logger at `<root>/logs`, evaluates the previous run's exit
/// marker, then marks this run as running. Called once the root is known.
pub fn attach_root(root: &Path) {
    init();
    let logs = root.join("logs");
    let _ = fs::create_dir_all(&logs);
    {
        let mut shared = LOGGER
            .shared
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if shared.log.is_none() {
            shared.log = Rolling::open(&logs.join(LOG_FILE)).ok();
        }
    }
    // Evaluate last run's marker once, before overwriting it — attach can
    // run again for the same process (run() then ReaderApp::new) and must
    // not read this run's fresh marker back as an abnormal exit.
    static EVALUATED: OnceLock<()> = OnceLock::new();
    if EVALUATED.get().is_none() {
        let notice = pending_exit_at(root);
        if let Ok(mut guard) = EXIT_NOTICE.lock() {
            *guard = notice;
        }
        let _ = EVALUATED.set(());
    }
    if let Ok(mut guard) = ROOT.write() {
        *guard = Some(root.to_path_buf());
    }
    mark_running_at(root);
    log::info!("{} starting; diagnostics attached", version_line());
}

fn app_root() -> Option<PathBuf> {
    ROOT.read().ok().and_then(|g| g.clone())
}

pub fn recent_lines() -> Vec<String> {
    LOGGER
        .shared
        .lock()
        .map(|s| s.ring.lines())
        .unwrap_or_default()
}

/// What the previous run left behind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExitNotice {
    /// Marker plus a fresh crash file: the app panicked last run.
    Crash,
    /// Marker without a crash file: the OS killed the app (usually memory).
    Oom,
}

/// Consumes the stored startup exit assessment.
pub fn take_exit_notice() -> Option<ExitNotice> {
    EXIT_NOTICE.lock().ok().and_then(|mut g| g.take())
}

/// Latest crash file, for the "导出日志" affordance on the crash notice.
fn crash_files(logs: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = fs::read_dir(logs)
        .map(|entries| {
            entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| {
                    p.file_name()
                        .and_then(|n| n.to_str())
                        .map(|n| n.starts_with(CRASH_PREFIX))
                        .unwrap_or(false)
                })
                .collect()
        })
        .unwrap_or_default();
    files.sort();
    files
}

/// Marker survived to this launch: classify by whether a crash file
/// written at or after the marker exists.
fn pending_exit_at(root: &Path) -> Option<ExitNotice> {
    let marker = root.join(MARKER);
    let marked = marker.metadata().and_then(|m| m.modified()).ok()?;
    let crashed = crash_files(&root.join("logs")).iter().any(|p| {
        p.metadata()
            .and_then(|m| m.modified())
            .map(|t| t + Duration::from_secs(1) >= marked)
            .unwrap_or(false)
    });
    Some(if crashed {
        ExitNotice::Crash
    } else {
        ExitNotice::Oom
    })
}

fn mark_running_at(root: &Path) {
    let _ = fs::write(root.join(MARKER), format!("{}\n", now_millis()));
}
fn clear_running_at(root: &Path) {
    let _ = fs::remove_file(root.join(MARKER));
}
/// Foregrounded or just started: the marker must exist.
pub fn mark_running() {
    if let Some(root) = app_root() {
        mark_running_at(&root);
    }
}
/// Clean exit or entering background — iOS may kill the process while
/// suspended, so the marker is cleared on the way out.
pub fn clear_running() {
    if let Some(root) = app_root() {
        clear_running_at(&root);
    }
}

/// Writes `logs/crash-<millis>.txt` with version/build, thread, panic
/// location, backtrace and the last [`RING_LINES`] log lines.
fn write_crash(name: &str, message: &str, location: &str, backtrace: &str) {
    let Some(root) = app_root() else {
        return;
    };
    let logs = root.join("logs");
    let _ = fs::create_dir_all(&logs);
    let path = logs.join(format!("{CRASH_PREFIX}{}.txt", now_millis()));
    let recent = recent_lines().join("");
    let body = format!(
        "{version}\nthread: {name}\npanic: {message}\nlocation: {location}\nbacktrace:\n{backtrace}\nrecent log:\n{recent}",
        version = version_line(),
    );
    let _ = fs::write(&path, body);
}

/// Keys whose values never leave the device inside summary.txt.
pub(crate) fn sensitive_key(key: &str) -> bool {
    let key = key.to_lowercase();
    ["token", "cookie", "secret", "password", "credential", "key", "auth"]
        .iter()
        .any(|needle| key.contains(needle))
}

/// Zips an allowlist — device.txt, the logs directory (logs + crash
/// files) and a caller-built summary.txt — into
/// `share/diagnostics-<millis>.zip`. No raw JSON state file is exported:
/// source-settings.json, device-identity.json and token stores stay on
/// the device.
pub fn export_diagnostics(root: &Path, summary: &str) -> io::Result<PathBuf> {
    init();
    let share = root.join("share");
    fs::create_dir_all(&share)?;
    let path = share.join(format!("diagnostics-{}.zip", now_millis()));
    let file = File::create(&path)?;
    let mut zip = zip::ZipWriter::new(file);
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    let device = format!(
        "{}\nplatform: {}\narch: {}\nfamily: {}\n",
        version_line(),
        std::env::consts::OS,
        std::env::consts::ARCH,
        std::env::consts::FAMILY,
    );
    zip.start_file("device.txt", options)
        .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;
    zip.write_all(device.as_bytes())?;
    if let Ok(entries) = fs::read_dir(root.join("logs")) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_file()
                && let Some(name) = p.file_name().and_then(|n| n.to_str())
            {
                let bytes = fs::read(&p)?;
                zip.start_file(format!("logs/{name}"), options)
                    .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;
                zip.write_all(&bytes)?;
            }
        }
    }
    zip.start_file("summary.txt", options)
        .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;
    zip.write_all(summary.as_bytes())?;
    zip.finish()
        .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;
    Ok(path)
}

/// Removes logs and crash files (the marker stays — the app is running).
pub fn clear_logs(root: &Path) -> io::Result<()> {
    let logs = root.join("logs");
    if logs.exists() {
        fs::remove_dir_all(&logs)?;
    }
    fs::create_dir_all(&logs)?;
    let mut shared = LOGGER
        .shared
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    shared.log = Rolling::open(&logs.join(LOG_FILE)).ok();
    Ok(())
}

/// How many panics inside [`GIVE_UP_WINDOW`] stop a restartable worker.
const GIVE_UP_PANICS: usize = 3;
/// Panics older than this no longer count towards giving up.
const GIVE_UP_WINDOW: Duration = Duration::from_secs(300);

/// Runs `f` on a named thread; a panic is caught, logged and written to a
/// crash file. Restartable workers respawn with exponential backoff
/// (200 ms doubling, capped at 30 s); three panics inside five minutes
/// stop the worker and report a fatal notice. Non-restartable workers
/// stop on the first panic and report a fatal notice.
pub fn spawn_supervised<F>(name: &str, module: &str, restartable: bool, f: F)
where
    F: Fn() + Send + 'static,
{
    init();
    let name = name.to_string();
    let module = module.to_string();
    let _ = thread::Builder::new()
        .name(name.clone())
        .spawn(move || {
            let mut failures: Vec<Instant> = Vec::new();
            let mut backoff = Duration::from_millis(200);
            loop {
                let outcome = std::panic::catch_unwind(AssertUnwindSafe(&f));
                match outcome {
                    Ok(()) => return,
                    Err(_) => {
                        log::error!("supervised thread {name} panicked; restartable={restartable}");
                        log::logger().flush();
                        if !restartable {
                            report_fatal(&module);
                            return;
                        }
                        failures.retain(|t| t.elapsed() < GIVE_UP_WINDOW);
                        failures.push(Instant::now());
                        if failures.len() >= GIVE_UP_PANICS {
                            report_stopped(&module);
                            return;
                        }
                        report_recovery(&module);
                        thread::sleep(backoff);
                        backoff = (backoff * 2).min(Duration::from_secs(30));
                    }
                }
            }
        });
}

/// UI-facing notices from supervised workers, drained once per frame.
static RECOVERIES: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// A restartable worker recovered from a panic.
pub fn report_recovery(module: &str) {
    if let Ok(mut guard) = RECOVERIES.lock() {
        guard.push(format!("{module}模块出错，已自动恢复"));
    }
}
/// A non-restartable worker died; only a restart brings it back.
pub fn report_fatal(module: &str) {
    if let Ok(mut guard) = RECOVERIES.lock() {
        guard.push(format!("{module}模块出错，请重启应用"));
    }
}
/// A restartable worker panicked too often and was stopped.
fn report_stopped(module: &str) {
    if let Ok(mut guard) = RECOVERIES.lock() {
        guard.push(format!("{module}模块多次出错，已停止；请重启应用"));
    }
}
pub fn take_recoveries() -> Vec<String> {
    RECOVERIES
        .lock()
        .map(|mut g| std::mem::take(&mut *g))
        .unwrap_or_default()
}

#[cfg(debug_assertions)]
pub mod test_hooks {
    //! Debug-only seams so acceptance can render crash/OOM notices and a
    //! recovered-worker toast without crashing the harness.
    use super::*;
    pub fn fake_exit_notice(root: &Path, crash: bool) {
        attach_root(root);
        if crash {
            let logs = root.join("logs");
            let _ = fs::create_dir_all(&logs);
            let _ = fs::write(logs.join(format!("{CRASH_PREFIX}1.txt")), "panic\n");
        }
        if let Ok(mut guard) = EXIT_NOTICE.lock() {
            *guard = Some(if crash {
                ExitNotice::Crash
            } else {
                ExitNotice::Oom
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_keeps_only_the_latest_lines() {
        let mut ring = Ring(VecDeque::new());
        for i in 0..RING_LINES + 10 {
            ring.push(format!("line {i}"));
        }
        let lines = ring.lines();
        assert_eq!(lines.len(), RING_LINES);
        assert_eq!(lines[0], "line 10");
        assert_eq!(
            lines.last().unwrap(),
            &format!("line {}", RING_LINES + 9)
        );
    }

    #[test]
    fn rolling_log_rotates_and_keeps_two_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(LOG_FILE);
        let mut log = Rolling::open(&path).unwrap();
        let line = "x".repeat(1024) + "\n";
        for _ in 0..(LOG_MAX_BYTES / 1024 + 4) {
            log.write_line(&line);
        }
        assert!(dir.path().join(format!("{LOG_FILE}.1")).exists());
        drop(log);
        // Second rotation still leaves exactly two files.
        let mut log = Rolling::open(&path).unwrap();
        for _ in 0..(LOG_MAX_BYTES / 1024 + 4) {
            log.write_line(&line);
        }
        assert!(!dir.path().join(format!("{LOG_FILE}.2")).exists());
    }

    #[test]
    fn crash_file_carries_version_thread_and_backtrace() {
        let dir = tempfile::tempdir().unwrap();
        *ROOT.write().unwrap() = Some(dir.path().to_path_buf());
        write_crash("yejian-test", "boom", "src/x.rs:1:1", "fake-backtrace");
        let crashes = crash_files(&dir.path().join("logs"));
        assert_eq!(crashes.len(), 1);
        let body = fs::read_to_string(&crashes[0]).unwrap();
        assert!(body.contains(&version_line()));
        assert!(body.contains("thread: yejian-test"));
        assert!(body.contains("panic: boom"));
        assert!(body.contains("location: src/x.rs:1:1"));
        assert!(body.contains("fake-backtrace"));
        *ROOT.write().unwrap() = None;
    }

    #[test]
    fn exit_marker_distinguishes_crash_from_oom() {
        let dir = tempfile::tempdir().unwrap();
        assert!(pending_exit_at(dir.path()).is_none());
        // Marker without a crash file -> killed while suspended.
        mark_running_at(dir.path());
        assert_eq!(pending_exit_at(dir.path()), Some(ExitNotice::Oom));
        // Crash file written after the marker -> crash notice.
        let logs = dir.path().join("logs");
        fs::create_dir_all(&logs).unwrap();
        fs::write(logs.join(format!("{CRASH_PREFIX}1.txt")), "panic").unwrap();
        assert_eq!(pending_exit_at(dir.path()), Some(ExitNotice::Crash));
        // Clean exit clears the marker -> nothing to report.
        clear_running_at(dir.path());
        assert!(pending_exit_at(dir.path()).is_none());
    }

    #[test]
    fn diagnostics_zip_keeps_secrets_on_device() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("logs")).unwrap();
        fs::write(root.join("logs").join(LOG_FILE), "log line\n").unwrap();
        // Sentinel payloads: TLS private key material and source logins
        // must never appear anywhere inside the exported archive.
        fs::write(
            root.join("device-identity.json"),
            r#"{"key":"TLS_PRIVATE_SENTINEL_7F3"}"#,
        )
        .unwrap();
        fs::write(
            root.join("source-settings.json"),
            r#"{"login":"SOURCE_LOGIN_SENTINEL_9A2"}"#,
        )
        .unwrap();
        for name in [
            "trackers.json",
            "webdav.json",
            "web-cookies.json",
            "oauth-token.json",
            "settings.json",
        ] {
            fs::write(root.join(name), "secret").unwrap();
        }
        let path = export_diagnostics(root, "books: 3\n").unwrap();
        let bytes = fs::read(&path).unwrap();
        let mut zip = zip::ZipArchive::new(File::open(&path).unwrap()).unwrap();
        let names: Vec<String> = (0..zip.len())
            .map(|i| zip.by_index(i).unwrap().name().to_string())
            .collect();
        assert!(names.iter().any(|n| n == "device.txt"));
        assert!(names.iter().any(|n| n == "summary.txt"));
        assert!(names.iter().any(|n| n == &format!("logs/{LOG_FILE}")));
        // Allowlist: no raw state JSON file is ever archived.
        assert!(
            !names.iter().any(|n| n.ends_with(".json")),
            "state file leaked: {names:?}"
        );
        for sentinel in [
            "TLS_PRIVATE_SENTINEL_7F3",
            "SOURCE_LOGIN_SENTINEL_9A2",
            "trackers.json",
            "webdav.json",
            "web-cookies.json",
        ] {
            assert!(
                !bytes.windows(sentinel.len()).any(|w| w == sentinel.as_bytes()),
                "{sentinel} appears in the archive"
            );
        }
    }

    #[test]
    fn supervised_thread_restarts_and_reports() {
        let runs = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        {
            let runs = runs.clone();
            spawn_supervised("yejian-test-supervised", "测试", true, move || {
                if runs.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                    panic!("first run dies");
                }
            });
        }
        // Second run exits cleanly; total invocations = 2.
        for _ in 0..50 {
            if runs.load(std::sync::atomic::Ordering::SeqCst) >= 2 {
                return;
            }
            thread::sleep(Duration::from_millis(20));
        }
        panic!("supervised thread did not restart");
    }

    #[test]
    fn supervised_thread_gives_up_after_repeated_panics() {
        let runs = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let runs2 = runs.clone();
        spawn_supervised("yejian-test-giveup", "测试模块", true, move || {
            runs2.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            panic!("always dies");
        });
        // 200 ms + 400 ms backoffs, then the third panic stops it.
        thread::sleep(Duration::from_secs(3));
        assert_eq!(
            runs.load(std::sync::atomic::Ordering::SeqCst),
            GIVE_UP_PANICS
        );
        let notices = take_recoveries();
        assert!(
            notices
                .iter()
                .any(|n| n.contains("测试模块模块多次出错，已停止")
                    || n.contains("测试模块多次出错，已停止")),
            "no give-up notice: {notices:?}"
        );
        thread::sleep(Duration::from_secs(1));
        assert_eq!(runs.load(std::sync::atomic::Ordering::SeqCst), GIVE_UP_PANICS);
    }
}
