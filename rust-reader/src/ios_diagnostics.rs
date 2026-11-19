//! Persist framework startup errors before eframe can terminate the process.
use std::{
    fs::{File, OpenOptions},
    io::Write,
    path::PathBuf,
    sync::{Mutex, OnceLock},
};

struct IosLogger {
    file: Mutex<Option<File>>,
}

impl log::Log for IosLogger {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        metadata.level() <= log::Level::Warn
            || (metadata.level() <= log::Level::Debug
                && ["yejian", "eframe", "egui_wgpu", "winit"]
                    .iter()
                    .any(|prefix| metadata.target().starts_with(prefix)))
    }

    fn log(&self, record: &log::Record<'_>) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let line = format!("{} {}: {}\n", record.level(), record.target(), record.args());
        let _ = std::io::stderr().write_all(line.as_bytes());
        if let Ok(mut guard) = self.file.lock()
            && let Some(file) = guard.as_mut()
        {
            let _ = file.write_all(line.as_bytes());
        }
    }

    fn flush(&self) {
        if let Ok(mut guard) = self.file.lock()
            && let Some(file) = guard.as_mut()
        {
            let _ = file.flush();
        }
    }
}

pub(crate) fn init() {
    static LOGGER: OnceLock<IosLogger> = OnceLock::new();
    let logger = LOGGER.get_or_init(|| {
        let file = std::env::var_os("HOME").and_then(|home| {
            let documents = PathBuf::from(home).join("Documents");
            std::fs::create_dir_all(&documents).ok()?;
            OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(documents.join("yejian-startup.log"))
                .ok()
        });
        IosLogger { file: Mutex::new(file) }
    });
    if log::set_logger(logger).is_ok() {
        log::set_max_level(log::LevelFilter::Debug);
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            log::error!("panic: {info}");
            log::logger().flush();
            previous(info);
        }));
    }
}
