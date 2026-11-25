#![allow(unsafe_code)]

use std::mem::size_of;

use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::Shell::{
    ExtractIconExW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY,
    NOTIFYICONDATAW, Shell_NotifyIconW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, DestroyIcon, DestroyMenu, HICON, IDI_APPLICATION, IMAGE_ICON,
    LR_DEFAULTSIZE, LR_LOADFROMFILE, LoadIconW, LoadImageW, MF_GRAYED, MF_SEPARATOR, MF_STRING,
    PostMessageW, SetForegroundWindow, TPM_RETURNCMD, TPM_RIGHTBUTTON, TrackPopupMenuEx, WM_APP,
    WM_CONTEXTMENU, WM_LBUTTONDBLCLK, WM_LBUTTONUP, WM_RBUTTONUP,
};
use windows::core::{PCWSTR, w};

use crate::backdrop;

pub const WM_TRAYICON: u32 = WM_APP + 42;

const CMD_OPEN: u32 = 1001;
const CMD_COLLAPSE: u32 = 1002;
const CMD_QUIT: u32 = 1003;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayCommand {
    Open,
    Collapse,
    Quit,
}

pub struct TrayIconManager {
    hwnd: HWND,
    hicon: HICON,
    owns_icon: bool,
    added: bool,
}

impl TrayIconManager {
    pub fn new(hwnd: HWND) -> Self {
        let (hicon, owns_icon) = load_tray_icon();
        let nid = create_base_data(hwnd, hicon, "NUP Extractor");
        let added = unsafe { Shell_NotifyIconW(NIM_ADD, &nid) }.as_bool();
        Self {
            hwnd,
            hicon,
            owns_icon,
            added,
        }
    }

    pub fn update_tooltip(&self, tooltip: &str) {
        if !self.added {
            return;
        }
        let nid = create_base_data(self.hwnd, self.hicon, tooltip);
        unsafe {
            let _ = Shell_NotifyIconW(NIM_MODIFY, &nid);
        }
    }

    pub fn handle_message(&self, lparam: LPARAM, is_expanded: bool) -> Option<TrayCommand> {
        let evt = (lparam.0 & 0xFFFF) as u32;
        if evt == WM_LBUTTONUP || evt == WM_LBUTTONDBLCLK {
            return Some(TrayCommand::Open);
        }
        if evt == WM_RBUTTONUP || evt == WM_CONTEXTMENU {
            return self.show_context_menu(is_expanded);
        }
        None
    }

    fn show_context_menu(&self, is_expanded: bool) -> Option<TrayCommand> {
        let Ok(menu) = (unsafe { CreatePopupMenu() }) else {
            return None;
        };

        unsafe {
            let open_flags = if is_expanded {
                MF_STRING | MF_GRAYED
            } else {
                MF_STRING
            };
            let collapse_flags = if !is_expanded {
                MF_STRING | MF_GRAYED
            } else {
                MF_STRING
            };
            let _ = AppendMenuW(
                menu,
                open_flags,
                CMD_OPEN as usize,
                w!("Open NUP Extractor"),
            );
            let _ = AppendMenuW(
                menu,
                collapse_flags,
                CMD_COLLAPSE as usize,
                w!("Collapse to Orb"),
            );
            let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
            let _ = AppendMenuW(menu, MF_STRING, CMD_QUIT as usize, w!("Quit"));

            let pt = backdrop::get_cursor_position_physical();
            let _ = SetForegroundWindow(self.hwnd);
            let cmd = TrackPopupMenuEx(
                menu,
                (TPM_RETURNCMD | TPM_RIGHTBUTTON).0,
                pt.x,
                pt.y,
                self.hwnd,
                None,
            );
            let _ = PostMessageW(Some(self.hwnd), 0, WPARAM(0), LPARAM(0));
            let _ = DestroyMenu(menu);

            match cmd.0 as u32 {
                CMD_OPEN => Some(TrayCommand::Open),
                CMD_COLLAPSE => Some(TrayCommand::Collapse),
                CMD_QUIT => Some(TrayCommand::Quit),
                _ => None,
            }
        }
    }
}

impl Drop for TrayIconManager {
    fn drop(&mut self) {
        if self.added {
            let nid = NOTIFYICONDATAW {
                cbSize: size_of::<NOTIFYICONDATAW>() as u32,
                hWnd: self.hwnd,
                uID: 1,
                ..Default::default()
            };
            unsafe {
                let _ = Shell_NotifyIconW(NIM_DELETE, &nid);
            }
            self.added = false;
        }
        if self.owns_icon && !self.hicon.0.is_null() {
            unsafe {
                let _ = DestroyIcon(self.hicon);
            }
            self.hicon = HICON::default();
        }
    }
}

fn create_base_data(hwnd: HWND, hicon: HICON, tip: &str) -> NOTIFYICONDATAW {
    let mut nid = NOTIFYICONDATAW {
        cbSize: size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: 1,
        uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP,
        uCallbackMessage: WM_TRAYICON,
        hIcon: hicon,
        ..Default::default()
    };
    let wide: Vec<u16> = tip.encode_utf16().take(127).collect();
    for (dst, &src) in nid.szTip.iter_mut().zip(wide.iter()) {
        *dst = src;
    }
    nid
}

fn load_tray_icon() -> (HICON, bool) {
    if let Ok(exe) = std::env::current_exe() {
        let exe_w: Vec<u16> = exe
            .to_string_lossy()
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        let mut small = [HICON::default(); 1];
        let count =
            unsafe { ExtractIconExW(PCWSTR(exe_w.as_ptr()), 0, None, Some(small.as_mut_ptr()), 1) };
        if count > 0 && !small[0].0.is_null() {
            return (small[0], true);
        }

        if let Some(dir) = exe.parent() {
            let ico = dir.join("app_icon.ico");
            if ico.exists() {
                let ico_w: Vec<u16> = ico
                    .to_string_lossy()
                    .encode_utf16()
                    .chain(std::iter::once(0))
                    .collect();
                if let Ok(h) = unsafe {
                    LoadImageW(
                        None,
                        PCWSTR(ico_w.as_ptr()),
                        IMAGE_ICON,
                        16,
                        16,
                        LR_LOADFROMFILE | LR_DEFAULTSIZE,
                    )
                } && !h.0.is_null()
                {
                    return (HICON(h.0), true);
                }
            }
        }
    }
    let fallback = unsafe { LoadIconW(None, IDI_APPLICATION) }.unwrap_or_default();
    (fallback, false)
}
