#![forbid(unsafe_code)]

use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use nup_core::format_bytes;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

pub const CURRENT_VERSION: &str = match option_env!("NUP_RELEASE_VERSION") {
    Some(v) if !v.is_empty() => v,
    _ => env!("CARGO_PKG_VERSION"),
};
pub const GITHUB_REPO: &str = "SPIN0ZAi/nup-extractor";
pub const DEFAULT_RELEASE_API_URL: &str =
    "https://api.github.com/repos/SPIN0ZAi/nup-extractor/releases/latest";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateCheckResult {
    pub update_available: bool,
    pub current_version: String,
    pub latest_version: String,
    pub release_name: String,
    pub release_notes: String,
    pub release_url: String,
    pub download_url: String,
    pub checksum_url: Option<String>,
    pub asset_name: String,
    pub download_size: u64,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateProgressInfo {
    pub stage: String,
    pub percent: f64,
    pub bytes_downloaded: u64,
    pub total_bytes: u64,
    pub speed_formatted: String,
    pub message: String,
    pub error: Option<String>,
}

pub fn parse_version(tag: &str) -> (u32, u32, u32) {
    let clean = tag.trim().trim_start_matches(['v', 'V']);
    let clean = clean.split('-').next().unwrap_or(clean);
    let mut parts = clean.split('.');
    let major = parts
        .next()
        .and_then(|p| p.parse::<u32>().ok())
        .unwrap_or(1);
    let minor = parts
        .next()
        .and_then(|p| p.parse::<u32>().ok())
        .unwrap_or(0);
    let patch = parts
        .next()
        .and_then(|p| p.parse::<u32>().ok())
        .unwrap_or(0);
    (major, minor, patch)
}

fn build_http_client() -> Result<reqwest::blocking::Client, String> {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(30))
        .user_agent(format!("NUP-Extractor/{CURRENT_VERSION}"))
        .build()
        .map_err(|e| e.to_string())
}

pub fn check_for_updates() -> UpdateCheckResult {
    check_for_updates_from_url(DEFAULT_RELEASE_API_URL, CURRENT_VERSION)
}

pub fn check_for_updates_from_url(api_url: &str, current_version: &str) -> UpdateCheckResult {
    let mut res = UpdateCheckResult {
        update_available: false,
        current_version: current_version.to_string(),
        latest_version: current_version.to_string(),
        release_name: String::new(),
        release_notes: String::new(),
        release_url: String::new(),
        download_url: String::new(),
        checksum_url: None,
        asset_name: String::new(),
        download_size: 0,
        error: None,
    };

    let client = match build_http_client() {
        Ok(c) => c,
        Err(e) => {
            res.error = Some(e);
            return res;
        }
    };

    let response = match client
        .get(api_url)
        .header("Accept", "application/vnd.github.v3+json")
        .send()
    {
        Ok(r) => r,
        Err(e) => {
            res.error = Some(e.to_string());
            return res;
        }
    };

    if !response.status().is_success() {
        res.error = Some(format!("GitHub API returned {}", response.status()));
        return res;
    }

    let root: Value = match response.json() {
        Ok(v) => v,
        Err(e) => {
            res.error = Some(e.to_string());
            return res;
        }
    };

    let tag_name = root["tag_name"].as_str().unwrap_or("");
    let release_name = root["name"].as_str().unwrap_or(tag_name);
    let release_notes = root["body"].as_str().unwrap_or("");
    let html_url = root["html_url"].as_str().unwrap_or("");

    let mut download_url = String::new();
    let mut asset_name = String::new();
    let mut download_size = 0u64;
    let mut checksum_url: Option<String> = None;

    if let Some(assets) = root["assets"].as_array() {
        for asset in assets {
            let name = asset["name"].as_str().unwrap_or("");
            let low = name.to_ascii_lowercase();
            if low == "sha256sums.txt" {
                checksum_url = asset["browser_download_url"].as_str().map(String::from);
            }
            if download_url.is_empty()
                && low.ends_with(".zip")
                && (low.contains("nup_extractor") || low.contains("windows"))
            {
                download_url = asset["browser_download_url"]
                    .as_str()
                    .unwrap_or("")
                    .to_string();
                asset_name = name.to_string();
                download_size = asset["size"].as_u64().unwrap_or(0);
            }
        }
        if download_url.is_empty() {
            for asset in assets {
                let name = asset["name"].as_str().unwrap_or("");
                if name.to_ascii_lowercase().ends_with(".zip") {
                    download_url = asset["browser_download_url"]
                        .as_str()
                        .unwrap_or("")
                        .to_string();
                    asset_name = name.to_string();
                    download_size = asset["size"].as_u64().unwrap_or(0);
                    break;
                }
            }
        }
    }

    let cur_ver = parse_version(current_version);
    let latest_ver = parse_version(tag_name);

    res.latest_version = tag_name.trim_start_matches(['v', 'V']).to_string();
    res.release_name = release_name.to_string();
    res.release_notes = release_notes.to_string();
    res.release_url = html_url.to_string();
    res.download_url = download_url.clone();
    res.checksum_url = checksum_url;
    res.asset_name = asset_name;
    res.download_size = download_size;
    res.update_available = latest_ver > cur_ver && !download_url.is_empty();
    res
}

/// Parse standard `SHA256SUMS.txt` lines (`<64-hex>  <filename>` or `<64-hex> *<filename>`).
pub fn parse_sha256_sums(contents: &str, target_filename: &str) -> Option<String> {
    let target_low = target_filename.to_ascii_lowercase();
    for line in contents.lines() {
        let trimmed = line.trim_start_matches('\u{feff}').trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let mut parts = trimmed.split_whitespace();
        let Some(hash) = parts.next() else { continue };
        let Some(fname) = parts.next() else { continue };
        let clean_name = fname.trim_start_matches('*').to_ascii_lowercase();
        let basename = clean_name.rsplit(['/', '\\']).next().unwrap_or(&clean_name);
        if hash.len() == 64
            && hash.chars().all(|c| c.is_ascii_hexdigit())
            && (basename == target_low || target_low.is_empty())
        {
            return Some(hash.to_ascii_lowercase());
        }
    }
    None
}

/// Downloads the update archive, verifies its SHA-256 against `checksum_url` when provided
/// (`DIV-004`), and extracts the new executable into a temp staging directory.
/// Returns `(staged_exe_path, temp_dir_path)` without exiting the current process (testable!).
pub fn download_and_stage_update<F>(
    download_url: &str,
    checksum_url: Option<&str>,
    mut progress: F,
    cancel: &AtomicBool,
) -> Result<(PathBuf, PathBuf), String>
where
    F: FnMut(UpdateProgressInfo),
{
    if cancel.load(Ordering::Relaxed) {
        return Err("Update cancelled by user.".into());
    }

    // Fail closed: require an accompanying SHA256SUMS.txt URL before downloading or staging.
    let sums_url = checksum_url
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            "Update verification failed: release does not provide SHA256SUMS.txt.".to_string()
        })?;

    static TEMP_DIR_SEQ: AtomicU64 = AtomicU64::new(0);
    let seq = TEMP_DIR_SEQ.fetch_add(1, Ordering::Relaxed);
    let client = build_http_client()?;
    let temp_base = std::env::temp_dir().join(format!(
        "NupExtractor_Update_{}_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
        seq,
    ));
    fs::create_dir_all(&temp_base).map_err(|e| e.to_string())?;
    let zip_path = temp_base.join("update.zip");
    let extract_dir = temp_base.join("extracted");

    let run = (|| -> Result<PathBuf, String> {
        progress(UpdateProgressInfo {
            stage: "downloading".into(),
            percent: 0.0,
            bytes_downloaded: 0,
            total_bytes: 0,
            speed_formatted: String::new(),
            message: "Connecting to GitHub Releases server...".into(),
            error: None,
        });

        let mut resp = client.get(download_url).send().map_err(|e| e.to_string())?;
        if !resp.status().is_success() {
            return Err(format!("Download failed with HTTP {}", resp.status()));
        }

        let total_bytes = resp.content_length().unwrap_or(0);
        let mut out = File::create(&zip_path).map_err(|e| e.to_string())?;
        let mut hasher = Sha256::new();
        let mut buf = vec![0u8; 65536];
        let mut total_downloaded = 0u64;
        let sw = Instant::now();
        let mut last_report_ms = 0u128;
        let mut bytes_since_last = 0u64;
        let mut speed = 0.0f64;

        loop {
            if cancel.load(Ordering::Relaxed) {
                return Err("Update cancelled by user.".into());
            }
            let n = resp.read(&mut buf).map_err(|e| e.to_string())?;
            if n == 0 {
                break;
            }
            out.write_all(&buf[..n]).map_err(|e| e.to_string())?;
            hasher.update(&buf[..n]);
            total_downloaded += n as u64;
            bytes_since_last += n as u64;

            let now_ms = sw.elapsed().as_millis();
            if now_ms.saturating_sub(last_report_ms) >= 100 {
                let secs = (now_ms - last_report_ms) as f64 / 1000.0;
                if secs > 0.0 {
                    speed = (bytes_since_last as f64 / (1024.0 * 1024.0)) / secs;
                }
                last_report_ms = now_ms;
                bytes_since_last = 0;

                let pct = if total_bytes > 0 {
                    ((total_downloaded as f64 / total_bytes as f64) * 1000.0).round() / 10.0
                } else {
                    0.0
                };
                let total_label = if total_bytes > 0 {
                    format_bytes(total_bytes)
                } else {
                    "...".to_string()
                };
                progress(UpdateProgressInfo {
                    stage: "downloading".into(),
                    percent: pct,
                    bytes_downloaded: total_downloaded,
                    total_bytes: if total_bytes > 0 {
                        total_bytes
                    } else {
                        total_downloaded
                    },
                    speed_formatted: format!("{speed:.1} MB/s"),
                    message: format!(
                        "Downloading update package ({} / {total_label})",
                        format_bytes(total_downloaded)
                    ),
                    error: None,
                });
            }
        }
        drop(out);

        let actual_digest = hasher.finalize();
        let mut actual_hex = String::with_capacity(64);
        for b in actual_digest {
            use std::fmt::Write as _;
            let _ = write!(&mut actual_hex, "{b:02x}");
        }

        // Verify SHA-256 against SHA256SUMS.txt (mandatory; fail closed).
        progress(UpdateProgressInfo {
            stage: "verifying".into(),
            percent: 100.0,
            bytes_downloaded: total_downloaded,
            total_bytes: total_downloaded,
            speed_formatted: String::new(),
            message: "Verifying SHA-256 checksum...".into(),
            error: None,
        });
        let sums_resp = client.get(sums_url).send().map_err(|e| e.to_string())?;
        if !sums_resp.status().is_success() {
            return Err(format!(
                "Failed to fetch SHA256SUMS.txt (HTTP {})",
                sums_resp.status()
            ));
        }
        let sums_text = sums_resp.text().map_err(|e| e.to_string())?;
        let asset_name = download_url.rsplit('/').next().unwrap_or("");
        let expected_hex = parse_sha256_sums(&sums_text, asset_name)
            .ok_or_else(|| format!("Asset '{asset_name}' not found in SHA256SUMS.txt"))?;
        if actual_hex != expected_hex {
            return Err(format!(
                "SHA-256 checksum mismatch for update archive (expected {expected_hex}, got {actual_hex})"
            ));
        }

        // Extract archive safely
        progress(UpdateProgressInfo {
            stage: "extracting".into(),
            percent: 100.0,
            bytes_downloaded: total_downloaded,
            total_bytes: total_downloaded,
            speed_formatted: String::new(),
            message: "Extracting update package...".into(),
            error: None,
        });

        fs::create_dir_all(&extract_dir).map_err(|e| e.to_string())?;
        let zip_file = File::open(&zip_path).map_err(|e| e.to_string())?;
        let mut archive = zip::ZipArchive::new(zip_file).map_err(|e| e.to_string())?;
        let mut found_exes: Vec<PathBuf> = Vec::new();

        for i in 0..archive.len() {
            if cancel.load(Ordering::Relaxed) {
                return Err("Update cancelled by user.".into());
            }
            let mut entry = archive.by_index(i).map_err(|e| e.to_string())?;
            let Some(enclosed) = entry.enclosed_name().map(|p| p.to_path_buf()) else {
                return Err("Unsafe path traversal entry in update archive.".into());
            };
            let out_path = extract_dir.join(&enclosed);
            if entry.is_dir() {
                fs::create_dir_all(&out_path).map_err(|e| e.to_string())?;
            } else {
                if let Some(parent) = out_path.parent() {
                    fs::create_dir_all(parent).map_err(|e| e.to_string())?;
                }
                let mut dest = File::create(&out_path).map_err(|e| e.to_string())?;
                std::io::copy(&mut entry, &mut dest).map_err(|e| e.to_string())?;
                if out_path
                    .extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(|e| e.eq_ignore_ascii_case("exe"))
                {
                    found_exes.push(out_path);
                }
            }
        }

        if found_exes.is_empty() {
            return Err("No executable found inside the update package.".into());
        }

        let chosen = found_exes
            .iter()
            .find(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.to_ascii_lowercase().contains("nup"))
            })
            .unwrap_or(&found_exes[0])
            .clone();

        Ok(chosen)
    })();

    match run {
        Ok(exe_path) => Ok((exe_path, temp_base)),
        Err(e) => {
            let _ = fs::remove_dir_all(&temp_base);
            Err(e)
        }
    }
}

/// Applies the staged update binary in-place using `self-replace` and restarts the executable.
pub fn apply_update_and_restart<F>(
    download_url: &str,
    checksum_url: Option<&str>,
    mut progress: F,
    cancel: &AtomicBool,
) -> Result<(), String>
where
    F: FnMut(UpdateProgressInfo),
{
    let current_exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let (staged_exe, temp_base) =
        download_and_stage_update(download_url, checksum_url, &mut progress, cancel)?;

    progress(UpdateProgressInfo {
        stage: "applying".into(),
        percent: 100.0,
        bytes_downloaded: 0,
        total_bytes: 0,
        speed_formatted: String::new(),
        message: "Applying update in-place...".into(),
        error: None,
    });

    if let Err(e) = self_replace::self_replace(&staged_exe) {
        let _ = fs::remove_dir_all(&temp_base);
        return Err(format!("Failed to replace executable: {e}"));
    }

    let _ = fs::remove_dir_all(&temp_base);

    progress(UpdateProgressInfo {
        stage: "ready".into(),
        percent: 100.0,
        bytes_downloaded: 0,
        total_bytes: 0,
        speed_formatted: String::new(),
        message: "Restarting application to apply update...".into(),
        error: None,
    });

    let _ = std::process::Command::new(&current_exe).spawn();
    std::thread::sleep(Duration::from_millis(300));
    std::process::exit(0);
}

/// Backward-compatible `--update-replace <targetExe> <oldPid>` helper for legacy updater handoff.
pub fn perform_legacy_update_replace(target_exe: &Path, old_pid: u32) -> Result<(), String> {
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(15) {
        let mut cmd = std::process::Command::new("tasklist");
        cmd.args(["/FI", &format!("PID eq {old_pid}"), "/NH"]);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
        let still_running = cmd
            .output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).contains(&old_pid.to_string()))
            .unwrap_or(false);
        if !still_running {
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    std::thread::sleep(Duration::from_millis(300));

    let current_exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let old_backup = PathBuf::from(format!("{}.old", target_exe.display()));
    let _ = fs::remove_file(&old_backup);

    let retry_fs = |mut op: Box<dyn FnMut() -> std::io::Result<()>>| -> Result<(), String> {
        let mut last_err = None;
        for attempt in 0..15 {
            match op() {
                Ok(()) => return Ok(()),
                Err(e) => {
                    last_err = Some(e);
                    if attempt < 14 {
                        std::thread::sleep(Duration::from_millis(200));
                    }
                }
            }
        }
        Err(last_err
            .map(|e| e.to_string())
            .unwrap_or_else(|| "File operation failed".to_string()))
    };

    if target_exe.exists() {
        retry_fs(Box::new(|| fs::rename(target_exe, &old_backup)))?;
    }
    retry_fs(Box::new(|| fs::copy(&current_exe, target_exe).map(|_| ())))?;
    let _ = fs::remove_file(&old_backup);
    let _ = std::process::Command::new(target_exe).spawn();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};
    use std::net::TcpListener;
    use zip::write::SimpleFileOptions;

    fn make_mock_zip(exe_bytes: &[u8]) -> Vec<u8> {
        let mut cursor = std::io::Cursor::new(Vec::new());
        {
            let mut zw = zip::ZipWriter::new(&mut cursor);
            zw.start_file("NUP_Extractor.exe", SimpleFileOptions::default())
                .unwrap();
            zw.write_all(exe_bytes).unwrap();
            zw.finish().unwrap();
        }
        cursor.into_inner()
    }

    fn spawn_mock_release_server(
        zip_bytes: Vec<u8>,
        sums_text: String,
        tag: &'static str,
    ) -> (String, std::thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let base = format!("http://{addr}");
        let base_clone = base.clone();

        let handle = std::thread::spawn(move || {
            for stream in listener.incoming().take(5) {
                let Ok(mut stream) = stream else { break };
                let mut req_line = String::new();
                {
                    let mut br = BufReader::new(&mut stream);
                    let _ = br.read_line(&mut req_line);
                    loop {
                        let mut h = String::new();
                        let _ = br.read_line(&mut h);
                        if h.trim().is_empty() {
                            break;
                        }
                    }
                }
                let path = req_line.split_whitespace().nth(1).unwrap_or("/");
                let (content_type, body): (&str, Vec<u8>) = match path {
                    "/releases/latest" => {
                        let json = serde_json::json!({
                            "tag_name": tag,
                            "name": format!("Release {tag}"),
                            "body": "Mock release notes",
                            "html_url": format!("{base_clone}/release"),
                            "assets": [
                                {
                                    "name": "NUP_Extractor-v9.9.9-windows-x64.zip",
                                    "browser_download_url": format!("{base_clone}/assets/NUP_Extractor-v9.9.9-windows-x64.zip"),
                                    "size": zip_bytes.len()
                                },
                                {
                                    "name": "SHA256SUMS.txt",
                                    "browser_download_url": format!("{base_clone}/assets/SHA256SUMS.txt"),
                                    "size": sums_text.len()
                                }
                            ]
                        });
                        ("application/json", json.to_string().into_bytes())
                    }
                    "/assets/NUP_Extractor-v9.9.9-windows-x64.zip" => {
                        ("application/zip", zip_bytes.clone())
                    }
                    "/assets/SHA256SUMS.txt" => ("text/plain", sums_text.as_bytes().to_vec()),
                    _ => {
                        let _ = write!(
                            stream,
                            "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                        );
                        continue;
                    }
                };
                let _ = write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(&body);
                let _ = stream.flush();
            }
        });
        (base, handle)
    }

    #[test]
    fn test_version_parsing() {
        assert_eq!(parse_version("v1.0.5"), (1, 0, 5));
        assert_eq!(parse_version("1.0.6"), (1, 0, 6));
        assert_eq!(parse_version("v2.1.0-alpha"), (2, 1, 0));
        assert!(parse_version("1.0.6") > parse_version("v1.0.5"));
        assert!(parse_version("v2.1.0-alpha") > parse_version("1.0.6"));
    }

    #[test]
    fn test_mock_update_check_download_and_sha256_pass() {
        let fake_exe = b"MZ_FAKE_NUP_EXTRACTOR_BINARY_2_0_0";
        let zip_bytes = make_mock_zip(fake_exe);
        let digest = Sha256::digest(&zip_bytes);
        let mut hex = String::new();
        for b in digest {
            use std::fmt::Write as _;
            let _ = write!(&mut hex, "{b:02x}");
        }
        let sums = format!("{hex}  NUP_Extractor-v9.9.9-windows-x64.zip\n");

        let (base, _srv) = spawn_mock_release_server(zip_bytes, sums, "v9.9.9");
        let check = check_for_updates_from_url(&format!("{base}/releases/latest"), "2.0.0");
        assert!(check.error.is_none());
        assert!(check.update_available);
        assert_eq!(check.latest_version, "9.9.9");
        assert!(check.checksum_url.is_some());

        let cancel = AtomicBool::new(false);
        let mut stages = Vec::new();
        let (staged_exe, temp_dir) = download_and_stage_update(
            &check.download_url,
            check.checksum_url.as_deref(),
            |p| stages.push(p.stage),
            &cancel,
        )
        .expect("download + sha256 verify should pass");

        assert_eq!(fs::read(&staged_exe).unwrap(), fake_exe);
        assert!(stages.iter().any(|s| s == "verifying"));
        assert!(stages.iter().any(|s| s == "extracting"));
        let _ = fs::remove_dir_all(temp_dir);
    }

    #[test]
    fn test_mock_update_rejects_bad_sha256() {
        let zip_bytes = make_mock_zip(b"MZ_TAMPERED");
        let bad_sums =
            "0000000000000000000000000000000000000000000000000000000000000000  NUP_Extractor-v9.9.9-windows-x64.zip\n"
                .to_string();
        let (base, _srv) = spawn_mock_release_server(zip_bytes, bad_sums, "v9.9.9");

        let cancel = AtomicBool::new(false);
        let err = download_and_stage_update(
            &format!("{base}/assets/NUP_Extractor-v9.9.9-windows-x64.zip"),
            Some(&format!("{base}/assets/SHA256SUMS.txt")),
            |_| {},
            &cancel,
        )
        .unwrap_err();
        assert!(err.contains("SHA-256 checksum mismatch"), "got: {err}");
    }

    #[test]
    fn test_mock_update_cancellation() {
        let zip_bytes = make_mock_zip(b"MZ_CANCEL_TEST");
        let (base, _srv) = spawn_mock_release_server(zip_bytes, String::new(), "v9.9.9");
        let cancel = AtomicBool::new(true);
        let err = download_and_stage_update(
            &format!("{base}/assets/NUP_Extractor-v9.9.9-windows-x64.zip"),
            Some(&format!("{base}/assets/SHA256SUMS.txt")),
            |_| {},
            &cancel,
        )
        .unwrap_err();
        assert!(err.contains("cancelled"), "got: {err}");
    }

    #[test]
    fn test_mock_update_rejects_missing_checksum_url() {
        let cancel = AtomicBool::new(false);
        let err = download_and_stage_update(
            "http://127.0.0.1:9/assets/NUP_Extractor-v9.9.9-windows-x64.zip",
            None,
            |_| {},
            &cancel,
        )
        .unwrap_err();
        assert!(err.contains("SHA256SUMS.txt"), "got: {err}");

        let err_empty = download_and_stage_update(
            "http://127.0.0.1:9/assets/NUP_Extractor-v9.9.9-windows-x64.zip",
            Some("   "),
            |_| {},
            &cancel,
        )
        .unwrap_err();
        assert!(err_empty.contains("SHA256SUMS.txt"), "got: {err_empty}");
    }
}
