#![forbid(unsafe_code)]

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::{NupError, NupResult};
use crate::format::{NupConstants, get_kind_name};
use crate::reader::STREAM_BUF_SIZE;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PackOptions {
    pub source_directory: String,
    pub output_file_path: String,
    pub title_name: String,
    pub publisher: String,
    pub title_id: u64,
    pub version_string: String,
    pub compression_level: i32,
    pub icon_path: Option<String>,
}

impl Default for PackOptions {
    fn default() -> Self {
        Self {
            source_directory: String::new(),
            output_file_path: String::new(),
            title_name: "Custom Game".to_string(),
            publisher: "Homebrew / Mod".to_string(),
            title_id: 0,
            version_string: "1.0.0".to_string(),
            compression_level: 3,
            icon_path: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PackProgressInfo {
    pub status: String,
    pub current_file: String,
    pub files_packed: usize,
    pub total_files: usize,
    pub bytes_processed: u64,
    pub total_bytes: u64,
    pub bytes_written: u64,
    pub compression_ratio: f64,
    pub speed_formatted: String,
    pub percent: f64,
    pub error: Option<String>,
    pub elapsed_seconds: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DetectedPackageSummary {
    pub kind: i32,
    pub kind_name: String,
    pub display_name: String,
    pub title_id_hex: String,
    pub version_str: String,
}

struct DirEntry {
    _index: usize,
    name: String,
    full_path: PathBuf,
    first_child: u32,
    child_count: u32,
    sub_dir_indices: Vec<usize>,
    file_indices: Vec<usize>,
}

struct FileEntry {
    name: String,
    full_path: PathBuf,
    size: u64,
    extent_offset: u64,
    extent_size: u64,
    sha256: [u8; 32],
    codec: u8,
}

struct DiscoveredPackage {
    kind: u8,
    source_path: PathBuf,
    title_id: u64,
    version: u32,
    flags: u8,
    display_name: String,
    root_dir_index: usize,
    exefs_dir_index: u32,
    romfs_dir_index: u32,
    meta_bytes: Vec<u8>,
}

pub fn detect_package_summary(source_directory: &str) -> Vec<DetectedPackageSummary> {
    let opts = PackOptions {
        source_directory: source_directory.to_string(),
        ..Default::default()
    };
    let Ok(pkgs) = discover_packages(&opts) else {
        return Vec::new();
    };
    pkgs.into_iter()
        .map(|p| DetectedPackageSummary {
            kind: i32::from(p.kind),
            kind_name: get_kind_name(p.kind),
            display_name: p.display_name,
            title_id_hex: format!("{:016X}", p.title_id),
            version_str: format!(
                "{}.{}.{}",
                p.version >> 16,
                (p.version >> 8) & 0xFF,
                p.version & 0xFF
            ),
        })
        .collect()
}

pub fn pack<F>(options: &PackOptions, mut progress: F, cancel: &AtomicBool) -> NupResult<()>
where
    F: FnMut(PackProgressInfo),
{
    let src_path = Path::new(&options.source_directory);
    if options.source_directory.trim().is_empty() || !src_path.is_dir() {
        return Err(NupError::SourceDirNotFound(
            options.source_directory.clone(),
        ));
    }
    if options.output_file_path.trim().is_empty() {
        return Err(NupError::InvalidArgument(
            "Output file path cannot be empty.".to_string(),
        ));
    }

    let out_path = Path::new(&options.output_file_path);
    if let Some(parent) = out_path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)?;
    }

    let mut discovered = discover_packages(options)?;

    let discovery_msg = if discovered.len() > 1 {
        let labels: Vec<String> = discovered
            .iter()
            .map(|p| match p.kind {
                NupConstants::KIND_BASE => "Base".to_string(),
                NupConstants::KIND_UPDATE => format!(
                    "Update (v{}.{}.{})",
                    p.version >> 16,
                    (p.version >> 8) & 0xFF,
                    p.version & 0xFF
                ),
                _ => "DLC".to_string(),
            })
            .collect();
        format!(
            "Found {} packages: {}...",
            discovered.len(),
            labels.join(", ")
        )
    } else {
        "Scanning directory hierarchy...".to_string()
    };

    progress(PackProgressInfo {
        status: "scanning".to_string(),
        current_file: discovery_msg,
        files_packed: 0,
        total_files: 0,
        bytes_processed: 0,
        total_bytes: 0,
        bytes_written: 0,
        compression_ratio: 0.0,
        speed_formatted: String::new(),
        percent: 0.0,
        error: None,
        elapsed_seconds: 0.0,
    });

    let mut dirs: Vec<DirEntry> = Vec::new();
    let mut files: Vec<FileEntry> = Vec::new();
    let mut total_uncompressed_bytes: u64 = 0;

    for pkg in &mut discovered {
        let root_idx = dirs.len();
        dirs.push(DirEntry {
            _index: root_idx,
            name: String::new(),
            full_path: pkg.source_path.clone(),
            first_child: 0,
            child_count: 0,
            sub_dir_indices: Vec::new(),
            file_indices: Vec::new(),
        });
        pkg.root_dir_index = root_idx;
        scan_directory(
            root_idx,
            &mut dirs,
            &mut files,
            &mut total_uncompressed_bytes,
            cancel,
        )?;

        let sub_indices = dirs[root_idx].sub_dir_indices.clone();
        for child_idx in sub_indices {
            if dirs[child_idx].name.eq_ignore_ascii_case("exefs")
                && subtree_has_files(child_idx, &dirs)
            {
                pkg.exefs_dir_index = child_idx as u32;
            } else if dirs[child_idx].name.eq_ignore_ascii_case("romfs")
                && subtree_has_files(child_idx, &dirs)
            {
                pkg.romfs_dir_index = child_idx as u32;
            }
        }
    }

    if files.is_empty() {
        return Err(NupError::NoPackableFiles(options.source_directory.clone()));
    }

    // String pool
    let mut string_pool: Vec<u8> = Vec::new();
    let mut dir_name_offs = Vec::with_capacity(dirs.len());
    let mut dir_name_lens = Vec::with_capacity(dirs.len());
    for d in &dirs {
        let b = d.name.as_bytes();
        dir_name_offs.push(string_pool.len() as u32);
        dir_name_lens.push(b.len() as u32);
        string_pool.extend_from_slice(b);
    }

    let mut file_name_offs = Vec::with_capacity(files.len());
    let mut file_name_lens = Vec::with_capacity(files.len());
    for f in &files {
        let b = f.name.as_bytes();
        file_name_offs.push(string_pool.len() as u32);
        file_name_lens.push(b.len() as u32);
        string_pool.extend_from_slice(b);
    }

    // Children array
    let mut children_list: Vec<u32> = Vec::new();
    let dir_count = dirs.len();
    for d in &mut dirs {
        d.first_child = children_list.len() as u32;
        d.child_count = (d.sub_dir_indices.len() + d.file_indices.len()) as u32;
        for &sub in &d.sub_dir_indices {
            children_list.push(sub as u32);
        }
        for &fi in &d.file_indices {
            children_list.push((dir_count + fi) as u32);
        }
    }

    // Metadata TLVs
    let mut total_meta_size = 0usize;
    for pkg in &mut discovered {
        pkg.meta_bytes = build_package_metadata(pkg, options);
        total_meta_size += pkg.meta_bytes.len();
    }

    let packages_size = discovered.len() * NupConstants::PKG_SIZE;
    let dirs_table_size = dirs.len() * NupConstants::DIR_SIZE;
    let files_table_size = files.len() * NupConstants::FILE_SIZE;
    let children_table_size = children_list.len() * 4;
    let pool_size = string_pool.len();
    let toc_size = NupConstants::TOC_HEADER_SIZE
        + packages_size
        + dirs_table_size
        + files_table_size
        + children_table_size
        + pool_size
        + total_meta_size;
    let toc_offset = NupConstants::HEADER_SIZE as u64;
    let blob_offset = align_up(toc_offset + toc_size as u64, NupConstants::EXTENT_ALIGN);

    let mut out_file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .read(true)
        .write(true)
        .open(out_path)?;
    out_file.seek(SeekFrom::Start(blob_offset))?;

    let mut current_extent_offset = blob_offset;
    let mut processed_bytes: u64 = 0;
    let mut written_blob_bytes: u64 = 0;
    let sw = Instant::now();
    let mut last_report_ms: u128 = 0;
    let mut bytes_since_last: u64 = 0;
    let pad_zeros = [0u8; 16];
    let mut io_buf = vec![0u8; STREAM_BUF_SIZE];
    let total_files = files.len();

    for (i, fe) in files.iter_mut().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            return Err(NupError::Cancelled);
        }

        let aligned = align_up(current_extent_offset, NupConstants::EXTENT_ALIGN);
        if aligned > current_extent_offset {
            let pad = (aligned - current_extent_offset) as usize;
            out_file.write_all(&pad_zeros[..pad])?;
            current_extent_offset = aligned;
        }
        fe.extent_offset = current_extent_offset;

        if fe.size == 0 {
            fe.extent_size = 0;
            fe.sha256 = Sha256::digest([]).into();
            fe.codec = NupConstants::CODEC_RAW;
        } else if options.compression_level <= 0 {
            let mut in_f = File::open(&fe.full_path)?;
            let mut hasher = Sha256::new();
            let start_pos = out_file.stream_position()?;
            loop {
                let r = in_f.read(&mut io_buf)?;
                if r == 0 {
                    break;
                }
                hasher.update(&io_buf[..r]);
                out_file.write_all(&io_buf[..r])?;
                processed_bytes += r as u64;
                bytes_since_last += r as u64;
                written_blob_bytes += r as u64;
            }
            let end_pos = out_file.stream_position()?;
            fe.extent_size = end_pos - start_pos;
            fe.sha256 = hasher.finalize().into();
            fe.codec = NupConstants::CODEC_RAW;
            current_extent_offset += fe.extent_size;
        } else {
            let mut in_f = File::open(&fe.full_path)?;
            let mut hasher = Sha256::new();
            let file_extent_start = out_file.stream_position()?;
            {
                let mut encoder =
                    zstd::stream::write::Encoder::new(&mut out_file, options.compression_level)?;
                loop {
                    let r = in_f.read(&mut io_buf)?;
                    if r == 0 {
                        break;
                    }
                    hasher.update(&io_buf[..r]);
                    encoder.write_all(&io_buf[..r])?;
                    processed_bytes += r as u64;
                    bytes_since_last += r as u64;
                }
                encoder.finish()?;
            }
            let compressed_size = out_file.stream_position()? - file_extent_start;
            let file_sha: [u8; 32] = hasher.finalize().into();

            if compressed_size >= fe.size {
                out_file.seek(SeekFrom::Start(file_extent_start))?;
                out_file.set_len(file_extent_start)?;
                in_f.seek(SeekFrom::Start(0))?;
                loop {
                    let r = in_f.read(&mut io_buf)?;
                    if r == 0 {
                        break;
                    }
                    out_file.write_all(&io_buf[..r])?;
                }
                fe.extent_size = fe.size;
                fe.sha256 = file_sha;
                fe.codec = NupConstants::CODEC_RAW;
            } else {
                fe.extent_size = compressed_size;
                fe.sha256 = file_sha;
                fe.codec = NupConstants::CODEC_ZSTD;
            }
            written_blob_bytes += fe.extent_size;
            current_extent_offset += fe.extent_size;
        }

        let now_ms = sw.elapsed().as_millis();
        if now_ms.saturating_sub(last_report_ms) >= 150 || i == total_files - 1 {
            let secs = (now_ms.saturating_sub(last_report_ms)) as f64 / 1000.0;
            let speed = if secs > 0.0 {
                format!(
                    "{:.1} MB/s",
                    (bytes_since_last as f64 / (1024.0 * 1024.0)) / secs
                )
            } else {
                "0.0 MB/s".to_string()
            };
            last_report_ms = now_ms;
            bytes_since_last = 0;

            let ratio =
                if total_uncompressed_bytes > 0 && written_blob_bytes < total_uncompressed_bytes {
                    ((1.0 - (written_blob_bytes as f64 / total_uncompressed_bytes as f64)) * 1000.0)
                        .round()
                        / 10.0
                } else {
                    0.0
                };
            let pct = if total_uncompressed_bytes > 0 {
                ((processed_bytes as f64 / total_uncompressed_bytes as f64) * 1000.0).round() / 10.0
            } else {
                100.0
            };

            progress(PackProgressInfo {
                status: "packing".to_string(),
                current_file: fe.name.clone(),
                files_packed: i + 1,
                total_files,
                bytes_processed: processed_bytes,
                total_bytes: total_uncompressed_bytes,
                bytes_written: written_blob_bytes,
                compression_ratio: ratio,
                speed_formatted: speed,
                percent: pct.min(99.0),
                error: None,
                elapsed_seconds: sw.elapsed().as_secs_f64(),
            });
        }
    }

    let total_file_size = out_file.metadata()?.len();

    progress(PackProgressInfo {
        status: "finalizing".to_string(),
        current_file: "Generating Table of Contents...".to_string(),
        files_packed: total_files,
        total_files,
        bytes_processed: processed_bytes,
        total_bytes: total_uncompressed_bytes,
        bytes_written: written_blob_bytes,
        compression_ratio: 0.0,
        speed_formatted: String::new(),
        percent: 99.5,
        error: None,
        elapsed_seconds: sw.elapsed().as_secs_f64(),
    });

    // Assemble TOC bytes
    let mut toc = vec![0u8; toc_size];
    toc[0..4].copy_from_slice(&NupConstants::TOC_MAGIC);
    toc[4..8].copy_from_slice(&(discovered.len() as u32).to_le_bytes());
    toc[8..16].copy_from_slice(&(dirs.len() as u64).to_le_bytes());
    toc[16..24].copy_from_slice(&(files.len() as u64).to_le_bytes());
    let pool_offset_in_toc = (NupConstants::TOC_HEADER_SIZE
        + packages_size
        + dirs_table_size
        + files_table_size
        + children_table_size) as u64;
    toc[24..32].copy_from_slice(&pool_offset_in_toc.to_le_bytes());
    toc[32..40].copy_from_slice(&(pool_size as u64).to_le_bytes());

    let mut pos = NupConstants::TOC_HEADER_SIZE;
    let mut cur_meta_off = toc_offset + pool_offset_in_toc + pool_size as u64;

    for pkg in &discovered {
        let p = &mut toc[pos..pos + NupConstants::PKG_SIZE];
        pos += NupConstants::PKG_SIZE;
        p[0..8].copy_from_slice(&pkg.title_id.to_le_bytes());
        p[8..12].copy_from_slice(&(pkg.root_dir_index as u32).to_le_bytes());
        p[12..16].copy_from_slice(&pkg.exefs_dir_index.to_le_bytes());
        p[16..20].copy_from_slice(&pkg.romfs_dir_index.to_le_bytes());
        p[20..24].copy_from_slice(&pkg.version.to_le_bytes());
        p[24] = pkg.kind;
        p[25] = pkg.flags;
        if !pkg.meta_bytes.is_empty() {
            p[32..40].copy_from_slice(&cur_meta_off.to_le_bytes());
            p[40..44].copy_from_slice(&(pkg.meta_bytes.len() as u32).to_le_bytes());
            cur_meta_off += pkg.meta_bytes.len() as u64;
        }
    }

    for (i, d) in dirs.iter().enumerate() {
        let ds = &mut toc[pos..pos + NupConstants::DIR_SIZE];
        pos += NupConstants::DIR_SIZE;
        ds[0..4].copy_from_slice(&dir_name_offs[i].to_le_bytes());
        ds[4..8].copy_from_slice(&dir_name_lens[i].to_le_bytes());
        ds[8..12].copy_from_slice(&d.first_child.to_le_bytes());
        ds[12..16].copy_from_slice(&d.child_count.to_le_bytes());
    }

    for (i, f) in files.iter().enumerate() {
        let fs_slice = &mut toc[pos..pos + NupConstants::FILE_SIZE];
        pos += NupConstants::FILE_SIZE;
        fs_slice[0..4].copy_from_slice(&file_name_offs[i].to_le_bytes());
        fs_slice[4..8].copy_from_slice(&file_name_lens[i].to_le_bytes());
        fs_slice[8..16].copy_from_slice(&f.size.to_le_bytes());
        fs_slice[16..24].copy_from_slice(&f.extent_offset.to_le_bytes());
        fs_slice[24..32].copy_from_slice(&f.extent_size.to_le_bytes());
        fs_slice[32..64].copy_from_slice(&f.sha256);
        fs_slice[64] = f.codec;
        fs_slice[65] = 0;
    }

    for cid in &children_list {
        toc[pos..pos + 4].copy_from_slice(&cid.to_le_bytes());
        pos += 4;
    }

    toc[pos..pos + pool_size].copy_from_slice(&string_pool);
    pos += pool_size;

    for pkg in &discovered {
        if !pkg.meta_bytes.is_empty() {
            toc[pos..pos + pkg.meta_bytes.len()].copy_from_slice(&pkg.meta_bytes);
            pos += pkg.meta_bytes.len();
        }
    }

    let toc_sha256: [u8; 32] = Sha256::digest(&toc).into();
    out_file.seek(SeekFrom::Start(toc_offset))?;
    out_file.write_all(&toc)?;

    // Superblock
    let mut header = [0u8; NupConstants::HEADER_SIZE];
    header[0..4].copy_from_slice(&NupConstants::MAGIC);
    header[4..6].copy_from_slice(&(NupConstants::HEADER_SIZE as u16).to_le_bytes());
    header[6..8].copy_from_slice(&1u16.to_le_bytes());
    header[8..16].copy_from_slice(&toc_offset.to_le_bytes());
    header[16..24].copy_from_slice(&(toc_size as u64).to_le_bytes());
    header[24..56].copy_from_slice(&toc_sha256);
    header[56..64].copy_from_slice(&blob_offset.to_le_bytes());
    header[64..72].copy_from_slice(&total_file_size.to_le_bytes());
    header[72..76].copy_from_slice(&(discovered.len() as u32).to_le_bytes());
    header[76..80].copy_from_slice(&0u32.to_le_bytes());
    let uuid = derive_uuid(&discovered);
    header[80..96].copy_from_slice(&uuid);

    out_file.seek(SeekFrom::Start(0))?;
    out_file.write_all(&header)?;
    out_file.flush()?;

    let final_ratio =
        if total_uncompressed_bytes > 0 && written_blob_bytes < total_uncompressed_bytes {
            ((1.0 - (written_blob_bytes as f64 / total_uncompressed_bytes as f64)) * 1000.0).round()
                / 10.0
        } else {
            0.0
        };

    progress(PackProgressInfo {
        status: "done".to_string(),
        current_file: if discovered.len() > 1 {
            format!("Container created with {} packages!", discovered.len())
        } else {
            "Container created successfully!".to_string()
        },
        files_packed: total_files,
        total_files,
        bytes_processed: total_uncompressed_bytes,
        total_bytes: total_uncompressed_bytes,
        bytes_written: written_blob_bytes,
        compression_ratio: final_ratio,
        speed_formatted: String::new(),
        percent: 100.0,
        error: None,
        elapsed_seconds: sw.elapsed().as_secs_f64(),
    });

    Ok(())
}

fn scan_directory(
    dir_idx: usize,
    dirs: &mut Vec<DirEntry>,
    files: &mut Vec<FileEntry>,
    total_bytes: &mut u64,
    cancel: &AtomicBool,
) -> NupResult<()> {
    if cancel.load(Ordering::Relaxed) {
        return Err(NupError::Cancelled);
    }

    let current_path = dirs[dir_idx].full_path.clone();
    let mut sub_dirs = Vec::new();
    let mut local_files = Vec::new();

    for entry in fs::read_dir(&current_path)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            sub_dirs.push(path);
        } else if path.is_file() {
            local_files.push(path);
        }
    }

    sub_dirs.sort_by_key(|p| p.to_string_lossy().to_lowercase());
    local_files.sort_by_key(|p| p.to_string_lossy().to_lowercase());

    for sd in sub_dirs {
        let name = sd
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let new_idx = dirs.len();
        dirs.push(DirEntry {
            _index: new_idx,
            name,
            full_path: sd,
            first_child: 0,
            child_count: 0,
            sub_dir_indices: Vec::new(),
            file_indices: Vec::new(),
        });
        dirs[dir_idx].sub_dir_indices.push(new_idx);
        scan_directory(new_idx, dirs, files, total_bytes, cancel)?;
    }

    for lf in local_files {
        let name = lf
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        if name.eq_ignore_ascii_case(".listing") {
            continue;
        }
        let size = fs::metadata(&lf)?.len();
        let new_fi = files.len();
        files.push(FileEntry {
            name,
            full_path: lf,
            size,
            extent_offset: 0,
            extent_size: 0,
            sha256: [0u8; 32],
            codec: NupConstants::CODEC_RAW,
        });
        dirs[dir_idx].file_indices.push(new_fi);
        *total_bytes = total_bytes.saturating_add(size);
    }

    Ok(())
}

fn subtree_has_files(dir_idx: usize, dirs: &[DirEntry]) -> bool {
    let d = &dirs[dir_idx];
    if !d.file_indices.is_empty() {
        return true;
    }
    d.sub_dir_indices
        .iter()
        .any(|&sub| subtree_has_files(sub, dirs))
}

fn directory_has_files(dir: &Path) -> bool {
    let mut stack = vec![dir.to_path_buf()];
    while let Some(cur) = stack.pop() {
        let Ok(entries) = fs::read_dir(&cur) else {
            continue;
        };
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.is_file() {
                let name = p
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                if !name.eq_ignore_ascii_case(".listing") {
                    return true;
                }
            }
        }
    }
    false
}

fn try_parse_npdm_program_id(pkg_dir: &Path) -> u64 {
    for cand in [
        pkg_dir.join("exefs").join("main.npdm"),
        pkg_dir.join("main.npdm"),
    ] {
        if let Ok(data) = fs::read(&cand)
            && let Some(off) = data.windows(4).position(|w| w == b"ACI0")
            && off + 0x18 <= data.len()
        {
            let pid = u64::from_le_bytes(data[off + 0x10..off + 0x18].try_into().unwrap_or([0; 8]));
            if pid != 0 {
                return pid;
            }
        }
    }
    0
}

fn try_read_build_id(pkg_dir: &Path) -> Option<[u8; 32]> {
    for cand in [pkg_dir.join("exefs").join("main"), pkg_dir.join("main")] {
        if let Ok(mut f) = File::open(&cand) {
            let mut head = [0u8; 0x60];
            if f.read_exact(&mut head).is_ok()
                && (&head[0..4] == b"NSO0" || &head[0x10..0x14] == b"NRO0")
            {
                let mut bid = [0u8; 32];
                bid.copy_from_slice(&head[0x40..0x60]);
                return Some(bid);
            }
        }
    }
    None
}

fn derive_uuid(packages: &[DiscoveredPackage]) -> [u8; 16] {
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 13];
    for pkg in packages {
        buf[0..8].copy_from_slice(&pkg.title_id.to_le_bytes());
        buf[8..12].copy_from_slice(&pkg.version.to_le_bytes());
        buf[12] = pkg.kind;
        hasher.update(buf);
    }
    let digest = hasher.finalize();
    let mut uuid = [0u8; 16];
    uuid.copy_from_slice(&digest[0..16]);
    uuid
}

fn discover_packages(options: &PackOptions) -> NupResult<Vec<DiscoveredPackage>> {
    let src = PathBuf::from(&options.source_directory);
    if !src.is_dir() {
        return Err(NupError::SourceDirNotFound(
            options.source_directory.clone(),
        ));
    }

    let mut list = Vec::new();
    let base_dir = src.join("base");
    let updates_dir = src.join("updates");
    let dlc_dir = src.join("dlc");

    let has_base = base_dir.is_dir() && directory_has_files(&base_dir);
    let has_updates = updates_dir.is_dir() && directory_has_files(&updates_dir);
    let has_dlc = dlc_dir.is_dir() && directory_has_files(&dlc_dir);

    let mut base_title_id = options.title_id;

    if has_base {
        let npdm_id = try_parse_npdm_program_id(&base_dir);
        if npdm_id != 0 && (base_title_id == 0 || base_title_id == 0x0100_0000_0000_1000) {
            base_title_id = npdm_id;
        }
        if base_title_id == 0 {
            base_title_id = 0x0100_0000_0000_1000;
        }
        list.push(DiscoveredPackage {
            kind: NupConstants::KIND_BASE,
            source_path: base_dir,
            title_id: base_title_id,
            version: parse_version_string(&options.version_string),
            flags: 0,
            display_name: options.title_name.clone(),
            root_dir_index: 0,
            exefs_dir_index: NupConstants::NO_DIR,
            romfs_dir_index: NupConstants::NO_DIR,
            meta_bytes: Vec::new(),
        });
    }

    if has_updates {
        let update_flags = if has_base {
            0
        } else {
            NupConstants::PKG_FLAG_STANDALONE_UPDATE
        };
        let mut sub_dirs: Vec<PathBuf> = fs::read_dir(&updates_dir)?
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_dir() && directory_has_files(p))
            .collect();
        sub_dirs.sort_by_key(|p| p.to_string_lossy().to_lowercase());

        if !sub_dirs.is_empty() {
            for upd_sub in sub_dirs {
                let folder_name = upd_sub
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let ver = folder_name.parse::<u32>().unwrap_or_else(|_| {
                    let parsed = parse_version_string(&folder_name);
                    if parsed == 0 { 65536 } else { parsed }
                });
                let mut upd_tid = try_parse_npdm_program_id(&upd_sub);
                if upd_tid == 0 {
                    if base_title_id != 0 && (base_title_id & 0xFFF) == 0 {
                        upd_tid = base_title_id | 0x800;
                    } else if base_title_id != 0 {
                        upd_tid = base_title_id;
                    } else {
                        upd_tid = options.title_id;
                    }
                }
                list.push(DiscoveredPackage {
                    kind: NupConstants::KIND_UPDATE,
                    source_path: upd_sub,
                    title_id: upd_tid,
                    version: ver,
                    flags: update_flags,
                    display_name: format!("{} (Update v{folder_name})", options.title_name),
                    root_dir_index: 0,
                    exefs_dir_index: NupConstants::NO_DIR,
                    romfs_dir_index: NupConstants::NO_DIR,
                    meta_bytes: Vec::new(),
                });
            }
        } else {
            let mut upd_tid = try_parse_npdm_program_id(&updates_dir);
            if upd_tid == 0 {
                if base_title_id != 0 && (base_title_id & 0xFFF) == 0 {
                    upd_tid = base_title_id | 0x800;
                } else {
                    upd_tid = if base_title_id != 0 {
                        base_title_id
                    } else {
                        options.title_id
                    };
                }
            }
            list.push(DiscoveredPackage {
                kind: NupConstants::KIND_UPDATE,
                source_path: updates_dir,
                title_id: upd_tid,
                version: 65536,
                flags: update_flags,
                display_name: format!("{} (Update)", options.title_name),
                root_dir_index: 0,
                exefs_dir_index: NupConstants::NO_DIR,
                romfs_dir_index: NupConstants::NO_DIR,
                meta_bytes: Vec::new(),
            });
        }
    }

    if has_dlc {
        let mut dlc_subs: Vec<PathBuf> = fs::read_dir(&dlc_dir)?
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect();
        dlc_subs.sort_by_key(|p| p.to_string_lossy().to_lowercase());

        let mut dlc_counter: u64 = 1;
        for sub in dlc_subs {
            let name = sub
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            if name.len() == 16
                && let Ok(hex_id) = u64::from_str_radix(&name, 16)
            {
                let mut idx_dirs: Vec<PathBuf> = fs::read_dir(&sub)?
                    .flatten()
                    .map(|e| e.path())
                    .filter(|p| p.is_dir() && directory_has_files(p))
                    .collect();
                idx_dirs.sort_by_key(|p| p.to_string_lossy().to_lowercase());
                if !idx_dirs.is_empty() {
                    for idir in idx_dirs {
                        list.push(DiscoveredPackage {
                            kind: NupConstants::KIND_ADDON,
                            source_path: idir,
                            title_id: hex_id,
                            version: 0,
                            flags: 0,
                            display_name: format!("{} (DLC {name})", options.title_name),
                            root_dir_index: 0,
                            exefs_dir_index: NupConstants::NO_DIR,
                            romfs_dir_index: NupConstants::NO_DIR,
                            meta_bytes: Vec::new(),
                        });
                    }
                } else if directory_has_files(&sub) {
                    list.push(DiscoveredPackage {
                        kind: NupConstants::KIND_ADDON,
                        source_path: sub,
                        title_id: hex_id,
                        version: 0,
                        flags: 0,
                        display_name: format!("{} (DLC {name})", options.title_name),
                        root_dir_index: 0,
                        exefs_dir_index: NupConstants::NO_DIR,
                        romfs_dir_index: NupConstants::NO_DIR,
                        meta_bytes: Vec::new(),
                    });
                }
            } else if directory_has_files(&sub) {
                let tid = if base_title_id != 0 {
                    base_title_id + dlc_counter
                } else {
                    options.title_id + dlc_counter
                };
                list.push(DiscoveredPackage {
                    kind: NupConstants::KIND_ADDON,
                    source_path: sub,
                    title_id: tid,
                    version: 0,
                    flags: 0,
                    display_name: format!("{} (DLC {name})", options.title_name),
                    root_dir_index: 0,
                    exefs_dir_index: NupConstants::NO_DIR,
                    romfs_dir_index: NupConstants::NO_DIR,
                    meta_bytes: Vec::new(),
                });
                dlc_counter += 1;
            }
        }
    }

    if list.is_empty() {
        let npdm_id = try_parse_npdm_program_id(&src);
        let tid = if options.title_id != 0 {
            options.title_id
        } else if npdm_id != 0 {
            npdm_id
        } else {
            0x0100_0000_0000_1000
        };
        list.push(DiscoveredPackage {
            kind: NupConstants::KIND_BASE,
            source_path: src,
            title_id: tid,
            version: parse_version_string(&options.version_string),
            flags: 0,
            display_name: options.title_name.clone(),
            root_dir_index: 0,
            exefs_dir_index: NupConstants::NO_DIR,
            romfs_dir_index: NupConstants::NO_DIR,
            meta_bytes: Vec::new(),
        });
    }

    Ok(list)
}

fn build_package_metadata(pkg: &DiscoveredPackage, options: &PackOptions) -> Vec<u8> {
    let mut out = Vec::new();
    if pkg.kind == NupConstants::KIND_BASE {
        if !options.title_name.is_empty() {
            write_tlv(
                &mut out,
                NupConstants::TLV_TITLE_NAME,
                options.title_name.as_bytes(),
            );
        }
        if !options.publisher.is_empty() {
            write_tlv(
                &mut out,
                NupConstants::TLV_PUBLISHER,
                options.publisher.as_bytes(),
            );
        }
        if !options.version_string.is_empty() {
            write_tlv(
                &mut out,
                NupConstants::TLV_DISPLAY_VERSION,
                options.version_string.as_bytes(),
            );
        }
        if let Some(ref icon_p) = options.icon_path
            && let Ok(bytes) = fs::read(icon_p)
        {
            write_tlv(&mut out, NupConstants::TLV_ICON, &bytes);
        }
    } else if pkg.kind == NupConstants::KIND_UPDATE {
        let ver_str = format!(
            "{}.{}.{}",
            pkg.version >> 16,
            (pkg.version >> 8) & 0xFF,
            pkg.version & 0xFF
        );
        write_tlv(
            &mut out,
            NupConstants::TLV_DISPLAY_VERSION,
            ver_str.as_bytes(),
        );
    }

    write_tlv(
        &mut out,
        NupConstants::TLV_SOURCE_DUMP,
        b"NUP Extractor Native",
    );

    for cand in [
        pkg.source_path.join("exefs").join("main.npdm"),
        pkg.source_path.join("main.npdm"),
    ] {
        if let Ok(npdm) = fs::read(&cand) {
            write_tlv(&mut out, NupConstants::TLV_NPDM, &npdm);
            break;
        }
    }

    if let Some(bid) = try_read_build_id(&pkg.source_path) {
        write_tlv(&mut out, NupConstants::TLV_BUILD_ID, &bid);
    }

    out
}

fn write_tlv(out: &mut Vec<u8>, tlv_type: u16, payload: &[u8]) {
    out.extend_from_slice(&tlv_type.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    out.extend_from_slice(payload);
}

fn parse_version_string(ver: &str) -> u32 {
    let clean = ver.trim().trim_start_matches(['v', 'V']);
    let clean = clean.split('-').next().unwrap_or(clean);
    let mut parts = clean.split('.');
    let major = parts
        .next()
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(1);
    let minor = parts
        .next()
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(0);
    let patch = parts
        .next()
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(0);
    (major << 16) | (minor << 8) | patch
}

fn align_up(value: u64, alignment: u64) -> u64 {
    let rem = value % alignment;
    if rem == 0 {
        value
    } else {
        value + (alignment - rem)
    }
}
