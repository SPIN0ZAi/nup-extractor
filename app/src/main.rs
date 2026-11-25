#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
#![allow(unsafe_code)]

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

fn write_stdout(msg: &str) {
    let _ = writeln!(std::io::stdout(), "{msg}");
}

fn write_stderr(msg: &str) {
    let _ = writeln!(std::io::stderr(), "{msg}");
}

use nup_core::{PackOptions, extract, inspect, pack};
use nup_extractor_app::{ipc, updater, window};
use windows::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW};
use windows::core::PCWSTR;

fn show_error_box(title: &str, msg: &str) {
    let t: Vec<u16> = title.encode_utf16().chain(std::iter::once(0)).collect();
    let m: Vec<u16> = msg.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        let _ = MessageBoxW(
            None,
            PCWSTR(m.as_ptr()),
            PCWSTR(t.as_ptr()),
            MB_OK | MB_ICONERROR,
        );
    }
}

fn normalize_clean_path(raw: &str) -> String {
    let abs = std::fs::canonicalize(raw).unwrap_or_else(|_| PathBuf::from(raw));
    let s = abs.to_string_lossy().into_owned();
    s.strip_prefix(r"\\?\").unwrap_or(&s).to_string()
}

fn is_nup_extension(raw: &str) -> bool {
    Path::new(raw)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("nup"))
}

fn derive_stem_extract_dir(target: &str) -> String {
    let clean = normalize_clean_path(target);
    let abs = Path::new(&clean);
    let base = abs.parent().unwrap_or_else(|| Path::new("."));
    let stem = abs
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "nup".to_string());
    let dest_pb = base.join(format!("{stem}_extracted"));
    let dest_s = dest_pb.to_string_lossy().into_owned();
    dest_s.strip_prefix(r"\\?\").unwrap_or(&dest_s).to_string()
}

#[derive(Debug, PartialEq, Eq)]
enum StartupAction {
    UpdateReplace {
        target_exe: String,
        old_pid: u32,
    },
    ExtractHere {
        target: String,
    },
    ExtractToFolder {
        target: String,
    },
    ExtractCliBatch {
        targets: Vec<String>,
    },
    ExtractToExplicitDir {
        target: String,
        dest_dir: String,
    },
    Pack {
        source_dir: String,
        output_nup: String,
    },
    PackHere {
        source_dir: String,
    },
    TestInspect {
        target: String,
    },
    TestExtract {
        target: String,
        dest: String,
    },
    LaunchGui {
        initial_file_path: Option<String>,
        auto_extract_files: Vec<String>,
        startup_mode_override: Option<String>,
    },
}

fn parse_startup_action_with<F>(args: &[String], is_existing_nup: F) -> StartupAction
where
    F: Fn(&str) -> bool,
{
    let mut startup_mode_override: Option<String> = None;
    let mut clean_args: Vec<String> = Vec::with_capacity(args.len());

    for arg in args {
        if let Some(rest) = arg
            .strip_prefix("--start-mode=")
            .or_else(|| arg.strip_prefix("--START-MODE="))
        {
            startup_mode_override = Some(rest.trim().to_string());
        } else {
            clean_args.push(arg.clone());
        }
    }

    if let Some(first) = clean_args.first() {
        if first == "--update-replace"
            && clean_args.len() >= 3
            && let Ok(old_pid) = clean_args[2].parse::<u32>()
        {
            return StartupAction::UpdateReplace {
                target_exe: clean_args[1].clone(),
                old_pid,
            };
        }

        if first == "--extract-here" && clean_args.len() > 1 {
            return StartupAction::ExtractHere {
                target: clean_args[1].clone(),
            };
        }

        if first == "--extract-to-folder" && clean_args.len() > 1 {
            return StartupAction::ExtractToFolder {
                target: clean_args[1].clone(),
            };
        }

        if first == "--extract" && clean_args.len() > 1 {
            let targets: Vec<String> = clean_args[1..]
                .iter()
                .filter(|a| !a.starts_with("--") && (is_existing_nup(a) || is_nup_extension(a)))
                .cloned()
                .collect();
            if !targets.is_empty() {
                return StartupAction::ExtractCliBatch { targets };
            }
        }

        if first == "--pack" && clean_args.len() > 2 {
            return StartupAction::Pack {
                source_dir: clean_args[1].clone(),
                output_nup: clean_args[2].clone(),
            };
        }

        if first == "--pack-here" && clean_args.len() > 1 {
            return StartupAction::PackHere {
                source_dir: clean_args[1].clone(),
            };
        }

        if first == "--test-inspect" {
            return StartupAction::TestInspect {
                target: clean_args
                    .get(1)
                    .cloned()
                    .unwrap_or_else(|| "sample_game.nup".to_string()),
            };
        }

        if first == "--test-extract" {
            return StartupAction::TestExtract {
                target: clean_args
                    .get(1)
                    .cloned()
                    .unwrap_or_else(|| "sample_game.nup".to_string()),
                dest: clean_args
                    .get(2)
                    .cloned()
                    .unwrap_or_else(|| "test_native_extract".to_string()),
            };
        }

        // HKCU Explorer double-click association launches with `--open <file.nup>` -> inspect only
        if first == "--open" && clean_args.len() > 1 {
            let candidate = &clean_args[1];
            let initial_file_path = if is_existing_nup(candidate) {
                Some(normalize_clean_path(candidate))
            } else {
                None
            };
            return StartupAction::LaunchGui {
                initial_file_path,
                auto_extract_files: Vec::new(),
                startup_mode_override,
            };
        }

        // ShadPS4Plus-style 2-arg CLI parity: `exe <file.nup> <dest_dir>` -> headless extraction
        if clean_args.len() == 2
            && is_nup_extension(&clean_args[0])
            && !is_nup_extension(&clean_args[1])
            && !clean_args[1].starts_with("--")
        {
            return StartupAction::ExtractToExplicitDir {
                target: clean_args[0].clone(),
                dest_dir: clean_args[1].clone(),
            };
        }

        // Dragging one or more .nup files onto NUP_Extractor.exe -> expand UI & auto-extract
        let mut dropped_nups: Vec<String> = Vec::new();
        for a in &clean_args {
            if !a.starts_with("--") && is_existing_nup(a) {
                let norm = normalize_clean_path(a);
                if !dropped_nups
                    .iter()
                    .any(|existing| existing.eq_ignore_ascii_case(&norm))
                {
                    dropped_nups.push(norm);
                }
            }
        }
        if !dropped_nups.is_empty() {
            let initial_file_path = if dropped_nups.len() == 1 {
                Some(dropped_nups[0].clone())
            } else {
                None
            };
            return StartupAction::LaunchGui {
                initial_file_path,
                auto_extract_files: dropped_nups,
                startup_mode_override,
            };
        }
    }

    StartupAction::LaunchGui {
        initial_file_path: None,
        auto_extract_files: Vec::new(),
        startup_mode_override,
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let action =
        parse_startup_action_with(&args, |p| Path::new(p).is_file() && is_nup_extension(p));
    let cancel = AtomicBool::new(false);

    match action {
        StartupAction::UpdateReplace {
            target_exe,
            old_pid,
        } => {
            if let Err(e) = updater::perform_legacy_update_replace(Path::new(&target_exe), old_pid)
            {
                show_error_box(
                    "Update Error",
                    &format!("Failed to apply update to '{target_exe}':\n{e}"),
                );
            }
        }

        StartupAction::ExtractHere { target } => {
            let clean = normalize_clean_path(&target);
            let dest = Path::new(&clean)
                .parent()
                .map(|p| p.to_string_lossy().into_owned())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| ".".to_string());
            match extract(&target, &dest, "all", None, None, |_| {}, &cancel) {
                Ok(()) => ipc::open_folder_in_explorer(&dest),
                Err(e) => show_error_box(
                    "Extraction Error",
                    &format!("Failed to extract '{target}':\n{e}"),
                ),
            }
        }

        StartupAction::ExtractToFolder { target } => {
            let dest = derive_stem_extract_dir(&target);
            match extract(&target, &dest, "all", None, None, |_| {}, &cancel) {
                Ok(()) => ipc::open_folder_in_explorer(&dest),
                Err(e) => show_error_box(
                    "Extraction Error",
                    &format!("Failed to extract '{target}':\n{e}"),
                ),
            }
        }

        StartupAction::ExtractCliBatch { targets } => {
            let mut last_dest = String::new();
            for target in &targets {
                let dest = derive_stem_extract_dir(target);
                match extract(target, &dest, "all", None, None, |_| {}, &cancel) {
                    Ok(()) => {
                        write_stdout(&format!("EXTRACT_SUCCESS: {target} -> {dest}"));
                        last_dest = dest;
                    }
                    Err(e) => {
                        show_error_box(
                            "Extraction Error",
                            &format!("Failed to extract '{target}':\n{e}"),
                        );
                        return;
                    }
                }
            }
            if targets.len() == 1 && !last_dest.is_empty() {
                ipc::open_folder_in_explorer(&last_dest);
            } else if let Some(parent) = Path::new(&last_dest).parent() {
                ipc::open_folder_in_explorer(&parent.to_string_lossy());
            }
        }

        StartupAction::ExtractToExplicitDir { target, dest_dir } => {
            match extract(&target, &dest_dir, "all", None, None, |_| {}, &cancel) {
                Ok(()) => write_stdout(&format!("EXTRACT_SUCCESS: {target} -> {dest_dir}")),
                Err(e) => {
                    write_stderr(&format!("EXTRACT_ERROR: {e}"));
                    show_error_box(
                        "Extraction Error",
                        &format!("Failed to extract '{target}' to '{dest_dir}':\n{e}"),
                    );
                    std::process::exit(1);
                }
            }
        }

        StartupAction::Pack {
            source_dir,
            output_nup,
        } => {
            let title = Path::new(&source_dir)
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "Custom Game".to_string());
            let opt = PackOptions {
                source_directory: source_dir.clone(),
                output_file_path: output_nup,
                title_name: title,
                ..PackOptions::default()
            };
            if let Err(e) = pack(&opt, |_| {}, &cancel) {
                show_error_box(
                    "Packaging Error",
                    &format!("Failed to pack '{source_dir}':\n{e}"),
                );
            }
        }

        StartupAction::PackHere { source_dir } => {
            let clean = normalize_clean_path(&source_dir);
            let abs = Path::new(&clean);
            let base = abs.parent().unwrap_or_else(|| Path::new("."));
            let name = abs
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "game".to_string());
            let out_pb = base.join(format!("{name}.nup"));
            let out_s = out_pb.to_string_lossy().into_owned();
            let out = out_s.strip_prefix(r"\\?\").unwrap_or(&out_s).to_string();
            let opt = PackOptions {
                source_directory: source_dir.clone(),
                output_file_path: out,
                title_name: name,
                ..PackOptions::default()
            };
            match pack(&opt, |_| {}, &cancel) {
                Ok(()) => ipc::open_folder_in_explorer(&base.to_string_lossy()),
                Err(e) => show_error_box(
                    "Packaging Error",
                    &format!("Failed to pack '{source_dir}':\n{e}"),
                ),
            }
        }

        StartupAction::TestInspect { target } => match inspect(&target) {
            Ok(data) => write_stdout(&format!("INSPECT_SUCCESS: {data}")),
            Err(e) => write_stdout(&format!("INSPECT_ERROR: {e}")),
        },

        StartupAction::TestExtract { target, dest } => {
            match extract(
                &target,
                &dest,
                "all",
                None,
                None,
                |p| {
                    if p.status == "extracting" {
                        write_stdout(&format!("PROGRESS: {}/{} - {}", p.current, p.total, p.file_name));
                    }
                },
                &cancel,
            ) {
                Ok(()) => write_stdout("EXTRACT_SUCCESS"),
                Err(e) => write_stdout(&format!("EXTRACT_ERROR: {e}")),
            }
        }

        StartupAction::LaunchGui {
            initial_file_path,
            auto_extract_files,
            startup_mode_override,
        } => {
            if let Err(e) =
                window::run_app(initial_file_path, auto_extract_files, startup_mode_override)
            {
                show_error_box(
                    "Initialization Error",
                    &format!(
                        "Failed to initialize NUP Extractor:\n{e}\n\nPlease ensure WebView2 Runtime is installed."
                    ),
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_open_flag_loads_inspector_without_auto_extract() {
        let args = vec!["--open".to_string(), r"C:\Games\sample.nup".to_string()];
        let action = parse_startup_action_with(&args, |p| p.ends_with(".nup"));
        match action {
            StartupAction::LaunchGui {
                initial_file_path,
                auto_extract_files,
                ..
            } => {
                assert!(initial_file_path.is_some());
                assert!(auto_extract_files.is_empty());
            }
            other => panic!("Unexpected action: {other:?}"),
        }
    }

    #[test]
    fn cli_drop_multiple_nups_triggers_gui_auto_extract() {
        let args = vec![
            r"C:\Games\base.nup".to_string(),
            r"C:\Games\update.nup".to_string(),
            r"C:\Games\BASE.nup".to_string(),
        ];
        let action = parse_startup_action_with(&args, |p| p.to_ascii_lowercase().ends_with(".nup"));
        match action {
            StartupAction::LaunchGui {
                initial_file_path,
                auto_extract_files,
                ..
            } => {
                assert!(initial_file_path.is_none());
                assert_eq!(auto_extract_files.len(), 2);
            }
            other => panic!("Unexpected action: {other:?}"),
        }
    }

    #[test]
    fn cli_drop_single_nup_onto_exe_sets_both_inspect_and_auto_extract() {
        let args = vec![r"C:\Games\game.nup".to_string()];
        let action = parse_startup_action_with(&args, |p| p.to_ascii_lowercase().ends_with(".nup"));
        match action {
            StartupAction::LaunchGui {
                initial_file_path,
                auto_extract_files,
                ..
            } => {
                assert!(initial_file_path.is_some());
                assert_eq!(auto_extract_files.len(), 1);
            }
            other => panic!("Unexpected action: {other:?}"),
        }
    }

    #[test]
    fn cli_two_args_file_and_dest_extracts_headlessly() {
        let args = vec![
            r"C:\Games\game.nup".to_string(),
            r"D:\Output\Game".to_string(),
        ];
        let action = parse_startup_action_with(&args, |p| p.to_ascii_lowercase().ends_with(".nup"));
        assert_eq!(
            action,
            StartupAction::ExtractToExplicitDir {
                target: r"C:\Games\game.nup".to_string(),
                dest_dir: r"D:\Output\Game".to_string(),
            }
        );
    }

    #[test]
    fn cli_extract_batch_flag_collects_targets() {
        let args = vec![
            "--extract".to_string(),
            r"C:\Games\base.nup".to_string(),
            r"C:\Games\dlc.nup".to_string(),
        ];
        let action = parse_startup_action_with(&args, |p| p.to_ascii_lowercase().ends_with(".nup"));
        assert_eq!(
            action,
            StartupAction::ExtractCliBatch {
                targets: vec![
                    r"C:\Games\base.nup".to_string(),
                    r"C:\Games\dlc.nup".to_string(),
                ],
            }
        );
    }
}
