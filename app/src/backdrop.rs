#![allow(unsafe_code)]

use std::ffi::c_void;
use std::mem::size_of;

use nup_host_logic::{MonitorWorkArea, PixelPoint, PixelRect};
use serde::{Deserialize, Serialize};
use windows::Win32::Foundation::{ERROR_SUCCESS, HWND, LPARAM, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DWMWA_SYSTEMBACKDROP_TYPE, DWMWA_USE_IMMERSIVE_DARK_MODE, DWMWA_WINDOW_CORNER_PREFERENCE,
    DwmExtendFrameIntoClientArea, DwmSetWindowAttribute,
};
use windows::Win32::Graphics::Gdi::{
    CreateEllipticRgn, CreateRoundRectRgn, DeleteObject, EnumDisplayMonitors, GetMonitorInfoW, HDC,
    HMONITOR, HRGN, MONITOR_DEFAULTTONEAREST, MONITORINFOEXW, MonitorFromPoint, SetWindowRgn,
};
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_READ, REG_VALUE_TYPE, RegCloseKey, RegOpenKeyExW, RegQueryValueExW,
};
use windows::Win32::UI::Accessibility::HIGHCONTRASTW;
use windows::Win32::UI::Controls::MARGINS;
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Input::KeyboardAndMouse::ReleaseCapture;
use windows::Win32::UI::WindowsAndMessaging::{
    GWL_EXSTYLE, GWL_STYLE, GetCursorPos, GetWindowLongPtrW, GetWindowRect, HTCAPTION,
    HWND_NOTOPMOST, HWND_TOPMOST, SPI_GETCLIENTAREAANIMATION, SPI_GETHIGHCONTRAST,
    SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOCOPYBITS, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER,
    SendMessageW, SetWindowLongPtrW, SetWindowPos, SystemParametersInfoW, WM_NCLBUTTONDOWN,
    WS_BORDER, WS_CAPTION, WS_CLIPCHILDREN, WS_CLIPSIBLINGS, WS_DLGFRAME, WS_EX_APPWINDOW,
    WS_EX_TOOLWINDOW, WS_MAXIMIZEBOX, WS_MINIMIZEBOX, WS_POPUP, WS_SYSMENU, WS_THICKFRAME,
};
use windows::core::{BOOL, PCSTR, s, w};

const DWMSBT_NONE: i32 = 1;
const DWMSBT_TRANSIENTWINDOW: i32 = 3; // Acrylic

const DWMWCP_DONOTROUND: i32 = 1;
const DWMWCP_ROUND: i32 = 2;

const WCA_ACCENT_POLICY: i32 = 19;
const ACCENT_DISABLED: i32 = 0;
const ACCENT_ENABLE_ACRYLICBLURBEHIND: i32 = 4;

#[repr(C)]
struct AccentPolicy {
    accent_state: i32,
    accent_flags: i32,
    gradient_color: u32,
    animation_id: i32,
}

#[repr(C)]
struct WindowCompositionAttributeData {
    attribute: i32,
    data: *mut c_void,
    size_of_data: usize,
}

type SetWindowCompositionAttributeFn =
    unsafe extern "system" fn(HWND, *mut WindowCompositionAttributeData) -> i32;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SystemAppearanceState {
    pub reduced_motion: bool,
    pub high_contrast: bool,
    pub transparency_enabled: bool,
}

pub fn is_windows_11_22h2_or_greater() -> bool {
    let mut hkey = HKEY::default();
    let status = unsafe {
        RegOpenKeyExW(
            windows::Win32::System::Registry::HKEY_LOCAL_MACHINE,
            w!(r"SOFTWARE\Microsoft\Windows NT\CurrentVersion"),
            Some(0),
            KEY_READ,
            &mut hkey,
        )
    };
    if status != ERROR_SUCCESS {
        return true;
    }
    let mut buf = [0u16; 32];
    let mut bytes = (buf.len() * 2) as u32;
    let mut ty = REG_VALUE_TYPE::default();
    let res = unsafe {
        RegQueryValueExW(
            hkey,
            w!("CurrentBuildNumber"),
            None,
            Some(&mut ty),
            Some(buf.as_mut_ptr() as *mut u8),
            Some(&mut bytes),
        )
    };
    unsafe {
        let _ = RegCloseKey(hkey);
    }
    if res == ERROR_SUCCESS {
        let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        let s = String::from_utf16_lossy(&buf[..len]);
        if let Ok(build) = s.trim().parse::<u32>() {
            return build >= 22621;
        }
    }
    true
}

pub fn is_reduced_motion_enabled() -> bool {
    let mut anim_enabled = BOOL(1);
    let ok = unsafe {
        SystemParametersInfoW(
            SPI_GETCLIENTAREAANIMATION,
            0,
            Some((&mut anim_enabled as *mut BOOL) as *mut c_void),
            windows::Win32::UI::WindowsAndMessaging::SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
    };
    if ok.is_ok() {
        !anim_enabled.as_bool()
    } else {
        false
    }
}

pub fn is_high_contrast_enabled() -> bool {
    let mut hc = HIGHCONTRASTW {
        cbSize: size_of::<HIGHCONTRASTW>() as u32,
        ..Default::default()
    };
    let ok = unsafe {
        SystemParametersInfoW(
            SPI_GETHIGHCONTRAST,
            hc.cbSize,
            Some((&mut hc as *mut HIGHCONTRASTW) as *mut c_void),
            windows::Win32::UI::WindowsAndMessaging::SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
    };
    if ok.is_ok() {
        (hc.dwFlags.0 & 0x0000_0001) != 0 // HCF_HIGHCONTRASTON
    } else {
        false
    }
}

pub fn is_transparency_enabled() -> bool {
    let mut hkey = HKEY::default();
    let status = unsafe {
        RegOpenKeyExW(
            HKEY_CURRENT_USER,
            w!(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize"),
            Some(0),
            KEY_READ,
            &mut hkey,
        )
    };
    if status != ERROR_SUCCESS {
        return true;
    }
    let mut val: u32 = 1;
    let mut bytes = size_of::<u32>() as u32;
    let mut ty = REG_VALUE_TYPE::default();
    let res = unsafe {
        RegQueryValueExW(
            hkey,
            w!("EnableTransparency"),
            None,
            Some(&mut ty),
            Some((&mut val as *mut u32) as *mut u8),
            Some(&mut bytes),
        )
    };
    unsafe {
        let _ = RegCloseKey(hkey);
    }
    if res == ERROR_SUCCESS { val != 0 } else { true }
}

pub fn query_system_appearance() -> SystemAppearanceState {
    let high_contrast = is_high_contrast_enabled();
    SystemAppearanceState {
        reduced_motion: is_reduced_motion_enabled(),
        high_contrast,
        transparency_enabled: is_transparency_enabled() && !high_contrast,
    }
}

pub fn apply_backdrop(
    hwnd: HWND,
    is_dark_theme: bool,
    transparency_enabled: bool,
    high_contrast: bool,
) {
    if hwnd.0.is_null() {
        return;
    }
    let margins = MARGINS {
        cxLeftWidth: -1,
        cxRightWidth: -1,
        cyTopHeight: -1,
        cyBottomHeight: -1,
    };
    unsafe {
        let _ = DwmExtendFrameIntoClientArea(hwnd, &margins);
    }

    let dark_flag: i32 = if is_dark_theme { 1 } else { 0 };
    unsafe {
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_USE_IMMERSIVE_DARK_MODE,
            (&dark_flag as *const i32) as *const c_void,
            size_of::<i32>() as u32,
        );
    }

    let enable_blur = transparency_enabled && !high_contrast;
    if is_windows_11_22h2_or_greater() {
        let corner_pref: i32 = DWMWCP_ROUND;
        unsafe {
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWA_WINDOW_CORNER_PREFERENCE,
                (&corner_pref as *const i32) as *const c_void,
                size_of::<i32>() as u32,
            );
        }
        let backdrop_type: i32 = if enable_blur {
            DWMSBT_TRANSIENTWINDOW
        } else {
            DWMSBT_NONE
        };
        let hr = unsafe {
            DwmSetWindowAttribute(
                hwnd,
                DWMWA_SYSTEMBACKDROP_TYPE,
                (&backdrop_type as *const i32) as *const c_void,
                size_of::<i32>() as u32,
            )
        };
        if hr.is_ok() {
            return;
        }
    }

    // Windows 10 fallback via SetWindowCompositionAttribute
    apply_wca_accent(hwnd, enable_blur, is_dark_theme);
}

/// Disables the rectangular Windows 11 DWM system backdrop so GDI `SetWindowRgn(CreateEllipticRgn)`
/// clips the collapsed 64 DIP floating orb into a pure circle with zero square corners.
pub fn disable_backdrop_for_orb(hwnd: HWND, is_dark_theme: bool) {
    if hwnd.0.is_null() {
        return;
    }
    let zero_margins = MARGINS {
        cxLeftWidth: 0,
        cxRightWidth: 0,
        cyTopHeight: 0,
        cyBottomHeight: 0,
    };
    unsafe {
        let _ = DwmExtendFrameIntoClientArea(hwnd, &zero_margins);
    }

    if is_windows_11_22h2_or_greater() {
        let corner_pref: i32 = DWMWCP_DONOTROUND;
        let backdrop_none: i32 = DWMSBT_NONE;
        unsafe {
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWA_WINDOW_CORNER_PREFERENCE,
                (&corner_pref as *const i32) as *const c_void,
                size_of::<i32>() as u32,
            );
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWA_SYSTEMBACKDROP_TYPE,
                (&backdrop_none as *const i32) as *const c_void,
                size_of::<i32>() as u32,
            );
        }
    }
    apply_wca_accent(hwnd, false, is_dark_theme);
}

fn apply_wca_accent(hwnd: HWND, enable_blur: bool, is_dark_theme: bool) {
    unsafe {
        if let Ok(user32) = GetModuleHandleW(w!("user32.dll"))
            && let Some(proc) = GetProcAddress(user32, PCSTR(s!("SetWindowCompositionAttribute").0))
        {
            let func: SetWindowCompositionAttributeFn = std::mem::transmute(proc);
            let mut accent = AccentPolicy {
                accent_state: if enable_blur {
                    ACCENT_ENABLE_ACRYLICBLURBEHIND
                } else {
                    ACCENT_DISABLED
                },
                accent_flags: if enable_blur { 2 } else { 0 },
                gradient_color: if is_dark_theme {
                    0x6612_1622
                } else {
                    0x66F0_F4FA
                },
                animation_id: 0,
            };
            let mut data = WindowCompositionAttributeData {
                attribute: WCA_ACCENT_POLICY,
                data: (&mut accent as *mut AccentPolicy) as *mut c_void,
                size_of_data: size_of::<AccentPolicy>(),
            };
            let _ = func(hwnd, &mut data);
        }
    }
}

pub fn apply_rounded_region(
    hwnd: HWND,
    width_px: i32,
    height_px: i32,
    corner_radius_px: i32,
    is_circle: bool,
) {
    apply_rounded_region_at(hwnd, 0, 0, width_px, height_px, corner_radius_px, is_circle);
}

pub fn apply_rounded_region_at(
    hwnd: HWND,
    x: i32,
    y: i32,
    width_px: i32,
    height_px: i32,
    corner_radius_px: i32,
    is_circle: bool,
) {
    if hwnd.0.is_null() || width_px <= 0 || height_px <= 0 {
        return;
    }
    let hrgn: HRGN = unsafe {
        if is_circle {
            CreateEllipticRgn(x, y, x + width_px, y + height_px)
        } else {
            let diam = (corner_radius_px * 2).clamp(0, width_px.min(height_px));
            CreateRoundRectRgn(x, y, x + width_px + 1, y + height_px + 1, diam, diam)
        }
    };
    if hrgn.0.is_null() {
        return;
    }
    let applied = unsafe { SetWindowRgn(hwnd, Some(hrgn), true) };
    if applied == 0 {
        unsafe {
            let _ = DeleteObject(hrgn.into());
        }
    }
}

pub fn clear_window_region(hwnd: HWND) {
    if !hwnd.0.is_null() {
        unsafe {
            let _ = SetWindowRgn(hwnd, None, true);
        }
    }
}

pub fn set_window_bounds_physical(
    hwnd: HWND,
    rect: PixelRect,
    topmost: Option<bool>,
    activate: bool,
    no_copy_bits: bool,
) {
    if hwnd.0.is_null() {
        return;
    }
    let mut flags = windows::Win32::UI::WindowsAndMessaging::SET_WINDOW_POS_FLAGS(0);
    if !activate {
        flags |= SWP_NOACTIVATE;
    }
    if no_copy_bits {
        flags |= SWP_NOCOPYBITS;
    }
    let insert_after = match topmost {
        Some(true) => HWND_TOPMOST,
        Some(false) => HWND_NOTOPMOST,
        None => {
            flags |= SWP_NOZORDER;
            HWND::default()
        }
    };
    unsafe {
        let _ = SetWindowPos(
            hwnd,
            Some(insert_after),
            rect.x,
            rect.y,
            rect.width.max(1),
            rect.height.max(1),
            flags,
        );
    }
}

pub fn set_topmost(hwnd: HWND, topmost: bool) {
    if hwnd.0.is_null() {
        return;
    }
    unsafe {
        let _ = SetWindowPos(
            hwnd,
            Some(if topmost {
                HWND_TOPMOST
            } else {
                HWND_NOTOPMOST
            }),
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
        );
    }
}

pub const FORBIDDEN_FRAME_STYLES: u32 = WS_CAPTION.0 | WS_SYSMENU.0 | WS_DLGFRAME.0 | WS_BORDER.0;

pub fn configure_borderless_style(hwnd: HWND) {
    if hwnd.0.is_null() {
        return;
    }
    unsafe {
        let mut style = GetWindowLongPtrW(hwnd, GWL_STYLE);
        style &= !(FORBIDDEN_FRAME_STYLES as isize);
        style |= (WS_POPUP.0
            | WS_THICKFRAME.0
            | WS_MINIMIZEBOX.0
            | WS_MAXIMIZEBOX.0
            | WS_CLIPCHILDREN.0
            | WS_CLIPSIBLINGS.0) as isize;
        SetWindowLongPtrW(hwnd, GWL_STYLE, style);
        let _ = SetWindowPos(
            hwnd,
            None,
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED,
        );
    }
}

pub fn set_tool_window_style(hwnd: HWND, is_tool_window: bool) {
    if hwnd.0.is_null() {
        return;
    }
    unsafe {
        let mut ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        if is_tool_window {
            ex |= WS_EX_TOOLWINDOW.0 as isize;
            ex &= !(WS_EX_APPWINDOW.0 as isize);
        } else {
            ex &= !(WS_EX_TOOLWINDOW.0 as isize);
            ex |= WS_EX_APPWINDOW.0 as isize;
        }
        SetWindowLongPtrW(hwnd, GWL_EXSTYLE, ex);
        let _ = SetWindowPos(
            hwnd,
            None,
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED,
        );
    }
}

pub fn begin_native_window_drag(hwnd: HWND) {
    if hwnd.0.is_null() {
        return;
    }
    unsafe {
        let _ = ReleaseCapture();
        let _ = SendMessageW(
            hwnd,
            WM_NCLBUTTONDOWN,
            Some(WPARAM(HTCAPTION as usize)),
            Some(LPARAM(0)),
        );
    }
}

pub fn get_cursor_position_physical() -> PixelPoint {
    let mut pt = POINT::default();
    if unsafe { GetCursorPos(&mut pt) }.is_ok() {
        PixelPoint::new(pt.x, pt.y)
    } else {
        PixelPoint::new(0, 0)
    }
}

pub fn get_window_bounds_physical(hwnd: HWND) -> PixelRect {
    let mut rc = RECT::default();
    if !hwnd.0.is_null() && unsafe { GetWindowRect(hwnd, &mut rc) }.is_ok() {
        PixelRect::new(rc.left, rc.top, rc.right - rc.left, rc.bottom - rc.top)
    } else {
        PixelRect::default()
    }
}

pub fn get_all_monitors() -> Vec<MonitorWorkArea> {
    let mut list: Vec<MonitorWorkArea> = Vec::new();
    unsafe extern "system" fn enum_proc(
        hmon: HMONITOR,
        _hdc: HDC,
        _rc: *mut RECT,
        data: LPARAM,
    ) -> BOOL {
        let out = unsafe { &mut *(data.0 as *mut Vec<MonitorWorkArea>) };
        let mut info = MONITORINFOEXW::default();
        info.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;
        if unsafe { GetMonitorInfoW(hmon, &mut info.monitorInfo) }.as_bool() {
            let mut dpi_x = 96u32;
            let mut dpi_y = 96u32;
            let dpi_scale =
                if unsafe { GetDpiForMonitor(hmon, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y) }
                    .is_ok()
                    && dpi_x >= 48
                {
                    dpi_x as f64 / 96.0
                } else {
                    1.0
                };

            let r = info.monitorInfo.rcMonitor;
            let w = info.monitorInfo.rcWork;
            let dev_len = info
                .szDevice
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(info.szDevice.len());
            let device_name = String::from_utf16_lossy(&info.szDevice[..dev_len]);
            let is_primary = (info.monitorInfo.dwFlags & 1) != 0;
            out.push(MonitorWorkArea::new(
                device_name,
                PixelRect::new(r.left, r.top, r.right - r.left, r.bottom - r.top),
                PixelRect::new(w.left, w.top, w.right - w.left, w.bottom - w.top),
                dpi_scale,
                is_primary,
            ));
        }
        BOOL(1)
    }

    unsafe {
        let _ = EnumDisplayMonitors(
            None,
            None,
            Some(enum_proc),
            LPARAM((&mut list as *mut Vec<MonitorWorkArea>) as isize),
        );
    }

    if list.is_empty() {
        list.push(MonitorWorkArea::new(
            "PRIMARY",
            PixelRect::new(0, 0, 1920, 1080),
            PixelRect::new(0, 0, 1920, 1040),
            1.0,
            true,
        ));
    }
    list
}

pub fn get_dpi_scale_for_point(pt: PixelPoint) -> f64 {
    let hmon = unsafe { MonitorFromPoint(POINT { x: pt.x, y: pt.y }, MONITOR_DEFAULTTONEAREST) };
    if !hmon.0.is_null() {
        let mut dpi_x = 96u32;
        let mut dpi_y = 96u32;
        if unsafe { GetDpiForMonitor(hmon, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y) }.is_ok()
            && dpi_x >= 48
        {
            return dpi_x as f64 / 96.0;
        }
    }
    1.0
}
