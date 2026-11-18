//! Rust port of the Wenku8 v0.5.6 / Shiori shelf choreography.
//! Copyrights and GPL-3.0 notices are retained in ../licenses.
use std::f32::consts::PI;

pub const ENTRANCE: f64 = 0.520;
pub const SELECT: f64 = 0.420;
pub const OPEN: f64 = 1.000;
pub const CLOSE: f64 = 0.840;

pub fn smooth(x: f32) -> f32 {
    let x = x.clamp(0., 1.);
    x * x * (3. - 2. * x)
}

pub fn weight(distance: f32) -> f32 {
    if distance.abs() >= 1.48 { 0. } else { (distance / 1.48 * PI / 2.).cos().powi(2) }
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
    cubic(((t - 0.12) / 0.46).clamp(0., 1.), 0.645, 0.045, 0.355, 1.)
}

pub fn paper_depth(t: f32, u: f32, v: f32) -> f32 {
    if t <= 0. || t >= 1. { return 0.; }
    let peel = smooth((t - 0.025 - v * 0.11) / 0.11);
    let landing = ((t - 0.58) / 0.42).clamp(0., 1.);
    let contact = (1. - v) * 0.66 + u * 0.34;
    let floating = 1. - smooth((landing - contact * 0.70) / 0.30);
    let breeze = ((v * 1.3 - t * 1.6) * PI * 2.).sin();
    peel * floating * (17. + 7. * (v * PI).sin() + 3. * breeze * (t * PI).sin())
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
        let directional = velocity.abs() * pitch >= 50.;
        let release = if directional { velocity } else { 0. };
        let mut target = (start + release / 4.4).round();
        if directional && (target - start) * release.signum() < release.abs() / 12. {
            let runway = start + release / 12.;
            target = if release > 0. { runway.ceil() } else { runway.floor() };
        }
        let coast = if directional && release.abs() > 4. {
            (release.abs() / 4.).ln() / 4.4
        } else { 0. };
        let handoff = start + release / 4.4 * (1. - (-4.4 * coast).exp());
        let handoff_velocity = release * (-4.4 * coast).exp();
        let landing = if directional { 3. * (target - handoff) / handoff_velocity } else { 0. };
        Self { start, velocity: release, target, coast, handoff, landing }
    }
    pub fn sample(&self, seconds: f32) -> (f32, bool) {
        if self.landing <= 0. {
            let delta = self.start - self.target;
            let e = (-12. * seconds).exp();
            let x = self.target + delta * (1. + 12. * seconds) * e;
            let v = -144. * delta * seconds * e;
            let done = (x - self.target).abs() < 0.0001 && v.abs() < 0.003;
            return (if done { self.target } else { x }, done);
        }
        if seconds < self.coast {
            return (self.start + self.velocity / 4.4 * (1. - (-4.4 * seconds).exp()), false);
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
            self.focus = from + (to - from) * cubic(t, 0.2, 1.06, 0.35, 1.);
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
}
