#![forbid(unsafe_code)]

use std::collections::{BTreeMap, HashMap};
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::Instant;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::{NupError, NupResult};
use crate::format::{NupConstants, get_kind_name};

/// Files larger than 256 MiB are always stored raw, matching the NUP spec and emulator cache limits.
const DEFAULT_RAW_THRESHOLD: u64 = 256 * 1024 * 1024;
/// Files up to 16 MiB are read, hashed, and compressed in parallel by worker threads.
const PARALLEL_IN_MEM_LIMIT: u64 = 16 * 1024 * 1024;
/// Upper bound on total buffered source bytes across in-flight worker tasks.
const MAX_INFLIGHT_BYTES: usize = 192 * 1024 * 1024;
/// 8 MiB buffered writer to coalesce small extents and eliminate per-file WriteFile syscalls.
const WRITE_BUF_SIZE: usize = 8 * 1024 * 1024;
/// 2 MiB streaming buffer for large files (> 16 MiB).
const STREAM_COPY_BUF_SIZE: usize = 2 * 1024 * 1024;
/// Fallback dummy Title ID when neither user override nor `main.npdm` supplies one.
const PLACEHOLDER_TITLE_ID: u64 = 0x0100_0000_0000_1000;
/// Exact byte length of an uncompressed Switch `control.nacp` (`RawNACP`).
const NACP_SIZE: usize = 0x4000;
/// Empty byte slice SHA-256 digest (`e3b0c44...`).
const EMPTY_SHA256: [u8; 32] = [
    0xe3, 0xb0, 0xc4, 0x42, 0x98, 0xfc, 0x1c, 0x14, 0x9a, 0xfb, 0xf4, 0xc8, 0x99, 0x6f, 0xb9, 0x24,
    0x27, 0xae, 0x41, 0xe4, 0x64, 0x9b, 0x93, 0x4c, 0xa4, 0x95, 0x99, 0x1b, 0x78, 0x52, 0xb8, 0x55,
];

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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detected_title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detected_publisher: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detected_version: Option<String>,
}

struct DirEntry {
    name: String,
    first_child: u32,
    child_count: u32,
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
    exclude_top_dirs: &'static [&'static str],
}

#[derive(Default)]
struct TempDir {
    name: String,
    /// Children sorted in bytewise UTF-8 lexicographic order (dirs and files together per SPEC §3.3).
    children: BTreeMap<Vec<u8>, TempChild>,
}

enum TempChild {
    Dir(TempDir),
    File { full_path: PathBuf, size: u64 },
}

#[derive(Default)]
struct NacpInfo {
    title: Option<String>,
    publisher: Option<String>,
    version: Option<String>,
}

pub fn detect_package_summary(source_directory: &str) -> Vec<DetectedPackageSummary> {
    let opts = PackOptions {
        source_directory: source_directory.to_string(),
        title_name: String::new(),
        publisher: String::new(),
        title_id: 0,
        version_string: String::new(),
        compression_level: 0,
        ..Default::default()
    };
    let Ok(pkgs) = discover_packages(&opts) else {
        return Vec::new();
    };
    pkgs.into_iter()
        .map(|p| {
            let nacp_bytes = try_read_nacp_bytes(&p.source_path);
            let nacp = nacp_bytes
                .as_deref()
                .map(parse_nacp_info)
                .unwrap_or_default();
            let detected_version = nacp.version.or_else(|| {
                if p.kind == NupConstants::KIND_UPDATE && p.version != 0 {
                    Some(format_version(p.version))
                } else {
                    None
                }
            });
            DetectedPackageSummary {
                kind: i32::from(p.kind),
                kind_name: get_kind_name(p.kind),
                display_name: p.display_name,
                title_id_hex: format!("{:016X}", p.title_id),
                version_str: format_version(p.version),
                detected_title: nacp.title,
                detected_publisher: nacp.publisher,
                detected_version,
            }
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
                NupConstants::KIND_UPDATE => format!("Update (v{})", format_version(p.version)),
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

    let mut package_trees = Vec::with_capacity(discovered.len());
    let mut scan_files_seen: usize = 0;
    let mut scan_bytes_seen: u64 = 0;
    let mut last_scan_report: usize = 0;

    for pkg in &discovered {
        let mut root_tree = TempDir::default();
        collect_tree(
            &pkg.source_path,
            0,
            pkg.exclude_top_dirs,
            &mut root_tree,
            &mut scan_files_seen,
            &mut scan_bytes_seen,
            &mut last_scan_report,
            &mut progress,
            cancel,
        )?;
        package_trees.push(root_tree);
    }

    let mut dirs: Vec<DirEntry> = Vec::new();
    let mut files: Vec<FileEntry> = Vec::new();
    let mut children_tagged: Vec<u64> = Vec::new();
    const FILE_TAG: u64 = 1u64 << 32;

    for (pkg, tree) in discovered.iter_mut().zip(package_trees.iter()) {
        let root_id = assign_dir(tree, &mut dirs, &mut files, &mut children_tagged);
        pkg.root_dir_index = root_id as usize;

        let first = dirs[root_id as usize].first_child as usize;
        let count = dirs[root_id as usize].child_count as usize;
        for &tagged in &children_tagged[first..first + count] {
            if tagged & FILE_TAG != 0 {
                continue;
            }
            let child_dir = tagged as usize;
            if dirs[child_dir].name.eq_ignore_ascii_case("exefs")
                && subtree_has_files_tagged(child_dir, &dirs, &children_tagged, FILE_TAG)
            {
                pkg.exefs_dir_index = child_dir as u32;
            } else if dirs[child_dir].name.eq_ignore_ascii_case("romfs")
                && subtree_has_files_tagged(child_dir, &dirs, &children_tagged, FILE_TAG)
            {
                pkg.romfs_dir_index = child_dir as u32;
            }
        }
    }

    if files.is_empty() {
        return Err(NupError::NoPackableFiles(options.source_directory.clone()));
    }

    let dir_count = dirs.len() as u64;
    let children_list: Vec<u32> = children_tagged
        .into_iter()
        .map(|tagged| {
            if tagged & FILE_TAG != 0 {
                (dir_count + (tagged & 0xFFFF_FFFF)) as u32
            } else {
                tagged as u32
            }
        })
        .collect();

    // String pool with deduplicated interning (SPEC §3.5)
    let mut string_pool: Vec<u8> = Vec::new();
    let mut intern_map: HashMap<Vec<u8>, (u32, u32)> = HashMap::new();
    let mut dir_name_offs = Vec::with_capacity(dirs.len());
    let mut dir_name_lens = Vec::with_capacity(dirs.len());
    for d in &dirs {
        let (off, len) = intern_string(&mut string_pool, &mut intern_map, d.name.as_bytes());
        dir_name_offs.push(off);
        dir_name_lens.push(len);
    }

    let mut file_name_offs = Vec::with_capacity(files.len());
    let mut file_name_lens = Vec::with_capacity(files.len());
    for f in &files {
        let (off, len) = intern_string(&mut string_pool, &mut intern_map, f.name.as_bytes());
        file_name_offs.push(off);
        file_name_lens.push(len);
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
        + pool_size;
    let toc_offset = NupConstants::HEADER_SIZE as u64;
    let blob_offset = align_up(toc_offset + toc_size as u64, NupConstants::EXTENT_ALIGN);

    // Prepare per-package TLV metadata (written after the blob extents per SPEC §5 & pack.py)
    for (idx, pkg) in discovered.iter_mut().enumerate() {
        pkg.meta_bytes = build_package_metadata(pkg, idx == 0, options);
    }

    let raw_file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .read(true)
        .write(true)
        .open(out_path)?;

    let res = write_container_body(
        raw_file,
        &mut discovered,
        &dirs,
        &mut files,
        &children_list,
        &string_pool,
        &dir_name_offs,
        &dir_name_lens,
        &file_name_offs,
        &file_name_lens,
        toc_offset,
        toc_size,
        blob_offset,
        scan_bytes_seen,
        options.compression_level,
        &mut progress,
        cancel,
    );

    if res.is_err() {
        let _ = fs::remove_file(out_path);
    }
    res
}

#[allow(clippy::too_many_arguments)]
fn write_container_body<F>(
    raw_file: File,
    discovered: &mut [DiscoveredPackage],
    dirs: &[DirEntry],
    files: &mut [FileEntry],
    children_list: &[u32],
    string_pool: &[u8],
    dir_name_offs: &[u32],
    dir_name_lens: &[u32],
    file_name_offs: &[u32],
    file_name_lens: &[u32],
    toc_offset: u64,
    toc_size: usize,
    blob_offset: u64,
    total_uncompressed_bytes: u64,
    compression_level: i32,
    progress: &mut F,
    cancel: &AtomicBool,
) -> NupResult<()>
where
    F: FnMut(PackProgressInfo),
{
    let mut writer = BufWriter::with_capacity(WRITE_BUF_SIZE, raw_file);
    writer.seek(SeekFrom::Start(blob_offset))?;

    let sw = Instant::now();
    let total_files = files.len();

    let (written_blob_bytes, mut current_offset) = write_all_extents(
        &mut writer,
        files,
        blob_offset,
        total_uncompressed_bytes,
        compression_level,
        progress,
        cancel,
        sw,
    )?;

    progress(PackProgressInfo {
        status: "finalizing".to_string(),
        current_file: "Writing metadata & Table of Contents...".to_string(),
        files_packed: total_files,
        total_files,
        bytes_processed: total_uncompressed_bytes,
        total_bytes: total_uncompressed_bytes,
        bytes_written: written_blob_bytes,
        compression_ratio: 0.0,
        speed_formatted: String::new(),
        percent: 99.5,
        error: None,
        elapsed_seconds: sw.elapsed().as_secs_f64(),
    });

    // Write package metadata blocks immediately after the last extent (SPEC §5 / pack.py)
    let mut pkg_meta_locs = Vec::with_capacity(discovered.len());
    for pkg in discovered.iter() {
        if pkg.meta_bytes.is_empty() {
            pkg_meta_locs.push((0u64, 0u32));
        } else {
            let offset = current_offset;
            writer.write_all(&pkg.meta_bytes)?;
            current_offset += pkg.meta_bytes.len() as u64;
            pkg_meta_locs.push((offset, pkg.meta_bytes.len() as u32));
        }
    }
    let total_file_size = current_offset;

    // Assemble canonical TOC bytes
    let packages_size = discovered.len() * NupConstants::PKG_SIZE;
    let dirs_table_size = dirs.len() * NupConstants::DIR_SIZE;
    let files_table_size = files.len() * NupConstants::FILE_SIZE;
    let children_table_size = children_list.len() * 4;
    let pool_size = string_pool.len();
    let pool_offset_in_toc = (NupConstants::TOC_HEADER_SIZE
        + packages_size
        + dirs_table_size
        + files_table_size
        + children_table_size) as u64;

    let mut toc = vec![0u8; toc_size];
    toc[0..4].copy_from_slice(&NupConstants::TOC_MAGIC);
    toc[4..8].copy_from_slice(&(discovered.len() as u32).to_le_bytes());
    toc[8..16].copy_from_slice(&(dirs.len() as u64).to_le_bytes());
    toc[16..24].copy_from_slice(&(files.len() as u64).to_le_bytes());
    toc[24..32].copy_from_slice(&pool_offset_in_toc.to_le_bytes());
    toc[32..40].copy_from_slice(&(pool_size as u64).to_le_bytes());

    let mut pos = NupConstants::TOC_HEADER_SIZE;
    for (pkg, &(meta_off, meta_len)) in discovered.iter().zip(pkg_meta_locs.iter()) {
        let p = &mut toc[pos..pos + NupConstants::PKG_SIZE];
        pos += NupConstants::PKG_SIZE;
        p[0..8].copy_from_slice(&pkg.title_id.to_le_bytes());
        p[8..12].copy_from_slice(&(pkg.root_dir_index as u32).to_le_bytes());
        p[12..16].copy_from_slice(&pkg.exefs_dir_index.to_le_bytes());
        p[16..20].copy_from_slice(&pkg.romfs_dir_index.to_le_bytes());
        p[20..24].copy_from_slice(&pkg.version.to_le_bytes());
        p[24] = pkg.kind;
        p[25] = pkg.flags;
        p[32..40].copy_from_slice(&meta_off.to_le_bytes());
        p[40..44].copy_from_slice(&meta_len.to_le_bytes());
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

    for cid in children_list {
        toc[pos..pos + 4].copy_from_slice(&cid.to_le_bytes());
        pos += 4;
    }

    toc[pos..pos + pool_size].copy_from_slice(string_pool);

    let toc_sha256: [u8; 32] = Sha256::digest(&toc).into();

    writer.seek(SeekFrom::Start(toc_offset))?;
    writer.write_all(&toc)?;

    // Zero-fill alignment gap between TOC end and blob_offset
    let gap = blob_offset.saturating_sub(toc_offset + toc_size as u64) as usize;
    if gap > 0 {
        let zeros = [0u8; 16];
        writer.write_all(&zeros[..gap])?;
    }

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
    let uuid = derive_uuid(discovered);
    header[80..96].copy_from_slice(&uuid);

    writer.seek(SeekFrom::Start(0))?;
    writer.write_all(&header)?;
    writer.flush()?;

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

struct PreparedExtent {
    raw_size: u64,
    sha256: [u8; 32],
    codec: u8,
    payload: Vec<u8>,
}

struct PipelineState {
    next_claim: usize,
    next_write: usize,
    inflight_bytes: usize,
    completed: BTreeMap<usize, PreparedExtent>,
    first_error: Option<String>,
    aborted: bool,
}

/// Bounded parallel worker pool + ordered sequential buffered extent writer.
#[allow(clippy::too_many_arguments)]
fn write_all_extents<F>(
    writer: &mut BufWriter<File>,
    files: &mut [FileEntry],
    blob_offset: u64,
    total_uncompressed_bytes: u64,
    compression_level: i32,
    progress: &mut F,
    cancel: &AtomicBool,
    sw: Instant,
) -> NupResult<(u64, u64)>
where
    F: FnMut(PackProgressInfo),
{
    let total_files = files.len();
    let num_workers = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .clamp(2, 16);
    let max_lookahead = num_workers * 8;

    let file_specs: Vec<(PathBuf, u64)> = files
        .iter()
        .map(|f| (f.full_path.clone(), f.size))
        .collect();

    let state = Mutex::new(PipelineState {
        next_claim: 0,
        next_write: 0,
        inflight_bytes: 0,
        completed: BTreeMap::new(),
        first_error: None,
        aborted: false,
    });
    let worker_cv = Condvar::new();
    let writer_cv = Condvar::new();

    let mut current_extent_offset = blob_offset;
    let mut processed_bytes: u64 = 0;
    let mut written_blob_bytes: u64 = 0;
    let mut last_report_ms: u128 = 0;
    let mut bytes_since_last: u64 = 0;
    let mut dedup: HashMap<(u64, [u8; 32]), (u64, u64, u8)> = HashMap::new();
    let mut large_io_buf = vec![0u8; STREAM_COPY_BUF_SIZE];
    let mut large_cctx = if compression_level > 0 {
        Some(new_streaming_cctx(compression_level)?)
    } else {
        None
    };

    std::thread::scope(|scope| -> NupResult<()> {
        for _ in 0..num_workers {
            scope.spawn(|| {
                let mut raw_buf = Vec::new();
                let mut comp_buf = Vec::new();
                let mut compressor = if compression_level > 0 {
                    new_bulk_compressor(compression_level).ok()
                } else {
                    None
                };

                loop {
                    if cancel.load(Ordering::Relaxed) {
                        let mut st = state.lock().expect("pipeline mutex poisoned");
                        st.aborted = true;
                        worker_cv.notify_all();
                        writer_cv.notify_all();
                        return;
                    }

                    // Claim up to a small batch of tiny/small files in one lock acquisition
                    let mut batch: Vec<(usize, u64)> = Vec::with_capacity(16);
                    {
                        let mut st = state.lock().expect("pipeline mutex poisoned");
                        loop {
                            if st.aborted || st.first_error.is_some() {
                                return;
                            }
                            while st.next_claim < total_files {
                                let sz = file_specs[st.next_claim].1;
                                if sz == 0 || sz > PARALLEL_IN_MEM_LIMIT {
                                    st.next_claim += 1;
                                } else {
                                    break;
                                }
                            }
                            if st.next_claim >= total_files {
                                return;
                            }
                            let idx = st.next_claim;
                            let sz = file_specs[idx].1 as usize;
                            let window_ok = idx.saturating_sub(st.next_write) < max_lookahead;
                            let mem_ok = idx == st.next_write
                                || st.inflight_bytes.saturating_add(sz) <= MAX_INFLIGHT_BYTES;
                            if window_ok && mem_ok {
                                let mut batch_bytes = 0usize;
                                while st.next_claim < total_files && batch.len() < 16 {
                                    let cidx = st.next_claim;
                                    let csz_u64 = file_specs[cidx].1;
                                    if csz_u64 == 0 || csz_u64 > PARALLEL_IN_MEM_LIMIT {
                                        break;
                                    }
                                    let csz = csz_u64 as usize;
                                    if !batch.is_empty()
                                        && (batch_bytes.saturating_add(csz) > 512 * 1024
                                            || st.inflight_bytes.saturating_add(csz)
                                                > MAX_INFLIGHT_BYTES
                                            || cidx.saturating_sub(st.next_write) >= max_lookahead)
                                    {
                                        break;
                                    }
                                    st.next_claim += 1;
                                    st.inflight_bytes = st.inflight_bytes.saturating_add(csz);
                                    batch_bytes = batch_bytes.saturating_add(csz);
                                    batch.push((cidx, csz_u64));
                                }
                                break;
                            }
                            st = worker_cv.wait(st).expect("pipeline mutex poisoned");
                        }
                    }

                    let mut produced: Vec<(usize, PreparedExtent)> =
                        Vec::with_capacity(batch.len());
                    for (idx, expected_size) in batch {
                        if cancel.load(Ordering::Relaxed) {
                            let mut st = state.lock().expect("pipeline mutex poisoned");
                            st.aborted = true;
                            worker_cv.notify_all();
                            writer_cv.notify_all();
                            return;
                        }
                        match process_small_file(
                            &file_specs[idx].0,
                            expected_size,
                            compression_level,
                            compressor.as_mut(),
                            &mut raw_buf,
                            &mut comp_buf,
                        ) {
                            Ok(ext) => produced.push((idx, ext)),
                            Err(e) => {
                                let mut st = state.lock().expect("pipeline mutex poisoned");
                                if st.first_error.is_none() {
                                    st.first_error = Some(e.to_string());
                                }
                                st.aborted = true;
                                worker_cv.notify_all();
                                writer_cv.notify_all();
                                return;
                            }
                        }
                    }

                    let mut st = state.lock().expect("pipeline mutex poisoned");
                    let mut wake_writer = false;
                    for (idx, ext) in produced {
                        if idx == st.next_write {
                            wake_writer = true;
                        }
                        st.completed.insert(idx, ext);
                    }
                    if wake_writer || st.completed.contains_key(&st.next_write) {
                        writer_cv.notify_one();
                    }
                }
            });
        }

        // Coordinator / Writer loop strictly writes in index order 0..total_files
        let pad_zeros = [0u8; 16];
        for (i, fe) in files.iter_mut().enumerate() {
            if cancel.load(Ordering::Relaxed) {
                let mut st = state.lock().expect("pipeline mutex poisoned");
                st.aborted = true;
                worker_cv.notify_all();
                return Err(NupError::Cancelled);
            }

            if fe.size == 0 {
                // Zero-length files have no blob bytes and MUST keep extent_offset == 0 (SPEC §3.6)
                fe.extent_offset = 0;
                fe.extent_size = 0;
                fe.sha256 = EMPTY_SHA256;
                fe.codec = NupConstants::CODEC_RAW;

                let mut st = state.lock().expect("pipeline mutex poisoned");
                st.next_write = i + 1;
                if st.next_claim < i + 1 {
                    st.next_claim = i + 1;
                }
                worker_cv.notify_all();
            } else if fe.size > PARALLEL_IN_MEM_LIMIT {
                let file_name = fe.name.clone();
                let (written_bytes_this, next_off) = write_large_file(
                    writer,
                    fe,
                    current_extent_offset,
                    compression_level,
                    large_cctx.as_mut(),
                    &mut large_io_buf,
                    &mut dedup,
                    cancel,
                    &mut |delta| {
                        processed_bytes += delta;
                        bytes_since_last += delta;
                        let now_ms = sw.elapsed().as_millis();
                        if now_ms.saturating_sub(last_report_ms) >= 150 {
                            emit_pack_progress(
                                progress,
                                i,
                                total_files,
                                &file_name,
                                processed_bytes,
                                total_uncompressed_bytes,
                                written_blob_bytes,
                                &mut last_report_ms,
                                &mut bytes_since_last,
                                sw,
                            );
                        }
                    },
                )?;
                current_extent_offset = next_off;
                written_blob_bytes += written_bytes_this;

                let mut st = state.lock().expect("pipeline mutex poisoned");
                st.next_write = i + 1;
                if st.next_claim < i + 1 {
                    st.next_claim = i + 1;
                }
                worker_cv.notify_all();
            } else {
                let ext = {
                    let mut st = state.lock().expect("pipeline mutex poisoned");
                    loop {
                        if let Some(err_msg) = st.first_error.take() {
                            st.aborted = true;
                            worker_cv.notify_all();
                            return Err(NupError::InvalidArgument(err_msg));
                        }
                        if cancel.load(Ordering::Relaxed) || st.aborted {
                            st.aborted = true;
                            worker_cv.notify_all();
                            return Err(NupError::Cancelled);
                        }
                        if let Some(ext) = st.completed.remove(&i) {
                            st.inflight_bytes = st.inflight_bytes.saturating_sub(fe.size as usize);
                            st.next_write = i + 1;
                            worker_cv.notify_all();
                            break ext;
                        }
                        st = writer_cv.wait(st).expect("pipeline mutex poisoned");
                    }
                };

                fe.sha256 = ext.sha256;
                fe.size = ext.raw_size;
                processed_bytes += ext.raw_size;
                bytes_since_last += ext.raw_size;

                if let Some(&(dup_off, dup_size, dup_codec)) =
                    dedup.get(&(ext.raw_size, ext.sha256))
                {
                    fe.extent_offset = dup_off;
                    fe.extent_size = dup_size;
                    fe.codec = dup_codec;
                } else {
                    let aligned = align_up(current_extent_offset, NupConstants::EXTENT_ALIGN);
                    if aligned > current_extent_offset {
                        let pad = (aligned - current_extent_offset) as usize;
                        writer.write_all(&pad_zeros[..pad])?;
                        current_extent_offset = aligned;
                    }
                    fe.extent_offset = current_extent_offset;
                    fe.extent_size = ext.payload.len() as u64;
                    fe.codec = ext.codec;
                    writer.write_all(&ext.payload)?;
                    current_extent_offset += fe.extent_size;
                    written_blob_bytes += fe.extent_size;
                    dedup.insert(
                        (ext.raw_size, ext.sha256),
                        (fe.extent_offset, fe.extent_size, fe.codec),
                    );
                }
            }

            let now_ms = sw.elapsed().as_millis();
            if now_ms.saturating_sub(last_report_ms) >= 150 || i == total_files - 1 {
                emit_pack_progress(
                    progress,
                    i + 1,
                    total_files,
                    &fe.name,
                    processed_bytes,
                    total_uncompressed_bytes,
                    written_blob_bytes,
                    &mut last_report_ms,
                    &mut bytes_since_last,
                    sw,
                );
            }
        }

        Ok(())
    })?;

    Ok((written_blob_bytes, current_extent_offset))
}

#[allow(clippy::too_many_arguments)]
fn emit_pack_progress<F>(
    progress: &mut F,
    files_done: usize,
    total_files: usize,
    current_name: &str,
    processed_bytes: u64,
    total_uncompressed_bytes: u64,
    written_blob_bytes: u64,
    last_report_ms: &mut u128,
    bytes_since_last: &mut u64,
    sw: Instant,
) where
    F: FnMut(PackProgressInfo),
{
    let now_ms = sw.elapsed().as_millis();
    let elapsed_interval = now_ms.saturating_sub(*last_report_ms);
    let secs = elapsed_interval as f64 / 1000.0;
    let speed = if secs > 0.0 && *bytes_since_last > 0 {
        format!(
            "{:.1} MB/s",
            (*bytes_since_last as f64 / (1024.0 * 1024.0)) / secs
        )
    } else if sw.elapsed().as_secs_f64() > 0.05 && processed_bytes > 0 {
        format!(
            "{:.1} MB/s",
            (processed_bytes as f64 / (1024.0 * 1024.0)) / sw.elapsed().as_secs_f64()
        )
    } else {
        "0.0 MB/s".to_string()
    };
    *last_report_ms = now_ms;
    *bytes_since_last = 0;

    let ratio = if total_uncompressed_bytes > 0 && written_blob_bytes < total_uncompressed_bytes {
        ((1.0 - (written_blob_bytes as f64 / total_uncompressed_bytes as f64)) * 1000.0).round()
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
        current_file: current_name.to_string(),
        files_packed: files_done,
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

fn new_bulk_compressor(level: i32) -> io::Result<zstd::bulk::Compressor<'static>> {
    let mut comp = zstd::bulk::Compressor::new(level)?;
    comp.include_contentsize(true)?;
    comp.include_checksum(false)?;
    comp.include_dictid(false)?;
    Ok(comp)
}

fn new_streaming_cctx(level: i32) -> io::Result<zstd::zstd_safe::CCtx<'static>> {
    let mut cctx = zstd::zstd_safe::CCtx::create();
    cctx.set_parameter(zstd::zstd_safe::CParameter::CompressionLevel(level))
        .map_err(|code| io::Error::other(zstd::zstd_safe::get_error_name(code)))?;
    cctx.set_parameter(zstd::zstd_safe::CParameter::ContentSizeFlag(true))
        .map_err(|code| io::Error::other(zstd::zstd_safe::get_error_name(code)))?;
    cctx.set_parameter(zstd::zstd_safe::CParameter::ChecksumFlag(false))
        .map_err(|code| io::Error::other(zstd::zstd_safe::get_error_name(code)))?;
    cctx.set_parameter(zstd::zstd_safe::CParameter::DictIdFlag(false))
        .map_err(|code| io::Error::other(zstd::zstd_safe::get_error_name(code)))?;
    Ok(cctx)
}

fn process_small_file(
    path: &Path,
    expected_size: u64,
    compression_level: i32,
    compressor: Option<&mut zstd::bulk::Compressor<'static>>,
    raw_buf: &mut Vec<u8>,
    comp_buf: &mut Vec<u8>,
) -> NupResult<PreparedExtent> {
    raw_buf.clear();
    raw_buf.reserve(expected_size as usize);
    let mut f = File::open(path)?;
    f.read_to_end(raw_buf)?;
    let raw_size = raw_buf.len() as u64;
    let sha256: [u8; 32] = Sha256::digest(raw_buf.as_slice()).into();

    if compression_level > 0
        && (8..=DEFAULT_RAW_THRESHOLD).contains(&raw_size)
        && let Some(comp) = compressor
    {
        let target_cap = (raw_size as usize).saturating_sub(1);
        if comp_buf.len() < target_cap {
            comp_buf.resize(target_cap, 0);
        }
        // Compress directly into `[..target_cap]` slice without reallocating `comp_buf`
        if let Ok(compressed_len) =
            comp.compress_to_buffer(raw_buf.as_slice(), &mut comp_buf[..target_cap])
            && (compressed_len as u64) < raw_size
        {
            return Ok(PreparedExtent {
                raw_size,
                sha256,
                codec: NupConstants::CODEC_ZSTD,
                payload: comp_buf[..compressed_len].to_vec(),
            });
        }
    }

    Ok(PreparedExtent {
        raw_size,
        sha256,
        codec: NupConstants::CODEC_RAW,
        payload: std::mem::take(raw_buf),
    })
}

struct CappedVecWriter {
    buf: Vec<u8>,
    max_len: usize,
}

impl Write for CappedVecWriter {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        if self.buf.len().saturating_add(data.len()) > self.max_len {
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "compressed output meets or exceeds uncompressed size",
            ));
        }
        self.buf.extend_from_slice(data);
        Ok(data.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[allow(clippy::too_many_arguments)]
fn write_large_file<P>(
    writer: &mut BufWriter<File>,
    fe: &mut FileEntry,
    mut current_extent_offset: u64,
    compression_level: i32,
    cctx: Option<&mut zstd::zstd_safe::CCtx<'static>>,
    io_buf: &mut [u8],
    dedup: &mut HashMap<(u64, [u8; 32]), (u64, u64, u8)>,
    cancel: &AtomicBool,
    on_bytes: &mut P,
) -> NupResult<(u64, u64)>
where
    P: FnMut(u64),
{
    let mut in_f = File::open(&fe.full_path)?;
    let actual_size = in_f.metadata()?.len();
    fe.size = actual_size;

    let aligned = align_up(current_extent_offset, NupConstants::EXTENT_ALIGN);
    let pad_len = (aligned - current_extent_offset) as usize;
    let pad_zeros = [0u8; 16];

    let mut did_single_pass_raw_copy = false;
    let mut reported_for_file = 0u64;
    let mut sha256 = [0u8; 32];
    let mut zstd_bytes: Option<Vec<u8>> = None;

    if compression_level > 0
        && actual_size <= DEFAULT_RAW_THRESHOLD
        && let Some(ctx) = cctx
    {
        let mut hasher = Sha256::new();
        let sink = CappedVecWriter {
            buf: Vec::with_capacity((actual_size as usize).min(32 * 1024 * 1024)),
            max_len: (actual_size as usize).saturating_sub(1),
        };
        let mut encoder = zstd::stream::write::Encoder::with_context(sink, ctx);
        let _ = encoder.set_pledged_src_size(Some(actual_size));

        let mut aborted_compress = false;
        loop {
            if cancel.load(Ordering::Relaxed) {
                return Err(NupError::Cancelled);
            }
            let r = in_f.read(io_buf)?;
            if r == 0 {
                break;
            }
            hasher.update(&io_buf[..r]);
            reported_for_file += r as u64;
            on_bytes(r as u64);
            if encoder.write_all(&io_buf[..r]).is_err() {
                aborted_compress = true;
                break;
            }
        }
        if !aborted_compress && let Ok(finished_sink) = encoder.finish() {
            sha256 = hasher.finalize().into();
            zstd_bytes = Some(finished_sink.buf);
        }
        if zstd_bytes.is_none() {
            // Rewind to stream raw once without touching `out_file` until alignment is written
            in_f.seek(SeekFrom::Start(0))?;
            did_single_pass_raw_copy = true;
        }
    } else {
        did_single_pass_raw_copy = true;
    }

    if let Some(comp_payload) = zstd_bytes {
        fe.sha256 = sha256;
        if let Some(&(dup_off, dup_size, dup_codec)) = dedup.get(&(actual_size, sha256)) {
            fe.extent_offset = dup_off;
            fe.extent_size = dup_size;
            fe.codec = dup_codec;
            return Ok((0, current_extent_offset));
        }
        if pad_len > 0 {
            writer.write_all(&pad_zeros[..pad_len])?;
            current_extent_offset = aligned;
        }
        fe.extent_offset = current_extent_offset;
        fe.extent_size = comp_payload.len() as u64;
        fe.codec = NupConstants::CODEC_ZSTD;
        writer.write_all(&comp_payload)?;
        current_extent_offset += fe.extent_size;
        dedup.insert(
            (actual_size, sha256),
            (fe.extent_offset, fe.extent_size, fe.codec),
        );
        return Ok((fe.extent_size, current_extent_offset));
    }

    // Stream RAW (either > 256 MiB, compression_level == 0, or Zstd didn't shrink)
    debug_assert!(did_single_pass_raw_copy);
    if pad_len > 0 {
        writer.write_all(&pad_zeros[..pad_len])?;
        current_extent_offset = aligned;
    }
    fe.extent_offset = current_extent_offset;

    let mut hasher = Sha256::new();
    let mut raw_pos = 0u64;
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(NupError::Cancelled);
        }
        let r = in_f.read(io_buf)?;
        if r == 0 {
            break;
        }
        hasher.update(&io_buf[..r]);
        writer.write_all(&io_buf[..r])?;
        raw_pos += r as u64;
        if raw_pos > reported_for_file {
            let delta = raw_pos - reported_for_file;
            reported_for_file = raw_pos;
            on_bytes(delta);
        }
    }
    fe.sha256 = hasher.finalize().into();
    fe.extent_size = actual_size;
    fe.codec = NupConstants::CODEC_RAW;
    current_extent_offset += fe.extent_size;
    dedup.insert(
        (actual_size, fe.sha256),
        (fe.extent_offset, fe.extent_size, fe.codec),
    );
    Ok((fe.extent_size, current_extent_offset))
}

#[allow(clippy::too_many_arguments)]
fn collect_tree<F>(
    dir_path: &Path,
    depth: usize,
    exclude_top_dirs: &[&str],
    out_dir: &mut TempDir,
    files_seen: &mut usize,
    bytes_seen: &mut u64,
    last_report: &mut usize,
    progress: &mut F,
    cancel: &AtomicBool,
) -> NupResult<()>
where
    F: FnMut(PackProgressInfo),
{
    if cancel.load(Ordering::Relaxed) {
        return Err(NupError::Cancelled);
    }

    for entry in fs::read_dir(dir_path)? {
        let entry = entry?;
        let ft = entry.file_type()?;
        if ft.is_symlink() {
            continue;
        }
        let path = entry.path();
        let raw_name = entry.file_name().to_string_lossy().into_owned();

        if ft.is_dir() {
            if depth == 0
                && exclude_top_dirs
                    .iter()
                    .any(|ex| raw_name.eq_ignore_ascii_case(ex))
            {
                continue;
            }
            // Canonical lowercase for package-level "exefs"/"romfs" folders while keeping game asset names exact
            let norm_name = if depth == 0 && raw_name.eq_ignore_ascii_case("exefs") {
                "exefs".to_string()
            } else if depth == 0 && raw_name.eq_ignore_ascii_case("romfs") {
                "romfs".to_string()
            } else {
                raw_name
            };
            let mut sub = TempDir {
                name: norm_name.clone(),
                children: BTreeMap::new(),
            };
            collect_tree(
                &path,
                depth + 1,
                &[],
                &mut sub,
                files_seen,
                bytes_seen,
                last_report,
                progress,
                cancel,
            )?;
            out_dir
                .children
                .insert(norm_name.into_bytes(), TempChild::Dir(sub));
        } else if ft.is_file() {
            if raw_name.eq_ignore_ascii_case(".listing") {
                continue;
            }
            let size = entry.metadata()?.len();
            *files_seen += 1;
            *bytes_seen = bytes_seen.saturating_add(size);
            if files_seen.saturating_sub(*last_report) >= 2000 {
                *last_report = *files_seen;
                progress(PackProgressInfo {
                    status: "scanning".to_string(),
                    current_file: format!("Scanning hierarchy ({files_seen} files found)..."),
                    files_packed: 0,
                    total_files: *files_seen,
                    bytes_processed: 0,
                    total_bytes: *bytes_seen,
                    bytes_written: 0,
                    compression_ratio: 0.0,
                    speed_formatted: String::new(),
                    percent: 0.0,
                    error: None,
                    elapsed_seconds: 0.0,
                });
            }
            out_dir.children.insert(
                raw_name.into_bytes(),
                TempChild::File {
                    full_path: path,
                    size,
                },
            );
        }
    }

    Ok(())
}

fn assign_dir(
    tree: &TempDir,
    dirs: &mut Vec<DirEntry>,
    files: &mut Vec<FileEntry>,
    children_tagged: &mut Vec<u64>,
) -> u64 {
    const FILE_TAG: u64 = 1u64 << 32;
    let id = dirs.len() as u64;
    let first_child = children_tagged.len() as u32;
    let child_count = tree.children.len() as u32;
    dirs.push(DirEntry {
        name: tree.name.clone(),
        first_child,
        child_count,
    });

    let slot_base = children_tagged.len();
    children_tagged.resize(slot_base + tree.children.len(), 0);

    for (slot_offset, (name_bytes, child)) in tree.children.iter().enumerate() {
        if let TempChild::File { full_path, size } = child {
            let fi = files.len() as u64;
            files.push(FileEntry {
                name: String::from_utf8_lossy(name_bytes).into_owned(),
                full_path: full_path.clone(),
                size: *size,
                extent_offset: 0,
                extent_size: 0,
                sha256: [0u8; 32],
                codec: NupConstants::CODEC_RAW,
            });
            children_tagged[slot_base + slot_offset] = FILE_TAG | fi;
        }
    }

    for (slot_offset, child) in tree.children.values().enumerate() {
        if let TempChild::Dir(sub) = child {
            let sub_id = assign_dir(sub, dirs, files, children_tagged);
            children_tagged[slot_base + slot_offset] = sub_id;
        }
    }

    id
}

fn subtree_has_files_tagged(
    dir_idx: usize,
    dirs: &[DirEntry],
    children_tagged: &[u64],
    file_tag: u64,
) -> bool {
    let d = &dirs[dir_idx];
    let first = d.first_child as usize;
    let end = first + d.child_count as usize;
    for &c in &children_tagged[first..end] {
        if c & file_tag != 0 {
            return true;
        }
        if subtree_has_files_tagged(c as usize, dirs, children_tagged, file_tag) {
            return true;
        }
    }
    false
}

fn intern_string(
    pool: &mut Vec<u8>,
    intern_map: &mut HashMap<Vec<u8>, (u32, u32)>,
    bytes: &[u8],
) -> (u32, u32) {
    if let Some(&loc) = intern_map.get(bytes) {
        return loc;
    }
    let off = pool.len() as u32;
    let len = bytes.len() as u32;
    pool.extend_from_slice(bytes);
    intern_map.insert(bytes.to_vec(), (off, len));
    (off, len)
}

fn find_child_dir_ci(parent: &Path, name: &str) -> Option<PathBuf> {
    let entries = fs::read_dir(parent).ok()?;
    for entry in entries.flatten() {
        if let Ok(ft) = entry.file_type()
            && ft.is_dir()
            && entry
                .file_name()
                .to_string_lossy()
                .eq_ignore_ascii_case(name)
        {
            return Some(entry.path());
        }
    }
    None
}

fn find_child_file_ci(parent: &Path, name: &str) -> Option<PathBuf> {
    let entries = fs::read_dir(parent).ok()?;
    for entry in entries.flatten() {
        if let Ok(ft) = entry.file_type()
            && ft.is_file()
            && entry
                .file_name()
                .to_string_lossy()
                .eq_ignore_ascii_case(name)
        {
            return Some(entry.path());
        }
    }
    None
}

fn directory_has_files(dir: &Path) -> bool {
    let mut stack = vec![dir.to_path_buf()];
    while let Some(cur) = stack.pop() {
        let Ok(entries) = fs::read_dir(&cur) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(ft) = entry.file_type() else {
                continue;
            };
            if ft.is_dir() {
                stack.push(entry.path());
            } else if ft.is_file()
                && !entry
                    .file_name()
                    .to_string_lossy()
                    .eq_ignore_ascii_case(".listing")
            {
                return true;
            }
        }
    }
    false
}

fn has_package_subdirs(dir: &Path) -> bool {
    find_child_dir_ci(dir, "exefs").is_some_and(|p| directory_has_files(&p))
        || find_child_dir_ci(dir, "romfs").is_some_and(|p| directory_has_files(&p))
}

fn try_parse_npdm_program_id(pkg_dir: &Path) -> u64 {
    let mut cands = Vec::new();
    if let Some(exefs) = find_child_dir_ci(pkg_dir, "exefs")
        && let Some(npdm) = find_child_file_ci(&exefs, "main.npdm")
    {
        cands.push(npdm);
    }
    if let Some(npdm) = find_child_file_ci(pkg_dir, "main.npdm") {
        cands.push(npdm);
    }
    for cand in cands {
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
    let mut cands = Vec::new();
    if let Some(exefs) = find_child_dir_ci(pkg_dir, "exefs")
        && let Some(main_f) = find_child_file_ci(&exefs, "main")
    {
        cands.push(main_f);
    }
    if let Some(main_f) = find_child_file_ci(pkg_dir, "main") {
        cands.push(main_f);
    }
    for cand in cands {
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

fn try_read_nacp_bytes(pkg_dir: &Path) -> Option<Vec<u8>> {
    let mut dirs_to_check = Vec::new();
    if let Some(exefs) = find_child_dir_ci(pkg_dir, "exefs") {
        dirs_to_check.push(exefs);
    }
    dirs_to_check.push(pkg_dir.to_path_buf());
    for d in dirs_to_check {
        if let Some(nacp) = find_child_file_ci(&d, "control.nacp")
            && let Ok(bytes) = fs::read(&nacp)
            && bytes.len() == NACP_SIZE
        {
            return Some(bytes);
        }
        if let Ok(entries) = fs::read_dir(&d) {
            for entry in entries.flatten() {
                if let Ok(ft) = entry.file_type()
                    && ft.is_file()
                    && entry
                        .path()
                        .extension()
                        .is_some_and(|ext| ext.eq_ignore_ascii_case("nacp"))
                    && let Ok(bytes) = fs::read(entry.path())
                    && bytes.len() == NACP_SIZE
                {
                    return Some(bytes);
                }
            }
        }
    }
    None
}

fn read_cstr(slice: &[u8]) -> Option<String> {
    let end = slice.iter().position(|&b| b == 0).unwrap_or(slice.len());
    let s = String::from_utf8_lossy(&slice[..end]).trim().to_string();
    if s.is_empty() { None } else { Some(s) }
}

fn parse_nacp_info(raw: &[u8]) -> NacpInfo {
    if raw.len() != NACP_SIZE {
        return NacpInfo::default();
    }
    // Check AmericanEnglish (slot 0), then fallback to first non-empty language entry (each 0x300 bytes)
    let mut title = None;
    let mut publisher = None;
    for slot in 0..16 {
        let off = slot * 0x300;
        if title.is_none() {
            title = read_cstr(&raw[off..off + 0x200]);
        }
        if publisher.is_none() {
            publisher = read_cstr(&raw[off + 0x200..off + 0x300]);
        }
        if title.is_some() && publisher.is_some() {
            break;
        }
    }
    let version = read_cstr(&raw[0x3060..0x3070]);
    NacpInfo {
        title,
        publisher,
        version,
    }
}

fn try_read_sidecar_icon(pkg_dir: &Path) -> Option<Vec<u8>> {
    let mut dirs_to_check = Vec::new();
    if let Some(exefs) = find_child_dir_ci(pkg_dir, "exefs") {
        dirs_to_check.push(exefs);
    }
    dirs_to_check.push(pkg_dir.to_path_buf());
    for d in &dirs_to_check {
        for preferred in ["icon_AmericanEnglish.dat", "icon_BritishEnglish.dat"] {
            if let Some(p) = find_child_file_ci(d, preferred)
                && let Ok(bytes) = fs::read(&p)
                && !bytes.is_empty()
            {
                return Some(bytes);
            }
        }
        if let Ok(entries) = fs::read_dir(d) {
            for entry in entries.flatten() {
                if let Ok(ft) = entry.file_type()
                    && ft.is_file()
                {
                    let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
                    if ((name.starts_with("icon_") && name.ends_with(".dat"))
                        || name == "icon.jpg"
                        || name == "icon.jpeg"
                        || name == "icon.png")
                        && let Ok(bytes) = fs::read(entry.path())
                        && !bytes.is_empty()
                    {
                        return Some(bytes);
                    }
                }
            }
        }
    }
    None
}

fn resolve_base_title_id(override_id: u64, npdm_id: u64) -> u64 {
    if override_id != 0 && (override_id != PLACEHOLDER_TITLE_ID || npdm_id == 0) {
        override_id
    } else if npdm_id != 0 {
        npdm_id
    } else {
        PLACEHOLDER_TITLE_ID
    }
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
    let base_dir_opt = find_child_dir_ci(&src, "base").filter(|p| directory_has_files(p));
    let updates_dir_opt = find_child_dir_ci(&src, "updates").filter(|p| directory_has_files(p));
    let dlc_dir_opt = find_child_dir_ci(&src, "dlc").filter(|p| directory_has_files(p));

    let mut has_base = base_dir_opt.is_some();
    let mut base_title_id = options.title_id;

    if let Some(base_dir) = base_dir_opt {
        let npdm_id = try_parse_npdm_program_id(&base_dir);
        base_title_id = resolve_base_title_id(base_title_id, npdm_id);
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
            exclude_top_dirs: &[],
        });
    } else if (updates_dir_opt.is_some() || dlc_dir_opt.is_some()) && has_package_subdirs(&src) {
        // Root folder contains `exefs/` or `romfs/` directly alongside `updates/` or `dlc/`
        let npdm_id = try_parse_npdm_program_id(&src);
        base_title_id = resolve_base_title_id(base_title_id, npdm_id);
        has_base = true;
        list.push(DiscoveredPackage {
            kind: NupConstants::KIND_BASE,
            source_path: src.clone(),
            title_id: base_title_id,
            version: parse_version_string(&options.version_string),
            flags: 0,
            display_name: options.title_name.clone(),
            root_dir_index: 0,
            exefs_dir_index: NupConstants::NO_DIR,
            romfs_dir_index: NupConstants::NO_DIR,
            meta_bytes: Vec::new(),
            exclude_top_dirs: &["base", "updates", "dlc"],
        });
    }

    if let Some(updates_dir) = updates_dir_opt {
        let update_flags = if has_base {
            0
        } else {
            NupConstants::PKG_FLAG_STANDALONE_UPDATE
        };
        let mut sub_dirs: Vec<PathBuf> = fs::read_dir(&updates_dir)?
            .flatten()
            .filter(|e| e.file_type().is_ok_and(|ft| ft.is_dir()))
            .map(|e| e.path())
            .filter(|p| directory_has_files(p))
            .collect();
        sub_dirs.sort_by_key(|p| {
            let name = p
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            (name.parse::<u32>().unwrap_or(u32::MAX), name.into_bytes())
        });

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
                    } else if options.title_id != 0 {
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
                    exclude_top_dirs: &[],
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
                exclude_top_dirs: &[],
            });
        }
    }

    if let Some(dlc_dir) = dlc_dir_opt {
        let mut dlc_subs: Vec<PathBuf> = fs::read_dir(&dlc_dir)?
            .flatten()
            .filter(|e| e.file_type().is_ok_and(|ft| ft.is_dir()))
            .map(|e| e.path())
            .collect();
        dlc_subs.sort_by_key(|p| {
            p.file_name()
                .map(|n| n.to_string_lossy().into_owned().into_bytes())
                .unwrap_or_default()
        });

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
                    .filter(|e| e.file_type().is_ok_and(|ft| ft.is_dir()))
                    .map(|e| e.path())
                    .filter(|p| directory_has_files(p))
                    .collect();
                idx_dirs.sort_by_key(|p| {
                    p.file_name()
                        .map(|n| n.to_string_lossy().into_owned().into_bytes())
                        .unwrap_or_default()
                });
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
                            exclude_top_dirs: &[],
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
                        exclude_top_dirs: &[],
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
                    exclude_top_dirs: &[],
                });
                dlc_counter += 1;
            }
        }
    }

    if list.is_empty() {
        let npdm_id = try_parse_npdm_program_id(&src);
        let tid = resolve_base_title_id(options.title_id, npdm_id);
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
            exclude_top_dirs: &[],
        });
    }

    Ok(list)
}

fn build_package_metadata(
    pkg: &DiscoveredPackage,
    is_first_package: bool,
    options: &PackOptions,
) -> Vec<u8> {
    let mut tlvs: Vec<(u16, Vec<u8>)> = Vec::new();

    // NPDM (authoritative Program ID for emulator loaders)
    let npdm_path = find_child_dir_ci(&pkg.source_path, "exefs")
        .and_then(|e| find_child_file_ci(&e, "main.npdm"))
        .or_else(|| find_child_file_ci(&pkg.source_path, "main.npdm"));
    if let Some(p) = npdm_path
        && let Ok(npdm) = fs::read(p)
        && !npdm.is_empty()
    {
        tlvs.push((NupConstants::TLV_NPDM, npdm));
    }

    // NACP (`control.nacp` if dump provides an exact 16 KiB NACP)
    let nacp_bytes = try_read_nacp_bytes(&pkg.source_path);
    let nacp_info = nacp_bytes
        .as_deref()
        .map(parse_nacp_info)
        .unwrap_or_default();
    if let Some(nacp) = nacp_bytes {
        tlvs.push((NupConstants::TLV_NACP, nacp));
    }

    // Application display metadata belongs on Base, or first package for base-less containers
    if pkg.kind == NupConstants::KIND_BASE || is_first_package {
        if let Some(ref icon_p) = options.icon_path
            && let Ok(bytes) = fs::read(icon_p)
            && !bytes.is_empty()
        {
            tlvs.push((NupConstants::TLV_ICON, bytes));
        } else if let Some(sidecar_icon) = try_read_sidecar_icon(&pkg.source_path) {
            tlvs.push((NupConstants::TLV_ICON, sidecar_icon));
        }

        if !options.version_string.is_empty() {
            tlvs.push((
                NupConstants::TLV_DISPLAY_VERSION,
                options.version_string.as_bytes().to_vec(),
            ));
        } else if let Some(ver) = nacp_info.version {
            tlvs.push((NupConstants::TLV_DISPLAY_VERSION, ver.into_bytes()));
        }

        if !options.title_name.is_empty() {
            tlvs.push((
                NupConstants::TLV_TITLE_NAME,
                options.title_name.as_bytes().to_vec(),
            ));
        } else if let Some(title) = nacp_info.title {
            tlvs.push((NupConstants::TLV_TITLE_NAME, title.into_bytes()));
        }

        if !options.publisher.is_empty() {
            tlvs.push((
                NupConstants::TLV_PUBLISHER,
                options.publisher.as_bytes().to_vec(),
            ));
        } else if let Some(pub_name) = nacp_info.publisher {
            tlvs.push((NupConstants::TLV_PUBLISHER, pub_name.into_bytes()));
        }
    } else if pkg.kind == NupConstants::KIND_UPDATE {
        let ver_str = format_version(pkg.version);
        tlvs.push((NupConstants::TLV_DISPLAY_VERSION, ver_str.into_bytes()));
    }

    if let Some(bid) = try_read_build_id(&pkg.source_path) {
        tlvs.push((NupConstants::TLV_BUILD_ID, bid.to_vec()));
    }

    let dump_name = Path::new(&options.source_directory)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "nup".to_string());
    tlvs.push((NupConstants::TLV_SOURCE_DUMP, dump_name.into_bytes()));

    tlvs.sort_by_key(|(tlv_type, _)| *tlv_type);

    let mut out = Vec::new();
    for (tlv_type, payload) in tlvs {
        write_tlv(&mut out, tlv_type, &payload);
    }
    out
}

fn write_tlv(out: &mut Vec<u8>, tlv_type: u16, payload: &[u8]) {
    out.extend_from_slice(&tlv_type.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    out.extend_from_slice(payload);
}

fn format_version(version: u32) -> String {
    format!(
        "{}.{}.{}",
        version >> 16,
        (version >> 8) & 0xFF,
        version & 0xFF
    )
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
