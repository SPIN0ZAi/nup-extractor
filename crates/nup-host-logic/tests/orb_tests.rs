#![forbid(unsafe_code)]

use std::fs;
use std::path::PathBuf;

use nup_host_logic::{
    DipSize, MonitorWorkArea, OrbDragThreshold, OrbGeometry, OrbLaunchMode, OrbSettings,
    OrbSettingsStore, OrbStateMachine, OrbWindowState, PixelPoint, PixelRect, ScreenEdge,
};
use serde_json::Value;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

#[test]
fn golden_replay_orb_host_logic() {
    let golden_path = repo_root()
        .join("tests_golden")
        .join("host")
        .join("orb_logic.json");
    let raw = fs::read_to_string(&golden_path).expect("read orb_logic.json");
    let golden: Value = serde_json::from_str(&raw).expect("parse orb_logic.json");

    // 1. Expand trace
    let mut sm = OrbStateMachine::new(OrbWindowState::Collapsed, false);
    assert!(sm.request_expand(0.0));
    let expand_trace = golden["expandTrace"].as_array().unwrap();
    for entry in expand_trace {
        let t = entry["tMs"].as_f64().unwrap();
        let snap = sm.tick(t);
        assert_eq!(snap.state.as_str(), entry["state"].as_str().unwrap());
        let expected_prog = entry["progress"].as_f64().unwrap();
        let actual_prog = (snap.progress * 1_000_000.0).round() / 1_000_000.0;
        assert!(
            (actual_prog - expected_prog).abs() <= 1e-6,
            "t={t}: expected {expected_prog}, got {actual_prog}"
        );
        assert_eq!(snap.is_animating, entry["isAnimating"].as_bool().unwrap());
    }

    // 2. Interrupt trace
    let mut sm_int = OrbStateMachine::new(OrbWindowState::Collapsed, false);
    sm_int.request_expand(0.0);
    for t in (16..=128).step_by(16) {
        sm_int.tick(t as f64);
    }
    let mid_prog = sm_int.progress();
    sm_int.request_collapse(128.0);
    let interrupt_trace = golden["interruptTrace"].as_array().unwrap();
    let first = &interrupt_trace[0];
    assert_eq!(sm_int.state().as_str(), first["state"].as_str().unwrap());
    assert!((mid_prog - first["progress"].as_f64().unwrap()).abs() <= 1e-6);
    for entry in &interrupt_trace[1..] {
        let t = entry["tMs"].as_f64().unwrap();
        let snap = sm_int.tick(t);
        assert_eq!(snap.state.as_str(), entry["state"].as_str().unwrap());
        let expected_prog = entry["progress"].as_f64().unwrap();
        assert!(
            (snap.progress - expected_prog).abs() <= 1e-6,
            "interrupt t={t}: expected {expected_prog}, got {}",
            snap.progress
        );
        assert_eq!(snap.is_animating, entry["isAnimating"].as_bool().unwrap());
    }

    // 3. Reduced motion trace
    let mut sm_red = OrbStateMachine::new(OrbWindowState::Collapsed, true);
    sm_red.request_expand(0.0);
    for entry in golden["reducedMotionTrace"].as_array().unwrap() {
        let t = entry["tMs"].as_f64().unwrap();
        let snap = sm_red.tick(t);
        assert_eq!(snap.state.as_str(), entry["state"].as_str().unwrap());
        assert!((snap.progress - entry["progress"].as_f64().unwrap()).abs() <= 1e-6);
        assert_eq!(snap.is_animating, entry["isAnimating"].as_bool().unwrap());
    }

    // 4. DPI table
    for entry in golden["dpiTable"].as_array().unwrap() {
        let scale = entry["dpiScale"].as_f64().unwrap();
        assert_eq!(
            OrbGeometry::compute_orb_physical_size(64.0, scale),
            entry["orbPx"].as_i64().unwrap() as i32
        );
        assert_eq!(
            OrbGeometry::dip_to_physical(14.0, scale),
            entry["marginPx"].as_i64().unwrap() as i32
        );
        assert_eq!(
            OrbGeometry::dip_to_physical(28.0, scale),
            entry["expandedRadiusPx"].as_i64().unwrap() as i32
        );
    }

    // 5. Snap cases
    for entry in golden["snapCases"].as_array().unwrap() {
        let work = parse_rect(&entry["work"]);
        let orb = parse_rect(&entry["orb"]);
        let margin = entry["margin"].as_i64().unwrap() as i32;
        let expected_snapped = parse_rect(&entry["snapped"]);
        let expected_edge = entry["edge"].as_str().unwrap();

        let (snapped, edge) = OrbGeometry::snap_orb_to_nearest_edge(orb, work, margin);
        assert_eq!(snapped, expected_snapped, "label={}", entry["label"]);
        assert_eq!(edge.as_str(), expected_edge, "label={}", entry["label"]);
    }

    // 6. Expand near orb cases
    for entry in golden["expandNearCases"].as_array().unwrap() {
        let orb = parse_rect(&entry["orb"]);
        let mon = parse_monitor(&entry["monitor"]);
        let expected = parse_rect(&entry["expanded"]);
        let actual = OrbGeometry::compute_expanded_rect_near_orb(
            orb,
            DipSize::new(1240.0, 820.0),
            &mon,
            OrbGeometry::DEFAULT_EXPANDED_MARGIN_DIP,
        );
        assert_eq!(actual, expected, "label={}", entry["label"]);
    }

    // 7. Settings deserialization from C# golden
    let parsed_settings: OrbSettings =
        serde_json::from_value(golden["sampleSettings"].clone()).expect("deserialize OrbSettings");
    assert!(parsed_settings.has_saved_position);
    assert_eq!(parsed_settings.orb_center_x, 1874);
    assert_eq!(parsed_settings.orb_center_y, 392);
    assert_eq!(parsed_settings.orb_edge, ScreenEdge::Right);
    assert_eq!(parsed_settings.monitor_device_name, r"\\.\DISPLAY1");
    assert_eq!(parsed_settings.last_mode, "Collapsed");
    assert_eq!(parsed_settings.launch_mode, OrbLaunchMode::Collapsed);
    assert_eq!(parsed_settings.theme, "dark");
}

fn parse_rect(v: &Value) -> PixelRect {
    PixelRect::new(
        v["x"].as_i64().unwrap() as i32,
        v["y"].as_i64().unwrap() as i32,
        v["width"].as_i64().unwrap() as i32,
        v["height"].as_i64().unwrap() as i32,
    )
}

fn parse_monitor(v: &Value) -> MonitorWorkArea {
    MonitorWorkArea::new(
        v["deviceName"].as_str().unwrap(),
        parse_rect(&v["bounds"]),
        parse_rect(&v["workArea"]),
        v["dpiScale"].as_f64().unwrap(),
        v["isPrimary"].as_bool().unwrap(),
    )
}

#[test]
fn state_machine_expand_and_collapse_settles_within_target_window() {
    let mut sm = OrbStateMachine::new(OrbWindowState::Collapsed, false);
    assert_eq!(sm.state(), OrbWindowState::Collapsed);
    assert_eq!(sm.progress(), 0.0);

    assert!(sm.request_expand(0.0));
    assert_eq!(sm.state(), OrbWindowState::Expanding);

    let mut prev = 0.0;
    let mut settled_ms = -1.0;
    for t in (16..=800).step_by(16) {
        let snap = sm.tick(t as f64);
        assert!(snap.progress >= prev - 0.0001);
        prev = snap.progress;
        if !snap.is_animating {
            settled_ms = t as f64;
            break;
        }
    }

    assert_eq!(sm.state(), OrbWindowState::Expanded);
    assert_eq!(sm.progress(), 1.0);
    assert!((350.0..=450.0).contains(&settled_ms));

    assert!(sm.request_collapse(1000.0));
    assert_eq!(sm.state(), OrbWindowState::Collapsing);

    let mut collapse_settled_ms = -1.0;
    for t in (1016..=1800).step_by(16) {
        let snap = sm.tick(t as f64);
        if !snap.is_animating {
            collapse_settled_ms = (t - 1000) as f64;
            break;
        }
    }

    assert_eq!(sm.state(), OrbWindowState::Collapsed);
    assert_eq!(sm.progress(), 0.0);
    assert!((350.0..=450.0).contains(&collapse_settled_ms));
}

#[test]
fn state_machine_interrupt_mid_expand_reverses_smoothly_without_jumping() {
    let mut sm = OrbStateMachine::new(OrbWindowState::Collapsed, false);
    sm.request_expand(0.0);

    let mut mid = sm.tick(0.0);
    for t in (16..=160).step_by(16) {
        mid = sm.tick(t as f64);
    }

    assert_eq!(mid.state, OrbWindowState::Expanding);
    assert!((0.25..=0.90).contains(&mid.progress));

    assert!(sm.request_collapse(160.0));
    let next_frame = sm.tick(176.0);
    assert!((next_frame.progress - mid.progress).abs() < 0.12);

    for t in (192..=1200).step_by(16) {
        if !sm.tick(t as f64).is_animating {
            break;
        }
    }

    assert_eq!(sm.state(), OrbWindowState::Collapsed);
    assert_eq!(sm.progress(), 0.0);
}

#[test]
fn state_machine_rapid_spamming_always_settles_cleanly() {
    let mut sm = OrbStateMachine::new(OrbWindowState::Collapsed, false);
    let mut now = 0.0;

    for _ in 0..12 {
        sm.toggle(now);
        for _ in 0..4 {
            now += 16.0;
            let snap = sm.tick(now);
            assert!(!snap.progress.is_nan());
            assert!((0.0..=1.0).contains(&snap.progress));
        }
    }

    sm.request_expand(now);
    for _ in 0..80 {
        now += 16.0;
        if !sm.tick(now).is_animating {
            break;
        }
    }

    assert_eq!(sm.state(), OrbWindowState::Expanded);
    assert_eq!(sm.progress(), 1.0);
}

#[test]
fn state_machine_reduced_motion_completes_quickly() {
    let mut sm = OrbStateMachine::new(OrbWindowState::Collapsed, true);
    sm.request_expand(0.0);

    let half = sm.tick(70.0);
    assert!(half.reduced_motion);
    assert!((0.45..=0.55).contains(&half.progress));

    let done = sm.tick(150.0);
    assert_eq!(done.state, OrbWindowState::Expanded);
    assert_eq!(done.progress, 1.0);
    assert!(!done.is_animating);
}

#[test]
fn drag_threshold_distinguishes_click_from_drag_at_4px() {
    let mut tracker = OrbDragThreshold::new(4.0);

    tracker.begin(100.0, 100.0);
    assert!(!tracker.update(102.0, 102.0));
    assert!(!tracker.is_dragging());
    assert!(tracker.end());

    tracker.begin(100.0, 100.0);
    assert!(!tracker.update(102.0, 101.0));
    assert!(tracker.update(103.0, 103.0));
    assert!(tracker.is_dragging());
    assert!(!tracker.end());
}

#[test]
fn mixed_dpi_orb_physical_size_scales_accurately() {
    for (dpi_scale, expected_px) in [(1.0, 64), (1.25, 80), (1.5, 96), (2.0, 128)] {
        assert_eq!(
            OrbGeometry::compute_orb_physical_size(64.0, dpi_scale),
            expected_px
        );
        assert!((OrbGeometry::physical_to_dip(expected_px as f64, dpi_scale) - 64.0).abs() < 1e-3);
    }
}

#[test]
fn snap_orb_to_nearest_edge_snaps_to_closest_work_area_edge() {
    let work_area = PixelRect::new(0, 0, 1920, 1032);
    let cases = [
        (200, 500, ScreenEdge::Left, 14),
        (1700, 500, ScreenEdge::Right, 1920 - 14 - 64),
        (960, 80, ScreenEdge::Top, 14),
        (960, 960, ScreenEdge::Bottom, 1032 - 14 - 64),
    ];
    for (x, y, expected_edge, expected_coord) in cases {
        let orb = PixelRect::new(x, y, 64, 64);
        let (snapped, edge) = OrbGeometry::snap_orb_to_nearest_edge(orb, work_area, 14);
        assert_eq!(edge, expected_edge);
        if matches!(expected_edge, ScreenEdge::Left | ScreenEdge::Right) {
            assert_eq!(snapped.x, expected_coord);
        } else {
            assert_eq!(snapped.y, expected_coord);
        }
        assert!(snapped.left() >= work_area.left());
        assert!(snapped.right() <= work_area.right());
        assert!(snapped.top() >= work_area.top());
        assert!(snapped.bottom() <= work_area.bottom());
    }
}

#[test]
fn snap_and_clamp_respects_taskbar_on_every_screen_edge() {
    let left_tb = PixelRect::new(72, 0, 1848, 1080);
    let (snap_l, edge_l) =
        OrbGeometry::snap_orb_to_nearest_edge(PixelRect::new(10, 400, 64, 64), left_tb, 14);
    assert_eq!(edge_l, ScreenEdge::Left);
    assert_eq!(snap_l.x, 72 + 14);

    let top_tb = PixelRect::new(0, 48, 1920, 1032);
    let (snap_t, edge_t) =
        OrbGeometry::snap_orb_to_nearest_edge(PixelRect::new(900, 5, 64, 64), top_tb, 14);
    assert_eq!(edge_t, ScreenEdge::Top);
    assert_eq!(snap_t.y, 48 + 14);

    let right_tb = PixelRect::new(0, 0, 1848, 1080);
    let (snap_r, edge_r) =
        OrbGeometry::snap_orb_to_nearest_edge(PixelRect::new(1890, 400, 64, 64), right_tb, 14);
    assert_eq!(edge_r, ScreenEdge::Right);
    assert_eq!(snap_r.x, 1848 - 14 - 64);

    let bottom_tb = PixelRect::new(0, 0, 1920, 1032);
    let (snap_b, edge_b) =
        OrbGeometry::snap_orb_to_nearest_edge(PixelRect::new(900, 1060, 64, 64), bottom_tb, 14);
    assert_eq!(edge_b, ScreenEdge::Bottom);
    assert_eq!(snap_b.y, 1032 - 14 - 64);
}

#[test]
fn multi_monitor_mixed_dpi_and_negative_offsets_resolves_and_expands() {
    let monitors = vec![
        MonitorWorkArea::new(
            r"\\.\DISPLAY1",
            PixelRect::new(0, 0, 1920, 1080),
            PixelRect::new(0, 0, 1920, 1032),
            1.0,
            true,
        ),
        MonitorWorkArea::new(
            r"\\.\DISPLAY2",
            PixelRect::new(1920, -116, 2560, 1440),
            PixelRect::new(1920, -116, 2560, 1380),
            1.5,
            false,
        ),
        MonitorWorkArea::new(
            r"\\.\DISPLAY3",
            PixelRect::new(-1366, 0, 1366, 768),
            PixelRect::new(-1366, 0, 1366, 728),
            1.25,
            false,
        ),
    ];

    let mon2 = OrbGeometry::find_monitor_for_point(PixelPoint::new(4400, 200), &monitors);
    assert_eq!(mon2.device_name, r"\\.\DISPLAY2");
    assert_eq!(mon2.dpi_scale, 1.5);

    let orb_px = OrbGeometry::compute_orb_physical_size(64.0, mon2.dpi_scale);
    assert_eq!(orb_px, 96);

    let orb2 = PixelRect::new(4400, 100, orb_px, orb_px);
    let exp2 = OrbGeometry::compute_expanded_rect_near_orb(
        orb2,
        DipSize::new(1240.0, 820.0),
        &mon2,
        OrbGeometry::DEFAULT_EXPANDED_MARGIN_DIP,
    );
    assert!(exp2.left() >= mon2.work_area.left());
    assert!(exp2.right() <= mon2.work_area.right());
    assert!(exp2.top() >= mon2.work_area.top());
    assert!(exp2.bottom() <= mon2.work_area.bottom());

    let mon3 = OrbGeometry::find_monitor_for_point(PixelPoint::new(-1300, 300), &monitors);
    assert_eq!(mon3.device_name, r"\\.\DISPLAY3");
    let orb3 = PixelRect::new(-1350, 300, 80, 80);
    let exp3 = OrbGeometry::compute_expanded_rect_near_orb(
        orb3,
        DipSize::new(1240.0, 820.0),
        &mon3,
        OrbGeometry::DEFAULT_EXPANDED_MARGIN_DIP,
    );
    assert!(exp3.left() >= mon3.work_area.left());
    assert!(exp3.right() <= mon3.work_area.right());
    assert!(exp3.top() >= mon3.work_area.top());
    assert!(exp3.bottom() <= mon3.work_area.bottom());
    assert!(exp3.width <= mon3.work_area.width);
    assert!(exp3.height <= mon3.work_area.height);
}

#[test]
fn resolve_initial_orb_position_clamps_to_primary_when_saved_monitor_disconnected() {
    let saved = OrbSettings {
        has_saved_position: true,
        orb_center_x: 3700,
        orb_center_y: 400,
        anchor_fraction_x: 0.95,
        anchor_fraction_y: 0.50,
        orb_edge: ScreenEdge::Right,
        monitor_device_name: r"\\.\DISPLAY_DISCONNECTED".to_string(),
        ..OrbSettings::default()
    };

    let current_monitors = vec![MonitorWorkArea::new(
        r"\\.\DISPLAY1",
        PixelRect::new(0, 0, 1920, 1080),
        PixelRect::new(0, 0, 1920, 1032),
        1.25,
        true,
    )];

    let (orb_rect, monitor, edge) = OrbGeometry::resolve_initial_orb_position(
        &saved,
        &current_monitors,
        OrbGeometry::DEFAULT_ORB_DIP_SIZE,
        OrbGeometry::DEFAULT_EDGE_MARGIN_DIP,
    );

    assert_eq!(monitor.device_name, r"\\.\DISPLAY1");
    assert_eq!(orb_rect.width, 80);
    assert_eq!(edge, ScreenEdge::Right);
    assert!(orb_rect.left() >= monitor.work_area.left());
    assert!(orb_rect.right() <= monitor.work_area.right());
    assert!(orb_rect.top() >= monitor.work_area.top());
    assert!(orb_rect.bottom() <= monitor.work_area.bottom());
}

#[test]
fn settings_store_round_trip_and_corrupt_fallback() {
    let tmp = tempfile::tempdir().unwrap();
    let file_path = tmp.path().join("orb-state.json");

    let original = OrbSettings {
        has_saved_position: true,
        orb_center_x: 1850,
        orb_center_y: 420,
        anchor_fraction_x: 0.96,
        anchor_fraction_y: 0.41,
        orb_edge: ScreenEdge::Right,
        monitor_device_name: r"\\.\DISPLAY1".to_string(),
        last_mode: "Expanded".to_string(),
        launch_mode: OrbLaunchMode::RememberLast,
        expanded_width_dip: 1320.0,
        expanded_height_dip: 860.0,
        theme: "light".to_string(),
    };

    assert!(OrbSettingsStore::save(&original, Some(&file_path)));
    assert!(file_path.exists());

    let mut loaded = OrbSettingsStore::load(Some(&file_path));
    assert_eq!(loaded, original);

    assert_eq!(
        loaded.resolve_startup_state(false),
        OrbWindowState::Expanded
    );
    loaded.launch_mode = OrbLaunchMode::Collapsed;
    assert_eq!(
        loaded.resolve_startup_state(false),
        OrbWindowState::Collapsed
    );
    assert_eq!(loaded.resolve_startup_state(true), OrbWindowState::Expanded);

    // Unknown future fields tolerated
    fs::write(
        &file_path,
        r#"{"theme":"light","futureField":123,"launchMode":"Expanded"}"#,
    )
    .unwrap();
    let tolerant = OrbSettingsStore::load(Some(&file_path));
    assert_eq!(tolerant.theme, "light");
    assert_eq!(tolerant.launch_mode, OrbLaunchMode::Expanded);

    // Corrupt JSON falls back safely to defaults
    fs::write(&file_path, "{ not valid json !!!").unwrap();
    let fallback = OrbSettingsStore::load(Some(&file_path));
    assert_eq!(fallback.launch_mode, OrbLaunchMode::Expanded);
    assert_eq!(fallback.theme, "dark");
    assert!(!fallback.has_saved_position);
}
