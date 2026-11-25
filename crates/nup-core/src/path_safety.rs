#![forbid(unsafe_code)]

use std::path::{Path, PathBuf};

use crate::error::{NupError, NupResult};

const WINDOWS_RESERVED_NAMES: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// Validates and converts an archive-relative path into a safe OS `PathBuf`.
///
/// Rejects path traversal (`..`), absolute paths, drive prefixes, alternate data streams (`:`),
/// Windows reserved device names, trailing dots/spaces, and control characters (see `DIV-002`).
pub fn validate_and_join_extraction_path(dest_root: &Path, rel_path: &str) -> NupResult<PathBuf> {
    if rel_path.is_empty() {
        return Err(NupError::PathSafety("empty relative path".to_string()));
    }
    if rel_path.starts_with('/') || rel_path.starts_with('\\') {
        return Err(NupError::PathSafety(format!(
            "absolute path not allowed: '{rel_path}'"
        )));
    }
    if rel_path.contains(':') {
        return Err(NupError::PathSafety(format!(
            "drive letter or alternate data stream ':' not allowed: '{rel_path}'"
        )));
    }

    let mut out = dest_root.to_path_buf();
    let mut segment_count = 0usize;

    for seg in rel_path.split(['/', '\\']) {
        if seg.is_empty() {
            continue;
        }
        if seg == "." || seg == ".." {
            return Err(NupError::PathSafety(format!(
                "path traversal segment '{seg}' rejected in '{rel_path}'"
            )));
        }
        if seg.ends_with('.') || seg.ends_with(' ') {
            return Err(NupError::PathSafety(format!(
                "trailing dot/space rejected in '{seg}'"
            )));
        }
        if seg
            .chars()
            .any(|c| c.is_control() || matches!(c, '<' | '>' | '"' | '|' | '?' | '*'))
        {
            return Err(NupError::PathSafety(format!(
                "illegal Windows filename character in '{seg}'"
            )));
        }

        let stem = seg.split('.').next().unwrap_or(seg).to_ascii_uppercase();
        if WINDOWS_RESERVED_NAMES.contains(&stem.as_str()) {
            return Err(NupError::PathSafety(format!(
                "Windows reserved device name '{seg}' rejected"
            )));
        }

        out.push(seg);
        segment_count += 1;
    }

    if segment_count == 0 {
        return Err(NupError::PathSafety(format!(
            "no valid path segments in '{rel_path}'"
        )));
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_normal_and_unicode_paths() {
        let root = Path::new("C:\\out");
        let p = validate_and_join_extraction_path(root, "base/romfs/日本語_café.txt").unwrap();
        assert!(p.starts_with(root));
        assert!(p.ends_with("日本語_café.txt"));
    }

    #[test]
    fn rejects_traversal_ads_reserved_and_trailing_dots() {
        let root = Path::new("C:\\out");
        for bad in [
            "../evil.txt",
            "romfs/../../evil.txt",
            "/etc/passwd",
            "\\Windows\\System32\\a",
            "C:\\evil.txt",
            "romfs/file.txt:ads",
            "romfs/CON",
            "romfs/nul.txt",
            "romfs/com1.bin",
            "romfs/trailing.",
            "romfs/trailing ",
        ] {
            assert!(
                validate_and_join_extraction_path(root, bad).is_err(),
                "expected rejection for '{bad}'"
            );
        }
    }
}
