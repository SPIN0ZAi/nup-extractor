#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};

use crate::settings::OrbSettings;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ScreenEdge {
    Left,
    Right,
    Top,
    Bottom,
}

impl ScreenEdge {
    pub fn as_str(&self) -> &'static str {
        match self {
            ScreenEdge::Left => "Left",
            ScreenEdge::Right => "Right",
            ScreenEdge::Top => "Top",
            ScreenEdge::Bottom => "Bottom",
        }
    }

    pub fn as_lower_str(&self) -> &'static str {
        match self {
            ScreenEdge::Left => "left",
            ScreenEdge::Right => "right",
            ScreenEdge::Top => "top",
            ScreenEdge::Bottom => "bottom",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PixelPoint {
    pub x: i32,
    pub y: i32,
}

impl PixelPoint {
    pub const fn new(x: i32, y: i32) -> Self {
        Self { x, y }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PixelRect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

impl PixelRect {
    pub const fn new(x: i32, y: i32, width: i32, height: i32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    pub const fn left(&self) -> i32 {
        self.x
    }

    pub const fn top(&self) -> i32 {
        self.y
    }

    pub const fn right(&self) -> i32 {
        self.x + self.width
    }

    pub const fn bottom(&self) -> i32 {
        self.y + self.height
    }

    pub const fn center_x(&self) -> i32 {
        self.x + (self.width / 2)
    }

    pub const fn center_y(&self) -> i32 {
        self.y + (self.height / 2)
    }

    pub const fn contains(&self, p: PixelPoint) -> bool {
        p.x >= self.left() && p.x < self.right() && p.y >= self.top() && p.y < self.bottom()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DipSize {
    pub width: f64,
    pub height: f64,
}

impl DipSize {
    pub const fn new(width: f64, height: f64) -> Self {
        Self { width, height }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MonitorWorkArea {
    pub device_name: String,
    pub bounds: PixelRect,
    pub work_area: PixelRect,
    pub dpi_scale: f64,
    pub is_primary: bool,
}

impl MonitorWorkArea {
    pub fn new(
        device_name: impl Into<String>,
        bounds: PixelRect,
        work_area: PixelRect,
        dpi_scale: f64,
        is_primary: bool,
    ) -> Self {
        Self {
            device_name: device_name.into(),
            bounds,
            work_area,
            dpi_scale,
            is_primary,
        }
    }
}

pub struct OrbGeometry;

impl OrbGeometry {
    pub const DEFAULT_ORB_DIP_SIZE: f64 = 64.0;
    pub const DEFAULT_EXPANDED_RADIUS_DIP: f64 = 28.0;
    pub const DEFAULT_EDGE_MARGIN_DIP: f64 = 14.0;
    pub const DEFAULT_EXPANDED_MARGIN_DIP: f64 = 18.0;
    pub const DEFAULT_EXPANDED_WIDTH_DIP: f64 = 1240.0;
    pub const DEFAULT_EXPANDED_HEIGHT_DIP: f64 = 820.0;
    pub const MIN_EXPANDED_WIDTH_DIP: f64 = 760.0;
    pub const MIN_EXPANDED_HEIGHT_DIP: f64 = 520.0;

    pub fn dip_to_physical(dips: f64, dpi_scale: f64) -> i32 {
        let safe_scale = if dpi_scale > 0.25 { dpi_scale } else { 1.0 };
        ((dips * safe_scale).round() as i32).max(1)
    }

    pub fn physical_to_dip(pixels: f64, dpi_scale: f64) -> f64 {
        let safe_scale = if dpi_scale > 0.25 { dpi_scale } else { 1.0 };
        pixels / safe_scale
    }

    pub fn compute_orb_physical_size(orb_dip_size: f64, dpi_scale: f64) -> i32 {
        let sz = if orb_dip_size > 0.0 {
            orb_dip_size
        } else {
            Self::DEFAULT_ORB_DIP_SIZE
        };
        Self::dip_to_physical(sz, dpi_scale)
    }

    pub fn find_monitor_for_point(
        point: PixelPoint,
        monitors: &[MonitorWorkArea],
    ) -> MonitorWorkArea {
        if monitors.is_empty() {
            return MonitorWorkArea::new(
                "DEFAULT",
                PixelRect::new(0, 0, 1920, 1080),
                PixelRect::new(0, 0, 1920, 1040),
                1.0,
                true,
            );
        }

        // 1. Exact containment inside monitor bounds
        for m in monitors {
            if m.bounds.contains(point) {
                return m.clone();
            }
        }

        // 2. Nearest monitor by squared distance to monitor bounds
        let mut nearest = &monitors[0];
        let mut best_dist_sq = f64::MAX;
        for m in monitors {
            let max_x = (m.bounds.right() - 1).max(m.bounds.left());
            let max_y = (m.bounds.bottom() - 1).max(m.bounds.top());
            let cx = point.x.clamp(m.bounds.left(), max_x);
            let cy = point.y.clamp(m.bounds.top(), max_y);
            let dx = (point.x - cx) as f64;
            let dy = (point.y - cy) as f64;
            let d2 = dx * dx + dy * dy;
            if d2 < best_dist_sq {
                best_dist_sq = d2;
                nearest = m;
            }
        }
        nearest.clone()
    }

    pub fn clamp_orb_to_work_area(
        orb_rect: PixelRect,
        work_area: PixelRect,
        edge_margin_px: i32,
    ) -> PixelRect {
        let w = orb_rect.width.min(work_area.width.max(1));
        let h = orb_rect.height.min(work_area.height.max(1));

        let eff_margin_x = edge_margin_px
            .max(0)
            .min(((work_area.width - w) / 2).max(0));
        let eff_margin_y = edge_margin_px
            .max(0)
            .min(((work_area.height - h) / 2).max(0));

        let min_x = work_area.left() + eff_margin_x;
        let max_x = work_area.right() - eff_margin_x - w;
        let min_y = work_area.top() + eff_margin_y;
        let max_y = work_area.bottom() - eff_margin_y - h;

        let x = orb_rect.x.clamp(min_x, max_x.max(min_x));
        let y = orb_rect.y.clamp(min_y, max_y.max(min_y));

        PixelRect::new(x, y, w, h)
    }

    pub fn snap_orb_to_nearest_edge(
        orb_rect: PixelRect,
        work_area: PixelRect,
        edge_margin_px: i32,
    ) -> (PixelRect, ScreenEdge) {
        let clamped = Self::clamp_orb_to_work_area(orb_rect, work_area, edge_margin_px);
        let eff_margin_x = edge_margin_px
            .max(0)
            .min(((work_area.width - clamped.width) / 2).max(0));
        let eff_margin_y = edge_margin_px
            .max(0)
            .min(((work_area.height - clamped.height) / 2).max(0));

        let min_x = work_area.left() + eff_margin_x;
        let max_x = (work_area.right() - eff_margin_x - clamped.width).max(min_x);
        let min_y = work_area.top() + eff_margin_y;
        let max_y = (work_area.bottom() - eff_margin_y - clamped.height).max(min_y);

        let dist_left = (clamped.x - min_x).abs();
        let dist_right = (max_x - clamped.x).abs();
        let dist_top = (clamped.y - min_y).abs();
        let dist_bottom = (max_y - clamped.y).abs();

        // Prefer Right on exact tie, then Left, then Top, then Bottom (AssistiveTouch feel)
        let mut best_edge = ScreenEdge::Right;
        let mut best_dist = dist_right;

        if dist_left < best_dist {
            best_dist = dist_left;
            best_edge = ScreenEdge::Left;
        }
        if dist_top < best_dist {
            best_dist = dist_top;
            best_edge = ScreenEdge::Top;
        }
        if dist_bottom < best_dist {
            best_edge = ScreenEdge::Bottom;
        }

        let snapped = match best_edge {
            ScreenEdge::Left => PixelRect::new(min_x, clamped.y, clamped.width, clamped.height),
            ScreenEdge::Right => PixelRect::new(max_x, clamped.y, clamped.width, clamped.height),
            ScreenEdge::Top => PixelRect::new(clamped.x, min_y, clamped.width, clamped.height),
            ScreenEdge::Bottom => PixelRect::new(clamped.x, max_y, clamped.width, clamped.height),
        };

        (snapped, best_edge)
    }

    pub fn compute_expanded_rect_near_orb(
        orb_rect: PixelRect,
        desired_expanded_dip: DipSize,
        monitor: &MonitorWorkArea,
        margin_dip: f64,
    ) -> PixelRect {
        let work = monitor.work_area;
        let margin_px = Self::dip_to_physical(margin_dip, monitor.dpi_scale);
        let desired_w = Self::dip_to_physical(desired_expanded_dip.width, monitor.dpi_scale);
        let desired_h = Self::dip_to_physical(desired_expanded_dip.height, monitor.dpi_scale);

        let max_avail_w = (work.width - (margin_px * 2)).max(200);
        let max_avail_h = (work.height - (margin_px * 2)).max(200);

        let w = desired_w.clamp(320_i32.min(max_avail_w), max_avail_w);
        let h = desired_h.clamp(240_i32.min(max_avail_h), max_avail_h);

        let min_x = work.left() + margin_px;
        let max_x = (work.right() - margin_px - w).max(min_x);
        let min_y = work.top() + margin_px;
        let max_y = (work.bottom() - margin_px - h).max(min_y);

        let rel_x = if work.width > 0 {
            (orb_rect.center_x() - work.left()) as f64 / work.width as f64
        } else {
            0.5
        };
        let rel_y = if work.height > 0 {
            (orb_rect.center_y() - work.top()) as f64 / work.height as f64
        } else {
            0.5
        };

        let target_x = if rel_x < 0.35 {
            orb_rect.left()
        } else if rel_x > 0.65 {
            orb_rect.right() - w
        } else {
            orb_rect.center_x() - (w / 2)
        };

        let target_y = if rel_y < 0.30 {
            orb_rect.top()
        } else if rel_y > 0.70 {
            orb_rect.bottom() - h
        } else {
            orb_rect.center_y() - (h / 2)
        };

        let clamped_x = target_x.clamp(min_x, max_x);
        let clamped_y = target_y.clamp(min_y, max_y);

        PixelRect::new(clamped_x, clamped_y, w, h)
    }

    pub fn resolve_initial_orb_position(
        settings: &OrbSettings,
        monitors: &[MonitorWorkArea],
        orb_dip_size: f64,
        edge_margin_dip: f64,
    ) -> (PixelRect, MonitorWorkArea, ScreenEdge) {
        let mut primary = if !monitors.is_empty() {
            monitors[0].clone()
        } else {
            MonitorWorkArea::new(
                "DEFAULT",
                PixelRect::new(0, 0, 1920, 1080),
                PixelRect::new(0, 0, 1920, 1040),
                1.0,
                true,
            )
        };
        for m in monitors {
            if m.is_primary {
                primary = m.clone();
                break;
            }
        }

        let mut chosen = primary;
        let mut matched_saved_monitor = false;

        if !settings.monitor_device_name.trim().is_empty() {
            for m in monitors {
                if m.device_name
                    .eq_ignore_ascii_case(&settings.monitor_device_name)
                {
                    chosen = m.clone();
                    matched_saved_monitor = true;
                    break;
                }
            }
        }

        if !matched_saved_monitor && settings.has_saved_position {
            let pt = PixelPoint::new(settings.orb_center_x, settings.orb_center_y);
            for m in monitors {
                if m.bounds.contains(pt) {
                    chosen = m.clone();
                    matched_saved_monitor = true;
                    break;
                }
            }
        }

        let orb_px = Self::compute_orb_physical_size(orb_dip_size, chosen.dpi_scale);
        let margin_px = Self::dip_to_physical(edge_margin_dip, chosen.dpi_scale);
        let work = chosen.work_area;

        let candidate = if settings.has_saved_position && matched_saved_monitor {
            PixelRect::new(
                settings.orb_center_x - (orb_px / 2),
                settings.orb_center_y - (orb_px / 2),
                orb_px,
                orb_px,
            )
        } else if settings.has_saved_position {
            let frac_x = settings.anchor_fraction_x.clamp(0.0, 1.0);
            let frac_y = settings.anchor_fraction_y.clamp(0.0, 1.0);
            let cx = work.left() + (frac_x * work.width as f64).round_ties_even() as i32;
            let cy = work.top() + (frac_y * work.height as f64).round_ties_even() as i32;
            match settings.orb_edge {
                ScreenEdge::Left => {
                    PixelRect::new(work.left() + margin_px, cy - (orb_px / 2), orb_px, orb_px)
                }
                ScreenEdge::Right => PixelRect::new(
                    work.right() - margin_px - orb_px,
                    cy - (orb_px / 2),
                    orb_px,
                    orb_px,
                ),
                ScreenEdge::Top => {
                    PixelRect::new(cx - (orb_px / 2), work.top() + margin_px, orb_px, orb_px)
                }
                ScreenEdge::Bottom => PixelRect::new(
                    cx - (orb_px / 2),
                    work.bottom() - margin_px - orb_px,
                    orb_px,
                    orb_px,
                ),
            }
        } else {
            let default_y =
                work.top() + (work.height as f64 * 0.38).round_ties_even() as i32 - (orb_px / 2);
            PixelRect::new(work.right() - margin_px - orb_px, default_y, orb_px, orb_px)
        };

        let (snapped, edge) = Self::snap_orb_to_nearest_edge(candidate, work, margin_px);
        (snapped, chosen, edge)
    }

    pub fn compute_anchor_fractions(orb_rect: PixelRect, work_area: PixelRect) -> (f64, f64) {
        let fx = if work_area.width > 0 {
            (orb_rect.center_x() - work_area.left()) as f64 / work_area.width as f64
        } else {
            0.95
        };
        let fy = if work_area.height > 0 {
            (orb_rect.center_y() - work_area.top()) as f64 / work_area.height as f64
        } else {
            0.38
        };
        (fx.clamp(0.0, 1.0), fy.clamp(0.0, 1.0))
    }

    pub fn union(a: PixelRect, b: PixelRect) -> PixelRect {
        let left = a.left().min(b.left());
        let top = a.top().min(b.top());
        let right = a.right().max(b.right());
        let bottom = a.bottom().max(b.bottom());
        PixelRect::new(left, top, (right - left).max(1), (bottom - top).max(1))
    }

    pub fn interpolate_rect(from: PixelRect, to: PixelRect, t: f64) -> PixelRect {
        let clamped = t.clamp(0.0, 1.0);
        let x = (from.x as f64 + ((to.x - from.x) as f64 * clamped)).round_ties_even() as i32;
        let y = (from.y as f64 + ((to.y - from.y) as f64 * clamped)).round_ties_even() as i32;
        let w = ((from.width as f64 + ((to.width - from.width) as f64 * clamped)).round_ties_even()
            as i32)
            .max(1);
        let h = ((from.height as f64 + ((to.height - from.height) as f64 * clamped))
            .round_ties_even() as i32)
            .max(1);
        PixelRect::new(x, y, w, h)
    }

    pub fn interpolate_corner_radius(orb_radius_px: f64, expanded_radius_px: f64, t: f64) -> f64 {
        let clamped = t.clamp(0.0, 1.0);
        orb_radius_px + ((expanded_radius_px - orb_radius_px) * clamped)
    }
}
