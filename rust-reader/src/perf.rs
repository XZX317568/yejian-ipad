//! Cheap performance counters shared by the worker threads and the UI.
//!
//! Rings keep the last few hundred samples; every record is a lock-free
//! atomic store or a short mutex push, so recording is safe on hot paths
//! (image decode, page turns, per-frame painting).

use std::collections::VecDeque;
use std::sync::Mutex;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Instant;

static T0: OnceLock<Instant> = OnceLock::new();

/// Called once at process start; later calls are ignored.
pub fn init() {
    T0.get_or_init(Instant::now);
}

/// Milliseconds since [`init`].
fn elapsed_ms() -> u64 {
    T0.get().map(|t| t.elapsed().as_millis() as u64).unwrap_or(0)
}

/// Startup milestones, recorded once.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    /// Platform/app initialization began (ReaderApp::new).
    PlatformInit = 0,
    /// First UI frame ran (renderer/context usable).
    RendererReady = 1,
    /// `Event::Ready` arrived — library + settings are loaded.
    LibraryOpen = 2,
    /// First shelf frame painted.
    FirstShelfFrame = 3,
    /// All covers the shelf asked for have been decoded.
    CoversReady = 4,
}

const STAGES: usize = 5;
static MARKS: [AtomicU64; STAGES] = [
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
];
static MARKED: [AtomicBool; STAGES] = [
    AtomicBool::new(false),
    AtomicBool::new(false),
    AtomicBool::new(false),
    AtomicBool::new(false),
    AtomicBool::new(false),
];

/// Records a startup stage once; later calls for the same stage are no-ops.
pub fn mark(stage: Stage) {
    let i = stage as usize;
    if !MARKED[i].swap(true, Ordering::Relaxed) {
        MARKS[i].store(elapsed_ms(), Ordering::Relaxed);
    }
}

const RING: usize = 256;

static DECODE_MS: Mutex<VecDeque<f64>> = Mutex::new(VecDeque::new());
static QUEUE_MS: Mutex<VecDeque<f64>> = Mutex::new(VecDeque::new());
static FRAME_MS: Mutex<VecDeque<f64>> = Mutex::new(VecDeque::new());

fn push(slot: &Mutex<VecDeque<f64>>, ms: f64) {
    let mut ring = slot.lock().unwrap_or_else(|e| e.into_inner());
    if ring.len() >= RING {
        ring.pop_front();
    }
    ring.push_back(ms);
}

/// Per-page image decode duration, measured in the decode worker.
pub fn record_decode(ms: f64) {
    push(&DECODE_MS, ms);
}

/// Time an image request spent queued before a worker picked it up.
pub fn record_queue(ms: f64) {
    push(&QUEUE_MS, ms);
}

/// Frame-to-frame wall time while the reader is open.
pub fn record_frame(ms: f64) {
    push(&FRAME_MS, ms);
}

static PAGE_BYTES: AtomicU64 = AtomicU64::new(0);
static COVER_BYTES: AtomicU64 = AtomicU64::new(0);
static PEAK_BYTES: AtomicU64 = AtomicU64::new(0);

/// Total resident page-texture bytes after each cache update.
pub fn set_page_bytes(bytes: u64) {
    PAGE_BYTES.store(bytes, Ordering::Relaxed);
    peak();
}

/// Total resident cover-texture bytes after each cache update.
pub fn set_cover_bytes(bytes: u64) {
    COVER_BYTES.store(bytes, Ordering::Relaxed);
    peak();
}

fn peak() {
    let total = PAGE_BYTES.load(Ordering::Relaxed) + COVER_BYTES.load(Ordering::Relaxed);
    PEAK_BYTES.fetch_max(total, Ordering::Relaxed);
}

/// Clears all rings and peaks — used by the bench between iterations.
#[cfg(any(test, debug_assertions))]
pub fn reset() {
    DECODE_MS.lock().unwrap_or_else(|e| e.into_inner()).clear();
    QUEUE_MS.lock().unwrap_or_else(|e| e.into_inner()).clear();
    FRAME_MS.lock().unwrap_or_else(|e| e.into_inner()).clear();
    PEAK_BYTES.store(0, Ordering::Relaxed);
    BLANK_FRAME.store(false, Ordering::Relaxed);
}

/// A visible page slot was painted without a texture during this frame;
/// the bench samples and clears the flag after each rendered frame.
static BLANK_FRAME: AtomicBool = AtomicBool::new(false);

pub fn note_blank_frame() {
    BLANK_FRAME.store(true, Ordering::Relaxed);
}

/// Returns and clears the "this frame had an untextured page" flag.
/// Used by the bench and tests only.
pub fn take_blank_frame() -> bool {
    BLANK_FRAME.swap(false, Ordering::Relaxed)
}

fn percentile(slot: &Mutex<VecDeque<f64>>, p: f64) -> Option<f64> {
    let ring = slot.lock().unwrap_or_else(|e| e.into_inner());
    if ring.is_empty() {
        return None;
    }
    let mut values: Vec<f64> = ring.iter().copied().collect();
    values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let index = ((values.len() - 1) as f64 * p) as usize;
    Some(values[index])
}

/// Point-in-time copy of all counters, for the overlay and the bench.
pub struct Snapshot {
    pub decode_p50_ms: Option<f64>,
    pub decode_p95_ms: Option<f64>,
    pub queue_p95_ms: Option<f64>,
    pub frame_p95_ms: Option<f64>,
    pub page_bytes: u64,
    pub cover_bytes: u64,
    pub peak_bytes: u64,
    pub marks_ms: [Option<u64>; STAGES],
}

pub fn snapshot() -> Snapshot {
    let mut marks = [None; STAGES];
    for (i, mark) in marks.iter_mut().enumerate() {
        if MARKED[i].load(Ordering::Relaxed) {
            *mark = Some(MARKS[i].load(Ordering::Relaxed));
        }
    }
    Snapshot {
        decode_p50_ms: percentile(&DECODE_MS, 0.5),
        decode_p95_ms: percentile(&DECODE_MS, 0.95),
        queue_p95_ms: percentile(&QUEUE_MS, 0.95),
        frame_p95_ms: percentile(&FRAME_MS, 0.95),
        page_bytes: PAGE_BYTES.load(Ordering::Relaxed),
        cover_bytes: COVER_BYTES.load(Ordering::Relaxed),
        peak_bytes: PEAK_BYTES.load(Ordering::Relaxed),
        marks_ms: marks,
    }
}

fn fmt_ms(value: Option<f64>) -> String {
    value.map_or("-".into(), |v| format!("{v:.0}ms"))
}

fn fmt_mb(bytes: u64) -> String {
    format!("{:.0}MB", bytes as f64 / 1048576.)
}

/// Multi-line text for the settings overlay.
pub fn overlay_text() -> String {
    let s = snapshot();
    let stage = |stage: Stage| s.marks_ms[stage as usize].map_or("-".into(), |v| format!("{v}ms"));
    format!(
        "解码 p50 {} / p95 {}\n排队 p95 {}\n帧 p95 {}\n页面纹理 {} · 封面 {} · 峰值 {}\n启动: 平台 {} 渲染 {} 书库 {} 书架 {} 封面 {}",
        fmt_ms(s.decode_p50_ms),
        fmt_ms(s.decode_p95_ms),
        fmt_ms(s.queue_p95_ms),
        fmt_ms(s.frame_p95_ms),
        fmt_mb(s.page_bytes),
        fmt_mb(s.cover_bytes),
        fmt_mb(s.peak_bytes),
        stage(Stage::PlatformInit),
        stage(Stage::RendererReady),
        stage(Stage::LibraryOpen),
        stage(Stage::FirstShelfFrame),
        stage(Stage::CoversReady),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentile_of_sorted_samples() {
        let ring = Mutex::new((1..=20).map(|v| v as f64).collect::<VecDeque<_>>());
        assert_eq!(percentile(&ring, 0.5), Some(10.));
        assert_eq!(percentile(&ring, 0.95), Some(19.));
        assert_eq!(percentile(&ring, 1.0), Some(20.));
    }

    #[test]
    fn blank_flag_is_sticky_until_taken() {
        assert!(!take_blank_frame());
        note_blank_frame();
        note_blank_frame();
        assert!(take_blank_frame());
        assert!(!take_blank_frame());
    }

    #[test]
    fn marks_fire_once() {
        let stage = Stage::PlatformInit;
        mark(stage);
        let first = MARKS[stage as usize].load(Ordering::Relaxed);
        std::thread::sleep(std::time::Duration::from_millis(2));
        mark(stage);
        assert_eq!(first, MARKS[stage as usize].load(Ordering::Relaxed));
    }
}
