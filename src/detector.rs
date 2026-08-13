// Port of FUN_140005bf0 — see analysis/RE-findings.md §6 and analysis/linux-design.md §6.
//
// NOTE: the accumulator is incremental. Each accepted sample advances the
// gesture by exactly ONE new chord-direction delta; the whole-history
// window of the original Windows port is gone. See the comment on `step`
// for why re-summing the whole window on every frame was a
// scrolling-correctness bug.

use std::f64::consts::PI;

const PI2: f64 = 2.0 * PI;

pub const TRIGGER_ANGLE: f64 = PI / 12.0;
pub const NOISE_REJECT_ANGLE: f64 = PI / 4.0;
pub const ZONE_RADIANS: f64 = PI / 8.0;
pub const SAMPLE_DEADBAND_SQ: i64 = 400;
pub const SENSITIVITY_TABLE: [i32; 5] = [10, 14, 20, 28, 40];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TouchSample {
    pub x: i32,
    pub y: i32,
}

pub struct CircularDetector {
    /// Last sample that passed the dead band. Used to reject sub-20-unit
    /// jitter and to derive each chord's direction.
    last_stored: Option<TouchSample>,
    /// Direction (atan2) of the most recent accepted chord. The next
    /// accepted chord is compared against this to get a delta.
    last_angle: Option<f64>,
    /// Delta produced by the most recent `push_if_moved` call, consumed
    /// by the next `step`. `None` means "no new movement this frame".
    pending_delta: Option<f64>,
    accumulator: f64,
}

impl Default for CircularDetector {
    fn default() -> Self {
        Self::new()
    }
}

impl CircularDetector {
    pub fn new() -> Self {
        Self {
            last_stored: None,
            last_angle: None,
            pending_delta: None,
            accumulator: 0.0,
        }
    }

    pub fn on_gesture_start(&mut self) {
        self.last_stored = None;
        self.last_angle = None;
        self.pending_delta = None;
        self.accumulator = 0.0;
    }

    /// Record a new sample if it moved past the 20-unit dead band. On
    /// success, compute the chord-direction delta vs. the previous chord
    /// and stash it in `pending_delta` for the next `step`.
    ///
    /// This is the incremental replacement for the old whole-history
    /// window: each accepted sample contributes at most ONE delta, and a
    /// stationary finger contributes nothing — so a finger resting on
    /// the pad can no longer keep scrolling from stale history.
    pub fn push_if_moved(&mut self, s: TouchSample) {
        let Some(prev) = self.last_stored else {
            self.last_stored = Some(s);
            return;
        };
        let dx = (s.x - prev.x) as i64;
        let dy = (s.y - prev.y) as i64;
        if dx * dx + dy * dy <= SAMPLE_DEADBAND_SQ {
            return;
        }
        self.last_stored = Some(s);

        let angle = (dy as f64).atan2(dx as f64);
        let delta = match self.last_angle {
            Some(prev_angle) => wrap_angle(angle - prev_angle),
            None => 0.0,
        };
        self.last_angle = Some(angle);
        self.pending_delta = Some(delta);
    }

    /// Consume the pending delta, apply the noise gate and sensitivity
    /// weighting, accumulate, then drain whole ±2π crossings as ticks.
    ///
    /// Why incremental instead of the Windows whole-window mean?
    /// The original port re-summed the *entire* history window on every
    /// `step` call and added `sensitivity * mean(delta)` to the
    /// accumulator each time. A finger that stopped drawing still left
    /// the last circle's angles in the window, so every subsequent
    /// SYN_REPORT kept re-adding that stale mean and kept emitting ticks
    /// long after the gesture was over. Incremental accumulation fixes
    /// that: no new movement, no new delta, no ticks.
    pub fn step(&mut self, scroll_speed_adjust: i32) -> i32 {
        if let Some(d) = self.pending_delta.take() {
            if d.abs() <= NOISE_REJECT_ANGLE {
                let idx = (scroll_speed_adjust.clamp(-2, 2) + 2) as usize;
                let sensitivity = SENSITIVITY_TABLE[idx] as f64;
                self.accumulator += sensitivity * d;
            }
            // If |d| > π/4 we treat it as noise and drop the delta.
            // `last_angle` has already been advanced so a genuine
            // direction change (e.g. reversing the circle) re-baselines
            // cleanly instead of being rejected forever.
        }

        // WHILE-LOOP DRAIN — Linux deviation from Windows. See
        // DECISIONS.md D-006. Windows FUN_140005bf0 (lines 113-136) is
        // a single-pass branch that emits at most one tick per packet
        // and silently loses angle on fast sweeps; we drain fully so
        // that arbitrarily fast circles still scroll the proportional
        // amount.
        //
        // Sign convention preserved from Windows: positive accumulator
        // overflow yields a tick value of -1. The user-visible
        // `WheelReverse` flip is applied at the emit layer
        // (uinput.rs), not here. A clockwise gesture (which integrates
        // positive in screen-Y-down coords) therefore returns negative
        // ticks; passing the value through to uinput unchanged scrolls
        // the page DOWN, matching Windows.
        //
        // Known quirk preserved as a comment for archaeology:
        // FUN_140005bf0 line 120 contains a defensive clamp that snaps
        // the accumulator to -π after the +2π correction rather than
        // to 0. Our while-loop makes the clamp unreachable, but the
        // note remains so future readers don't think the quirk was
        // overlooked.
        let mut ticks: i32 = 0;
        while self.accumulator > PI {
            self.accumulator -= PI2;
            ticks -= 1;
        }
        while self.accumulator < -PI {
            self.accumulator += PI2;
            ticks += 1;
        }
        ticks
    }

    /// Test-only setter. Visible to integration tests under `tests/`.
    #[doc(hidden)]
    pub fn set_accumulator_for_test(&mut self, v: f64) {
        self.accumulator = v;
    }
}

/// Symmetric ±2π wrap into [-π, π]. Safe for chord-angle deltas because
/// `angle - prev_angle` is always within ±2π.
fn wrap_angle(d: f64) -> f64 {
    if d > PI {
        d - PI2
    } else if d < -PI {
        d + PI2
    } else {
        d
    }
}

/// Engagement gate — state 3 → state 4 transition test. Center-relative
/// atan2 sweep from the engagement-start point to the current sample,
/// with symmetric ±2π wrap (we deliberately use symmetric form across
/// the daemon — see RE-findings.md §5 footnote on the asymmetric wrap).
pub fn engagement_swept_angle(
    center_x: i32,
    center_y: i32,
    engage_start: TouchSample,
    current: TouchSample,
) -> f64 {
    let ax = (engage_start.x - center_x) as f64;
    let ay = (engage_start.y - center_y) as f64;
    let bx = (current.x - center_x) as f64;
    let by = (current.y - center_y) as f64;
    let mut d = by.atan2(bx) - ay.atan2(ax);
    if d > PI {
        d -= PI2;
    }
    if d < -PI {
        d += PI2;
    }
    d
}

/// Radial gate from FUN_140005a00 line 31 and FUN_1400046a0 lines 129/187.
/// Returns true if the centered sample is in the outer ring (i.e., outside
/// the inner dead-zone radius).
pub fn radial_gate_ok(
    center_x: i32,
    center_y: i32,
    s: TouchSample,
    detect_area_width: i32,
) -> bool {
    let dx = (s.x - center_x) as i64;
    let dy = (s.y - center_y) as i64;
    let r2 = dx * dx + dy * dy;
    let w = (10 - detect_area_width.clamp(0, 10)) as i64;
    r2 >= (w * w) * 400
}

/// Horizontal-arc test (FUN_140005a00 lines 65-74). Returns true if the
/// centered sample's atan2 lies within the configured wedge. Wraparound
/// (`start > end`) is handled by splitting the test. Caller guarantees
/// `horizontal_enable = true`; this function MUST NOT be called when
/// horizontal scrolling is disabled (see linux-design.md §5 "Vertical
/// scroll is NOT angle-gated").
pub fn within_horizontal_arc(
    center_x: i32,
    center_y: i32,
    s: TouchSample,
    horizontal_start: i32,
    horizontal_end: i32,
) -> bool {
    let dx = (s.x - center_x) as f64;
    let dy = (s.y - center_y) as f64;
    let mut theta = dy.atan2(dx);
    if theta < 0.0 {
        theta += PI2;
    }
    let start = horizontal_start as f64 * ZONE_RADIANS;
    let end = horizontal_end as f64 * ZONE_RADIANS;
    if start <= end {
        theta >= start && theta <= end
    } else {
        // Wedge wraps across 2π.
        theta >= start || theta <= end
    }
}
