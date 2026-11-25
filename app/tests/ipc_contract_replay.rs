#![forbid(unsafe_code)]

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use nup_extractor_app::backdrop::SystemAppearanceState;
use nup_extractor_app::ipc::{IpcRouter, IpcSink, WindowCommand, build_orb_settings_reply};
use nup_host_logic::{OrbSettings, ScreenEdge};
use serde_json::Value;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf()
}

fn normalize_path_token(raw: &str, root: &Path) -> String {
    let norm_root = root
        .to_string_lossy()
        .replace('\\', "/")
        .trim_end_matches('/')
        .to_string();
    let norm_raw = raw.replace('\\', "/");
    if norm_raw
        .to_ascii_lowercase()
        .starts_with(&norm_root.to_ascii_lowercase())
    {
        format!("<REPO>{}", &norm_raw[norm_root.len()..])
    } else {
        norm_raw
    }
}

struct RecordingSink {
    messages: Mutex<Vec<Value>>,
    commands: Mutex<Vec<WindowCommand>>,
}

impl RecordingSink {
    fn new() -> Self {
        Self {
            messages: Mutex::new(Vec::new()),
            commands: Mutex::new(Vec::new()),
        }
    }

    fn take_messages(&self) -> Vec<Value> {
        std::mem::take(&mut *self.messages.lock().unwrap())
    }

    fn take_commands(&self) -> Vec<WindowCommand> {
        std::mem::take(&mut *self.commands.lock().unwrap())
    }
}

impl IpcSink for RecordingSink {
    fn send_json(&self, value: Value) {
        self.messages.lock().unwrap().push(value);
    }

    fn dispatch_window(&self, cmd: WindowCommand) {
        self.commands.lock().unwrap().push(cmd);
    }
}

#[test]
fn replay_ipc_contract_goldens() {
    let root = repo_root();
    let contract_path = root.join("tests_golden").join("ipc").join("contract.json");
    let contract: Value =
        serde_json::from_str(&fs::read_to_string(&contract_path).unwrap()).unwrap();

    let sink = Arc::new(RecordingSink::new());
    let router = IpcRouter::new(Arc::clone(&sink) as Arc<dyn IpcSink>, None, Vec::new());

    for ex in contract["exchanges"].as_array().unwrap() {
        let name = ex["name"].as_str().unwrap();
        let req_str = ex["request"]
            .to_string()
            .replace("<REPO>", &root.to_string_lossy().replace('\\', "/"));
        let expected = &ex["response"];

        sink.take_messages();
        sink.take_commands();
        router.handle_message_sync(&req_str, Vec::new());

        let msgs = sink.take_messages();
        let cmds = sink.take_commands();

        match name {
            "get_app_info" => {
                let actual = msgs.last().expect("app_info reply");
                assert_eq!(actual["id"], expected["id"]);
                assert_eq!(actual["action"], expected["action"]);
                assert_eq!(actual["success"], expected["success"]);
                assert_eq!(actual["initialFilePath"], expected["initialFilePath"]);
                assert!(!actual["version"].as_str().unwrap().is_empty());
            }
            "inspect" => {
                let mut actual = msgs.last().expect("inspect reply").clone();
                assert_eq!(actual["id"], expected["id"]);
                assert_eq!(actual["action"], expected["action"]);
                assert_eq!(actual["success"], expected["success"]);
                for k in [
                    "file_path",
                    "filePath",
                    "default_dest_dir",
                    "defaultDestDir",
                ] {
                    if let Some(s) = actual["data"][k].as_str() {
                        actual["data"][k] = Value::String(normalize_path_token(s, &root));
                    }
                }
                assert_eq!(actual["data"], expected["data"]);
            }
            "get_tree" => {
                let actual = msgs.last().expect("get_tree reply");
                assert_eq!(actual, expected);
            }
            "search" => {
                let actual = msgs.last().expect("search reply");
                assert_eq!(actual, expected);
            }
            "verify_done" => {
                let actual = msgs
                    .iter()
                    .find(|m| m["type"] == "verify_done")
                    .expect("verify_done event");
                assert_eq!(actual["type"], expected["type"]);
                assert_eq!(actual["info"]["valid"], expected["info"]["valid"]);
                assert_eq!(
                    actual["info"]["totalFilesChecked"],
                    expected["info"]["totalFilesChecked"]
                );
                assert_eq!(actual["info"]["problems"], expected["info"]["problems"]);
            }
            "get_orb_settings" => {
                assert!(matches!(
                    cmds.last(),
                    Some(WindowCommand::GetOrbSettings { reply_id }) if reply_id == "req_5"
                ));
                let reply = build_orb_settings_reply(
                    "req_5",
                    &OrbSettings::default(),
                    ScreenEdge::Right,
                    "expanded",
                    Some(SystemAppearanceState {
                        reduced_motion: false,
                        high_contrast: false,
                        transparency_enabled: true,
                    }),
                );
                assert_eq!(&reply, expected);
            }
            "morph_frame_event" => {
                assert!(matches!(cmds.last(), Some(WindowCommand::OrbExpand)));
            }
            other => panic!("Unhandled IPC contract exchange: {other}"),
        }
    }
}

#[test]
fn test_embedded_static_assets_present() {
    for path in [
        "index.html",
        "styles.css",
        "ipc.js",
        "app.js",
        "app_icon.ico",
    ] {
        let (bytes, mime) = nup_extractor_app::assets::get_asset(path)
            .unwrap_or_else(|| panic!("Missing embedded asset: {path}"));
        assert!(!bytes.is_empty(), "Empty asset: {path}");
        assert!(!mime.is_empty());
    }
    assert!(nup_extractor_app::assets::get_asset("../secret").is_none());
}

#[test]
fn test_shell_integration_status_does_not_panic() {
    let _ = nup_extractor_app::shell_integration::is_registered();
}
