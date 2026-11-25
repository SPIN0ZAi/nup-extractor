#![forbid(unsafe_code)]

use std::fs::{self, File};
use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use base64::Engine as _;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::error::{NupError, NupResult};
use crate::format::{NupConstants, hex_lower, hex_upper};
use crate::path_safety::validate_and_join_extraction_path;
use crate::reader::{NupReader, STREAM_BUF_SIZE};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProgressInfo {
    pub status: String,
    pub current: usize,
    pub total: usize,
    pub percent: f64,
    pub file_name: String,
    pub file_size: u64,
    pub bytes_written: u64,
    pub total_bytes: u64,
    pub dest_dir: Option<String>,
    pub error: Option<String>,
    pub elapsed_seconds: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VerifyProgressInfo {
    pub status: String,
    pub current: usize,
    pub total: usize,
    pub percent: f64,
    pub file_name: String,
    pub valid: bool,
    pub problems: Vec<String>,
    pub total_files_checked: usize,
    pub error: Option<String>,
}

pub fn format_bytes(size: u64) -> String {
    if size < 1024 {
        format!("{size} B")
    } else if size < 1024 * 1024 {
        format!("{:.2} KB", size as f64 / 1024.0)
    } else if size < 1024 * 1024 * 1024 {
        format!("{:.2} MB", size as f64 / (1024.0 * 1024.0))
    } else {
        format!("{:.2} GB", size as f64 / (1024.0 * 1024.0 * 1024.0))
    }
}

fn round_1_decimal(v: f64) -> f64 {
    (v * 10.0).round() / 10.0
}

pub fn inspect<P: AsRef<Path>>(path: P) -> NupResult<Value> {
    let reader = NupReader::open(path.as_ref())?;
    inspect_reader(&reader, path.as_ref())
}

pub fn inspect_reader(reader: &NupReader, raw_path: &Path) -> NupResult<Value> {
    let s = &reader.superblock;

    let mut primary_title: Option<String> = None;
    let mut primary_publisher: Option<String> = None;
    let mut primary_version: Option<String> = None;
    let mut primary_icon: Option<String> = None;

    let mut total_uncompressed: u64 = 0;
    let mut packages_list = Vec::with_capacity(reader.packages.len());

    for pkg in &reader.packages {
        let meta = reader.get_package_metadata(pkg);

        let title_name = meta
            .get(&NupConstants::TLV_TITLE_NAME)
            .map(|b| String::from_utf8_lossy(b).into_owned());
        let publisher = meta
            .get(&NupConstants::TLV_PUBLISHER)
            .map(|b| String::from_utf8_lossy(b).into_owned());
        let disp_ver = meta
            .get(&NupConstants::TLV_DISPLAY_VERSION)
            .map(|b| String::from_utf8_lossy(b).into_owned());
        let build_id = meta.get(&NupConstants::TLV_BUILD_ID).map(|b| hex_upper(b));
        let src_dump = meta
            .get(&NupConstants::TLV_SOURCE_DUMP)
            .map(|b| String::from_utf8_lossy(b).into_owned());

        let icon_data_uri = meta.get(&NupConstants::TLV_ICON).and_then(|bytes| {
            if bytes.is_empty() {
                None
            } else {
                let mime = if bytes.len() > 4 && &bytes[0..4] == b"\x89PNG" {
                    "image/png"
                } else {
                    "image/jpeg"
                };
                let b64 = base64::engine::general_purpose::STANDARD.encode(bytes);
                Some(format!("data:{mime};base64,{b64}"))
            }
        });

        if pkg.kind == NupConstants::KIND_BASE || primary_title.is_none() {
            if let Some(ref t) = title_name {
                primary_title = Some(t.clone());
            }
            if let Some(ref p) = publisher {
                primary_publisher = Some(p.clone());
            }
            if let Some(ref v) = disp_ver {
                primary_version = Some(v.clone());
            }
            if let Some(ref ic) = icon_data_uri {
                primary_icon = Some(ic.clone());
            }
        }

        let (file_count, uncomp, comp) = calculate_subtree_stats(reader, pkg.root_dir);
        total_uncompressed = total_uncompressed.saturating_add(uncomp);

        let kind_name = pkg.kind_name();
        let title_id_hex = pkg.title_id_hex();
        let version_str = pkg.version_str();
        let display_version = disp_ver.unwrap_or_else(|| version_str.clone());
        let resolved_title = title_name.unwrap_or_else(|| format!("Title {title_id_hex}"));
        let resolved_pub = publisher.unwrap_or_else(|| "Unknown Publisher".to_string());
        let build_id_str = build_id.unwrap_or_default();
        let src_dump_str = src_dump.unwrap_or_default();
        let has_icon = icon_data_uri.is_some();
        let icon_preview = icon_data_uri.unwrap_or_default();
        let has_exefs = pkg.exe_fs_dir != NupConstants::NO_DIR;
        let has_romfs = pkg.rom_fs_dir != NupConstants::NO_DIR;
        let uncomp_fmt = format_bytes(uncomp);
        let comp_fmt = format_bytes(comp);

        let mut pkg_map = Map::new();
        pkg_map.insert("index".into(), json!(pkg.index));
        pkg_map.insert("kind".into(), json!(pkg.kind));
        pkg_map.insert("kind_name".into(), json!(kind_name));
        pkg_map.insert("kindName".into(), json!(kind_name));
        pkg_map.insert("title_id".into(), json!(title_id_hex));
        pkg_map.insert("titleId".into(), json!(title_id_hex));
        pkg_map.insert("version".into(), json!(pkg.version));
        pkg_map.insert("version_str".into(), json!(version_str));
        pkg_map.insert("versionStr".into(), json!(version_str));
        pkg_map.insert("display_version".into(), json!(display_version));
        pkg_map.insert("displayVersion".into(), json!(display_version));
        pkg_map.insert("title_name".into(), json!(resolved_title));
        pkg_map.insert("titleName".into(), json!(resolved_title));
        pkg_map.insert("publisher".into(), json!(resolved_pub));
        pkg_map.insert("build_id".into(), json!(build_id_str));
        pkg_map.insert("buildId".into(), json!(build_id_str));
        pkg_map.insert("source_dump".into(), json!(src_dump_str));
        pkg_map.insert("sourceDump".into(), json!(src_dump_str));
        pkg_map.insert("has_icon".into(), json!(has_icon));
        pkg_map.insert("hasIcon".into(), json!(has_icon));
        pkg_map.insert("icon_preview".into(), json!(icon_preview));
        pkg_map.insert("iconPreview".into(), json!(icon_preview));
        pkg_map.insert("has_exefs".into(), json!(has_exefs));
        pkg_map.insert("hasExeFs".into(), json!(has_exefs));
        pkg_map.insert("has_romfs".into(), json!(has_romfs));
        pkg_map.insert("hasRomFs".into(), json!(has_romfs));
        pkg_map.insert("root_dir_index".into(), json!(pkg.root_dir));
        pkg_map.insert("rootDirIndex".into(), json!(pkg.root_dir));
        pkg_map.insert("exefs_dir_index".into(), json!(pkg.exe_fs_dir));
        pkg_map.insert("exeFsDirIndex".into(), json!(pkg.exe_fs_dir));
        pkg_map.insert("romfs_dir_index".into(), json!(pkg.rom_fs_dir));
        pkg_map.insert("romFsDirIndex".into(), json!(pkg.rom_fs_dir));
        pkg_map.insert("file_count".into(), json!(file_count));
        pkg_map.insert("fileCount".into(), json!(file_count));
        pkg_map.insert("uncompressed_bytes".into(), json!(uncomp));
        pkg_map.insert("uncompressedBytes".into(), json!(uncomp));
        pkg_map.insert("uncompressed_formatted".into(), json!(uncomp_fmt));
        pkg_map.insert("uncompressedFormatted".into(), json!(uncomp_fmt));
        pkg_map.insert("compressed_bytes".into(), json!(comp));
        pkg_map.insert("compressedBytes".into(), json!(comp));
        pkg_map.insert("compressed_formatted".into(), json!(comp_fmt));
        pkg_map.insert("compressedFormatted".into(), json!(comp_fmt));

        packages_list.push(Value::Object(pkg_map));
    }

    let compression_ratio = if total_uncompressed > 0 && s.file_size < total_uncompressed {
        round_1_decimal((1.0 - (s.file_size as f64 / total_uncompressed as f64)) * 100.0)
    } else {
        0.0
    };

    let file_name = raw_path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let base_name = raw_path
        .file_stem()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let parent_dir = reader
        .file_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    let dest_folder_name = format!(
        "{}_extracted",
        primary_title.as_deref().unwrap_or(&base_name)
    );
    let default_dest = parent_dir
        .join(dest_folder_name)
        .to_string_lossy()
        .into_owned();

    let p_title = primary_title.unwrap_or_else(|| base_name.clone());
    let p_pub = primary_publisher.unwrap_or_else(|| "Nintendo Switch Dump".to_string());
    let p_ver = primary_version.unwrap_or_else(|| "1.0.0".to_string());
    let p_icon = primary_icon.unwrap_or_default();

    let meta_dict = json!({
        "title_name": p_title,
        "titleName": p_title,
        "publisher": p_pub,
        "display_version": p_ver,
        "displayVersion": p_ver,
        "icon": p_icon,
    });

    let toc_sha_hex = hex_lower(&s.toc_sha256);
    let superblock_dict = json!({
        "magic": "NUP1",
        "header_size": s.header_size,
        "headerSize": s.header_size,
        "header_version": s.header_version,
        "headerVersion": s.header_version,
        "toc_offset": s.toc_offset,
        "tocOffset": s.toc_offset,
        "toc_size": s.toc_size,
        "tocSize": s.toc_size,
        "toc_sha256": toc_sha_hex,
        "tocSha256": toc_sha_hex,
        "blob_offset": s.blob_offset,
        "blobOffset": s.blob_offset,
        "package_count": s.package_count,
        "packageCount": s.package_count,
        "uuid": s.formatted_uuid(),
    });

    let total_uncomp_fmt = format_bytes(total_uncompressed);
    let stats_dict = json!({
        "total_dirs": reader.dirs.len(),
        "totalDirs": reader.dirs.len(),
        "total_files": reader.files.len(),
        "totalFiles": reader.files.len(),
        "total_uncompressed_bytes": total_uncompressed,
        "totalUncompressedBytes": total_uncompressed,
        "total_uncompressed_formatted": total_uncomp_fmt,
        "totalUncompressedFormatted": total_uncomp_fmt,
        "compression_ratio": compression_ratio,
        "compressionRatio": compression_ratio,
    });

    let file_path_str = reader.file_path.to_string_lossy().into_owned();
    let file_size_fmt = format_bytes(s.file_size);

    Ok(json!({
        "file_path": file_path_str,
        "filePath": file_path_str,
        "file_name": file_name,
        "fileName": file_name,
        "base_name": base_name,
        "baseName": base_name,
        "file_size": s.file_size,
        "fileSize": s.file_size,
        "file_size_formatted": file_size_fmt,
        "fileSizeFormatted": file_size_fmt,
        "default_dest_dir": default_dest,
        "defaultDestDir": default_dest,
        "superblock": superblock_dict,
        "primary_meta": meta_dict,
        "primaryMeta": meta_dict,
        "packages": packages_list,
        "stats": stats_dict,
    }))
}

fn calculate_subtree_stats(reader: &NupReader, dir_index: u32) -> (usize, u64, u64) {
    let mut count = 0usize;
    let mut uncomp = 0u64;
    let mut comp = 0u64;
    let mut stack = vec![dir_index];
    let mut visited_guard = 0usize;
    let max_visits = reader.dirs.len().saturating_add(1);

    while let Some(di) = stack.pop() {
        visited_guard += 1;
        if visited_guard > max_visits {
            break;
        }
        for (is_dir, idx, _) in reader.get_children(di) {
            if is_dir {
                stack.push(idx as u32);
            } else if let Some(f) = reader.files.get(idx) {
                count += 1;
                uncomp = uncomp.saturating_add(f.size);
                comp = comp.saturating_add(f.extent_size);
            }
        }
    }
    (count, uncomp, comp)
}

pub fn get_dir_children<P: AsRef<Path>>(path: P, dir_index: u32) -> NupResult<Value> {
    let reader = NupReader::open(path)?;
    get_dir_children_reader(&reader, dir_index)
}

pub fn get_dir_children_reader(reader: &NupReader, dir_index: u32) -> NupResult<Value> {
    let d = reader
        .dirs
        .get(dir_index as usize)
        .ok_or(NupError::DirIndexOutOfBounds(dir_index))?;

    let mut children = Vec::new();
    for (is_dir, idx, name) in reader.get_children(dir_index) {
        if is_dir {
            let child_dir = &reader.dirs[idx];
            let has_children = child_dir.child_count > 0;
            children.push(json!({
                "type": "dir",
                "index": idx,
                "name": name,
                "child_count": child_dir.child_count,
                "childCount": child_dir.child_count,
                "has_children": has_children,
                "hasChildren": has_children,
            }));
        } else {
            let f = &reader.files[idx];
            let size_fmt = format_bytes(f.size);
            let ext_fmt = format_bytes(f.extent_size);
            let codec = if f.codec == NupConstants::CODEC_ZSTD {
                "zstd"
            } else {
                "raw"
            };
            children.push(json!({
                "type": "file",
                "index": idx,
                "name": name,
                "size": f.size,
                "size_formatted": size_fmt,
                "sizeFormatted": size_fmt,
                "extent_size": f.extent_size,
                "extentSize": f.extent_size,
                "extent_size_formatted": ext_fmt,
                "extentSizeFormatted": ext_fmt,
                "codec": codec,
                "sha256": hex_lower(&f.sha256),
            }));
        }
    }

    let dname = if d.name.is_empty() {
        "root".to_string()
    } else {
        d.name.clone()
    };

    Ok(json!({
        "type": "dir",
        "index": dir_index,
        "name": dname,
        "children": children,
    }))
}

pub fn search_files<P: AsRef<Path>>(path: P, query: &str, limit: usize) -> NupResult<Vec<Value>> {
    let reader = NupReader::open(path)?;
    Ok(search_files_reader(&reader, query, limit))
}

pub fn search_files_reader(reader: &NupReader, query: &str, limit: usize) -> Vec<Value> {
    let q_lower = query.to_lowercase();
    let mut results = Vec::new();

    for (i, f) in reader.files.iter().enumerate() {
        if f.name.to_lowercase().contains(&q_lower) {
            let size_fmt = format_bytes(f.size);
            let ext_fmt = format_bytes(f.extent_size);
            let codec = if f.codec == NupConstants::CODEC_ZSTD {
                "zstd"
            } else {
                "raw"
            };
            results.push(json!({
                "index": i,
                "name": f.name,
                "size": f.size,
                "size_formatted": size_fmt,
                "sizeFormatted": size_fmt,
                "extent_size": f.extent_size,
                "extentSize": f.extent_size,
                "extent_size_formatted": ext_fmt,
                "extentSizeFormatted": ext_fmt,
                "codec": codec,
                "sha256": hex_lower(&f.sha256),
            }));
            if results.len() >= limit {
                break;
            }
        }
    }
    results
}

pub fn extract<F>(
    nup_path: &str,
    dest_dir: &str,
    mode: &str,
    package_index: Option<usize>,
    subtree_path: Option<&str>,
    mut progress: F,
    cancel: &AtomicBool,
) -> NupResult<()>
where
    F: FnMut(ProgressInfo),
{
    if nup_path.trim().is_empty() {
        return Err(NupError::InvalidArgument(
            "NUP file path cannot be empty.".to_string(),
        ));
    }
    if dest_dir.trim().is_empty() {
        return Err(NupError::InvalidArgument(
            "Destination folder cannot be empty.".to_string(),
        ));
    }

    let clean_nup = nup_path.trim().trim_matches(['"', '\'']);
    let clean_dest = dest_dir.trim().trim_matches(['"', '\'']);

    let reader = NupReader::open(clean_nup)?;
    let dest_root = PathBuf::from(clean_dest);
    fs::create_dir_all(&dest_root)?;

    let mut files_to_extract: Vec<(usize, String)> = Vec::new();

    match mode {
        "subtree" => {
            let p_idx = package_index.unwrap_or(0);
            let pkg = reader
                .packages
                .get(p_idx)
                .ok_or(NupError::InvalidPackageIndex(p_idx))?;
            let target_dir = match subtree_path {
                Some(sub) if !sub.is_empty() => resolve_path(&reader, pkg.root_dir, sub)?,
                _ => pkg.root_dir,
            };
            gather_files(&reader, target_dir, "", &mut files_to_extract);
        }
        "package" => {
            let p_idx = package_index.unwrap_or(0);
            let pkg = reader
                .packages
                .get(p_idx)
                .ok_or(NupError::InvalidPackageIndex(p_idx))?;
            gather_files(&reader, pkg.root_dir, "", &mut files_to_extract);
        }
        _ => {
            let single_pkg = reader.packages.len() == 1;
            for pkg in &reader.packages {
                let prefix = if single_pkg {
                    String::new()
                } else {
                    match pkg.kind {
                        NupConstants::KIND_BASE => "base".to_string(),
                        NupConstants::KIND_UPDATE => format!("updates/{}", pkg.version),
                        NupConstants::KIND_ADDON | NupConstants::KIND_ADDON_UPDATE => {
                            let use_version = pkg.version != 0
                                && reader
                                    .packages
                                    .iter()
                                    .filter(|other| {
                                        other.kind == pkg.kind
                                            && other.title_id == pkg.title_id
                                            && other.version == pkg.version
                                    })
                                    .count()
                                    == 1;
                            let suffix = if use_version {
                                pkg.version as usize
                            } else {
                                pkg.index
                            };
                            format!("dlc/{}/{}", pkg.title_id_hex().to_lowercase(), suffix)
                        }
                        _ => format!("pkg_{}_{}", pkg.index, pkg.kind_name()),
                    }
                };
                gather_files(&reader, pkg.root_dir, &prefix, &mut files_to_extract);
            }
        }
    }

    let total = files_to_extract.len();
    let total_bytes: u64 = files_to_extract
        .iter()
        .map(|(fi, _)| reader.files[*fi].size)
        .sum();

    progress(ProgressInfo {
        status: "starting".to_string(),
        current: 0,
        total,
        percent: 0.0,
        file_name: "Initializing extraction...".to_string(),
        file_size: 0,
        bytes_written: 0,
        total_bytes,
        dest_dir: Some(clean_dest.to_string()),
        error: None,
        elapsed_seconds: 0.0,
    });

    let sw = Instant::now();
    let mut last_report_ms: u128 = 0;
    let mut bytes_written: u64 = 0;

    for (i, (fi, rel_path)) in files_to_extract.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            return Err(NupError::Cancelled);
        }

        let full_path = validate_and_join_extraction_path(&dest_root, rel_path)?;
        if let Some(parent) = full_path.parent() {
            fs::create_dir_all(parent)?;
        }

        let f_size = reader.files[*fi].size;
        let extract_res = (|| -> NupResult<()> {
            let file = File::create(&full_path)?;
            let mut bw = BufWriter::with_capacity(STREAM_BUF_SIZE, file);
            reader.extract_file_to_writer(*fi, &mut bw)?;
            Ok(())
        })();

        if let Err(err) = extract_res {
            return Err(NupError::ExtractFileFailed {
                rel_path: rel_path.clone(),
                size_formatted: format_bytes(f_size),
                source: Box::new(err),
            });
        }

        bytes_written = bytes_written.saturating_add(f_size);
        let now_ms = sw.elapsed().as_millis();
        if now_ms.saturating_sub(last_report_ms) >= 20 || i == total - 1 {
            last_report_ms = now_ms;
            let pct = if total > 0 {
                round_1_decimal(((i + 1) as f64 / total as f64) * 100.0)
            } else {
                100.0
            };
            progress(ProgressInfo {
                status: "extracting".to_string(),
                current: i + 1,
                total,
                percent: pct,
                file_name: rel_path.replace('\\', "/"),
                file_size: f_size,
                bytes_written,
                total_bytes,
                dest_dir: Some(clean_dest.to_string()),
                error: None,
                elapsed_seconds: sw.elapsed().as_secs_f64(),
            });
        }
    }

    progress(ProgressInfo {
        status: "completed".to_string(),
        current: total,
        total,
        percent: 100.0,
        file_name: "Done".to_string(),
        file_size: 0,
        bytes_written,
        total_bytes,
        dest_dir: Some(clean_dest.to_string()),
        error: None,
        elapsed_seconds: sw.elapsed().as_secs_f64(),
    });

    Ok(())
}

pub fn extract_single_file(
    nup_path: &str,
    file_index: usize,
    dest_file_path: &str,
) -> NupResult<()> {
    if nup_path.trim().is_empty() {
        return Err(NupError::InvalidArgument(
            "NUP path cannot be empty.".to_string(),
        ));
    }
    if dest_file_path.trim().is_empty() {
        return Err(NupError::InvalidArgument(
            "Destination file path cannot be empty.".to_string(),
        ));
    }

    let clean_nup = nup_path.trim().trim_matches(['"', '\'']);
    let clean_dest = dest_file_path.trim().trim_matches(['"', '\'']);

    let reader = NupReader::open(clean_nup)?;
    if file_index >= reader.files.len() {
        return Err(NupError::FileIndexOutOfBounds(file_index));
    }

    let dest = PathBuf::from(clean_dest);
    if let Some(parent) = dest.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)?;
    }

    let file = File::create(&dest)?;
    let mut bw = BufWriter::with_capacity(STREAM_BUF_SIZE, file);
    reader.extract_file_to_writer(file_index, &mut bw)?;
    Ok(())
}

pub fn verify<F>(
    nup_path: &str,
    deep: bool,
    mut progress: F,
    cancel: &AtomicBool,
) -> NupResult<VerifyProgressInfo>
where
    F: FnMut(VerifyProgressInfo),
{
    let clean_nup = nup_path.trim().trim_matches(['"', '\'']);
    let reader = NupReader::open(clean_nup)?;
    let total = reader.files.len();
    let mut problems = Vec::new();

    if deep {
        let sw = Instant::now();
        let mut last_report_ms: u128 = 0;

        for (i, f) in reader.files.iter().enumerate() {
            if cancel.load(Ordering::Relaxed) {
                return Err(NupError::Cancelled);
            }

            match reader.hash_file_sha256(i) {
                Ok(hash) => {
                    if hash != f.sha256 {
                        problems.push(format!("File #{i} ({}): SHA-256 hash mismatch", f.name));
                    }
                }
                Err(err) => {
                    problems.push(format!(
                        "File #{i} ({}): Failed to decompress/read: {err}",
                        f.name
                    ));
                }
            }

            let now_ms = sw.elapsed().as_millis();
            if now_ms.saturating_sub(last_report_ms) >= 25 || i == total - 1 {
                last_report_ms = now_ms;
                let pct = if total > 0 {
                    round_1_decimal(((i + 1) as f64 / total as f64) * 100.0)
                } else {
                    100.0
                };
                progress(VerifyProgressInfo {
                    status: "verifying".to_string(),
                    current: i + 1,
                    total,
                    percent: pct,
                    file_name: f.name.clone(),
                    valid: false,
                    problems: Vec::new(),
                    total_files_checked: 0,
                    error: None,
                });
            }
        }
    }

    let final_info = VerifyProgressInfo {
        status: "done".to_string(),
        current: total,
        total,
        percent: 100.0,
        file_name: String::new(),
        valid: problems.is_empty(),
        problems,
        total_files_checked: total,
        error: None,
    };
    progress(final_info.clone());
    Ok(final_info)
}

fn resolve_path(reader: &NupReader, root_dir: u32, rel_path: &str) -> NupResult<u32> {
    let mut current = root_dir;
    for part in rel_path.split(['/', '\\']).filter(|p| !p.is_empty()) {
        let mut found = None;
        for (is_dir, idx, name) in reader.get_children(current) {
            if is_dir && name.eq_ignore_ascii_case(part) {
                found = Some(idx as u32);
                break;
            }
        }
        current = found.ok_or_else(|| NupError::SubtreeNotFound(part.to_string()))?;
    }
    Ok(current)
}

fn gather_files(reader: &NupReader, dir_index: u32, prefix: &str, out: &mut Vec<(usize, String)>) {
    let mut stack = vec![(dir_index, prefix.to_string())];
    let mut guard = 0usize;
    let max_dirs = reader.dirs.len().saturating_add(1);

    while let Some((di, pref)) = stack.pop() {
        guard += 1;
        if guard > max_dirs {
            break;
        }
        for (is_dir, idx, name) in reader.get_children(di) {
            let rel = if pref.is_empty() {
                name
            } else {
                format!("{pref}/{name}")
            };
            if is_dir {
                stack.push((idx as u32, rel));
            } else {
                out.push((idx, rel));
            }
        }
    }
}
