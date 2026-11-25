#![allow(unsafe_code)]

use windows::Win32::Foundation::ERROR_SUCCESS;
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_READ, KEY_WRITE, REG_OPTION_NON_VOLATILE, REG_SZ, RegCloseKey,
    RegCreateKeyExW, RegDeleteTreeW, RegOpenKeyExW, RegSetValueExW,
};
use windows::core::{PCWSTR, w};

const PROG_ID: &str = "NupExtractor.Package";
const EXTENSION: &str = ".nup";

fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn set_sz_value(subkey: &str, value_name: Option<&str>, data: &str) -> bool {
    let subkey_w = to_wide(subkey);
    let mut hkey = HKEY::default();
    let status = unsafe {
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR(subkey_w.as_ptr()),
            Some(0),
            PCWSTR::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_WRITE,
            None,
            &mut hkey,
            None,
        )
    };
    if status != ERROR_SUCCESS {
        return false;
    }

    let name_w = value_name.map(to_wide);
    let name_ptr = match &name_w {
        Some(v) => PCWSTR(v.as_ptr()),
        None => PCWSTR::null(),
    };
    let data_w = to_wide(data);
    let byte_slice =
        unsafe { std::slice::from_raw_parts(data_w.as_ptr() as *const u8, data_w.len() * 2) };
    let res = unsafe { RegSetValueExW(hkey, name_ptr, Some(0), REG_SZ, Some(byte_slice)) };
    unsafe {
        let _ = RegCloseKey(hkey);
    }
    res == ERROR_SUCCESS
}

pub fn is_registered() -> bool {
    let mut hkey = HKEY::default();
    let status = unsafe {
        RegOpenKeyExW(
            HKEY_CURRENT_USER,
            w!(r"Software\Classes\NupExtractor.Package\shell\ExtractHere"),
            Some(0),
            KEY_READ,
            &mut hkey,
        )
    };
    if status == ERROR_SUCCESS {
        unsafe {
            let _ = RegCloseKey(hkey);
        }
        true
    } else {
        false
    }
}

/// Refreshes existing HKCU shell registrations when already enabled so `open` uses `--open "%1"`.
pub fn refresh_if_registered() {
    if is_registered() {
        let _ = register();
    }
}

pub fn register() -> bool {
    let Ok(exe) = std::env::current_exe() else {
        return false;
    };
    if !exe.exists() {
        return false;
    }
    let exe_str = exe.to_string_lossy();

    let ok1 = set_sz_value(&format!(r"Software\Classes\{EXTENSION}"), None, PROG_ID);
    let ok2 = set_sz_value(
        &format!(r"Software\Classes\{PROG_ID}"),
        None,
        "Nintendo Switch NX Package",
    );
    let ok3 = set_sz_value(
        &format!(r"Software\Classes\{PROG_ID}\DefaultIcon"),
        None,
        &format!("\"{exe_str}\",0"),
    );
    let ok4 = set_sz_value(
        &format!(r"Software\Classes\{PROG_ID}\shell\open\command"),
        None,
        &format!("\"{exe_str}\" --open \"%1\""),
    );
    let ok5 = set_sz_value(
        &format!(r"Software\Classes\{PROG_ID}\shell\ExtractToFolder"),
        None,
        "Extract to folder with NUP Extractor",
    ) && set_sz_value(
        &format!(r"Software\Classes\{PROG_ID}\shell\ExtractToFolder"),
        Some("Icon"),
        &format!("\"{exe_str}\""),
    ) && set_sz_value(
        &format!(r"Software\Classes\{PROG_ID}\shell\ExtractToFolder\command"),
        None,
        &format!("\"{exe_str}\" --extract-to-folder \"%1\""),
    );
    let ok6 = set_sz_value(
        &format!(r"Software\Classes\{PROG_ID}\shell\ExtractHere"),
        None,
        "Extract Here with NUP Extractor",
    ) && set_sz_value(
        &format!(r"Software\Classes\{PROG_ID}\shell\ExtractHere"),
        Some("Icon"),
        &format!("\"{exe_str}\""),
    ) && set_sz_value(
        &format!(r"Software\Classes\{PROG_ID}\shell\ExtractHere\command"),
        None,
        &format!("\"{exe_str}\" --extract-here \"%1\""),
    );
    let ok7 = set_sz_value(
        r"Software\Classes\Directory\shell\NupExtractor.Pack",
        None,
        "Pack to .NUP with NUP Extractor",
    ) && set_sz_value(
        r"Software\Classes\Directory\shell\NupExtractor.Pack",
        Some("Icon"),
        &format!("\"{exe_str}\""),
    ) && set_sz_value(
        r"Software\Classes\Directory\shell\NupExtractor.Pack\command",
        None,
        &format!("\"{exe_str}\" --pack-here \"%1\""),
    );

    ok1 && ok2 && ok3 && ok4 && ok5 && ok6 && ok7
}

pub fn unregister() -> bool {
    for sub in [
        format!(r"Software\Classes\{EXTENSION}"),
        format!(r"Software\Classes\{PROG_ID}"),
        r"Software\Classes\Directory\shell\NupExtractor.Pack".to_string(),
    ] {
        let wsub = to_wide(&sub);
        unsafe {
            let _ = RegDeleteTreeW(HKEY_CURRENT_USER, PCWSTR(wsub.as_ptr()));
        }
    }
    true
}
