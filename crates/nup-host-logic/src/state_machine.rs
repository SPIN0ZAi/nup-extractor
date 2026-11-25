#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum OrbWindowState {
    Collapsed,
    Expanding,
    Expanded,
    Collapsing,
}

impl OrbWindowState {
    pub fn as_str(&self) -> &'static str {
        match self {
            OrbWindowState::Collapsed => "Collapsed",
            OrbWindowState::Expanding => "Expanding",
            OrbWindowState::Expanded => "Expanded",
            OrbWindowState::Collapsing => "Collapsing",
        }
    }

    pub fn as_lower_str(&self) -> &'static str {
        match self {
            OrbWindowState::Collapsed => "collapsed",
            OrbWindowState::Expanding => "expanding",
            OrbWindowState::Expanded => "expanded",
            OrbWindowState::Collapsing => "collapsing",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MorphSnapshot {
    pub state: OrbWindowState,
    pub progress: f64,
    pub is_animating: bool,
    pub reduced_motion: bool,
}

/// Pure, deterministic, interruptible state machine driving the Orb <-> Expanded morph.
/// Uses a critically damped spring integrator so mid-flight expand/collapse reversals
/// preserve position and velocity with zero jumps.
pub struct OrbStateMachine {
    // Critically damped spring: omega = sqrt(480) ~= 21.91 rad/s -> taut, crisp ~355 ms settle
    position: f64,
    velocity: f64,
    target: f64,
    last_tick_ms: f64,
    has_last_tick: bool,
    state: OrbWindowState,
    pub reduced_motion: bool,
}

impl OrbStateMachine {
    const SPRING_STIFFNESS: f64 = 480.0;
    const SPRING_DAMPING: f64 = 43.81780460041329; // 2 * sqrt(480)
    const SETTLE_POSITION_EPSILON: f64 = 0.006;
    const SETTLE_VELOCITY_EPSILON: f64 = 0.08;
    const REDUCED_MOTION_DURATION_MS: f64 = 140.0;

    pub fn new(initial_state: OrbWindowState, reduced_motion: bool) -> Self {
        let mut sm = Self {
            position: 0.0,
            velocity: 0.0,
            target: 0.0,
            last_tick_ms: 0.0,
            has_last_tick: false,
            state: OrbWindowState::Collapsed,
            reduced_motion,
        };
        sm.set_immediate_state(initial_state);
        sm
    }

    pub fn state(&self) -> OrbWindowState {
        self.state
    }

    pub fn progress(&self) -> f64 {
        self.position.clamp(0.0, 1.0)
    }

    pub fn velocity(&self) -> f64 {
        self.velocity
    }

    pub fn is_animating(&self) -> bool {
        matches!(
            self.state,
            OrbWindowState::Expanding | OrbWindowState::Collapsing
        )
    }

    pub fn set_immediate_state(&mut self, state: OrbWindowState) {
        if matches!(state, OrbWindowState::Expanded | OrbWindowState::Expanding) {
            self.state = OrbWindowState::Expanded;
            self.position = 1.0;
            self.target = 1.0;
        } else {
            self.state = OrbWindowState::Collapsed;
            self.position = 0.0;
            self.target = 0.0;
        }
        self.velocity = 0.0;
        self.has_last_tick = false;
    }

    pub fn request_expand(&mut self, now_ms: f64) -> bool {
        if matches!(
            self.state,
            OrbWindowState::Expanded | OrbWindowState::Expanding
        ) {
            return false;
        }
        self.state = OrbWindowState::Expanding;
        self.target = 1.0;
        self.last_tick_ms = now_ms;
        self.has_last_tick = true;
        true
    }

    pub fn request_collapse(&mut self, now_ms: f64) -> bool {
        if matches!(
            self.state,
            OrbWindowState::Collapsed | OrbWindowState::Collapsing
        ) {
            return false;
        }
        self.state = OrbWindowState::Collapsing;
        self.target = 0.0;
        self.last_tick_ms = now_ms;
        self.has_last_tick = true;
        true
    }

    pub fn toggle(&mut self, now_ms: f64) -> bool {
        if matches!(
            self.state,
            OrbWindowState::Collapsed | OrbWindowState::Collapsing
        ) {
            self.request_expand(now_ms)
        } else {
            self.request_collapse(now_ms)
        }
    }

    pub fn tick(&mut self, now_ms: f64) -> MorphSnapshot {
        if !self.is_animating() {
            return MorphSnapshot {
                state: self.state,
                progress: self.progress(),
                is_animating: false,
                reduced_motion: self.reduced_motion,
            };
        }

        if !self.has_last_tick {
            self.last_tick_ms = now_ms;
            self.has_last_tick = true;
            return MorphSnapshot {
                state: self.state,
                progress: self.progress(),
                is_animating: true,
                reduced_motion: self.reduced_motion,
            };
        }

        let dt_ms = (now_ms - self.last_tick_ms).max(0.0);
        self.last_tick_ms = now_ms;

        if dt_ms <= 0.0 {
            return MorphSnapshot {
                state: self.state,
                progress: self.progress(),
                is_animating: true,
                reduced_motion: self.reduced_motion,
            };
        }

        if self.reduced_motion {
            let delta = dt_ms / Self::REDUCED_MOTION_DURATION_MS;
            if self.target > 0.5 {
                self.position = (self.position + delta).min(1.0);
                if self.position >= 1.0 {
                    self.position = 1.0;
                    self.velocity = 0.0;
                    self.state = OrbWindowState::Expanded;
                }
            } else {
                self.position = (self.position - delta).max(0.0);
                if self.position <= 0.0 {
                    self.position = 0.0;
                    self.velocity = 0.0;
                    self.state = OrbWindowState::Collapsed;
                }
            }
            return MorphSnapshot {
                state: self.state,
                progress: self.progress(),
                is_animating: self.is_animating(),
                reduced_motion: true,
            };
        }

        // Sub-stepped semi-implicit Euler for unconditional stability even on slow frames
        let mut remaining_sec = (dt_ms / 1000.0).min(0.25);
        while remaining_sec > 0.0 {
            let h = remaining_sec.min(0.008); // <= 8ms sub-steps
            let displacement = self.position - self.target;
            let accel =
                (-Self::SPRING_STIFFNESS * displacement) - (Self::SPRING_DAMPING * self.velocity);
            self.velocity += accel * h;
            self.position += self.velocity * h;
            remaining_sec -= h;
        }

        if (self.target - self.position).abs() <= Self::SETTLE_POSITION_EPSILON
            && self.velocity.abs() <= Self::SETTLE_VELOCITY_EPSILON
        {
            self.position = self.target;
            self.velocity = 0.0;
            self.state = if self.target > 0.5 {
                OrbWindowState::Expanded
            } else {
                OrbWindowState::Collapsed
            };
        }

        MorphSnapshot {
            state: self.state,
            progress: self.progress(),
            is_animating: self.is_animating(),
            reduced_motion: false,
        }
    }
}

/// Pure click-vs-drag threshold detector for the floating orb.
pub struct OrbDragThreshold {
    start_x: f64,
    start_y: f64,
    is_pointer_down: bool,
    is_dragging: bool,
    threshold_px: f64,
}

impl OrbDragThreshold {
    pub const DEFAULT_THRESHOLD_PX: f64 = 4.0;

    pub fn new(threshold_px: f64) -> Self {
        Self {
            start_x: 0.0,
            start_y: 0.0,
            is_pointer_down: false,
            is_dragging: false,
            threshold_px: if threshold_px > 0.0 {
                threshold_px
            } else {
                Self::DEFAULT_THRESHOLD_PX
            },
        }
    }

    pub fn threshold_px(&self) -> f64 {
        self.threshold_px
    }

    pub fn is_pointer_down(&self) -> bool {
        self.is_pointer_down
    }

    pub fn is_dragging(&self) -> bool {
        self.is_dragging
    }

    pub fn begin(&mut self, x: f64, y: f64) {
        self.start_x = x;
        self.start_y = y;
        self.is_pointer_down = true;
        self.is_dragging = false;
    }

    /// Updates pointer coordinates. Returns true on the exact frame dragging begins.
    pub fn update(&mut self, x: f64, y: f64) -> bool {
        if !self.is_pointer_down || self.is_dragging {
            return false;
        }
        let dx = x - self.start_x;
        let dy = y - self.start_y;
        if (dx * dx) + (dy * dy) >= self.threshold_px * self.threshold_px {
            self.is_dragging = true;
            return true;
        }
        false
    }

    /// Ends pointer interaction. Returns true if the gesture was a click (never crossed drag threshold).
    pub fn end(&mut self) -> bool {
        let was_click = self.is_pointer_down && !self.is_dragging;
        self.is_pointer_down = false;
        self.is_dragging = false;
        was_click
    }
}
