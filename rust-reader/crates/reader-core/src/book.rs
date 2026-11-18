use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// One book in the visible library.
///
/// `current_page` is zero-based. `last_read_at` is Unix milliseconds and stays
/// `None` until the first non-private progress save.
///
/// `page_fraction` is the reading position inside `current_page`: the top edge
/// of the viewport relative to the full image height, clamped to `0.0..=1.0`.
/// It makes tall, continuously scrolled pages reopen where the reader left
/// off. Values written by readers that predate this field decode as `0.0`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Book {
    pub id: String,
    pub title: String,
    pub author: String,
    pub archive_path: PathBuf,
    pub cover_path: Option<PathBuf>,
    pub page_count: usize,
    pub current_page: usize,
    #[serde(default)]
    pub page_fraction: f32,
    pub updated_at: i64,
    pub last_read_at: Option<i64>,
    pub category: String,
}

/// Repairs a position fraction coming from storage or another device.
///
/// A stored value is always finite and inside `0.0..=1.0`; invalid input is
/// mapped to the top of the page instead of poisoning later comparisons.
pub(crate) fn sanitize_fraction(value: f32) -> f32 {
    if value.is_finite() {
        value.clamp(0.0, 1.0)
    } else {
        0.0
    }
}
