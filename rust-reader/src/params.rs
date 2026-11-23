//! Tunable motion and paper constants, externalised for the polish loop.
//!
//! Editing `params.json` next to the executable (or the file named by
//! `YEJIAN_PARAMS`) changes the feel **without recompiling**: the acceptance
//! renderer picks it up on the next run. Defaults below mirror the values that
//! were previously hard-coded in `motion.rs`.

use std::path::PathBuf;
use std::sync::{Arc, OnceLock, RwLock};

use serde::{Deserialize, Serialize};

/// Root of every tunable constant used by the shelf choreography.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Params {
    pub paper: Paper,
    pub snap: Snap,
    pub weight: Weight,
    pub ease: Ease,
    pub shelf: Shelf,
    pub spread: Spread,
    pub flip: Flip,
    pub close: Close,
}

impl Default for Params {
    fn default() -> Self {
        Self {
            paper: Paper::default(),
            snap: Snap::default(),
            weight: Weight::default(),
            ease: Ease::default(),
            shelf: Shelf::default(),
            spread: Spread::default(),
            flip: Flip::default(),
            close: Close::default(),
        }
    }
}

/// Cover-flight paper deformation (`paper_depth`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Paper {
    /// Peel starts after this normalised time.
    pub peel_start: f32,
    /// Peel ramp length after `peel_start`.
    pub peel_span: f32,
    /// Per-row (`v`) delay applied to the peel.
    pub peel_row_delay: f32,
    /// Landing starts at this normalised time.
    pub landing_start: f32,
    /// Landing span after `landing_start`.
    pub landing_span: f32,
    /// Contact weight of the spine side `(1 - v)`.
    pub contact_spine: f32,
    /// Contact weight of the fore-edge side `u`.
    pub contact_face: f32,
    pub float_scale: f32,
    pub float_span: f32,
    pub breeze_v: f32,
    pub breeze_t: f32,
    /// Base lift in points once fully peeled.
    pub base: f32,
    /// Row arch amplitude across `v`.
    pub arch: f32,
    /// Breeze ripple amplitude.
    pub breeze_gain: f32,
}

impl Default for Paper {
    fn default() -> Self {
        Self {
            peel_start: 0.025,
            peel_span: 0.11,
            peel_row_delay: 0.11,
            landing_start: 0.58,
            landing_span: 0.42,
            contact_spine: 0.66,
            contact_face: 0.34,
            float_scale: 0.70,
            float_span: 0.30,
            breeze_v: 1.3,
            breeze_t: 1.6,
            base: 17.,
            arch: 7.,
            breeze_gain: 3.,
        }
    }
}

/// Fling/spring behaviour of the shelf lane snap.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Snap {
    /// `|velocity| * pitch` above which a release counts as directional.
    pub directional_px: f32,
    /// Exponential friction rate (per second).
    pub friction: f32,
    /// Minimum runway in lanes when a release would not clear one lane.
    pub runway_lanes: f32,
    /// Below this speed the release is not treated as a coast.
    pub coast_min_velocity: f32,
    /// Critically damped spring stiffness for non-directional releases.
    pub spring_k: f32,
    /// Critically damped spring damping.
    pub spring_c: f32,
}

impl Default for Snap {
    fn default() -> Self {
        Self { directional_px: 50., friction: 4.4, runway_lanes: 12., coast_min_velocity: 4., spring_k: 144., spring_c: 12. }
    }
}

/// Shelf extraction wave.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Weight {
    /// Neighbour reach in lanes (focus book is pulled, +/- this drops to 0).
    pub radius: f32,
}

impl Default for Weight {
    fn default() -> Self {
        Self { radius: 1.48 }
    }
}

/// Cubic bezier control points for the shared easing curves.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Ease {
    pub out_x1: f32,
    pub out_y1: f32,
    pub out_x2: f32,
    pub out_y2: f32,
    pub select_x1: f32,
    pub select_y1: f32,
    pub select_x2: f32,
    pub select_y2: f32,
    pub travel_start: f32,
    pub travel_span: f32,
}

impl Default for Ease {
    fn default() -> Self {
        Self {
            out_x1: 0.22,
            out_y1: 1.,
            out_x2: 0.36,
            out_y2: 1.,
            select_x1: 0.2,
            select_y1: 1.06,
            select_x2: 0.35,
            select_y2: 1.,
            travel_start: 0.12,
            travel_span: 0.46,
        }
    }
}

/// Shelf extraction (抽出) and cover flight (摊开) shape.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Shelf {
    /// Base pull distance as a fraction of the cover width.
    pub pull_base: f32,
    /// Linear term of the pull falloff `w * (a - b * w)`.
    pub pull_linear: f32,
    /// Quadratic term of the pull falloff.
    pub pull_quad: f32,
    /// Rail sag per lane.
    pub rail_linear: f32,
    /// Rail sag per lane squared.
    pub rail_quad: f32,
    /// Peak lift of the flying cover in points.
    pub lift_px: f32,
    /// Peak roll of the flying cover in radians.
    pub roll_rad: f32,
    /// Arc height base in points.
    pub arc_base: f32,
    /// Arc height per point of travel distance.
    pub arc_pct: f32,
    /// Arc height cap.
    pub arc_max: f32,
    /// Cover mesh columns (rows are derived from the aspect ratio).
    pub columns: usize,
    /// Strength of the finite-difference paper shading.
    pub shade_gain: f32,
}

impl Default for Shelf {
    fn default() -> Self {
        Self {
            pull_base: 0.84,
            pull_linear: 1.24,
            pull_quad: 0.24,
            rail_linear: 2.5,
            rail_quad: 0.45,
            lift_px: 18.,
            roll_rad: 0.045,
            arc_base: 12.,
            arc_pct: 0.075,
            arc_max: 32.,
            columns: 17,
            shade_gain: 0.012,
        }
    }
}

/// Page spread (摊开): the slanted-edge reveal that wipes the detail page
/// in over the landed cover — ArchivePlaneTransition `incoming`, ported.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Spread {
    /// Slanted clip edge cap in points.
    pub slant_px: f32,
    /// Slanted clip edge as a fraction of the plane width; the effective
    /// slant is `min(slant_px, w * slant_pct)`.
    pub slant_pct: f32,
    /// Horizontal component of the incoming-plane offset (times dir, times
    /// `1 - p`).
    pub shift_x: f32,
    /// Vertical component of the incoming-plane offset (times `1 - p`).
    pub shift_y: f32,
    /// Point on the flight t axis where the spread begins.
    pub start: f32,
    /// Length of the t axis the spread occupies.
    pub span: f32,
    /// Wipe direction: +1 sweeps left to right, -1 mirrors it.
    pub dir: f32,
    /// Cubic bezier control points of the spread easing.
    pub ease_x1: f32,
    pub ease_y1: f32,
    pub ease_x2: f32,
    pub ease_y2: f32,
}

impl Default for Spread {
    fn default() -> Self {
        Self {
            slant_px: 180.,
            slant_pct: 0.46,
            shift_x: 24.,
            shift_y: 12.,
            start: 0.45,
            span: 0.55,
            dir: 1.,
            ease_x1: 0.18,
            ease_y1: 0.72,
            ease_x2: 0.18,
            ease_y2: 1.,
        }
    }
}

/// Page flip (翻页): the depth ridge riding the turning edge plus the turn
/// commit and entry timings. Only shapes motion — the PageSlide offset axis
/// keeps its existing rubber-band behaviour.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Flip {
    /// Tap/committed turn duration in milliseconds.
    pub turn_ms: f32,
    /// Tap turn starting offset as a fraction of the page width.
    pub tap_strip_pct: f32,
    /// Drag commit displacement threshold in points.
    pub commit_px: f32,
    /// Drag commit velocity threshold in points per second.
    pub commit_vx: f32,
    /// Peak lift of the turning-edge depth ridge in points.
    pub depth_px: f32,
    /// Ridge band width as a fraction of the page width.
    pub band: f32,
    /// Per-column peel delay of the depth envelope.
    pub peel_span: f32,
    /// Cubic bezier control points of the turn easing.
    pub ease_x1: f32,
    pub ease_y1: f32,
    pub ease_x2: f32,
    pub ease_y2: f32,
    /// Crease shading strength at the turning edge (0 disables).
    pub edge_shade: f32,
    /// Detail -> reader peel-entry duration in milliseconds.
    pub entry_ms: f32,
    /// Cubic bezier control points of the entry easing.
    pub entry_ease_x1: f32,
    pub entry_ease_y1: f32,
    pub entry_ease_x2: f32,
    pub entry_ease_y2: f32,
}

impl Default for Flip {
    fn default() -> Self {
        Self {
            turn_ms: 180.,
            tap_strip_pct: 0.24,
            commit_px: 45.,
            commit_vx: 500.,
            depth_px: 22.,
            band: 0.35,
            peel_span: 0.11,
            ease_x1: 0.22,
            ease_y1: 1.,
            ease_x2: 0.36,
            ease_y2: 1.,
            edge_shade: 0.10,
            entry_ms: 420.,
            entry_ease_x1: 0.18,
            entry_ease_y1: 0.72,
            entry_ease_x2: 0.18,
            entry_ease_y2: 1.,
        }
    }
}

/// Close (合上): the reader -> detail peel-exit mirror of the entry, plus
/// the detail -> shelf return duration lifted out of `motion::CLOSE`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Close {
    /// Reader -> target screen peel-exit duration in milliseconds
    /// (reference `NovelMotion.exit` = 320).
    pub exit_ms: f32,
    /// Cubic bezier control points of the exit easing (defaults mirror
    /// `flip.entry_ease_*` so the exit replays the entry curve in reverse).
    pub exit_ease_x1: f32,
    pub exit_ease_y1: f32,
    pub exit_ease_x2: f32,
    pub exit_ease_y2: f32,
    /// Exit band direction: 0 mirrors the entry sweep (rtl-aware),
    /// non-zero forces the closing edge toward that side.
    pub peel_dir: f32,
    /// Under-screen content (detail text column) starts fading in once the
    /// band progress `p` drops below this threshold.
    pub under_alpha_start: f32,
    /// Reader chrome lead-out fade in milliseconds.
    pub chrome_ms: f32,
    /// Detail -> shelf cover-return duration in milliseconds
    /// (was `motion::CLOSE` = 0.580 s).
    pub close_travel_ms: f32,
}

impl Default for Close {
    fn default() -> Self {
        Self {
            exit_ms: 320.,
            exit_ease_x1: 0.18,
            exit_ease_y1: 0.72,
            exit_ease_x2: 0.18,
            exit_ease_y2: 1.,
            peel_dir: 0.,
            under_alpha_start: 0.5,
            chrome_ms: 160.,
            close_travel_ms: 580.,
        }
    }
}

static SLOT: OnceLock<RwLock<Arc<Params>>> = OnceLock::new();

fn slot() -> &'static RwLock<Arc<Params>> {
    SLOT.get_or_init(|| RwLock::new(Arc::new(load_or_default())))
}

/// Path of the parameter file: `YEJIAN_PARAMS` when set, otherwise
/// `params.json` next to the working directory or the executable.
pub fn params_path() -> PathBuf {
    if let Some(explicit) = std::env::var_os("YEJIAN_PARAMS") {
        return PathBuf::from(explicit);
    }
    let local = PathBuf::from("params.json");
    if local.exists() {
        return local;
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            return dir.join("params.json");
        }
    }
    local
}

fn load_or_default() -> Params {
    let path = params_path();
    match std::fs::read_to_string(&path) {
        Ok(text) => serde_json::from_str::<Params>(&text).unwrap_or_else(|err| {
            eprintln!("params: {} is invalid ({}); using defaults", path.display(), err);
            Params::default()
        }),
        Err(_) => Params::default(),
    }
}

/// Current parameters. Cheap to call per frame: it clones an `Arc`.
pub fn params() -> Arc<Params> {
    slot().read().map(|guard| Arc::clone(&guard)).unwrap_or_else(|_| Arc::new(Params::default()))
}

#[allow(dead_code)] // 保留：热加载钩子，未接线
/// Re-read the parameter file, replacing the live values.
/// Returns the path and whether it parsed.
pub fn reload() -> (PathBuf, bool) {
    let path = params_path();
    let ok = match std::fs::read_to_string(&path) {
        Ok(text) => match serde_json::from_str::<Params>(&text) {
            Ok(parsed) => {
                if let Ok(mut guard) = slot().write() {
                    *guard = Arc::new(parsed);
                }
                true
            }
            Err(err) => {
                eprintln!("params: {} is invalid ({}); keeping previous", path.display(), err);
                false
            }
        },
        Err(err) => {
            eprintln!("params: cannot read {} ({}); keeping previous", path.display(), err);
            false
        }
    };
    (path, ok)
}

#[allow(dead_code)] // 保留：录制指纹，未接线
/// Serialise the current live values, for recording what a video was rendered with.
pub fn fingerprint() -> String {
    serde_json::to_string(&*params()).unwrap_or_else(|_| "{}".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_round_trip() {
        let text = serde_json::to_string_pretty(&Params::default()).unwrap();
        let back: Params = serde_json::from_str(&text).unwrap();
        assert_eq!(back, Params::default());
    }

    #[test]
    fn unknown_keys_are_rejected() {
        let bad = r#"{"paper":{"nope":1}}"#;
        assert!(serde_json::from_str::<Params>(bad).is_err());
    }
}
