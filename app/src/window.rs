#![allow(unsafe_code)]

use std::ffi::c_void;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use nup_host_logic::{
    DipSize, MonitorWorkArea, OrbGeometry, OrbSettings, OrbSettingsStore, OrbStateMachine,
    OrbWindowState, PixelPoint, PixelRect, ScreenEdge,
};
use serde_json::{Value, json};
use tao::dpi::{PhysicalPosition, PhysicalSize};
use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy};
use tao::platform::windows::WindowExtWindows;
use tao::window::WindowBuilder;
use webview2_com::Microsoft::Web::WebView2::Win32::{
    COREWEBVIEW2_COLOR, COREWEBVIEW2_WEB_RESOURCE_CONTEXT_ALL, ICoreWebView2, ICoreWebView2_2,
    ICoreWebView2Controller2, ICoreWebView2Controller4, ICoreWebView2File,
    ICoreWebView2WebMessageReceivedEventArgs2,
};
use webview2_com::{
    NavigationCompletedEventHandler, WebMessageReceivedEventHandler,
    WebResourceRequestedEventHandler, take_pwstr,
};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::Shell::{DefSubclassProc, SHCreateMemStream, SetWindowSubclass};
use windows::Win32::UI::WindowsAndMessaging::{
    GWL_STYLE, HTBOTTOM, HTBOTTOMLEFT, HTBOTTOMRIGHT, HTCLIENT, HTLEFT, HTRIGHT, HTTOP, HTTOPLEFT,
    HTTOPRIGHT, KillTimer, MINMAXINFO, SC_MINIMIZE, STYLESTRUCT, SW_RESTORE, SetForegroundWindow,
    SetTimer, ShowWindow, WM_DISPLAYCHANGE, WM_DPICHANGED, WM_ERASEBKGND, WM_EXITSIZEMOVE,
    WM_GETMINMAXINFO, WM_NCACTIVATE, WM_NCCALCSIZE, WM_NCHITTEST, WM_NCPAINT, WM_SETTINGCHANGE,
    WM_SIZE, WM_STYLECHANGING, WM_SYSCOMMAND, WM_TIMER,
};
use windows::core::{HSTRING, Interface, PCWSTR, PWSTR, w};
use wry::{WebContext, WebViewBuilder, WebViewExtWindows};

use crate::assets;
use crate::backdrop::{self, SystemAppearanceState};
use crate::ipc::{
    IpcRouter, IpcSink, WindowCommand, apply_orb_settings_update, build_orb_settings_reply,
    run_extract_single_file_worker,
};
use crate::tray::{TrayCommand, TrayIconManager, WM_TRAYICON};

const ANIM_TIMER_ID: usize = 7001;
const SNAP_DURATION_MS: f64 = 190.0;
const SUBCLASS_ID: usize = 4242;

#[derive(Debug)]
enum UserEvent {
    SendJson(String),
    WindowCmd(WindowCommand),
    NavigationCompleted,
    AnimTick,
    MinimizeIntercepted,
    WindowSizeChanged(i32, i32),
    ExitSizeMove,
    DpiChanged,
    SystemAppearanceChanged,
    DisplaySettingsChanged,
    TrayMsg(LPARAM),
    WebMessage {
        json: String,
        dropped_paths: Vec<String>,
    },
}

struct LiveSink {
    proxy: EventLoopProxy<UserEvent>,
}

impl IpcSink for LiveSink {
    fn send_json(&self, value: Value) {
        let _ = self
            .proxy
            .send_event(UserEvent::SendJson(value.to_string()));
    }

    fn dispatch_window(&self, cmd: WindowCommand) {
        let _ = self.proxy.send_event(UserEvent::WindowCmd(cmd));
    }
}

struct SubclassData {
    proxy: EventLoopProxy<UserEvent>,
    expanded_flag: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

unsafe extern "system" fn host_subclass_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _uid: usize,
    ref_data: usize,
) -> LRESULT {
    if ref_data == 0 {
        return unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) };
    }
    let data = unsafe { &*(ref_data as *const SubclassData) };

    match msg {
        WM_STYLECHANGING => {
            // Prevent tao or Windows from ever restoring WS_CAPTION / WS_SYSMENU / WS_BORDER,
            // which causes DWM to paint a ghost native titlebar + caption buttons behind Liquid Glass.
            if wparam.0 as i32 == GWL_STYLE.0 && lparam.0 != 0 {
                let ss = unsafe { &mut *(lparam.0 as *mut STYLESTRUCT) };
                ss.styleNew &= !backdrop::FORBIDDEN_FRAME_STYLES;
            }
        }
        WM_NCCALCSIZE => {
            if wparam.0 != 0 {
                // Make client area cover 100% of HWND (zero native frame/titlebar border)
                return LRESULT(0);
            }
        }
        WM_NCPAINT => {
            // Suppress all legacy non-client frame painting
            return LRESULT(0);
        }
        WM_NCACTIVATE => {
            // Suppress non-client titlebar repaint when window gains/loses focus
            return LRESULT(1);
        }
        WM_ERASEBKGND => {
            // Prevent white GDI background flash under transparent WebView2 + Acrylic
            return LRESULT(1);
        }
        WM_NCHITTEST => {
            if data
                .expanded_flag
                .load(std::sync::atomic::Ordering::Relaxed)
            {
                let x = (lparam.0 & 0xFFFF) as i16 as i32;
                let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;
                let rc = backdrop::get_window_bounds_physical(hwnd);
                let border = 6;
                let on_left = x >= rc.left() && x < rc.left() + border;
                let on_right = x <= rc.right() && x > rc.right() - border;
                let on_top = y >= rc.top() && y < rc.top() + border;
                let on_bottom = y <= rc.bottom() && y > rc.bottom() - border;
                let hit = match (on_left, on_right, on_top, on_bottom) {
                    (true, _, true, _) => HTTOPLEFT,
                    (_, true, true, _) => HTTOPRIGHT,
                    (true, _, _, true) => HTBOTTOMLEFT,
                    (_, true, _, true) => HTBOTTOMRIGHT,
                    (true, _, _, _) => HTLEFT,
                    (_, true, _, _) => HTRIGHT,
                    (_, _, true, _) => HTTOP,
                    (_, _, _, true) => HTBOTTOM,
                    _ => HTCLIENT,
                };
                if hit != HTCLIENT {
                    return LRESULT(hit as isize);
                }
            }
        }
        WM_SYSCOMMAND => {
            let cmd = (wparam.0 & 0xFFF0) as u32;
            if cmd == SC_MINIMIZE {
                let _ = data.proxy.send_event(UserEvent::MinimizeIntercepted);
                return LRESULT(0);
            }
        }
        WM_GETMINMAXINFO => {
            let mmi = unsafe { &mut *(lparam.0 as *mut MINMAXINFO) };
            let bounds = backdrop::get_window_bounds_physical(hwnd);
            let monitors = backdrop::get_all_monitors();
            let mon = OrbGeometry::find_monitor_for_point(
                PixelPoint::new(bounds.center_x(), bounds.center_y()),
                &monitors,
            );
            mmi.ptMaxPosition.x = mon.work_area.left() - mon.bounds.left();
            mmi.ptMaxPosition.y = mon.work_area.top() - mon.bounds.top();
            mmi.ptMaxSize.x = mon.work_area.width;
            mmi.ptMaxSize.y = mon.work_area.height;
            if data
                .expanded_flag
                .load(std::sync::atomic::Ordering::Relaxed)
            {
                mmi.ptMinTrackSize.x = OrbGeometry::dip_to_physical(
                    OrbGeometry::MIN_EXPANDED_WIDTH_DIP,
                    mon.dpi_scale,
                );
                mmi.ptMinTrackSize.y = OrbGeometry::dip_to_physical(
                    OrbGeometry::MIN_EXPANDED_HEIGHT_DIP,
                    mon.dpi_scale,
                );
            } else {
                mmi.ptMinTrackSize.x = 1;
                mmi.ptMinTrackSize.y = 1;
            }
            return LRESULT(0);
        }
        WM_SIZE => {
            let w = (lparam.0 & 0xFFFF) as i32;
            let h = ((lparam.0 >> 16) & 0xFFFF) as i32;
            if w > 100 && h > 100 {
                let _ = data.proxy.send_event(UserEvent::WindowSizeChanged(w, h));
            }
        }
        WM_EXITSIZEMOVE => {
            let _ = data.proxy.send_event(UserEvent::ExitSizeMove);
        }
        WM_DPICHANGED => {
            let _ = data.proxy.send_event(UserEvent::DpiChanged);
        }
        WM_SETTINGCHANGE => {
            let _ = data.proxy.send_event(UserEvent::SystemAppearanceChanged);
        }
        WM_DISPLAYCHANGE => {
            let _ = data.proxy.send_event(UserEvent::DisplaySettingsChanged);
        }
        WM_TIMER if wparam.0 == ANIM_TIMER_ID => {
            let _ = data.proxy.send_event(UserEvent::AnimTick);
            return LRESULT(0);
        }
        WM_TRAYICON => {
            let _ = data.proxy.send_event(UserEvent::TrayMsg(lparam));
            return LRESULT(0);
        }
        _ => {}
    }

    unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) }
}

struct OrbHostState {
    hwnd: HWND,
    clock: Instant,
    settings: OrbSettings,
    state_machine: OrbStateMachine,
    appearance: SystemAppearanceState,
    web_view_ready: bool,
    anim_timer_active: bool,
    is_maximized: bool,

    orb_rect: PixelRect,
    expanded_rect: PixelRect,
    morph_host_rect: PixelRect,
    morph_host_active: bool,
    morph_warmup_frames: i32,
    orb_monitor: MonitorWorkArea,
    orb_edge: ScreenEdge,

    is_dragging_orb: bool,
    drag_grab_frac_x: f64,
    drag_grab_frac_y: f64,
    last_orb_region_size: i32,

    is_snapping_orb: bool,
    snap_start_rect: PixelRect,
    snap_target_rect: PixelRect,
    snap_start_ms: f64,

    expanded_flag: Arc<std::sync::atomic::AtomicBool>,
    tray: TrayIconManager,
    core_webview: ICoreWebView2,
}

impl OrbHostState {
    fn now_ms(&self) -> f64 {
        self.clock.elapsed().as_secs_f64() * 1000.0
    }

    fn send_json(&self, val: &Value) {
        if !self.web_view_ready {
            return;
        }
        let s = HSTRING::from(val.to_string());
        unsafe {
            let _ = self.core_webview.PostWebMessageAsJson(PCWSTR(s.as_ptr()));
        }
    }

    fn send_raw_json(&self, raw: &str) {
        let s = HSTRING::from(raw);
        unsafe {
            let _ = self.core_webview.PostWebMessageAsJson(PCWSTR(s.as_ptr()));
        }
    }

    fn apply_native_backdrop(&self) {
        let is_dark = !self.settings.theme.eq_ignore_ascii_case("light");
        if self.state_machine.state() == OrbWindowState::Expanded
            && !self.state_machine.is_animating()
        {
            backdrop::apply_backdrop(
                self.hwnd,
                is_dark,
                self.appearance.transparency_enabled,
                self.appearance.high_contrast,
            );
        } else {
            backdrop::disable_backdrop_for_orb(self.hwnd, is_dark);
        }
    }

    fn push_system_appearance(&self) {
        if !self.web_view_ready {
            return;
        }
        self.send_json(&json!({
            "type": "system_appearance",
            "reducedMotion": self.appearance.reduced_motion,
            "highContrast": self.appearance.high_contrast,
            "transparencyEnabled": self.appearance.transparency_enabled,
            "theme": self.settings.theme
        }));
    }

    fn refresh_system_appearance(&mut self) {
        self.appearance = backdrop::query_system_appearance();
        self.state_machine.reduced_motion = self.appearance.reduced_motion;
        self.apply_native_backdrop();
        self.push_system_appearance();
    }

    fn sync_expanded_flag(&self) {
        let is_exp = self.state_machine.state() == OrbWindowState::Expanded
            && !self.state_machine.is_animating();
        self.expanded_flag
            .store(is_exp, std::sync::atomic::Ordering::Relaxed);
    }

    fn attach_anim_timer(&mut self) {
        if self.anim_timer_active {
            return;
        }
        self.anim_timer_active = true;
        unsafe {
            let _ = SetTimer(Some(self.hwnd), ANIM_TIMER_ID, 8, None);
        }
    }

    fn detach_anim_timer(&mut self) {
        if !self.anim_timer_active {
            return;
        }
        self.anim_timer_active = false;
        unsafe {
            let _ = KillTimer(Some(self.hwnd), ANIM_TIMER_ID);
        }
    }

    fn apply_region_for_current_state(&mut self, rect: PixelRect, progress: f64) {
        if rect.width <= 0 || rect.height <= 0 {
            return;
        }
        if self.is_maximized && progress >= 0.99 {
            backdrop::clear_window_region(self.hwnd);
            return;
        }
        let dpi_scale =
            backdrop::get_dpi_scale_for_point(PixelPoint::new(rect.center_x(), rect.center_y()));
        let orb_radius_px = self.orb_rect.width as f64 / 2.0;
        let expanded_radius_px =
            OrbGeometry::dip_to_physical(OrbGeometry::DEFAULT_EXPANDED_RADIUS_DIP, dpi_scale)
                as f64;
        let radius_px =
            OrbGeometry::interpolate_corner_radius(orb_radius_px, expanded_radius_px, progress);
        let is_circle = progress <= 0.02 && (rect.width - rect.height).abs() <= 2;
        backdrop::apply_rounded_region(
            self.hwnd,
            rect.width,
            rect.height,
            radius_px.round() as i32,
            is_circle,
        );
        self.last_orb_region_size = if is_circle { rect.width } else { 0 };
    }

    fn apply_clipped_morph_region(&self, visible_screen_rect: PixelRect, progress: f64) {
        if visible_screen_rect.width <= 0 || visible_screen_rect.height <= 0 {
            return;
        }
        let rel_x = visible_screen_rect.x - self.morph_host_rect.x;
        let rel_y = visible_screen_rect.y - self.morph_host_rect.y;
        let dpi_scale = backdrop::get_dpi_scale_for_point(PixelPoint::new(
            visible_screen_rect.center_x(),
            visible_screen_rect.center_y(),
        ));
        let orb_radius_px = self.orb_rect.width as f64 / 2.0;
        let expanded_radius_px =
            OrbGeometry::dip_to_physical(OrbGeometry::DEFAULT_EXPANDED_RADIUS_DIP, dpi_scale)
                as f64;
        let radius_px =
            OrbGeometry::interpolate_corner_radius(orb_radius_px, expanded_radius_px, progress);
        let is_circle =
            progress <= 0.02 && (visible_screen_rect.width - visible_screen_rect.height).abs() <= 2;
        backdrop::apply_rounded_region_at(
            self.hwnd,
            rel_x,
            rel_y,
            visible_screen_rect.width,
            visible_screen_rect.height,
            radius_px.round() as i32,
            is_circle,
        );
    }

    fn send_morph_frame(&self, state: OrbWindowState, progress: f64, current_rect: PixelRect) {
        if !self.web_view_ready {
            return;
        }
        let scale = if self.orb_monitor.dpi_scale > 0.25 {
            self.orb_monitor.dpi_scale
        } else {
            1.0
        };
        let shell_x = if self.morph_host_active {
            OrbGeometry::physical_to_dip((current_rect.x - self.morph_host_rect.x) as f64, scale)
        } else {
            0.0
        };
        let shell_y = if self.morph_host_active {
            OrbGeometry::physical_to_dip((current_rect.y - self.morph_host_rect.y) as f64, scale)
        } else {
            0.0
        };
        let orb_rel_x = if self.morph_host_active {
            OrbGeometry::physical_to_dip((self.orb_rect.x - self.morph_host_rect.x) as f64, scale)
        } else {
            0.0
        };
        let orb_rel_y = if self.morph_host_active {
            OrbGeometry::physical_to_dip((self.orb_rect.y - self.morph_host_rect.y) as f64, scale)
        } else {
            0.0
        };
        let cur_w = OrbGeometry::physical_to_dip(current_rect.width as f64, scale);
        let cur_h = OrbGeometry::physical_to_dip(current_rect.height as f64, scale);
        let exp_w = OrbGeometry::physical_to_dip(self.expanded_rect.width as f64, scale);
        let exp_h = OrbGeometry::physical_to_dip(self.expanded_rect.height as f64, scale);
        let radius_dip = OrbGeometry::interpolate_corner_radius(
            OrbGeometry::DEFAULT_ORB_DIP_SIZE / 2.0,
            OrbGeometry::DEFAULT_EXPANDED_RADIUS_DIP,
            progress,
        );

        let r1 = |v: f64| (v * 10.0).round() / 10.0;
        let r2 = |v: f64| (v * 100.0).round() / 100.0;

        self.send_json(&json!({
            "type": "morph_frame",
            "state": state.as_lower_str(),
            "progress": progress,
            "radiusPx": r2(radius_dip),
            "shellX": r1(shell_x),
            "shellY": r1(shell_y),
            "orbRelX": r1(orb_rel_x),
            "orbRelY": r1(orb_rel_y),
            "width": r1(cur_w),
            "height": r1(cur_h),
            "expandedWidth": r1(exp_w),
            "expandedHeight": r1(exp_h),
            "reducedMotion": self.appearance.reduced_motion,
            "edge": self.orb_edge.as_lower_str()
        }));
    }

    fn enter_morph_host(&mut self, initial_visible_rect: PixelRect, initial_progress: f64) {
        self.morph_host_rect = OrbGeometry::union(self.orb_rect, self.expanded_rect);
        if !self.morph_host_active {
            self.morph_host_active = true;
            self.morph_warmup_frames = if initial_progress < 0.05 { 2 } else { 0 };
            backdrop::set_window_bounds_physical(
                self.hwnd,
                self.morph_host_rect,
                Some(true),
                false,
                false,
            );
        }
        self.apply_clipped_morph_region(initial_visible_rect, initial_progress);
        self.send_morph_frame(
            self.state_machine.state(),
            initial_progress,
            initial_visible_rect,
        );
    }

    fn start_expand(&mut self, tao_win: &tao::window::Window) {
        if matches!(
            self.state_machine.state(),
            OrbWindowState::Expanded | OrbWindowState::Expanding
        ) {
            return;
        }
        self.is_snapping_orb = false;
        self.is_dragging_orb = false;

        if self.state_machine.state() == OrbWindowState::Collapsed {
            let monitors = backdrop::get_all_monitors();
            self.orb_monitor = OrbGeometry::find_monitor_for_point(
                PixelPoint::new(self.orb_rect.center_x(), self.orb_rect.center_y()),
                &monitors,
            );
            self.expanded_rect = OrbGeometry::compute_expanded_rect_near_orb(
                self.orb_rect,
                DipSize::new(
                    self.settings.expanded_width_dip,
                    self.settings.expanded_height_dip,
                ),
                &self.orb_monitor,
                OrbGeometry::DEFAULT_EXPANDED_MARGIN_DIP,
            );
        }

        let _ = tao_win.set_skip_taskbar(false);
        backdrop::set_tool_window_style(self.hwnd, false);

        let now = self.now_ms();
        self.state_machine.request_expand(now);
        self.sync_expanded_flag();

        if self.appearance.reduced_motion {
            self.morph_host_active = false;
            backdrop::set_window_bounds_physical(
                self.hwnd,
                self.expanded_rect,
                Some(false),
                true,
                false,
            );
            self.apply_region_for_current_state(self.expanded_rect, 1.0);
        } else {
            let p = self.state_machine.progress();
            self.enter_morph_host(self.orb_rect, p);
        }
        self.attach_anim_timer();
    }

    fn start_collapse(&mut self, tao_win: &tao::window::Window) {
        if matches!(
            self.state_machine.state(),
            OrbWindowState::Collapsed | OrbWindowState::Collapsing
        ) {
            return;
        }
        self.is_snapping_orb = false;

        if self.is_maximized {
            tao_win.set_maximized(false);
            self.is_maximized = false;
            self.send_json(&json!({ "type": "window_maximized_changed", "maximized": false }));
        }

        if self.state_machine.state() == OrbWindowState::Expanded {
            let cur = backdrop::get_window_bounds_physical(self.hwnd);
            if cur.width > 200 && cur.height > 200 {
                self.expanded_rect = cur;
                let scale = backdrop::get_dpi_scale_for_point(PixelPoint::new(
                    cur.center_x(),
                    cur.center_y(),
                ));
                self.settings.expanded_width_dip = OrbGeometry::MIN_EXPANDED_WIDTH_DIP
                    .max(OrbGeometry::physical_to_dip(cur.width as f64, scale));
                self.settings.expanded_height_dip = OrbGeometry::MIN_EXPANDED_HEIGHT_DIP
                    .max(OrbGeometry::physical_to_dip(cur.height as f64, scale));
            }
        }

        let monitors = backdrop::get_all_monitors();
        self.orb_monitor = OrbGeometry::find_monitor_for_point(
            PixelPoint::new(self.orb_rect.center_x(), self.orb_rect.center_y()),
            &monitors,
        );
        let orb_px = OrbGeometry::compute_orb_physical_size(
            OrbGeometry::DEFAULT_ORB_DIP_SIZE,
            self.orb_monitor.dpi_scale,
        );
        let margin_px = OrbGeometry::dip_to_physical(
            OrbGeometry::DEFAULT_EDGE_MARGIN_DIP,
            self.orb_monitor.dpi_scale,
        );
        let candidate = PixelRect::new(
            self.orb_rect.center_x() - orb_px / 2,
            self.orb_rect.center_y() - orb_px / 2,
            orb_px,
            orb_px,
        );
        let (snapped, edge) =
            OrbGeometry::snap_orb_to_nearest_edge(candidate, self.orb_monitor.work_area, margin_px);
        self.orb_rect = snapped;
        self.orb_edge = edge;

        backdrop::set_topmost(self.hwnd, true);

        let now = self.now_ms();
        self.state_machine.request_collapse(now);
        self.sync_expanded_flag();

        // Disable rectangular DWM Acrylic immediately on collapse so SetWindowRgn clips cleanly
        // with zero ghost box or square corners while shrinking into the circular orb.
        let is_dark = !self.settings.theme.eq_ignore_ascii_case("light");
        backdrop::disable_backdrop_for_orb(self.hwnd, is_dark);

        // Direct HWND shrink with SWP_NOCOPYBITS prevents Windows 11 DWM Acrylic ghost backdrop!
        self.morph_host_active = false;
        self.morph_warmup_frames = 0;

        if !self.appearance.reduced_motion {
            let p = self.state_machine.progress();
            let current_rect = OrbGeometry::interpolate_rect(self.orb_rect, self.expanded_rect, p);
            backdrop::set_window_bounds_physical(self.hwnd, current_rect, Some(true), false, true);
            self.apply_region_for_current_state(current_rect, p);
            self.send_morph_frame(self.state_machine.state(), p, current_rect);
        }

        self.attach_anim_timer();
    }

    fn on_anim_tick(&mut self, tao_win: &tao::window::Window) {
        let now_ms = self.now_ms();
        let mut still_active = false;

        if self.state_machine.is_animating() {
            if self.morph_warmup_frames > 0 {
                self.morph_warmup_frames -= 1;
                if self.morph_warmup_frames == 0
                    && self.state_machine.state() == OrbWindowState::Expanding
                {
                    self.state_machine
                        .set_immediate_state(OrbWindowState::Collapsed);
                    self.state_machine.request_expand(now_ms);
                }
                return;
            }

            let snap = self.state_machine.tick(now_ms);
            let frame_rect;

            if self.appearance.reduced_motion {
                frame_rect = if snap.state == OrbWindowState::Collapsed {
                    self.orb_rect
                } else {
                    self.expanded_rect
                };
                if !snap.is_animating {
                    backdrop::set_window_bounds_physical(
                        self.hwnd,
                        frame_rect,
                        Some(snap.state == OrbWindowState::Collapsed),
                        snap.state == OrbWindowState::Expanded,
                        false,
                    );
                    self.apply_region_for_current_state(frame_rect, snap.progress);
                }
            } else if self.morph_host_active {
                frame_rect =
                    OrbGeometry::interpolate_rect(self.orb_rect, self.expanded_rect, snap.progress);
                self.apply_clipped_morph_region(frame_rect, snap.progress);
            } else {
                frame_rect =
                    OrbGeometry::interpolate_rect(self.orb_rect, self.expanded_rect, snap.progress);
                backdrop::set_window_bounds_physical(self.hwnd, frame_rect, None, false, true);
                self.apply_region_for_current_state(frame_rect, snap.progress);
            }

            self.send_morph_frame(snap.state, snap.progress, frame_rect);

            if !snap.is_animating {
                self.on_morph_settled(snap.state, tao_win);
            } else {
                still_active = true;
            }
        } else if self.is_snapping_orb {
            let elapsed = now_ms - self.snap_start_ms;
            let raw_t = if self.appearance.reduced_motion {
                1.0
            } else {
                (elapsed / SNAP_DURATION_MS).clamp(0.0, 1.0)
            };
            let eased_t = 1.0 - (1.0 - raw_t).powi(3);
            self.orb_rect =
                OrbGeometry::interpolate_rect(self.snap_start_rect, self.snap_target_rect, eased_t);
            backdrop::set_window_bounds_physical(
                self.hwnd,
                self.orb_rect,
                Some(true),
                false,
                false,
            );
            self.apply_region_for_current_state(self.orb_rect, 0.0);

            if raw_t >= 1.0 {
                self.is_snapping_orb = false;
                self.orb_rect = self.snap_target_rect;
                self.persist_current_state();
                self.send_morph_frame(OrbWindowState::Collapsed, 0.0, self.orb_rect);
            } else {
                still_active = true;
            }
        }

        if !still_active {
            self.detach_anim_timer();
        }
    }

    fn on_morph_settled(&mut self, final_state: OrbWindowState, tao_win: &tao::window::Window) {
        self.morph_host_active = false;
        self.sync_expanded_flag();
        if final_state == OrbWindowState::Expanded {
            backdrop::set_topmost(self.hwnd, false);
            let _ = tao_win.set_skip_taskbar(false);
            backdrop::set_tool_window_style(self.hwnd, false);
            backdrop::set_window_bounds_physical(
                self.hwnd,
                self.expanded_rect,
                Some(false),
                true,
                false,
            );
            self.apply_native_backdrop();
            self.apply_region_for_current_state(self.expanded_rect, 1.0);
            self.send_morph_frame(OrbWindowState::Expanded, 1.0, self.expanded_rect);
            unsafe {
                let _ = SetForegroundWindow(self.hwnd);
            }
            self.settings.last_mode = "Expanded".to_string();
            self.persist_current_state();
        } else if final_state == OrbWindowState::Collapsed {
            backdrop::set_topmost(self.hwnd, true);
            let _ = tao_win.set_skip_taskbar(true);
            backdrop::set_tool_window_style(self.hwnd, true);
            self.apply_native_backdrop();
            backdrop::set_window_bounds_physical(
                self.hwnd,
                self.orb_rect,
                Some(true),
                false,
                false,
            );
            self.apply_region_for_current_state(self.orb_rect, 0.0);
            self.send_morph_frame(OrbWindowState::Collapsed, 0.0, self.orb_rect);
            self.settings.last_mode = "Collapsed".to_string();
            self.persist_current_state();
        }
    }

    fn handle_orb_drag_start(&mut self) {
        if self.state_machine.state() != OrbWindowState::Collapsed
            || self.state_machine.is_animating()
        {
            return;
        }
        self.is_snapping_orb = false;
        self.is_dragging_orb = true;
        let cursor = backdrop::get_cursor_position_physical();
        let win_rect = backdrop::get_window_bounds_physical(self.hwnd);
        if win_rect.width > 0 && win_rect.height > 0 {
            self.orb_rect = win_rect;
            self.drag_grab_frac_x =
                ((cursor.x - win_rect.x) as f64 / win_rect.width as f64).clamp(0.1, 0.9);
            self.drag_grab_frac_y =
                ((cursor.y - win_rect.y) as f64 / win_rect.height as f64).clamp(0.1, 0.9);
        } else {
            self.drag_grab_frac_x = 0.5;
            self.drag_grab_frac_y = 0.5;
        }
    }

    fn handle_orb_drag_move(&mut self) {
        if !self.is_dragging_orb || self.state_machine.state() != OrbWindowState::Collapsed {
            return;
        }
        let cursor = backdrop::get_cursor_position_physical();
        let monitors = backdrop::get_all_monitors();
        let mon = OrbGeometry::find_monitor_for_point(cursor, &monitors);
        let orb_px = OrbGeometry::compute_orb_physical_size(
            OrbGeometry::DEFAULT_ORB_DIP_SIZE,
            mon.dpi_scale,
        );
        let new_x = cursor.x - (self.drag_grab_frac_x * orb_px as f64).round() as i32;
        let new_y = cursor.y - (self.drag_grab_frac_y * orb_px as f64).round() as i32;
        self.orb_rect = PixelRect::new(new_x, new_y, orb_px, orb_px);
        self.orb_monitor = mon;

        backdrop::set_window_bounds_physical(self.hwnd, self.orb_rect, Some(true), false, false);
        if self.last_orb_region_size != orb_px {
            self.apply_region_for_current_state(self.orb_rect, 0.0);
        }
    }

    fn handle_orb_drag_end(&mut self) {
        if !self.is_dragging_orb {
            return;
        }
        self.is_dragging_orb = false;
        let monitors = backdrop::get_all_monitors();
        let mon = OrbGeometry::find_monitor_for_point(
            PixelPoint::new(self.orb_rect.center_x(), self.orb_rect.center_y()),
            &monitors,
        );
        let orb_px = OrbGeometry::compute_orb_physical_size(
            OrbGeometry::DEFAULT_ORB_DIP_SIZE,
            mon.dpi_scale,
        );
        let margin_px =
            OrbGeometry::dip_to_physical(OrbGeometry::DEFAULT_EDGE_MARGIN_DIP, mon.dpi_scale);
        let current_sized = PixelRect::new(self.orb_rect.x, self.orb_rect.y, orb_px, orb_px);
        let (snapped, edge) =
            OrbGeometry::snap_orb_to_nearest_edge(current_sized, mon.work_area, margin_px);

        self.orb_monitor = mon;
        self.orb_edge = edge;
        self.snap_start_rect = current_sized;
        self.snap_target_rect = snapped;
        self.snap_start_ms = self.now_ms();
        self.is_snapping_orb = true;
        self.attach_anim_timer();
    }

    fn persist_current_state(&mut self) {
        self.settings.has_saved_position = true;
        self.settings.orb_center_x = self.orb_rect.center_x();
        self.settings.orb_center_y = self.orb_rect.center_y();
        self.settings.orb_edge = self.orb_edge;
        self.settings.monitor_device_name = self.orb_monitor.device_name.clone();
        let (fx, fy) =
            OrbGeometry::compute_anchor_fractions(self.orb_rect, self.orb_monitor.work_area);
        self.settings.anchor_fraction_x = fx;
        self.settings.anchor_fraction_y = fy;
        self.settings.last_mode = if matches!(
            self.state_machine.state(),
            OrbWindowState::Expanded | OrbWindowState::Expanding
        ) {
            "Expanded".to_string()
        } else {
            "Collapsed".to_string()
        };
        OrbSettingsStore::save(&self.settings, None);
    }
}

pub fn run_app(
    initial_file_path: Option<String>,
    auto_extract_files: Vec<String>,
    startup_mode_override: Option<String>,
) -> Result<(), String> {
    crate::shell_integration::refresh_if_registered();
    let settings = OrbSettingsStore::load(None);
    let appearance = backdrop::query_system_appearance();

    let startup_state = match startup_mode_override.as_deref() {
        Some(m) if m.eq_ignore_ascii_case("expanded") => OrbWindowState::Expanded,
        Some(m) if m.eq_ignore_ascii_case("collapsed") => OrbWindowState::Collapsed,
        _ => settings
            .resolve_startup_state(initial_file_path.is_some() || !auto_extract_files.is_empty()),
    };

    let monitors = backdrop::get_all_monitors();
    let (orb_rect, orb_monitor, orb_edge) = OrbGeometry::resolve_initial_orb_position(
        &settings,
        &monitors,
        OrbGeometry::DEFAULT_ORB_DIP_SIZE,
        OrbGeometry::DEFAULT_EDGE_MARGIN_DIP,
    );
    let expanded_rect = OrbGeometry::compute_expanded_rect_near_orb(
        orb_rect,
        DipSize::new(settings.expanded_width_dip, settings.expanded_height_dip),
        &orb_monitor,
        OrbGeometry::DEFAULT_EXPANDED_MARGIN_DIP,
    );

    let start_expanded = startup_state == OrbWindowState::Expanded;
    let init_rect = if start_expanded {
        expanded_rect
    } else {
        orb_rect
    };

    let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    let proxy = event_loop.create_proxy();

    let window = WindowBuilder::new()
        .with_title("NUP Extractor")
        .with_decorations(false)
        .with_transparent(false) // CRITICAL: Plan B — never WS_EX_LAYERED!
        .with_resizable(start_expanded)
        .with_always_on_top(!start_expanded)
        .with_visible(false)
        .with_inner_size(PhysicalSize::new(
            init_rect.width.max(1) as u32,
            init_rect.height.max(1) as u32,
        ))
        .with_position(PhysicalPosition::new(init_rect.x, init_rect.y))
        .build(&event_loop)
        .map_err(|e| format!("Failed to create window: {e}"))?;

    let hwnd = HWND(window.hwnd() as *mut c_void);

    let expanded_flag = Arc::new(std::sync::atomic::AtomicBool::new(start_expanded));
    let subclass_box = Box::new(SubclassData {
        proxy: proxy.clone(),
        expanded_flag: Arc::clone(&expanded_flag),
    });
    let subclass_ptr = Box::into_raw(subclass_box) as usize;
    unsafe {
        let _ = SetWindowSubclass(hwnd, Some(host_subclass_proc), SUBCLASS_ID, subclass_ptr);
    }

    let is_dark = !settings.theme.eq_ignore_ascii_case("light");
    backdrop::configure_borderless_style(hwnd);
    if start_expanded {
        backdrop::apply_backdrop(
            hwnd,
            is_dark,
            appearance.transparency_enabled,
            appearance.high_contrast,
        );
    } else {
        backdrop::disable_backdrop_for_orb(hwnd, is_dark);
    }
    let _ = window.set_skip_taskbar(!start_expanded);
    backdrop::set_tool_window_style(hwnd, !start_expanded);
    backdrop::set_window_bounds_physical(
        hwnd,
        init_rect,
        Some(!start_expanded),
        start_expanded,
        false,
    );

    // WebView2 user data directory matching C# (%TEMP%\NupExtractor_WebView2Data)
    let user_data_dir = std::env::temp_dir().join("NupExtractor_WebView2Data");
    let mut web_context = WebContext::new(Some(user_data_dir));

    let webview = WebViewBuilder::new_with_web_context(&mut web_context)
        .with_transparent(true)
        .with_background_color((0, 0, 0, 0))
        .with_devtools(true)
        .with_url("about:blank")
        .build(&window)
        .map_err(|e| format!("Failed to initialize WebView2: {e}"))?;

    // Hook raw ICoreWebView2 directly for https://nup.local/* + AdditionalObjects + PostWebMessageAsJson
    let controller = webview.controller();
    if let Ok(c2) = controller.cast::<ICoreWebView2Controller2>() {
        unsafe {
            let _ = c2.SetDefaultBackgroundColor(COREWEBVIEW2_COLOR {
                A: 0,
                R: 0,
                G: 0,
                B: 0,
            });
        }
    }
    if let Ok(c4) = controller.cast::<ICoreWebView2Controller4>() {
        unsafe {
            let _ = c4.SetAllowExternalDrop(true);
        }
    }
    let core_webview = unsafe { controller.CoreWebView2() }
        .map_err(|e| format!("CoreWebView2 unavailable: {e}"))?;
    let core_v2 = core_webview
        .cast::<ICoreWebView2_2>()
        .map_err(|e| format!("ICoreWebView2_2 unavailable: {e}"))?;
    let env = unsafe { core_v2.Environment() }
        .map_err(|e| format!("CoreWebView2Environment unavailable: {e}"))?;

    // Disable status bar just like C#
    if let Ok(wv_settings) = unsafe { core_webview.Settings() } {
        unsafe {
            let _ = wv_settings.SetIsStatusBarEnabled(false);
            let _ = wv_settings.SetAreDevToolsEnabled(true);
        }
    }

    // 1. Register https://nup.local/* embedded resource handler
    unsafe {
        let _ = core_webview.AddWebResourceRequestedFilter(
            w!("https://nup.local/*"),
            COREWEBVIEW2_WEB_RESOURCE_CONTEXT_ALL,
        );
    }
    let env_for_res = env.clone();
    let res_handler = WebResourceRequestedEventHandler::create(Box::new(move |_sender, args| {
        let Some(args) = args else {
            return Ok(());
        };
        let req = unsafe { args.Request()? };
        let mut uri_pwstr = PWSTR::null();
        unsafe { req.Uri(&mut uri_pwstr)? };
        let uri_str = take_pwstr(uri_pwstr);
        let rel = uri_str
            .strip_prefix("https://nup.local/")
            .unwrap_or(uri_str.trim_start_matches("https://nup.local"));

        if let Some((bytes, mime)) = assets::get_asset(rel) {
            let stream = unsafe { SHCreateMemStream(Some(&bytes)) };
            let headers = HSTRING::from(format!(
                "Content-Type: {mime}\r\nAccess-Control-Allow-Origin: *\r\n"
            ));
            let resp = unsafe {
                env_for_res.CreateWebResourceResponse(
                    stream.as_ref(),
                    200,
                    w!("OK"),
                    PCWSTR(headers.as_ptr()),
                )?
            };
            unsafe { args.SetResponse(&resp)? };
        } else {
            let resp = unsafe {
                env_for_res.CreateWebResourceResponse(None, 404, w!("Not Found"), w!(""))?
            };
            unsafe { args.SetResponse(&resp)? };
        }
        Ok(())
    }));
    let mut res_token = 0i64;
    unsafe {
        let _ = core_webview.add_WebResourceRequested(&res_handler, &mut res_token);
    }

    // 2. Register WebMessageReceived (with AdditionalObjects -> ICoreWebView2File for file drops!)
    let proxy_msg = proxy.clone();
    let msg_handler = WebMessageReceivedEventHandler::create(Box::new(move |_sender, args| {
        let Some(args) = args else {
            return Ok(());
        };
        let mut json_pwstr = PWSTR::null();
        if unsafe { args.WebMessageAsJson(&mut json_pwstr) }.is_err() {
            return Ok(());
        }
        let json_str = take_pwstr(json_pwstr);

        let mut dropped_paths = Vec::new();
        if let Ok(args2) = args.cast::<ICoreWebView2WebMessageReceivedEventArgs2>()
            && let Ok(coll) = unsafe { args2.AdditionalObjects() }
        {
            let mut count = 0u32;
            if unsafe { coll.Count(&mut count) }.is_ok() {
                for i in 0..count {
                    if let Ok(obj) = unsafe { coll.GetValueAtIndex(i) }
                        && let Ok(file) = obj.cast::<ICoreWebView2File>()
                    {
                        let mut path_pwstr = PWSTR::null();
                        if unsafe { file.Path(&mut path_pwstr) }.is_ok() {
                            let p = take_pwstr(path_pwstr);
                            if !p.trim().is_empty() {
                                dropped_paths.push(p);
                            }
                        }
                    }
                }
            }
        }

        let _ = proxy_msg.send_event(UserEvent::WebMessage {
            json: json_str,
            dropped_paths,
        });
        Ok(())
    }));
    let mut msg_token = 0i64;
    unsafe {
        let _ = core_webview.add_WebMessageReceived(&msg_handler, &mut msg_token);
    }

    // 3. Register NavigationCompleted
    let proxy_nav = proxy.clone();
    let nav_handler = NavigationCompletedEventHandler::create(Box::new(move |_sender, _args| {
        let _ = proxy_nav.send_event(UserEvent::NavigationCompleted);
        Ok(())
    }));
    let mut nav_token = 0i64;
    unsafe {
        let _ = core_webview.add_NavigationCompleted(&nav_handler, &mut nav_token);
    }

    // 4. Navigate to embedded UI
    unsafe {
        let _ = core_webview.Navigate(w!("https://nup.local/index.html"));
    }

    let tray = TrayIconManager::new(hwnd);

    let mut host = OrbHostState {
        hwnd,
        clock: Instant::now(),
        settings,
        state_machine: OrbStateMachine::new(startup_state, appearance.reduced_motion),
        appearance,
        web_view_ready: false,
        anim_timer_active: false,
        is_maximized: false,
        orb_rect,
        expanded_rect,
        morph_host_rect: init_rect,
        morph_host_active: false,
        morph_warmup_frames: 0,
        orb_monitor,
        orb_edge,
        is_dragging_orb: false,
        drag_grab_frac_x: 0.5,
        drag_grab_frac_y: 0.5,
        last_orb_region_size: 0,
        is_snapping_orb: false,
        snap_start_rect: orb_rect,
        snap_target_rect: orb_rect,
        snap_start_ms: 0.0,
        expanded_flag,
        tray,
        core_webview,
    };

    host.apply_region_for_current_state(init_rect, if start_expanded { 1.0 } else { 0.0 });
    window.set_visible(true);
    let _ = window.set_skip_taskbar(!start_expanded);
    backdrop::set_tool_window_style(hwnd, !start_expanded);
    backdrop::set_window_bounds_physical(
        hwnd,
        init_rect,
        Some(!start_expanded),
        start_expanded,
        false,
    );

    let sink: Arc<dyn IpcSink> = Arc::new(LiveSink {
        proxy: proxy.clone(),
    });
    let router = Arc::new(IpcRouter::new(
        Arc::clone(&sink),
        initial_file_path,
        auto_extract_files,
    ));

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;
        // Keep webview alive for entire event loop lifetime
        let _keep_webview = &webview;

        match event {
            Event::WindowEvent {
                event: WindowEvent::CloseRequested,
                ..
            } => {
                host.detach_anim_timer();
                host.persist_current_state();
                router.cancel_all();
                *control_flow = ControlFlow::Exit;
            }
            Event::UserEvent(ue) => match ue {
                UserEvent::SendJson(raw) => host.send_raw_json(&raw),
                UserEvent::NavigationCompleted => {
                    host.web_view_ready = true;
                    host.push_system_appearance();
                    let rect = if host.state_machine.state() == OrbWindowState::Expanded {
                        host.expanded_rect
                    } else {
                        host.orb_rect
                    };
                    host.send_morph_frame(
                        host.state_machine.state(),
                        host.state_machine.progress(),
                        rect,
                    );
                }
                UserEvent::AnimTick => host.on_anim_tick(&window),
                UserEvent::MinimizeIntercepted => host.start_collapse(&window),
                UserEvent::WindowSizeChanged(w, h) => {
                    if host.state_machine.state() == OrbWindowState::Expanded
                        && !host.state_machine.is_animating()
                        && !host.is_maximized
                    {
                        let b = backdrop::get_window_bounds_physical(host.hwnd);
                        host.apply_region_for_current_state(PixelRect::new(b.x, b.y, w, h), 1.0);
                    }
                }
                UserEvent::ExitSizeMove => {
                    if host.state_machine.state() == OrbWindowState::Expanded
                        && !host.state_machine.is_animating()
                        && !host.is_maximized
                    {
                        let rect = backdrop::get_window_bounds_physical(host.hwnd);
                        if rect.width > 200 && rect.height > 200 {
                            host.expanded_rect = rect;
                            let scale = backdrop::get_dpi_scale_for_point(PixelPoint::new(
                                rect.center_x(),
                                rect.center_y(),
                            ));
                            host.settings.expanded_width_dip = OrbGeometry::MIN_EXPANDED_WIDTH_DIP
                                .max(OrbGeometry::physical_to_dip(rect.width as f64, scale));
                            host.settings.expanded_height_dip =
                                OrbGeometry::MIN_EXPANDED_HEIGHT_DIP
                                    .max(OrbGeometry::physical_to_dip(rect.height as f64, scale));
                            host.apply_region_for_current_state(rect, 1.0);
                            OrbSettingsStore::save(&host.settings, None);
                        }
                    }
                }
                UserEvent::DpiChanged => {
                    if host.state_machine.state() == OrbWindowState::Collapsed
                        && !host.is_dragging_orb
                        && !host.state_machine.is_animating()
                    {
                        let monitors = backdrop::get_all_monitors();
                        let mon = OrbGeometry::find_monitor_for_point(
                            PixelPoint::new(host.orb_rect.center_x(), host.orb_rect.center_y()),
                            &monitors,
                        );
                        let orb_px = OrbGeometry::compute_orb_physical_size(
                            OrbGeometry::DEFAULT_ORB_DIP_SIZE,
                            mon.dpi_scale,
                        );
                        let margin_px = OrbGeometry::dip_to_physical(
                            OrbGeometry::DEFAULT_EDGE_MARGIN_DIP,
                            mon.dpi_scale,
                        );
                        let resized = PixelRect::new(
                            host.orb_rect.center_x() - orb_px / 2,
                            host.orb_rect.center_y() - orb_px / 2,
                            orb_px,
                            orb_px,
                        );
                        let (snapped, edge) = OrbGeometry::snap_orb_to_nearest_edge(
                            resized,
                            mon.work_area,
                            margin_px,
                        );
                        host.orb_rect = snapped;
                        host.orb_edge = edge;
                        host.orb_monitor = mon;
                        backdrop::set_window_bounds_physical(
                            host.hwnd,
                            host.orb_rect,
                            Some(true),
                            false,
                            false,
                        );
                        host.apply_region_for_current_state(host.orb_rect, 0.0);
                    }
                }
                UserEvent::SystemAppearanceChanged => host.refresh_system_appearance(),
                UserEvent::DisplaySettingsChanged => {
                    let monitors = backdrop::get_all_monitors();
                    let (orb_r, orb_m, orb_e) = OrbGeometry::resolve_initial_orb_position(
                        &host.settings,
                        &monitors,
                        OrbGeometry::DEFAULT_ORB_DIP_SIZE,
                        OrbGeometry::DEFAULT_EDGE_MARGIN_DIP,
                    );
                    host.orb_rect = orb_r;
                    host.orb_monitor = orb_m;
                    host.orb_edge = orb_e;
                    host.expanded_rect = OrbGeometry::compute_expanded_rect_near_orb(
                        host.orb_rect,
                        DipSize::new(
                            host.settings.expanded_width_dip,
                            host.settings.expanded_height_dip,
                        ),
                        &host.orb_monitor,
                        OrbGeometry::DEFAULT_EXPANDED_MARGIN_DIP,
                    );
                    if host.state_machine.state() == OrbWindowState::Collapsed
                        && !host.is_dragging_orb
                        && !host.is_snapping_orb
                    {
                        backdrop::set_window_bounds_physical(
                            host.hwnd,
                            host.orb_rect,
                            Some(true),
                            false,
                            false,
                        );
                        host.apply_region_for_current_state(host.orb_rect, 0.0);
                        host.send_morph_frame(OrbWindowState::Collapsed, 0.0, host.orb_rect);
                    }
                }
                UserEvent::TrayMsg(lparam) => {
                    let is_exp = matches!(
                        host.state_machine.state(),
                        OrbWindowState::Expanded | OrbWindowState::Expanding
                    );
                    if let Some(cmd) = host.tray.handle_message(lparam, is_exp) {
                        match cmd {
                            TrayCommand::Open => {
                                if matches!(
                                    host.state_machine.state(),
                                    OrbWindowState::Collapsed | OrbWindowState::Collapsing
                                ) {
                                    host.start_expand(&window);
                                } else {
                                    unsafe {
                                        let _ = ShowWindow(host.hwnd, SW_RESTORE);
                                        let _ = SetForegroundWindow(host.hwnd);
                                    }
                                }
                            }
                            TrayCommand::Collapse => {
                                if matches!(
                                    host.state_machine.state(),
                                    OrbWindowState::Expanded | OrbWindowState::Expanding
                                ) {
                                    host.start_collapse(&window);
                                }
                            }
                            TrayCommand::Quit => {
                                host.detach_anim_timer();
                                host.persist_current_state();
                                router.cancel_all();
                                *control_flow = ControlFlow::Exit;
                            }
                        }
                    }
                }
                UserEvent::WebMessage {
                    json,
                    dropped_paths,
                } => {
                    router.handle_message(&json, dropped_paths);
                }
                UserEvent::WindowCmd(cmd) => match cmd {
                    WindowCommand::OrbExpand => host.start_expand(&window),
                    WindowCommand::OrbCollapse => host.start_collapse(&window),
                    WindowCommand::OrbDragStart => host.handle_orb_drag_start(),
                    WindowCommand::OrbDragMove => host.handle_orb_drag_move(),
                    WindowCommand::OrbDragEnd => host.handle_orb_drag_end(),
                    WindowCommand::WindowDragStart => {
                        if host.state_machine.state() == OrbWindowState::Expanded
                            && !host.state_machine.is_animating()
                        {
                            backdrop::begin_native_window_drag(host.hwnd);
                        }
                    }
                    WindowCommand::WindowToggleMaximize => {
                        if host.state_machine.state() == OrbWindowState::Expanded
                            && !host.state_machine.is_animating()
                        {
                            host.is_maximized = !host.is_maximized;
                            window.set_maximized(host.is_maximized);
                            if host.is_maximized {
                                host.expanded_flag
                                    .store(false, std::sync::atomic::Ordering::Relaxed);
                                backdrop::clear_window_region(host.hwnd);
                            } else {
                                host.expanded_flag
                                    .store(true, std::sync::atomic::Ordering::Relaxed);
                                let rect = backdrop::get_window_bounds_physical(host.hwnd);
                                host.apply_region_for_current_state(rect, 1.0);
                            }
                            host.send_json(&json!({
                                "type": "window_maximized_changed",
                                "maximized": host.is_maximized
                            }));
                        }
                    }
                    WindowCommand::WindowClose => {
                        host.detach_anim_timer();
                        host.persist_current_state();
                        router.cancel_all();
                        *control_flow = ControlFlow::Exit;
                    }
                    WindowCommand::UpdateTooltip(tip) => host.tray.update_tooltip(&tip),
                    WindowCommand::GetOrbSettings { reply_id } => {
                        let reply = build_orb_settings_reply(
                            &reply_id,
                            &host.settings,
                            host.orb_edge,
                            host.state_machine.state().as_lower_str(),
                            Some(host.appearance),
                        );
                        host.send_json(&reply);
                    }
                    WindowCommand::ThemeOrSettingsChanged {
                        launch_mode,
                        theme,
                        reply_id,
                    } => {
                        let theme_changed = apply_orb_settings_update(
                            &mut host.settings,
                            launch_mode,
                            theme.as_deref(),
                        );
                        if theme_changed {
                            host.apply_native_backdrop();
                            host.push_system_appearance();
                        }
                        let reply = build_orb_settings_reply(
                            &reply_id,
                            &host.settings,
                            host.orb_edge,
                            host.state_machine.state().as_lower_str(),
                            None,
                        );
                        host.send_json(&reply);
                    }
                    WindowCommand::BrowseFile { reply_id } => {
                        let picked = rfd::FileDialog::new()
                            .set_title("Select Nintendo Switch NUP Container")
                            .add_filter("NUP Game Containers (*.nup)", &["nup"])
                            .add_filter("All files (*.*)", &["*"])
                            .pick_file();
                        host.send_json(&json!({
                            "id": reply_id,
                            "action": "browse_file_result",
                            "success": picked.is_some(),
                            "path": picked.map(|p| p.to_string_lossy().into_owned())
                        }));
                    }
                    WindowCommand::BrowseFolder { reply_id } => {
                        let picked = rfd::FileDialog::new()
                            .set_title("Select Destination Extraction Folder")
                            .pick_folder();
                        host.send_json(&json!({
                            "id": reply_id,
                            "action": "browse_folder_result",
                            "success": picked.is_some(),
                            "path": picked.map(|p| p.to_string_lossy().into_owned())
                        }));
                    }
                    WindowCommand::BrowsePackFolder { reply_id } => {
                        let picked = rfd::FileDialog::new()
                            .set_title("Select Game Folder to Pack into .NUP")
                            .pick_folder();
                        let name = picked
                            .as_ref()
                            .and_then(|p| p.file_name())
                            .map(|n| n.to_string_lossy().into_owned());
                        host.send_json(&json!({
                            "id": reply_id,
                            "action": "browse_pack_folder_result",
                            "success": picked.is_some(),
                            "path": picked.map(|p| p.to_string_lossy().into_owned()),
                            "name": name
                        }));
                    }
                    WindowCommand::BrowsePackOutput {
                        reply_id,
                        default_name,
                    } => {
                        let fname = if default_name.to_ascii_lowercase().ends_with(".nup") {
                            default_name
                        } else {
                            format!("{default_name}.nup")
                        };
                        let picked = rfd::FileDialog::new()
                            .set_title("Save .NUP Container As")
                            .set_file_name(&fname)
                            .add_filter("NUP Container (*.nup)", &["nup"])
                            .add_filter("All files (*.*)", &["*"])
                            .save_file();
                        host.send_json(&json!({
                            "id": reply_id,
                            "action": "browse_pack_output_result",
                            "success": picked.is_some(),
                            "path": picked.map(|p| p.to_string_lossy().into_owned())
                        }));
                    }
                    WindowCommand::ExtractSingleFile {
                        reply_id,
                        nup_path,
                        file_index,
                        suggested_name,
                    } => {
                        let picked = rfd::FileDialog::new()
                            .set_title(format!("Save {suggested_name} As"))
                            .set_file_name(&suggested_name)
                            .add_filter("All files (*.*)", &["*"])
                            .save_file();
                        match picked {
                            Some(dest) => {
                                run_extract_single_file_worker(
                                    Arc::clone(&sink),
                                    reply_id,
                                    nup_path,
                                    file_index,
                                    dest.to_string_lossy().into_owned(),
                                );
                            }
                            None => {
                                host.send_json(&json!({
                                    "id": reply_id,
                                    "action": "extract_single_file_result",
                                    "success": false,
                                    "cancelled": true
                                }));
                            }
                        }
                    }
                    WindowCommand::FileDropped { target, paths } => {
                        let mut dropped_nups: Vec<String> = Vec::new();
                        let mut dropped_folder: Option<String> = None;
                        for p in paths {
                            let pb = PathBuf::from(&p);
                            if pb.is_file() && p.to_ascii_lowercase().ends_with(".nup") {
                                if !dropped_nups
                                    .iter()
                                    .any(|existing| existing.eq_ignore_ascii_case(&p))
                                {
                                    dropped_nups.push(p);
                                }
                                continue;
                            }
                            if pb.is_dir() && dropped_folder.is_none() {
                                dropped_folder = Some(p);
                            }
                        }
                        if dropped_nups.len() > 1 {
                            if matches!(
                                host.state_machine.state(),
                                OrbWindowState::Collapsed | OrbWindowState::Collapsing
                            ) {
                                host.start_expand(&window);
                            }
                            host.send_json(&json!({
                                "type": "batch_extract_dropped_files",
                                "files": dropped_nups
                            }));
                        } else if let Some(nup) = dropped_nups.into_iter().next() {
                            if matches!(
                                host.state_machine.state(),
                                OrbWindowState::Collapsed | OrbWindowState::Collapsing
                            ) {
                                host.start_expand(&window);
                            }
                            host.send_json(&json!({
                                "type": "load_dropped_file",
                                "filePath": nup
                            }));
                        } else if let Some(folder) = dropped_folder
                            && target == "pack"
                        {
                            host.send_json(&json!({
                                "type": "load_dropped_pack_folder",
                                "folderPath": folder
                            }));
                        }
                    }
                },
            },
            _ => {}
        }
    });
}
