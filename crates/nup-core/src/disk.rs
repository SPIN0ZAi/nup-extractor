#![forbid(unsafe_code)]

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::engine::format_bytes;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiskSpaceInfo {
    pub has_sufficient_space: bool,
    pub free_bytes: u64,
    pub required_bytes: u64,
    pub total_bytes: u64,
    pub free_formatted: String,
    pub required_formatted: String,
    pub drive_name: String,
    pub drive_letter: String,
}

pub fn check_disk_space(destination_dir: &str, required_bytes: u64) -> DiskSpaceInfo {
    let resolved = if destination_dir.trim().is_empty() {
        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
    } else {
        let p = PathBuf::from(destination_dir.trim().trim_matches(['"', '\'']));
        if p.is_absolute() {
            p
        } else {
            std::env::current_dir().map(|cwd| cwd.join(&p)).unwrap_or(p)
        }
    };

    let drive_root = resolved
        .components()
        .take(2)
        .collect::<PathBuf>()
        .to_string_lossy()
        .to_string();

    let query_path = if drive_root.ends_with('\\') {
        drive_root
    } else if !drive_root.is_empty() {
        format!("{drive_root}\\")
    } else {
        resolved.to_string_lossy().to_string()
    };

    match crate::mmap::query_disk_space(&query_path) {
        Some((free, total)) => DiskSpaceInfo {
            has_sufficient_space: free >= required_bytes,
            free_bytes: free,
            required_bytes,
            total_bytes: total,
            free_formatted: format_bytes(free),
            required_formatted: format_bytes(required_bytes),
            drive_name: query_path.clone(),
            drive_letter: query_path,
        },
        None => DiskSpaceInfo {
            has_sufficient_space: true,
            free_bytes: u64::MAX,
            required_bytes,
            total_bytes: 0,
            free_formatted: "Unknown".to_string(),
            required_formatted: format_bytes(required_bytes),
            drive_name: destination_dir.to_string(),
            drive_letter: destination_dir.to_string(),
        },
    }
}
