//! Reading statistics persisted atomically at `<root>/reading-stats.json`.
//!
//! Each local day (`YYYY-MM-DD`) keeps one record per device id so a later
//! cloud sync can merge devices by taking the larger values. Time counts
//! only while the reader is open, the app is in the foreground, the app
//! lock is off, the session is not private, and the last input or page
//! change was within [`IDLE_GRACE`]. Seconds that cross midnight are split
//! onto the day they were spent on. Nothing is recorded in private mode.
//!
//! The device id reuses `device-identity.json`'s certificate fingerprint
//! when present so sync and stats agree on "this device"; a fresh random id
//! is persisted inside the stats file otherwise.

use chrono::{Datelike, TimeZone};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const FILE: &str = "reading-stats.json";
/// Seconds without input after which reading time stops accruing.
pub const IDLE_GRACE: f64 = 120.;
/// Minimum interval between automatic disk flushes.
pub const FLUSH_EVERY: f64 = 60.;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DayStats {
    #[serde(default)]
    pub seconds: u64,
    #[serde(default)]
    pub pages: u64,
    #[serde(default)]
    pub chapters: u64,
}

#[derive(Debug, Serialize, Deserialize)]
struct StatsFile {
    /// This device's id; other keys under `days` belong to other devices.
    device: String,
    #[serde(default)]
    days: BTreeMap<String, BTreeMap<String, DayStats>>,
}

#[derive(Debug)]
pub struct Stats {
    device: String,
    days: BTreeMap<String, BTreeMap<String, DayStats>>,
    open: bool,
    private: bool,
    foreground: bool,
    locked: bool,
    /// Last input/page change, epoch seconds.
    last_activity: Option<f64>,
    /// Tick boundary; settled seconds live in `days`, pending ones here.
    last_tick: Option<f64>,
    dirty: bool,
    last_flush: f64,
}

impl Stats {
    pub fn load(root: &Path) -> Self {
        let path = path(root);
        let stored: Option<StatsFile> = std::fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok());
        let device = stored
            .as_ref()
            .map(|file| file.device.clone())
            .filter(|device| !device.is_empty())
            .unwrap_or_else(|| device_id(root));
        Self {
            device,
            days: stored.map(|file| file.days).unwrap_or_default(),
            open: false,
            private: false,
            foreground: true,
            locked: false,
            last_activity: None,
            last_tick: None,
            dirty: false,
            last_flush: 0.,
        }
    }

    pub fn save(&self, root: &Path) -> anyhow::Result<()> {
        let file = StatsFile {
            device: self.device.clone(),
            days: self.days.clone(),
        };
        std::fs::create_dir_all(root)?;
        let tmp = root.join(format!("{FILE}.tmp"));
        std::fs::write(&tmp, serde_json::to_vec(&file)?)?;
        std::fs::rename(&tmp, path(root)).map_err(|error| {
            let _ = std::fs::remove_file(&tmp);
            anyhow::anyhow!("写入 {} 失败：{error}", path(root).display())
        })
    }

    /// This device's stable id — the key cloud sync merges stats under.
    #[allow(dead_code)] // used by the WebDAV merge in the next round
    pub fn device_id(&self) -> &str {
        &self.device
    }

    /// Whether any time/page/chapter can be recorded right now.
    fn counting(&self) -> bool {
        self.open && self.foreground && !self.locked && !self.private
    }

    /// Settles elapsed time since `last_tick` (split at midnight), then is
    /// called by every state setter so a span is never credited to a state
    /// that did not earn it.
    fn settle(&mut self, now: f64) {
        let Some(from) = self.last_tick else {
            self.last_tick = Some(now);
            return;
        };
        let from = from;
        self.last_tick = Some(now);
        if now <= from {
            return;
        }
        // Time accrues only up to IDLE_GRACE after the last activity; an
        // idle reader simply stops counting mid-span.
        let active_until = match self.last_activity {
            Some(activity) if self.counting() => now.min(activity + IDLE_GRACE),
            _ => from.min(now),
        };
        if active_until <= from {
            return;
        }
        // Split the accrual at each midnight boundary so seconds land on the
        // day they were actually spent.
        let mut end = active_until;
        while end > from {
            // `end` is an exclusive bound: an end exactly at midnight counts
            // toward the day that just started, never the previous one.
            let day_start = day_start_of(end - 0.5);
            let slice_from = from.max(day_start);
            let seconds = (end - slice_from).ceil() as u64;
            if seconds > 0 {
                let day = day_key(slice_from);
                let entry = self
                    .days
                    .entry(day)
                    .or_default()
                    .entry(self.device.clone())
                    .or_default();
                entry.seconds += seconds;
                self.dirty = true;
            }
            if slice_from <= from {
                break;
            }
            end = slice_from;
        }
    }

    /// Acceptance seed: a deterministic last-seven-days pattern so the
    /// 阅读统计 card and chart render real data.
    #[cfg(debug_assertions)]
    pub fn seed_capture(&mut self, now: f64) {
        let today = local_date(now);
        for offset in 0..7i64 {
            let date = today - chrono::Duration::days(offset);
            let (seconds, pages, chapters) = match offset {
                0 => (1500, 18, 1),
                1 => (2400, 40, 1),
                2 => (900, 12, 0),
                3 => (3600, 55, 2),
                5 => (1800, 20, 0),
                _ => (0, 0, 0),
            };
            if seconds == 0 {
                continue;
            }
            let chapters = if date.month() == today.month() {
                chapters
            } else {
                0
            };
            let entry = self
                .days
                .entry(date.format("%Y-%m-%d").to_string())
                .or_default()
                .entry(self.device.clone())
                .or_default();
            entry.seconds = seconds;
            entry.pages = pages;
            entry.chapters = chapters;
        }
    }

    /// Called once per frame with the epoch second.
    pub fn tick(&mut self, now: f64) {
        self.settle(now);
    }

    /// User input or a page change: (re)starts the idle window.
    pub fn activity(&mut self, now: f64) {
        self.settle(now);
        if self.counting() {
            self.last_activity = Some(now);
        }
    }

    pub fn page_turned(&mut self, now: f64) {
        if !self.counting() {
            return;
        }
        self.activity(now);
        let entry = self
            .days
            .entry(day_key(now))
            .or_default()
            .entry(self.device.clone())
            .or_default();
        entry.pages += 1;
        self.dirty = true;
    }

    pub fn chapter_finished(&mut self, now: f64) {
        if !self.counting() {
            return;
        }
        self.activity(now);
        let entry = self
            .days
            .entry(day_key(now))
            .or_default()
            .entry(self.device.clone())
            .or_default();
        entry.chapters += 1;
        self.dirty = true;
    }

    pub fn set_reader_open(&mut self, open: bool, now: f64) {
        if self.open != open {
            self.settle(now);
            self.open = open;
            if !open {
                self.last_activity = None;
            }
        }
    }

    pub fn set_private(&mut self, private: bool, now: f64) {
        if self.private != private {
            self.settle(now);
            self.private = private;
        }
    }

    pub fn set_foreground(&mut self, foreground: bool, now: f64) {
        if self.foreground != foreground {
            self.settle(now);
            self.foreground = foreground;
        }
    }

    pub fn set_locked(&mut self, locked: bool, now: f64) {
        if self.locked != locked {
            self.settle(now);
            self.locked = locked;
        }
    }

    /// Periodic flush: writes at most once per [`FLUSH_EVERY`] while dirty.
    /// Returns true when a write happened.
    pub fn flush_due(&mut self, now: f64, root: &Path) -> bool {
        if !self.dirty || now - self.last_flush < FLUSH_EVERY {
            return false;
        }
        self.last_flush = now;
        if self.save(root).is_ok() {
            self.dirty = false;
            true
        } else {
            false
        }
    }

    /// Forced flush on reader close / background / exit.
    pub fn flush(&mut self, root: &Path) {
        if self.dirty && self.save(root).is_ok() {
            self.dirty = false;
        }
    }

    // -- queries for the stats card ---------------------------------------

    fn record(&self, day: &str) -> DayStats {
        self.days
            .get(day)
            .and_then(|devices| devices.get(&self.device))
            .copied()
            .unwrap_or_default()
    }

    /// Seconds read on the local day containing `now`.
    pub fn today_seconds(&self, now: f64) -> u64 {
        self.record(&day_key(now)).seconds
    }

    /// Seconds read this ISO week (Mon–Sun) containing `now`.
    pub fn week_seconds(&self, now: f64) -> u64 {
        self.days_in_week(now)
            .iter()
            .map(|day| self.record(day).seconds)
            .sum()
    }

    /// Days of the ISO week containing `now`, Monday first.
    fn days_in_week(&self, now: f64) -> Vec<String> {
        let today = local_date(now);
        let monday = today - chrono::Duration::days(today.weekday().num_days_from_monday() as i64);
        (0..7)
            .map(|offset| (monday + chrono::Duration::days(offset)).format("%Y-%m-%d").to_string())
            .collect()
    }

    /// Consecutive days with reading time ending today (or yesterday when
    /// today has none yet).
    pub fn streak(&self, now: f64) -> u64 {
        let mut day = local_date(now);
        if self.record(&day.format("%Y-%m-%d").to_string()).seconds == 0 {
            day -= chrono::Duration::days(1);
        }
        let mut count = 0;
        loop {
            let key = day.format("%Y-%m-%d").to_string();
            if self.record(&key).seconds == 0 {
                return count;
            }
            count += 1;
            day -= chrono::Duration::days(1);
        }
    }

    /// Chapters finished in the calendar month containing `now`.
    pub fn month_chapters(&self, now: f64) -> u64 {
        let prefix = local_date(now).format("%Y-%m").to_string();
        self.days
            .range(prefix.clone()..)
            .take_while(|(day, _)| day.starts_with(&prefix))
            .map(|(_, devices)| devices.get(&self.device).copied().unwrap_or_default().chapters)
            .sum()
    }

    /// This device's per-day seconds for the last seven local days,
    /// oldest first.
    pub fn last_seven(&self, now: f64) -> Vec<(String, u64)> {
        let today = local_date(now);
        (0..7)
            .rev()
            .map(|offset| {
                let day = today - chrono::Duration::days(offset);
                let key = day.format("%Y-%m-%d").to_string();
                (key.clone(), self.record(&key).seconds)
            })
            .collect()
    }
}

fn path(root: &Path) -> PathBuf {
    root.join(FILE)
}

/// Local calendar date for an epoch-second timestamp.
fn local_date(secs: f64) -> chrono::NaiveDate {
    chrono::DateTime::from_timestamp(secs as i64, 0)
        .map(|stamp| stamp.with_timezone(&chrono::Local).date_naive())
        .unwrap_or_else(|| chrono::NaiveDate::from_ymd_opt(1970, 1, 1).unwrap())
}

/// `YYYY-MM-DD` key for an epoch-second timestamp.
fn day_key(secs: f64) -> String {
    local_date(secs).format("%Y-%m-%d").to_string()
}

/// Epoch seconds of the local midnight starting `secs`' day.
fn day_start_of(secs: f64) -> f64 {
    let date = local_date(secs);
    date.and_hms_opt(0, 0, 0)
        .and_then(|midnight| {
            chrono::Local
                .from_local_datetime(&midnight)
                .single()
                .map(|stamp| stamp.timestamp() as f64)
        })
        .unwrap_or(secs - secs.rem_euclid(86400.))
}

/// This device's stable id: the sync identity fingerprint when present, a
/// fresh random token otherwise (persisted inside the stats file).
fn device_id(root: &Path) -> String {
    if let Ok(bytes) = std::fs::read(root.join("device-identity.json"))
        && let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes)
        && let Some(certificate) = value.get("certificate").and_then(|c| c.as_str())
    {
        return format!("sync-{:016x}", fnv(certificate.as_bytes()));
    }
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|span| span.as_nanos())
        .unwrap_or(0);
    format!("dev-{:016x}", fnv(&nanos.to_le_bytes()) ^ (std::process::id() as u64))
}

fn fnv(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in bytes {
        hash = (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stats() -> Stats {
        let mut stats = Stats::load(Path::new("/nonexistent-stats-dir"));
        stats.device = "test-device".into();
        stats
    }

    /// Noon UTC so `YYYY-MM-DD` matches in every plausible CI timezone.
    fn noon(day_offset: i64) -> f64 {
        // 2026-01-14 is a Wednesday: `noon(-1)` stays inside the same ISO
        // week and month in every timezone, which week_seconds() relies on.
        let base = chrono::NaiveDate::from_ymd_opt(2026, 1, 14)
            .unwrap()
            .and_hms_opt(12, 0, 0)
            .unwrap();
        let stamp = chrono::Local
            .from_local_datetime(&base)
            .single()
            .unwrap()
            .timestamp() as f64;
        // 2026-01-12 is a Monday; a same-week base keeps week_seconds() in scope.
        debug_assert_eq!(
            chrono::DateTime::from_timestamp(stamp as i64, 0)
                .unwrap()
                .with_timezone(&chrono::Local)
                .date_naive()
                .iso_week()
                .week(),
            chrono::DateTime::from_timestamp(stamp as i64 - 86400, 0)
                .unwrap()
                .with_timezone(&chrono::Local)
                .date_naive()
                .iso_week()
                .week()
        );
        stamp + (day_offset * 86400) as f64
    }

    #[test]
    fn counts_only_while_active() {
        let mut stats = stats();
        stats.set_reader_open(true, 0.);
        stats.activity(1.);
        stats.tick(60.);
        assert_eq!(stats.today_seconds(60.), 59);
        // Idle for over two minutes: the span beyond IDLE_GRACE is dropped.
        stats.tick(60. + 500.);
        assert_eq!(stats.today_seconds(60. + 500.), 120);
        // Fresh activity resumes counting.
        stats.activity(700.);
        stats.tick(760.);
        assert_eq!(stats.today_seconds(760.), 180);
    }

    #[test]
    fn private_and_locked_and_background_record_nothing() {
        let mut stats = stats();
        stats.set_reader_open(true, 0.);
        stats.activity(1.);
        stats.tick(30.);
        let counted = stats.today_seconds(30.);
        assert!(counted > 0);
        stats.set_private(true, 30.);
        stats.tick(90.);
        assert_eq!(stats.today_seconds(90.), counted);
        stats.set_private(false, 90.);
        stats.set_locked(true, 90.);
        stats.tick(150.);
        assert_eq!(stats.today_seconds(150.), counted);
        stats.set_locked(false, 150.);
        stats.set_foreground(false, 150.);
        stats.tick(210.);
        assert_eq!(stats.today_seconds(210.), counted);
        // Closing the reader stops the clock; the earlier activity within
        // the grace window still counts the foreground part only.
        stats.set_foreground(true, 210.);
        stats.set_reader_open(false, 210.);
        stats.tick(300.);
        assert_eq!(stats.today_seconds(300.), counted);
    }

    #[test]
    fn splits_at_midnight() {
        let mut stats = stats();
        let day = local_date(noon(0));
        let midnight = day
            .succ_opt()
            .unwrap()
            .and_hms_opt(0, 0, 0)
            .unwrap();
        let boundary = chrono::Local
            .from_local_datetime(&midnight)
            .single()
            .unwrap()
            .timestamp() as f64;
        stats.set_reader_open(true, boundary - 100.);
        stats.activity(boundary - 90.);
        stats.tick(boundary + 30.);
        let today = day_key(boundary - 60.);
        let tomorrow = day_key(boundary + 60.);
        assert_eq!(stats.record(&today).seconds, 90);
        assert_eq!(stats.record(&tomorrow).seconds, 30);
    }

    #[test]
    fn pages_chapters_streak_week_and_month() {
        let mut stats = stats();
        // Yesterday: seed settled seconds so the streak spans two days.
        stats
            .days
            .entry(day_key(noon(-1)))
            .or_default()
            .entry("test-device".into())
            .or_default()
            .seconds = 30;
        stats.set_reader_open(true, noon(0) - 10.);
        stats.activity(noon(0));
        stats.tick(noon(0) + 60.);
        stats.page_turned(noon(0) + 60.);
        stats.chapter_finished(noon(0) + 61.);
        assert_eq!(stats.streak(noon(0) + 120.), 2);
        assert_eq!(stats.month_chapters(noon(0) + 120.), 1);
        let week = stats.week_seconds(noon(0) + 120.);
        let day_secs = stats.today_seconds(noon(0) + 120.)
            + stats.record(&day_key(noon(-1))).seconds;
        assert_eq!(week, day_secs);
        let seven = stats.last_seven(noon(0));
        assert_eq!(seven.len(), 7);
        assert_eq!(seven[6].0, day_key(noon(0)));
        assert!(seven[6].1 > 0);
    }

    #[test]
    fn saves_atomically_and_keeps_other_devices() {
        let dir = tempfile::tempdir().unwrap();
        let mut stats = stats();
        stats.set_reader_open(true, noon(0));
        stats.activity(noon(0));
        stats.tick(noon(0) + 10.);
        stats.save(dir.path()).unwrap();
        // A second device's record survives a load/save round trip.
        let mut loaded = Stats::load(dir.path());
        loaded
            .days
            .entry(day_key(noon(0)))
            .or_default()
            .insert("other-device".into(), DayStats {
                seconds: 5,
                pages: 0,
                chapters: 0,
            });
        loaded.dirty = true;
        loaded.flush(dir.path());
        let reloaded = Stats::load(dir.path());
        assert_eq!(
            reloaded.record(&day_key(noon(0))).seconds,
            stats.record(&day_key(noon(0))).seconds
        );
        assert_eq!(
            reloaded
                .days
                .get(&day_key(noon(0)))
                .unwrap()
                .get("other-device")
                .unwrap()
                .seconds,
            5
        );
        assert!(!dir.path().join("reading-stats.json.tmp").exists());
    }
}
