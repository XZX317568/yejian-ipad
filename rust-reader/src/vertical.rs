//! A page-relative scroll anchor for variable-height comic pages.
//!
//! The anchor is independent of viewport pixels, so rotating or resizing does
//! not return a long page to its top. Only nearby pages need to be decoded.

#[derive(Default, Debug)]
pub struct VerticalReader {
    pub page: usize,
    pub fraction: f32,
    pending_pixels: f32,
    velocity: f32,
    last_tick: Option<f64>,
}

impl VerticalReader {
    pub fn reset(&mut self, page: usize, fraction: f32) {
        *self = Self {
            page,
            fraction: if fraction.is_finite() {
                fraction.clamp(0., 1.)
            } else {
                0.
            },
            ..Self::default()
        };
    }

    pub fn scroll(&mut self, pixels: f32) {
        if pixels.is_finite() {
            self.pending_pixels += pixels;
        }
    }

    pub fn stop(&mut self) {
        self.velocity = 0.;
        self.last_tick = None;
    }

    pub fn release(&mut self, velocity: f32, now: f64, reduced: bool) {
        self.velocity = if reduced || !velocity.is_finite() {
            0.
        } else {
            velocity.clamp(-6000., 6000.)
        };
        self.last_tick = Some(now);
    }

    pub fn tick(&mut self, now: f64) -> bool {
        if self.velocity.abs() < 8. {
            self.stop();
            return false;
        }
        let dt = (now - self.last_tick.replace(now).unwrap_or(now)).clamp(0., 0.050) as f32;
        let decay = (-6. * dt).exp();
        self.scroll(self.velocity * (1. - decay) / 6.);
        self.velocity *= decay;
        true
    }

    pub fn active(&self) -> bool {
        self.velocity.abs() >= 8.
    }

    /// Carries excess pixels over a seam without resetting to the next page.
    /// If a page is not loaded yet, keeps the unconsumed distance until it is.
    pub fn settle(
        &mut self,
        count: usize,
        viewport_height: f32,
        mut height: impl FnMut(usize) -> Option<f32>,
    ) {
        if count == 0 {
            self.reset(0, 0.);
            return;
        }
        self.page = self.page.min(count - 1);
        let Some(first_height) = height(self.page).filter(|h| h.is_finite() && *h > 0.) else {
            return;
        };
        let mut offset = self.fraction * first_height + self.pending_pixels;
        self.pending_pixels = 0.;
        for _ in 0..count.min(256) {
            let Some(current_height) = height(self.page).filter(|h| h.is_finite() && *h > 0.)
            else {
                self.fraction = 0.;
                self.pending_pixels = offset;
                return;
            };
            if offset < 0. && self.page > 0 {
                let Some(previous_height) =
                    height(self.page - 1).filter(|h| h.is_finite() && *h > 0.)
                else {
                    self.fraction = 0.;
                    self.pending_pixels = offset;
                    self.stop();
                    return;
                };
                self.page -= 1;
                offset += previous_height;
                continue;
            }
            if offset >= current_height && self.page + 1 < count {
                self.page += 1;
                offset -= current_height;
                continue;
            }
            let upper = if self.page + 1 == count {
                (current_height - viewport_height).max(0.)
            } else {
                current_height
            };
            let clamped = offset.clamp(0., upper);
            if clamped != offset {
                self.stop();
            }
            self.fraction = clamped / current_height;
            return;
        }
        // An extreme input cannot force an unbounded traversal in one frame.
        self.fraction = 0.;
        self.pending_pixels = offset;
    }

    pub fn top_offset(&self, height: f32) -> f32 {
        self.fraction * height + self.pending_pixels
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scrolling_across_pages_preserves_the_excess_and_allows_reverse() {
        let mut reader = VerticalReader::default();
        reader.scroll(1250.);
        reader.settle(3, 600., |page| Some([1000., 1800., 1200.][page]));
        assert_eq!(reader.page, 1);
        assert!((reader.top_offset(1800.) - 250.).abs() < 0.001);
        reader.scroll(-400.);
        reader.settle(3, 600., |page| Some([1000., 1800., 1200.][page]));
        assert_eq!(reader.page, 0);
        assert!((reader.fraction - 0.85).abs() < 0.001);
    }

    #[test]
    fn unloaded_page_waits_without_losing_scroll_distance() {
        let mut reader = VerticalReader::default();
        reader.scroll(1200.);
        reader.settle(3, 500., |page| if page == 0 { Some(1000.) } else { None });
        assert_eq!(reader.page, 1);
        reader.settle(3, 500., |_| Some(1000.));
        assert!((reader.fraction - 0.2).abs() < 0.001);
    }

    #[test]
    fn viewport_resize_keeps_the_same_place_in_the_page() {
        let mut reader = VerticalReader::default();
        reader.reset(1, 0.4);
        reader.settle(3, 500., |_| Some(1800.));
        reader.settle(3, 700., |_| Some(2600.));
        assert_eq!(reader.page, 1);
        assert!((reader.fraction - 0.4).abs() < 0.001);
    }

    #[test]
    fn inertia_finishes_and_does_not_tick_at_rest() {
        let mut reader = VerticalReader::default();
        assert!(!reader.tick(0.));
        reader.release(1000., 0., false);
        for frame in 1..120 {
            reader.tick(frame as f64 / 60.);
        }
        assert!(!reader.active());
        reader.release(1000., 2., true);
        assert!(!reader.tick(2.1));
    }
}
