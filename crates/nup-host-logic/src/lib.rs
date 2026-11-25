#![forbid(unsafe_code)]

pub mod geometry;
pub mod settings;
pub mod state_machine;

pub use geometry::{DipSize, MonitorWorkArea, OrbGeometry, PixelPoint, PixelRect, ScreenEdge};
pub use settings::{OrbLaunchMode, OrbSettings, OrbSettingsStore};
pub use state_machine::{MorphSnapshot, OrbDragThreshold, OrbStateMachine, OrbWindowState};
