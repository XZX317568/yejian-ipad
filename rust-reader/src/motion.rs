//! Rust port of the Wenku8 v0.5.6 / Shiori shelf choreography.
//! Copyrights and GPL-3.0 notices are retained in ../licenses.
use std::f32::consts::PI;

use crate::params::params;

pub const ENTRANCE: f64 = 0.520;
pub const SELECT: f64 = 0.420;
pub const OPEN: f64 = 0.720;
pub const CLOSE: f64 = 0.580;

/// Standard ease-out deceleration for UI transitions.
pub fn ease_out(t: f32) -> f32 {
    let e = params().ease.clone();
    cubic(t, e.out_x1, e.out_y1, e.out_x2, e.out_y2)
}

/// Asymptotic edge damping: `sign * limit * (1 - 1 / (1 + |d|/limit))`.
/// Returns 0 at 0 and always stays strictly within `(-limit, limit)`.
pub fn rubber_band(distance: f32, limit: f32) -> f32 {
    if limit <= 0. || distance == 0. {
        return 0.;
    }
    distance.signum() * limit * (1. - 1. / (1. + distance.abs() / limit))
}

pub fn smooth(x: f32) -> f32 {
    let x = x.clamp(0., 1.);
    x * x * (3. - 2. * x)
}

pub fn weight(distance: f32) -> f32 {
    let radius = params().weight.radius;
    if distance.abs() >= radius { 0. } else { (distance / radius * PI / 2.).cos().powi(2) }
}

pub fn cubic(t: f32, x1: f32, y1: f32, x2: f32, y2: f32) -> f32 {
    if t <= 0. { return 0.; }
    if t >= 1. { return 1.; }
    let curve = |s: f32, a: f32, b: f32| {
        3. * (1. - s).powi(2) * s * a + 3. * (1. - s) * s * s * b + s.powi(3)
    };
    let (mut lo, mut hi) = (0., 1.);
    for _ in 0..18 {
        let s = (lo + hi) / 2.;
        if curve(s, x1, x2) < t { lo = s; } else { hi = s; }
    }
    curve((lo + hi) / 2., y1, y2)
}

pub fn travel(t: f32) -> f32 {
    let e = params().ease.clone();
    cubic(((t - e.travel_start) / e.travel_span).clamp(0., 1.), 0.645, 0.045, 0.355, 1.)
}

pub fn paper_depth(t: f32, u: f32, v: f32) -> f32 {
    if t <= 0. || t >= 1. { return 0.; }
    let p = params().paper.clone();
    let peel = smooth((t - p.peel_start - v * p.peel_row_delay) / p.peel_span);
    let landing = ((t - p.landing_start) / p.landing_span).clamp(0., 1.);
    let contact = (1. - v) * p.contact_spine + u * p.contact_face;
    let floating = 1. - smooth((landing - contact * p.float_scale) / p.float_span);
    let breeze = ((v * p.breeze_v - t * p.breeze_t) * PI * 2.).sin();
    peel * floating * (p.base + p.arch * (v * PI).sin() + p.breeze_gain * breeze * (t * PI).sin())
}

#[derive(Clone, Copy, Debug)]
pub struct Snap {
    start: f32,
    velocity: f32,
    pub target: f32,
    coast: f32,
    handoff: f32,
    landing: f32,
}

impl Snap {
    /// Positions and velocities are in lanes, making resize continuous.
    pub fn new(start: f32, velocity: f32, pitch: f32) -> Self {
        let s = params().snap.clone();
        let directional = velocity.abs() * pitch >= s.directional_px;
        let release = if directional { velocity } else { 0. };
        let mut target = (start + release / s.friction).round();
        if directional && (target - start) * release.signum() < release.abs() / s.runway_lanes {
            let runway = start + release / s.runway_lanes;
            target = if release > 0. { runway.ceil() } else { runway.floor() };
        }
        let coast = if directional && release.abs() > s.coast_min_velocity {
            (release.abs() / s.coast_min_velocity).ln() / s.friction
        } else { 0. };
        let handoff = start + release / s.friction * (1. - (-s.friction * coast).exp());
        let handoff_velocity = release * (-s.friction * coast).exp();
        let landing = if directional { 3. * (target - handoff) / handoff_velocity } else { 0. };
        Self { start, velocity: release, target, coast, handoff, landing }
    }
    pub fn sample(&self, seconds: f32) -> (f32, bool) {
        if self.landing <= 0. {
            let s = params().snap.clone();
            let delta = self.start - self.target;
            let e = (-s.spring_c * seconds).exp();
            let x = self.target + delta * (1. + s.spring_c * seconds) * e;
            let v = -s.spring_k * delta * seconds * e;
            let done = (x - self.target).abs() < 0.0001 && v.abs() < 0.003;
            return (if done { self.target } else { x }, done);
        }
        if seconds < self.coast {
            let friction = params().snap.friction;
            return (self.start + self.velocity / friction * (1. - (-friction * seconds).exp()), false);
        }
        let remaining = (1. - (seconds - self.coast) / self.landing).clamp(0., 1.);
        (self.target - (self.target - self.handoff) * remaining.powi(3), remaining == 0.)
    }
}

#[derive(Default)]
pub struct ShelfMotion {
    pub focus: f32,
    tween: Option<(f64, f32, f32)>,
    snap: Option<(f64, Snap)>,
}
impl ShelfMotion {
    pub fn active(&self)->bool {self.tween.is_some()||self.snap.is_some()}
    pub fn select(&mut self, lane: f32, now: f64, reduced: bool) {
        self.snap = None;
        if reduced { self.focus = lane; self.tween = None; }
        else { self.tween = Some((now, self.focus, lane)); }
    }
    pub fn release(&mut self, velocity: f32, pitch: f32, now: f64, reduced: bool) {
        self.tween = None;
        if reduced { self.focus = self.focus.round(); self.snap = None; }
        else { self.snap = Some((now, Snap::new(self.focus, velocity, pitch))); }
    }
    pub fn drag(&mut self, delta: f32) {
        self.tween = None;
        self.snap = None;
        self.focus += delta;
    }
    pub fn tick(&mut self, now: f64) -> bool {
        if let Some((start, from, to)) = self.tween {
            let t = ((now - start) / SELECT).clamp(0., 1.) as f32;
            let e = params().ease.clone();
            self.focus = from + (to - from) * cubic(t, e.select_x1, e.select_y1, e.select_x2, e.select_y2);
            if t >= 1. { self.tween = None; }
        }
        if let Some((start, snap)) = self.snap {
            let (value, done) = snap.sample((now - start) as f32);
            self.focus = value;
            if done { self.snap = None; }
        }
        self.tween.is_some() || self.snap.is_some()
    }
}

/// A single eased scalar tween between two absolute timestamps.
#[derive(Clone, Copy, Debug)]
pub struct TimedTween {
    start: f64,
    duration: f64,
    from: f32,
    to: f32,
}

impl TimedTween {
    pub fn new(start: f64, duration: f64, from: f32, to: f32) -> Self {
        Self {
            start,
            duration: duration.max(0.),
            from,
            to,
        }
    }
    pub fn value(&self, now: f64) -> f32 {
        if self.duration <= 0. {
            return self.to;
        }
        let t = ((now - self.start) / self.duration).clamp(0., 1.) as f32;
        self.from + (self.to - self.from) * ease_out(t)
    }
    pub fn done(&self, now: f64) -> bool {
        self.duration <= 0. || now - self.start >= self.duration
    }
    #[allow(dead_code)]
    pub fn target(&self) -> f32 {
        self.to
    }
}

/// An outgoing page sliding away while the current page settles to offset 0.
#[derive(Clone, Copy, Debug)]
pub struct PageTransition {
    pub from_page: usize,
    pub start: f64,
    pub duration: f64,
    pub start_offset: f32,
}

/// Horizontal paged-reader motion: finger drag (with rubber-banded edges),
/// tap-driven page turns, and a settle back to zero on release.
///
/// [`Self::tick`] only reports in-flight animations, so an idle drag state
/// never schedules repaints on its own.
#[derive(Default, Clone, Debug)]
pub struct PageSlide {
    /// Current page; callers advance it before [`Self::animate_turn`].
    pub page: usize,
    offset: f32,
    drag_raw: f32,
    dragging: bool,
    settle: Option<TimedTween>,
    transition: Option<PageTransition>,
}

impl PageSlide {
    /// True while dragging or while a settle/turn is animating at `now`.
    pub fn active(&self, now: f64) -> bool {
        self.dragging
            || self.settle.is_some_and(|t| !t.done(now))
            || self.transition.is_some_and(|t| now - t.start < t.duration)
    }
    /// Drop all motion state and land on the current page instantly.
    pub fn cancel(&mut self) {
        self.dragging = false;
        self.offset = 0.;
        self.drag_raw = 0.;
        self.settle = None;
        self.transition = None;
    }
    pub fn begin_drag(&mut self) {
        self.dragging = true;
        self.offset = 0.;
        self.drag_raw = 0.;
        self.settle = None;
        self.transition = None;
    }
    /// Accumulate finger delta `dx` in px. A positive offset drags content
    /// right, revealing the previous page in LTR and the next page in RTL;
    /// [`rubber_band`] damps drags past a missing page.
    pub fn drag(&mut self, dx: f32, can_prev: bool, can_next: bool, rtl: bool, width: f32) {
        self.settle = None;
        self.transition = None;
        self.drag_raw += dx;
        let reveals_prev = if rtl {
            self.drag_raw < 0.
        } else {
            self.drag_raw > 0.
        };
        let allowed = if reveals_prev { can_prev } else { can_next };
        let limit = (width * 0.35).max(24.);
        self.offset = if allowed || self.drag_raw == 0. {
            self.drag_raw
        } else {
            rubber_band(self.drag_raw, limit)
        };
    }
    /// Slide `from_page` out while `self.page` slides in from `start_offset`
    /// to 0 over 0.18s; instantaneous when `reduced`.
    pub fn animate_turn(&mut self, from_page: usize, start_offset: f32, now: f64, reduced: bool) {
        self.dragging = false;
        self.offset = 0.;
        self.drag_raw = 0.;
        self.settle = None;
        self.transition = if reduced || start_offset == 0. {
            None
        } else {
            Some(PageTransition {
                from_page,
                start: now,
                duration: 0.18,
                start_offset,
            })
        };
    }
    /// Spring the dragged offset back to zero (instant when `reduced`).
    pub fn rebound(&mut self, now: f64, reduced: bool) {
        self.dragging = false;
        self.drag_raw = 0.;
        self.transition = None;
        if reduced || self.offset == 0. {
            self.offset = 0.;
            self.settle = None;
        } else {
            self.settle = Some(TimedTween::new(now, 0.22, self.offset, 0.));
        }
    }
    /// Advance animations; returns true while still moving (repaint needed).
    pub fn tick(&mut self, now: f64) -> bool {
        if let Some(settle) = self.settle {
            self.offset = settle.value(now);
            if settle.done(now) {
                self.offset = 0.;
                self.settle = None;
            }
        }
        if self.transition.is_some_and(|t| now - t.start >= t.duration) {
            self.transition = None;
        }
        self.settle.is_some() || self.transition.is_some()
    }
    /// Horizontal pixel offset to render the current page at `now`.
    pub fn current_offset(&self, now: f64) -> f32 {
        if let Some(t) = &self.transition {
            let k = ((now - t.start) / t.duration).clamp(0., 1.) as f32;
            return t.start_offset * (1. - ease_out(k));
        }
        if let Some(settle) = &self.settle {
            return settle.value(now);
        }
        self.offset
    }
    /// While a turn runs: `(from_page, eased progress 0..=1)` for drawing the
    /// outgoing page sliding away in the direction of `start_offset`.
    pub fn outgoing(&self, now: f64) -> Option<(usize, f32)> {
        self.transition.map(|t| {
            let k = ((now - t.start) / t.duration).clamp(0., 1.) as f32;
            (t.from_page, ease_out(k))
        })
    }
}

/// Bounded double-tap focal zoom plus pan rebound, sharing one timeline.
#[derive(Default, Clone, Debug)]
pub struct ZoomAnim {
    zoom: Option<TimedTween>,
    pan: Option<(TimedTween, TimedTween)>,
}

impl ZoomAnim {
    pub fn start(
        &mut self,
        from_zoom: f32,
        to_zoom: f32,
        from_pan: [f32; 2],
        to_pan: [f32; 2],
        now: f64,
        reduced: bool,
    ) {
        let duration = if reduced { 0. } else { 0.24 };
        self.zoom = Some(TimedTween::new(now, duration, from_zoom, to_zoom));
        self.pan = Some((
            TimedTween::new(now, duration, from_pan[0], to_pan[0]),
            TimedTween::new(now, duration, from_pan[1], to_pan[1]),
        ));
    }
    pub fn active(&self, now: f64) -> bool {
        self.zoom.is_some_and(|t| !t.done(now))
    }
    pub fn cancel(&mut self) {
        self.zoom = None;
        self.pan = None;
    }
    /// Sample `(zoom, pan)` at `now`; returns the target values once and
    /// clears itself when the tween has finished.
    pub fn sample(&mut self, now: f64) -> Option<(f32, [f32; 2])> {
        let zoom = self.zoom?;
        let (px, py) = self.pan?;
        let out = (zoom.value(now), [px.value(now), py.value(now)]);
        if zoom.done(now) {
            *self = Self::default();
        }
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fling_finishes_exactly_and_never_reverses() {
        for velocity in [-80., -9., -2.5, 2.5, 9., 80.] {
            let s = Snap::new(0.25, velocity, 28.);
            let mut previous = 0.25;
            let mut stopped = false;
            for i in 1..400 {
                let (x, done) = s.sample(i as f32 / 60.);
                assert!((x - previous) * velocity.signum() >= -0.00001);
                previous = x;
                if done { assert_eq!(x, s.target); stopped = true; break; }
            }
            assert!(stopped);
        }
    }
    #[test]
    fn paper_meets_both_endpoints_and_idle_stops() {
        for u in [0., 0.5, 1.] {
            for v in [0., 0.5, 1.] {
                assert_eq!(paper_depth(0., u, v), 0.);
                assert_eq!(paper_depth(1., u, v), 0.);
            }
        }
        let mut motion = ShelfMotion::default();
        motion.select(1., 0., false);
        assert!(!motion.tick(1.));
        assert_eq!(motion.focus, 1.);
        assert!(!motion.tick(2.));
    }

    #[test]
    fn timed_tween_is_bounded_and_snaps() {
        let tw = TimedTween::new(2., 0.1, 0., 5.);
        assert_eq!(tw.value(2.), 0.);
        let mid = tw.value(2.05);
        assert!(mid > 0. && mid < 5.);
        assert_eq!(tw.value(2.1), 5.);
        assert!(tw.done(2.1));
        assert!(!tw.done(2.09));
        assert_eq!(tw.target(), 5.);
        let instant = TimedTween::new(0., 0., 3., 7.);
        assert_eq!(instant.value(0.), 7.);
        assert!(instant.done(0.));
    }

    #[test]
    fn rubber_band_stays_strictly_inside_limits() {
        assert_eq!(rubber_band(0., 80.), 0.);
        for d in [1., 10., 50., 200., 2000.] {
            let r = rubber_band(d, 80.);
            assert!(r > 0. && r < 80.);
            assert_eq!(rubber_band(-d, 80.), -r);
        }
        assert!(rubber_band(40., 80.) < rubber_band(60., 80.));
        assert_eq!(rubber_band(50., 0.), 0.);
    }

    #[test]
    fn page_slide_drag_turn_and_rebound_finish() {
        let mut slide = PageSlide::default();
        // Drag into a missing previous page: rubber band caps the offset.
        slide.begin_drag();
        slide.drag(300., false, true, false, 400.);
        assert!(slide.offset > 0. && slide.offset < 400. * 0.35);
        slide.rebound(0., false);
        assert!(slide.active(0.));
        assert!(slide.tick(0.1));
        assert!(!slide.tick(0.23));
        assert_eq!(slide.current_offset(1.), 0.);

        // Tap-slide: outgoing page reported, offset eases to zero in 0.18s.
        slide.page = 4;
        slide.animate_turn(3, 400., 1., false);
        assert!(slide.active(1.));
        assert_eq!(slide.outgoing(1.).map(|o| o.0), Some(3));
        assert_eq!(slide.current_offset(1.), 400.);
        assert!(slide.current_offset(1.09).abs() < 400.);
        assert!(!slide.tick(1.19));
        assert!(slide.outgoing(1.2).is_none());
        assert_eq!(slide.current_offset(1.2), 0.);

        // Reduced mode snaps instantly.
        slide.page = 5;
        slide.animate_turn(4, -400., 2., true);
        assert!(!slide.active(2.));
        assert_eq!(slide.current_offset(2.), 0.);
        slide.begin_drag();
        slide.drag(80., true, true, false, 400.);
        slide.rebound(3., true);
        assert_eq!(slide.current_offset(3.), 0.);
        assert!(!slide.tick(3.01));
        slide.cancel();
        assert!(!slide.active(4.));
    }

    #[test]
    fn page_slide_settles_to_zero_offset_and_no_outgoing() {
        // A committed turn: once the 0.18s slide elapses the offset is
        // exactly 0 and no outgoing page remains.
        let mut slide = PageSlide::default();
        slide.page = 1;
        slide.animate_turn(0, -600., 10., false);
        assert!(slide.outgoing(10.05).is_some());
        assert_ne!(slide.current_offset(10.05), 0.);
        assert!(!slide.tick(10.19));
        assert_eq!(slide.current_offset(10.19), 0.);
        assert!(slide.outgoing(10.19).is_none());
        assert!(!slide.active(10.19));

        // A released drag: the 0.22s rebound lands on exactly 0 as well.
        slide.begin_drag();
        slide.drag(120., true, true, false, 400.);
        slide.rebound(11., false);
        assert_ne!(slide.current_offset(11.05), 0.);
        assert!(!slide.tick(11.23));
        assert_eq!(slide.current_offset(11.23), 0.);
        assert!(slide.outgoing(11.23).is_none());
        assert!(!slide.active(11.23));

        // Reduced motion: both are instant, at the start timestamp itself.
        slide.page = 2;
        slide.animate_turn(1, 600., 12., true);
        assert_eq!(slide.current_offset(12.), 0.);
        assert!(slide.outgoing(12.).is_none());
        assert!(!slide.active(12.));
        slide.begin_drag();
        slide.drag(-90., true, true, false, 400.);
        slide.rebound(13., true);
        assert_eq!(slide.current_offset(13.), 0.);
        assert!(slide.outgoing(13.).is_none());
        assert!(!slide.active(13.));
    }

    #[test]
    fn zoom_anim_returns_target_and_reduced_snaps() {
        let mut z = ZoomAnim::default();
        z.start(1., 2., [10., 5.], [30., 15.], 0., false);
        assert!(z.active(0.));
        let (zoom, pan) = z.sample(0.12).unwrap();
        assert!(zoom > 1. && zoom < 2.);
        assert!(pan[0] > 10. && pan[0] < 30.);
        assert_eq!(z.sample(0.25), Some((2., [30., 15.])));
        assert!(!z.active(0.3));
        assert!(z.sample(0.3).is_none());

        z.start(1., 2.5, [0., 0.], [9., 9.], 5., true);
        assert!(!z.active(5.));
        assert_eq!(z.sample(5.), Some((2.5, [9., 9.])));
        assert!(z.sample(5.).is_none());
        z.cancel();
    }
}
