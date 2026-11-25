#![forbid(unsafe_code)]

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::geometry::{OrbGeometry, ScreenEdge};
use crate::state_machine::OrbWindowState;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum OrbLaunchMode {
    Collapsed,
    Expanded,
    RememberLast,
}

impl OrbLaunchMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            OrbLaunchMode::Collapsed => "Collapsed",
            OrbLaunchMode::Expanded => "Expanded",
            OrbLaunchMode::RememberLast => "RememberLast",
        }
    }

    pub fn parse_lossy(s: &str) -> Self {
        if s.eq_ignore_ascii_case("expanded") {
            OrbLaunchMode::Expanded
        } else if s.eq_ignore_ascii_case("rememberlast") || s.eq_ignore_ascii_case("remember_last")
        {
            OrbLaunchMode::RememberLast
        } else {
            OrbLaunchMode::Collapsed
        }
    }
}

fn default_anchor_x() -> f64 {
    0.96
}
fn default_anchor_y() -> f64 {
    0.38
}
fn default_edge() -> ScreenEdge {
    ScreenEdge::Right
}
fn default_last_mode() -> String {
    "Expanded".to_string()
}
fn default_launch_mode() -> OrbLaunchMode {
    OrbLaunchMode::Expanded
}
fn default_expanded_width() -> f64 {
    OrbGeometry::DEFAULT_EXPANDED_WIDTH_DIP
}
fn default_expanded_height() -> f64 {
    OrbGeometry::DEFAULT_EXPANDED_HEIGHT_DIP
}
fn default_theme() -> String {
    "dark".to_string()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct OrbSettings {
    pub has_saved_position: bool,
    pub orb_center_x: i32,
    pub orb_center_y: i32,
    #[serde(default = "default_anchor_x")]
    pub anchor_fraction_x: f64,
    #[serde(default = "default_anchor_y")]
    pub anchor_fraction_y: f64,
    #[serde(default = "default_edge")]
    pub orb_edge: ScreenEdge,
    pub monitor_device_name: String,
    #[serde(default = "default_last_mode")]
    pub last_mode: String,
    #[serde(default = "default_launch_mode")]
    pub launch_mode: OrbLaunchMode,
    #[serde(default = "default_expanded_width")]
    pub expanded_width_dip: f64,
    #[serde(default = "default_expanded_height")]
    pub expanded_height_dip: f64,
    #[serde(default = "default_theme")]
    pub theme: String,
}

impl Default for OrbSettings {
    fn default() -> Self {
        Self {
            has_saved_position: false,
            orb_center_x: 0,
            orb_center_y: 0,
            anchor_fraction_x: 0.96,
            anchor_fraction_y: 0.38,
            orb_edge: ScreenEdge::Right,
            monitor_device_name: String::new(),
            last_mode: "Expanded".to_string(),
            launch_mode: OrbLaunchMode::Expanded,
            expanded_width_dip: OrbGeometry::DEFAULT_EXPANDED_WIDTH_DIP,
            expanded_height_dip: OrbGeometry::DEFAULT_EXPANDED_HEIGHT_DIP,
            theme: "dark".to_string(),
        }
    }
}

impl OrbSettings {
    pub fn resolve_startup_state(&self, has_initial_file_arg: bool) -> OrbWindowState {
        if has_initial_file_arg {
            return OrbWindowState::Expanded;
        }
        match self.launch_mode {
            OrbLaunchMode::Expanded => OrbWindowState::Expanded,
            OrbLaunchMode::RememberLast => {
                if self.last_mode.eq_ignore_ascii_case("Expanded") {
                    OrbWindowState::Expanded
                } else {
                    OrbWindowState::Collapsed
                }
            }
            OrbLaunchMode::Collapsed => OrbWindowState::Collapsed,
        }
    }
}

pub struct OrbSettingsStore;

impl OrbSettingsStore {
    pub fn default_path() -> PathBuf {
        if let Ok(app_data) = std::env::var("APPDATA")
            && !app_data.trim().is_empty()
        {
            return PathBuf::from(app_data)
                .join("NupExtractor")
                .join("orb-state.json");
        }
        std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.to_path_buf()))
            .unwrap_or_else(|| PathBuf::from("."))
            .join("NupExtractor")
            .join("orb-state.json")
    }

    pub fn load(file_path: Option<&Path>) -> OrbSettings {
        let path = file_path
            .map(PathBuf::from)
            .unwrap_or_else(Self::default_path);
        let Ok(json) = fs::read_to_string(&path) else {
            return OrbSettings::default();
        };
        let Ok(mut loaded) = serde_json::from_str::<OrbSettings>(&json) else {
            return OrbSettings::default();
        };

        // Defensive clamping of restored dimensions
        if !(OrbGeometry::MIN_EXPANDED_WIDTH_DIP..=6000.0).contains(&loaded.expanded_width_dip) {
            loaded.expanded_width_dip = OrbGeometry::DEFAULT_EXPANDED_WIDTH_DIP;
        }
        if !(OrbGeometry::MIN_EXPANDED_HEIGHT_DIP..=4000.0).contains(&loaded.expanded_height_dip) {
            loaded.expanded_height_dip = OrbGeometry::DEFAULT_EXPANDED_HEIGHT_DIP;
        }
        if loaded.theme.eq_ignore_ascii_case("light") {
            loaded.theme = "light".to_string();
        } else {
            loaded.theme = "dark".to_string();
        }

        loaded
    }

    pub fn save(settings: &OrbSettings, file_path: Option<&Path>) -> bool {
        let path = file_path
            .map(PathBuf::from)
            .unwrap_or_else(Self::default_path);
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
            && fs::create_dir_all(parent).is_err()
        {
            return false;
        }
        let Ok(json) = serde_json::to_string_pretty(settings) else {
            return false;
        };
        let mut tmp_os = path.as_os_str().to_os_string();
        tmp_os.push(".tmp");
        let tmp_path = PathBuf::from(tmp_os);
        if fs::write(&tmp_path, json).is_err() {
            return false;
        }
        // Atomic rename with Windows overwrite support
        if fs::rename(&tmp_path, &path).is_err() {
            // Fallback if target exists on some filesystems
            let _ = fs::remove_file(&path);
            if fs::rename(&tmp_path, &path).is_err() {
                let _ = fs::remove_file(&tmp_path);
                return false;
            }
        }
        true
    }
}
