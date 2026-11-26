#![forbid(unsafe_code)]

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use nup_core::{
    PackOptions, check_disk_space, detect_package_summary, extract, extract_single_file,
    get_dir_children, inspect, pack, search_files, verify,
};
use nup_host_logic::{OrbLaunchMode, OrbSettings, OrbSettingsStore, ScreenEdge};
use serde_json::{Value, json};

use crate::assets;
use crate::backdrop::SystemAppearanceState;
use crate::shell_integration;
use crate::updater;

/// Commands routed from IPC back to the native window/UI thread.
#[derive(Debug, Clone)]
pub enum WindowCommand {
    OrbExpand,
    OrbCollapse,
    OrbDragStart,
    OrbDragMove,
    OrbDragEnd,
    WindowDragStart,
    WindowToggleMaximize,
    WindowClose,
    UpdateTooltip(String),
    ThemeOrSettingsChanged {
        launch_mode: Option<OrbLaunchMode>,
        theme: Option<String>,
        reply_id: String,
    },
    GetOrbSettings {
        reply_id: String,
    },
    BrowseFile {
        reply_id: String,
    },
    BrowseFolder {
        reply_id: String,
    },
    BrowsePackFolder {
        reply_id: String,
    },
    BrowsePackOutput {
        reply_id: String,
        default_name: String,
    },
    ExtractSingleFile {
        reply_id: String,
        nup_path: String,
        file_index: usize,
        suggested_name: String,
    },
    FileDropped {
        target: String,
        paths: Vec<String>,
    },
}

/// Sink for sending JSON IPC messages back to WebView2 (or capturing them in contract tests).
pub trait IpcSink: Send + Sync + 'static {
    fn send_json(&self, value: Value);
    fn dispatch_window(&self, cmd: WindowCommand);
}

#[derive(Debug, Clone)]
struct CachedUpdateTarget {
    download_url: String,
    checksum_url: Option<String>,
}

fn cache_update_result(slot: &Mutex<Option<CachedUpdateTarget>>, res: &updater::UpdateCheckResult) {
    if let Ok(mut guard) = slot.lock() {
        if res.error.is_none() && res.update_available && !res.download_url.trim().is_empty() {
            *guard = Some(CachedUpdateTarget {
                download_url: res.download_url.clone(),
                checksum_url: res.checksum_url.clone(),
            });
        } else {
            *guard = None;
        }
    }
}

pub struct IpcRouter {
    sink: Arc<dyn IpcSink>,
    initial_file_path: Option<String>,
    auto_extract_files: Vec<String>,
    extract_cancel: Mutex<Arc<AtomicBool>>,
    verify_cancel: Mutex<Arc<AtomicBool>>,
    pack_cancel: Mutex<Arc<AtomicBool>>,
    update_cancel: Mutex<Arc<AtomicBool>>,
    cached_update: Arc<Mutex<Option<CachedUpdateTarget>>>,
}

impl IpcRouter {
    pub fn new(
        sink: Arc<dyn IpcSink>,
        initial_file_path: Option<String>,
        auto_extract_files: Vec<String>,
    ) -> Self {
        Self {
            sink,
            initial_file_path,
            auto_extract_files,
            extract_cancel: Mutex::new(Arc::new(AtomicBool::new(false))),
            verify_cancel: Mutex::new(Arc::new(AtomicBool::new(false))),
            pack_cancel: Mutex::new(Arc::new(AtomicBool::new(false))),
            update_cancel: Mutex::new(Arc::new(AtomicBool::new(false))),
            cached_update: Arc::new(Mutex::new(None)),
        }
    }

    pub fn cancel_all(&self) {
        for slot in [
            &self.extract_cancel,
            &self.verify_cancel,
            &self.pack_cancel,
            &self.update_cancel,
        ] {
            if let Ok(guard) = slot.lock() {
                guard.store(true, Ordering::Relaxed);
            }
        }
    }

    /// Synchronous dispatcher for unit/golden contract testing (runs inline without spawning threads).
    pub fn handle_message_sync(&self, raw_json: &str, dropped_paths: Vec<String>) {
        self.handle_message_inner(raw_json, dropped_paths, false);
    }

    /// Async dispatcher for live GUI host (offloads heavy engine/network work to worker threads).
    pub fn handle_message(&self, raw_json: &str, dropped_paths: Vec<String>) {
        self.handle_message_inner(raw_json, dropped_paths, true);
    }

    fn handle_message_inner(&self, raw_json: &str, dropped_paths: Vec<String>, spawn_bg: bool) {
        let root: Value = match serde_json::from_str(raw_json) {
            Ok(v) => v,
            Err(e) => {
                self.sink.send_json(json!({
                    "type": "error",
                    "error": e.to_string()
                }));
                return;
            }
        };

        let action = get_str(&root, &["action"]).unwrap_or_default();
        let id = get_str(&root, &["id"]).unwrap_or_default();

        match action.as_str() {
            // --- Floating Orb & Window Controls ---
            "orb_expand" => self.sink.dispatch_window(WindowCommand::OrbExpand),
            "orb_collapse" => self.sink.dispatch_window(WindowCommand::OrbCollapse),
            "orb_drag_start" => self.sink.dispatch_window(WindowCommand::OrbDragStart),
            "orb_drag_move" => self.sink.dispatch_window(WindowCommand::OrbDragMove),
            "orb_drag_end" => self.sink.dispatch_window(WindowCommand::OrbDragEnd),
            "window_drag_start" => self.sink.dispatch_window(WindowCommand::WindowDragStart),
            "window_toggle_maximize" => self
                .sink
                .dispatch_window(WindowCommand::WindowToggleMaximize),
            "window_close" => self.sink.dispatch_window(WindowCommand::WindowClose),
            "orb_file_drop" => {
                let target = get_str(&root, &["target"]).unwrap_or_else(|| "orb".to_string());
                self.sink.dispatch_window(WindowCommand::FileDropped {
                    target,
                    paths: dropped_paths,
                });
            }
            "get_orb_settings" => {
                self.sink
                    .dispatch_window(WindowCommand::GetOrbSettings { reply_id: id });
            }
            "update_orb_settings" => {
                let launch_mode =
                    get_str(&root, &["launchMode"]).map(|s| OrbLaunchMode::parse_lossy(&s));
                let theme = get_str(&root, &["theme"]);
                self.sink
                    .dispatch_window(WindowCommand::ThemeOrSettingsChanged {
                        launch_mode,
                        theme,
                        reply_id: id,
                    });
            }
            "task_progress_update" => {
                let task = get_str(&root, &["task"]).unwrap_or_default();
                let state = get_str(&root, &["state"]).unwrap_or_else(|| "idle".to_string());
                let pct = get_i64(&root, &["percent"]).unwrap_or(0);
                let tip = if state == "running" {
                    let label = match task.as_str() {
                        "verify" => "Verifying",
                        "pack" => "Packing",
                        _ => "Extracting",
                    };
                    format!("NUP Extractor — {label} ({pct}%)")
                } else if state == "done" {
                    "NUP Extractor — Task Complete".to_string()
                } else {
                    "NUP Extractor".to_string()
                };
                self.sink.dispatch_window(WindowCommand::UpdateTooltip(tip));
            }

            // --- Dialogs (must run on UI thread) ---
            "browse_file" => self
                .sink
                .dispatch_window(WindowCommand::BrowseFile { reply_id: id }),
            "browse_folder" => self
                .sink
                .dispatch_window(WindowCommand::BrowseFolder { reply_id: id }),
            "browse_pack_folder" => self
                .sink
                .dispatch_window(WindowCommand::BrowsePackFolder { reply_id: id }),
            "browse_pack_output" => {
                let default_name =
                    get_str(&root, &["defaultName", "name"]).unwrap_or_else(|| "game".to_string());
                self.sink.dispatch_window(WindowCommand::BrowsePackOutput {
                    reply_id: id,
                    default_name,
                });
            }
            "extract_single_file" => {
                let nup_path = assets::resolve_sample_nup_path(
                    &get_str(&root, &["filePath", "file_path"]).unwrap_or_default(),
                );
                let file_index = get_i64(&root, &["fileIndex", "file_index"]).unwrap_or(-1);
                let suggested_name = get_str(&root, &["fileName", "file_name", "name"])
                    .unwrap_or_else(|| "extracted_file".to_string());
                if nup_path.trim().is_empty() || !Path::new(&nup_path).exists() || file_index < 0 {
                    self.sink.send_json(json!({
                        "id": id,
                        "action": "extract_single_file_result",
                        "success": false,
                        "error": "Invalid file parameters."
                    }));
                } else {
                    self.sink.dispatch_window(WindowCommand::ExtractSingleFile {
                        reply_id: id,
                        nup_path,
                        file_index: file_index as usize,
                        suggested_name,
                    });
                }
            }

            // --- Core NUP Engine Operations ---
            "get_app_info" => {
                self.sink.send_json(json!({
                    "id": id,
                    "action": "app_info_result",
                    "success": true,
                    "version": updater::CURRENT_VERSION,
                    "initialFilePath": self.initial_file_path,
                    "autoExtractFiles": self.auto_extract_files
                }));
            }
            "inspect" => {
                let file_path = get_str(&root, &["filePath", "file_path", "path"])
                    .map(|p| assets::resolve_sample_nup_path(&p));
                let sink = Arc::clone(&self.sink);
                let run = move || match file_path {
                    Some(fp) if !fp.trim().is_empty() => match inspect(&fp) {
                        Ok(data) => sink.send_json(json!({
                            "id": id,
                            "action": "inspect_result",
                            "success": true,
                            "data": data
                        })),
                        Err(e) => sink.send_json(json!({
                            "id": id,
                            "action": "inspect_result",
                            "success": false,
                            "error": e.to_string()
                        })),
                    },
                    _ => sink.send_json(json!({
                        "id": id,
                        "action": "inspect_result",
                        "success": false,
                        "error": "File path is required"
                    })),
                };
                if spawn_bg {
                    std::thread::spawn(run);
                } else {
                    run();
                }
            }
            "get_tree" => {
                let file_path = get_str(&root, &["filePath", "file_path"])
                    .map(|p| assets::resolve_sample_nup_path(&p));
                let dir_index = get_i64(&root, &["dirIndex", "dir_index"]).unwrap_or(0) as u32;
                let sink = Arc::clone(&self.sink);
                let run = move || match file_path {
                    Some(fp) if !fp.trim().is_empty() => match get_dir_children(&fp, dir_index) {
                        Ok(data) => sink.send_json(json!({
                            "id": id,
                            "action": "get_tree_result",
                            "success": true,
                            "data": data
                        })),
                        Err(e) => sink.send_json(json!({
                            "id": id,
                            "action": "get_tree_result",
                            "success": false,
                            "error": e.to_string()
                        })),
                    },
                    _ => sink.send_json(json!({
                        "id": id,
                        "action": "get_tree_result",
                        "success": false,
                        "error": "File path is required"
                    })),
                };
                if spawn_bg {
                    std::thread::spawn(run);
                } else {
                    run();
                }
            }
            "search" => {
                let file_path = get_str(&root, &["filePath", "file_path"])
                    .map(|p| assets::resolve_sample_nup_path(&p));
                let query = get_str(&root, &["query", "q"]).unwrap_or_default();
                let sink = Arc::clone(&self.sink);
                let run = move || match file_path {
                    Some(fp) if !fp.trim().is_empty() => match search_files(&fp, &query, 500) {
                        Ok(results) => sink.send_json(json!({
                            "id": id,
                            "action": "search_result",
                            "success": true,
                            "query": query,
                            "results": results
                        })),
                        Err(e) => sink.send_json(json!({
                            "id": id,
                            "action": "search_result",
                            "success": false,
                            "error": e.to_string()
                        })),
                    },
                    _ => sink.send_json(json!({
                        "id": id,
                        "action": "search_result",
                        "success": false,
                        "error": "File path is required"
                    })),
                };
                if spawn_bg {
                    std::thread::spawn(run);
                } else {
                    run();
                }
            }
            "extract" => {
                let file_path = assets::resolve_sample_nup_path(
                    &get_str(&root, &["filePath", "file_path"]).unwrap_or_default(),
                );
                let dest_dir = get_str(&root, &["destDir", "dest_dir"]).unwrap_or_default();
                let mode = get_str(&root, &["mode"]).unwrap_or_else(|| "all".to_string());
                let pkg_idx =
                    get_i64(&root, &["packageIndex", "package_index"]).map(|v| v as usize);
                let subtree = get_str(&root, &["subtreePath", "subtree_path"]);
                let open_when_done =
                    get_bool(&root, &["openWhenDone", "open_when_done"]).unwrap_or(true);

                if file_path.trim().is_empty() || !Path::new(&file_path).exists() {
                    self.sink.send_json(json!({
                        "type": "extract_error",
                        "error": format!("NUP container file not found: {file_path}")
                    }));
                    return;
                }
                if dest_dir.trim().is_empty() {
                    self.sink.send_json(json!({
                        "type": "extract_error",
                        "error": "Destination directory cannot be empty."
                    }));
                    return;
                }

                let cancel = reset_cancel(&self.extract_cancel);
                let sink = Arc::clone(&self.sink);
                let run = move || {
                    let sw = Instant::now();
                    let res = extract(
                        &file_path,
                        &dest_dir,
                        &mode,
                        pkg_idx,
                        subtree.as_deref(),
                        |info| {
                            sink.dispatch_window(WindowCommand::UpdateTooltip(format!(
                                "NUP Extractor — Extracting {}%",
                                info.percent.round() as i64
                            )));
                            sink.send_json(json!({
                                "type": "extract_progress",
                                "info": info
                            }));
                        },
                        &cancel,
                    );
                    match res {
                        Ok(()) => {
                            if open_when_done {
                                open_folder_in_explorer(&dest_dir);
                            }
                            sink.dispatch_window(WindowCommand::UpdateTooltip(
                                "NUP Extractor — Extraction Complete".to_string(),
                            ));
                            sink.send_json(json!({
                                "type": "extract_done",
                                "success": true,
                                "destDir": dest_dir,
                                "elapsedSeconds": sw.elapsed().as_secs_f64()
                            }));
                        }
                        Err(nup_core::NupError::Cancelled) => {
                            sink.dispatch_window(WindowCommand::UpdateTooltip(
                                "NUP Extractor".to_string(),
                            ));
                            sink.send_json(json!({
                                "type": "extract_error",
                                "error": "Extraction cancelled by user."
                            }));
                        }
                        Err(e) => {
                            sink.dispatch_window(WindowCommand::UpdateTooltip(
                                "NUP Extractor".to_string(),
                            ));
                            sink.send_json(json!({
                                "type": "extract_error",
                                "error": e.to_string()
                            }));
                        }
                    }
                };
                if spawn_bg {
                    std::thread::spawn(run);
                } else {
                    run();
                }
            }
            "cancel_extract" => trigger_cancel(&self.extract_cancel),

            "verify" => {
                let file_path = assets::resolve_sample_nup_path(
                    &get_str(&root, &["filePath", "file_path"]).unwrap_or_default(),
                );
                let deep = get_bool(&root, &["deep"]).unwrap_or(true);
                if file_path.trim().is_empty() || !Path::new(&file_path).exists() {
                    self.sink.send_json(json!({
                        "type": "verify_error",
                        "error": format!("NUP container file not found: {file_path}")
                    }));
                    return;
                }
                let cancel = reset_cancel(&self.verify_cancel);
                let sink = Arc::clone(&self.sink);
                let run = move || {
                    let res = verify(
                        &file_path,
                        deep,
                        |info| {
                            if info.status == "done" {
                                sink.dispatch_window(WindowCommand::UpdateTooltip(
                                    "NUP Extractor — Verification Complete".to_string(),
                                ));
                                sink.send_json(json!({
                                    "type": "verify_done",
                                    "info": info
                                }));
                            } else {
                                sink.dispatch_window(WindowCommand::UpdateTooltip(format!(
                                    "NUP Extractor — Verifying {}%",
                                    info.percent.round() as i64
                                )));
                                sink.send_json(json!({
                                    "type": "verify_progress",
                                    "info": info
                                }));
                            }
                        },
                        &cancel,
                    );
                    match res {
                        Ok(_) => {}
                        Err(nup_core::NupError::Cancelled) => {
                            sink.dispatch_window(WindowCommand::UpdateTooltip(
                                "NUP Extractor".to_string(),
                            ));
                            sink.send_json(json!({
                                "type": "verify_error",
                                "error": "Verification cancelled."
                            }));
                        }
                        Err(e) => {
                            sink.dispatch_window(WindowCommand::UpdateTooltip(
                                "NUP Extractor".to_string(),
                            ));
                            sink.send_json(json!({
                                "type": "verify_error",
                                "error": e.to_string()
                            }));
                        }
                    }
                };
                if spawn_bg {
                    std::thread::spawn(run);
                } else {
                    run();
                }
            }
            "cancel_verify" => trigger_cancel(&self.verify_cancel),

            "open_folder" => {
                if let Some(p) = get_str(&root, &["path", "folderPath"]) {
                    open_folder_in_explorer(&p);
                }
            }

            "check_disk_space" => {
                let dest = get_str(&root, &["destDir", "dest_dir", "path"]).unwrap_or_default();
                let req_bytes = get_u64(&root, &["requiredBytes", "required_bytes"]).unwrap_or(0);
                let info = check_disk_space(&dest, req_bytes);
                self.sink.send_json(json!({
                    "id": id,
                    "action": "check_disk_space_result",
                    "success": true,
                    "data": info
                }));
            }

            "get_shell_integration_status" => {
                self.sink.send_json(json!({
                    "id": id,
                    "action": "shell_integration_status_result",
                    "success": true,
                    "registered": shell_integration::is_registered()
                }));
            }
            "toggle_shell_integration" => {
                let enable = get_bool(&root, &["enable"]).unwrap_or(false);
                let ok = if enable {
                    shell_integration::register()
                } else {
                    shell_integration::unregister()
                };
                self.sink.send_json(json!({
                    "id": id,
                    "action": "toggle_shell_integration_result",
                    "success": ok,
                    "registered": shell_integration::is_registered()
                }));
            }

            "inspect_pack_source" => {
                let src = get_str(&root, &["sourceDir", "source_dir"]).unwrap_or_default();
                let sink = Arc::clone(&self.sink);
                let run = move || {
                    if !src.trim().is_empty() && Path::new(&src).is_dir() {
                        let pkgs = detect_package_summary(&src);
                        sink.send_json(json!({
                            "id": id,
                            "action": "inspect_pack_source_result",
                            "success": true,
                            "packages": pkgs
                        }));
                    } else {
                        sink.send_json(json!({
                            "id": id,
                            "action": "inspect_pack_source_result",
                            "success": false,
                            "packages": []
                        }));
                    }
                };
                if spawn_bg {
                    std::thread::spawn(run);
                } else {
                    run();
                }
            }

            "start_pack" => {
                let source_dir = get_str(&root, &["sourceDir", "source_dir"]).unwrap_or_default();
                let output_path =
                    get_str(&root, &["outputPath", "output_path"]).unwrap_or_default();
                if source_dir.trim().is_empty() || !Path::new(&source_dir).is_dir() {
                    self.sink.send_json(json!({
                        "type": "pack_error",
                        "error": format!("Source directory not found: {source_dir}")
                    }));
                    return;
                }
                if output_path.trim().is_empty() {
                    self.sink.send_json(json!({
                        "type": "pack_error",
                        "error": "Output file path cannot be empty."
                    }));
                    return;
                }
                let title_name = get_str(&root, &["titleName", "title_name"]).unwrap_or_default();
                let publisher = get_str(&root, &["publisher"]).unwrap_or_default();
                let version_str = get_str(&root, &["version"]).unwrap_or_default();
                let compression_level =
                    get_i64(&root, &["compressionLevel", "compression_level"]).unwrap_or(3) as i32;
                let mut title_id = 0u64;
                if let Some(tid_s) = get_str(&root, &["titleId", "title_id"]) {
                    let clean = tid_s
                        .trim()
                        .trim_start_matches("0x")
                        .trim_start_matches("0X");
                    if let Ok(parsed) = u64::from_str_radix(clean, 16) {
                        title_id = parsed;
                    }
                }

                let opts = PackOptions {
                    source_directory: source_dir,
                    output_file_path: output_path,
                    title_name,
                    publisher,
                    title_id,
                    version_string: version_str,
                    compression_level,
                    icon_path: None,
                };
                let cancel = reset_cancel(&self.pack_cancel);
                let sink = Arc::clone(&self.sink);
                let run = move || {
                    let sw = Instant::now();
                    match pack(
                        &opts,
                        |info| {
                            sink.dispatch_window(WindowCommand::UpdateTooltip(format!(
                                "NUP Extractor — Packing {}%",
                                info.percent.round() as i64
                            )));
                            sink.send_json(json!({
                                "type": "pack_progress",
                                "info": info
                            }));
                        },
                        &cancel,
                    ) {
                        Ok(()) => {
                            sink.dispatch_window(WindowCommand::UpdateTooltip(
                                "NUP Extractor — Packaging Complete".to_string(),
                            ));
                            sink.send_json(json!({
                                "type": "pack_done",
                                "outputPath": opts.output_file_path,
                                "sourceDir": opts.source_directory,
                                "elapsedSeconds": sw.elapsed().as_secs_f64()
                            }));
                        }
                        Err(nup_core::NupError::Cancelled) => {
                            sink.dispatch_window(WindowCommand::UpdateTooltip(
                                "NUP Extractor".to_string(),
                            ));
                            sink.send_json(json!({
                                "type": "pack_error",
                                "error": "Packaging cancelled by user."
                            }));
                        }
                        Err(e) => {
                            sink.dispatch_window(WindowCommand::UpdateTooltip(
                                "NUP Extractor".to_string(),
                            ));
                            sink.send_json(json!({
                                "type": "pack_error",
                                "error": e.to_string()
                            }));
                        }
                    }
                };
                if spawn_bg {
                    std::thread::spawn(run);
                } else {
                    run();
                }
            }
            "cancel_pack" => trigger_cancel(&self.pack_cancel),

            // --- Auto-Updater ---
            "check_update" => {
                let manual = get_bool(&root, &["manual"]).unwrap_or(false);
                let sink = Arc::clone(&self.sink);
                let update_cache = Arc::clone(&self.cached_update);
                if spawn_bg {
                    let sink_c = Arc::clone(&sink);
                    std::thread::spawn(move || {
                        let res = updater::check_for_updates();
                        cache_update_result(&update_cache, &res);
                        let success = res.error.is_none();
                        sink_c.send_json(json!({
                            "id": id,
                            "action": "check_update_result",
                            "success": success,
                            "error": res.error,
                            "manual": manual,
                            "data": res
                        }));
                    });
                } else {
                    let res = updater::check_for_updates();
                    cache_update_result(&update_cache, &res);
                    let success = res.error.is_none();
                    sink.send_json(json!({
                        "id": id,
                        "action": "check_update_result",
                        "success": success,
                        "error": res.error,
                        "manual": manual,
                        "data": res
                    }));
                }
            }
            "start_update" => {
                // Always ignore JS-supplied downloadUrl / checksumUrl; only execute updates
                // cached by a prior Rust-side `check_update` response.
                let cached = self.cached_update.lock().ok().and_then(|g| g.clone());
                let Some(target) = cached else {
                    self.sink.send_json(json!({
                        "type": "update_error",
                        "error": "No checked update is available. Please check for updates first."
                    }));
                    return;
                };
                if target.download_url.trim().is_empty() {
                    self.sink.send_json(json!({
                        "type": "update_error",
                        "error": "No valid download URL cached for update."
                    }));
                    return;
                }
                let cancel = reset_cancel(&self.update_cancel);
                let sink = Arc::clone(&self.sink);
                let run = move || {
                    let res = updater::apply_update_and_restart(
                        &target.download_url,
                        target.checksum_url.as_deref(),
                        |info| {
                            sink.send_json(json!({
                                "type": "update_progress",
                                "info": info
                            }));
                        },
                        &cancel,
                    );
                    if let Err(err) = res {
                        sink.send_json(json!({
                            "type": "update_error",
                            "error": err
                        }));
                    }
                };
                if spawn_bg {
                    std::thread::spawn(run);
                } else {
                    run();
                }
            }
            "cancel_update" => trigger_cancel(&self.update_cancel),

            _ => {}
        }
    }
}

pub fn build_orb_settings_reply(
    id: &str,
    settings: &OrbSettings,
    orb_edge: ScreenEdge,
    state_lower: &str,
    appearance: Option<SystemAppearanceState>,
) -> Value {
    if let Some(app) = appearance {
        json!({
            "id": id,
            "action": "orb_settings_result",
            "success": true,
            "settings": {
                "launchMode": settings.launch_mode.as_str(),
                "theme": settings.theme,
                "orbEdge": orb_edge.as_str(),
                "state": state_lower
            },
            "appearance": {
                "reducedMotion": app.reduced_motion,
                "highContrast": app.high_contrast,
                "transparencyEnabled": app.transparency_enabled
            }
        })
    } else {
        json!({
            "id": id,
            "action": "orb_settings_result",
            "success": true,
            "settings": {
                "launchMode": settings.launch_mode.as_str(),
                "theme": settings.theme,
                "orbEdge": orb_edge.as_str(),
                "state": state_lower
            }
        })
    }
}

pub fn apply_orb_settings_update(
    settings: &mut OrbSettings,
    launch_mode: Option<OrbLaunchMode>,
    theme: Option<&str>,
) -> bool {
    if let Some(lm) = launch_mode {
        settings.launch_mode = lm;
    }
    let mut theme_changed = false;
    if let Some(t) = theme
        && !t.trim().is_empty()
    {
        settings.theme = if t.eq_ignore_ascii_case("light") {
            "light".to_string()
        } else {
            "dark".to_string()
        };
        theme_changed = true;
    }
    OrbSettingsStore::save(settings, None);
    theme_changed
}

pub fn run_extract_single_file_worker(
    sink: Arc<dyn IpcSink>,
    reply_id: String,
    nup_path: String,
    file_index: usize,
    dest_path: String,
) {
    std::thread::spawn(
        move || match extract_single_file(&nup_path, file_index, &dest_path) {
            Ok(()) => {
                let file_name = Path::new(&dest_path)
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                sink.send_json(json!({
                    "id": reply_id,
                    "action": "extract_single_file_result",
                    "success": true,
                    "destPath": dest_path,
                    "fileName": file_name
                }));
            }
            Err(e) => {
                sink.send_json(json!({
                    "id": reply_id,
                    "action": "extract_single_file_result",
                    "success": false,
                    "error": e.to_string()
                }));
            }
        },
    );
}

pub fn open_folder_in_explorer(folder_path: &str) {
    if !folder_path.trim().is_empty() && Path::new(folder_path).exists() {
        let _ = std::process::Command::new("explorer.exe")
            .arg(folder_path)
            .spawn();
    }
}

fn reset_cancel(slot: &Mutex<Arc<AtomicBool>>) -> Arc<AtomicBool> {
    let mut guard = slot.lock().unwrap();
    guard.store(true, Ordering::Relaxed);
    let fresh = Arc::new(AtomicBool::new(false));
    *guard = Arc::clone(&fresh);
    fresh
}

fn trigger_cancel(slot: &Mutex<Arc<AtomicBool>>) {
    if let Ok(guard) = slot.lock() {
        guard.store(true, Ordering::Relaxed);
    }
}

fn get_str(root: &Value, keys: &[&str]) -> Option<String> {
    for k in keys {
        if let Some(s) = root.get(*k).and_then(|v| v.as_str()) {
            return Some(s.to_string());
        }
    }
    None
}

fn get_i64(root: &Value, keys: &[&str]) -> Option<i64> {
    for k in keys {
        if let Some(n) = root.get(*k).and_then(|v| v.as_i64()) {
            return Some(n);
        }
    }
    None
}

fn get_u64(root: &Value, keys: &[&str]) -> Option<u64> {
    for k in keys {
        if let Some(n) = root.get(*k).and_then(|v| v.as_u64()) {
            return Some(n);
        }
    }
    None
}

fn get_bool(root: &Value, keys: &[&str]) -> Option<bool> {
    for k in keys {
        if let Some(b) = root.get(*k).and_then(|v| v.as_bool()) {
            return Some(b);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;

    struct VecSink {
        messages: Mutex<Vec<Value>>,
    }

    impl VecSink {
        fn new() -> Self {
            Self {
                messages: Mutex::new(Vec::new()),
            }
        }
    }

    impl IpcSink for VecSink {
        fn send_json(&self, value: Value) {
            self.messages.lock().unwrap().push(value);
        }
        fn dispatch_window(&self, _cmd: WindowCommand) {}
    }

    #[test]
    fn start_update_refuses_without_cached_check() {
        let sink = Arc::new(VecSink::new());
        let router = IpcRouter::new(Arc::clone(&sink) as Arc<dyn IpcSink>, None, Vec::new());

        // Even with attacker-supplied URLs in JS payload, start_update must refuse
        // because check_update has not cached an update target.
        let payload = json!({
            "action": "start_update",
            "downloadUrl": "http://127.0.0.1:9/evil.zip",
            "checksumUrl": "http://127.0.0.1:9/SHA256SUMS.txt"
        });
        router.handle_message_sync(&payload.to_string(), Vec::new());

        let msgs = sink.messages.lock().unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0]["type"], "update_error");
        assert!(
            msgs[0]["error"]
                .as_str()
                .unwrap()
                .contains("No checked update"),
            "unexpected error: {:?}",
            msgs[0]
        );
    }

    #[test]
    fn start_update_ignores_js_urls_and_uses_cached_urls_only() {
        // Two local listeners: attacker (must never be contacted) and cached (used by updater).
        let attacker_listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let attacker_addr = attacker_listener.local_addr().unwrap();
        let attacker_hit = Arc::new(AtomicBool::new(false));
        let attacker_hit_c = Arc::clone(&attacker_hit);
        attacker_listener.set_nonblocking(true).unwrap();

        let cached_listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let cached_addr = cached_listener.local_addr().unwrap();
        let cached_hit = Arc::new(AtomicBool::new(false));
        let cached_hit_c = Arc::clone(&cached_hit);
        let srv = std::thread::spawn(move || {
            if let Ok((mut stream, _)) = cached_listener.accept() {
                cached_hit_c.store(true, Ordering::SeqCst);
                let mut br = BufReader::new(&mut stream);
                let mut line = String::new();
                let _ = br.read_line(&mut line);
                let _ = write!(
                    stream,
                    "HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                );
                let _ = stream.flush();
            }
        });

        let sink = Arc::new(VecSink::new());
        let router = IpcRouter::new(Arc::clone(&sink) as Arc<dyn IpcSink>, None, Vec::new());

        let check_res = updater::UpdateCheckResult {
            update_available: true,
            current_version: "1.0.0".into(),
            latest_version: "9.9.9".into(),
            release_name: "v9.9.9".into(),
            release_notes: String::new(),
            release_url: String::new(),
            download_url: format!("http://{cached_addr}/good.zip"),
            checksum_url: Some(format!("http://{cached_addr}/SHA256SUMS.txt")),
            asset_name: "good.zip".into(),
            download_size: 100,
            error: None,
        };
        cache_update_result(&router.cached_update, &check_res);

        let payload = json!({
            "action": "start_update",
            "downloadUrl": format!("http://{attacker_addr}/evil.zip"),
            "checksumUrl": format!("http://{attacker_addr}/SHA256SUMS.txt")
        });
        router.handle_message_sync(&payload.to_string(), Vec::new());

        if attacker_listener.accept().is_ok() {
            attacker_hit_c.store(true, Ordering::SeqCst);
        }
        let _ = srv.join();

        assert!(
            !attacker_hit.load(Ordering::SeqCst),
            "attacker downloadUrl must never be contacted"
        );
        assert!(
            cached_hit.load(Ordering::SeqCst),
            "cached download_url must be the one contacted"
        );
    }
}
