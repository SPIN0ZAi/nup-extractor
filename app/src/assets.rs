#![forbid(unsafe_code)]

use std::fs;
use std::path::PathBuf;

const INDEX_HTML: &[u8] = include_bytes!("../../static/index.html");
const STYLES_CSS: &[u8] = include_bytes!("../../static/styles.css");
const IPC_JS: &[u8] = include_bytes!("../../static/ipc.js");
const APP_JS: &[u8] = include_bytes!("../../static/app.js");
const APP_ICON_ICO: &[u8] = include_bytes!("../../app_icon.ico");

pub fn get_asset(raw_path: &str) -> Option<(Vec<u8>, &'static str)> {
    let trimmed = raw_path
        .split('?')
        .next()
        .unwrap_or(raw_path)
        .trim_start_matches('/');
    let path = if trimmed.is_empty() {
        "index.html"
    } else {
        trimmed
    };

    // Reject path traversal in resource lookup
    if path.contains("..") || path.contains('\\') {
        return None;
    }

    let mime = mime_type(path);

    // 1. Prefer embedded compile-time assets for single-binary reliability,
    //    or check disk if an override static/ folder exists next to the executable in debug mode.
    #[cfg(debug_assertions)]
    {
        if let Some(disk_bytes) = try_read_from_disk(path) {
            return Some((disk_bytes, mime));
        }
    }

    let embedded: Option<&'static [u8]> = match path.to_ascii_lowercase().as_str() {
        "index.html" => Some(INDEX_HTML),
        "styles.css" => Some(STYLES_CSS),
        "ipc.js" => Some(IPC_JS),
        "app.js" => Some(APP_JS),
        "app_icon.ico" => Some(APP_ICON_ICO),
        _ => None,
    };

    if let Some(bytes) = embedded {
        return Some((bytes.to_vec(), mime));
    }

    try_read_from_disk(path).map(|b| (b, mime))
}

fn try_read_from_disk(path: &str) -> Option<Vec<u8>> {
    let mut candidates = Vec::new();
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        candidates.push(dir.join("static").join(path));
        if path.eq_ignore_ascii_case("app_icon.ico") {
            candidates.push(dir.join("app_icon.ico"));
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        candidates.push(cwd.join("static").join(path));
        if path.eq_ignore_ascii_case("app_icon.ico") {
            candidates.push(cwd.join("app_icon.ico"));
        }
    }
    for c in candidates {
        if let Ok(bytes) = fs::read(PathBuf::from(&c)) {
            return Some(bytes);
        }
    }
    None
}

pub fn mime_type(path: &str) -> &'static str {
    let low = path.to_ascii_lowercase();
    if low.ends_with(".html") {
        "text/html; charset=utf-8"
    } else if low.ends_with(".css") {
        "text/css; charset=utf-8"
    } else if low.ends_with(".js") {
        "application/javascript; charset=utf-8"
    } else if low.ends_with(".png") {
        "image/png"
    } else if low.ends_with(".jpg") || low.ends_with(".jpeg") {
        "image/jpeg"
    } else if low.ends_with(".svg") {
        "image/svg+xml"
    } else if low.ends_with(".ico") {
        "image/x-icon"
    } else if low.ends_with(".json") {
        "application/json"
    } else {
        "application/octet-stream"
    }
}
