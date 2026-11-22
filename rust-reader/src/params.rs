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
}

impl Default for Params {
    fn default() -> Self {
        Self { paper: Paper::default(), snap: Snap::default(), weight: Weight::default(), ease: Ease::default() }
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
