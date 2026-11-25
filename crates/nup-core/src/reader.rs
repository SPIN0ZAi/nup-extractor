#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::error::{NupError, NupResult};
use crate::format::{NupConstants, NupDirNode, NupFileNode, NupPackage, NupSuperblock};
use crate::mmap::BackingBytes;

const MAX_TOC_SIZE: u64 = 512 * 1024 * 1024; // 512 MiB safety cap
const MAX_PACKAGES: u32 = 100_000;
const MAX_DIRS: u64 = 10_000_000;
const MAX_FILES: u64 = 10_000_000;
const MAX_CHILDREN: usize = 20_000_000;
pub const STREAM_BUF_SIZE: usize = 131_072; // 128 KiB

pub struct NupReader {
    backing: BackingBytes,
    pub file_path: PathBuf,
    pub superblock: NupSuperblock,
    pub packages: Vec<NupPackage>,
    pub dirs: Vec<NupDirNode>,
    pub files: Vec<NupFileNode>,
    pub children: Vec<u32>,
    pub string_pool: Vec<u8>,
}

impl NupReader {
    pub fn open<P: AsRef<Path>>(path: P) -> NupResult<Self> {
        let abs =
            std::fs::canonicalize(path.as_ref()).unwrap_or_else(|_| path.as_ref().to_path_buf());
        // Strip Windows verbatim prefix `\\?\` when canonicalize adds it, matching C# Path.GetFullPath
        let clean_abs = strip_verbatim_prefix(abs);
        let backing = BackingBytes::open_readonly(path.as_ref())?;
        Self::from_backing(backing, clean_abs)
    }

    pub fn from_bytes(bytes: Vec<u8>, virtual_path: PathBuf) -> NupResult<Self> {
        let backing = BackingBytes::from_vec(bytes);
        Self::from_backing(backing, virtual_path)
    }

    fn from_backing(backing: BackingBytes, file_path: PathBuf) -> NupResult<Self> {
        let data = backing.as_slice();
        let superblock = parse_superblock(data)?;
        let (packages, dirs, files, children, string_pool) = parse_toc(data, &superblock)?;

        Ok(Self {
            backing,
            file_path,
            superblock,
            packages,
            dirs,
            files,
            children,
            string_pool,
        })
    }

    #[inline]
    pub fn file_size(&self) -> u64 {
        self.backing.as_slice().len() as u64
    }

    /// Returns `(is_dir, index, name)` for every valid child of `dir_index`.
    pub fn get_children(&self, dir_index: u32) -> Vec<(bool, usize, String)> {
        let Some(d) = self.dirs.get(dir_index as usize) else {
            return Vec::new();
        };

        let start = d.first_child as usize;
        let end = start
            .saturating_add(d.child_count as usize)
            .min(self.children.len());
        if start >= end {
            return Vec::new();
        }

        let dir_count = self.dirs.len() as u32;
        let mut result = Vec::with_capacity(end - start);

        for &cid in &self.children[start..end] {
            if cid < dir_count {
                let idx = cid as usize;
                result.push((true, idx, self.dirs[idx].name.clone()));
            } else {
                let fi = (cid - dir_count) as usize;
                if let Some(f) = self.files.get(fi) {
                    result.push((false, fi, f.name.clone()));
                }
            }
        }
        result
    }

    pub fn get_package_metadata(&self, pkg: &NupPackage) -> HashMap<u16, Vec<u8>> {
        let mut meta = HashMap::new();
        let data = self.backing.as_slice();
        if pkg.meta_size == 0 {
            return meta;
        }
        let Some(end) = pkg.meta_offset.checked_add(u64::from(pkg.meta_size)) else {
            return meta;
        };
        if end > data.len() as u64 {
            return meta;
        }
        let Ok(start_idx) = usize::try_from(pkg.meta_offset) else {
            return meta;
        };
        let Ok(end_idx) = usize::try_from(end) else {
            return meta;
        };
        let block = &data[start_idx..end_idx];

        let mut pos = 0usize;
        while pos + 8 <= block.len() {
            let tlv_type = u16::from_le_bytes([block[pos], block[pos + 1]]);
            let tlv_size = u32::from_le_bytes([
                block[pos + 4],
                block[pos + 5],
                block[pos + 6],
                block[pos + 7],
            ]) as usize;
            pos += 8;

            let Some(next_pos) = pos.checked_add(tlv_size) else {
                break;
            };
            if next_pos > block.len() {
                break;
            }
            meta.insert(tlv_type, block[pos..next_pos].to_vec());
            pos = next_pos;
        }
        meta
    }

    pub fn extent_slice(&self, file_index: usize) -> NupResult<(&NupFileNode, &[u8])> {
        let f = self
            .files
            .get(file_index)
            .ok_or(NupError::FileIndexOutOfBounds(file_index))?;
        if f.size == 0 && f.extent_size == 0 {
            return Ok((f, &[]));
        }
        let end = f
            .extent_offset
            .checked_add(f.extent_size)
            .ok_or_else(|| NupError::ExtentOutOfBounds(f.name.clone()))?;
        let data = self.backing.as_slice();
        if end > data.len() as u64 {
            return Err(NupError::ExtentOutOfBounds(f.name.clone()));
        }
        let start_idx = usize::try_from(f.extent_offset)
            .map_err(|_| NupError::ExtentOutOfBounds(f.name.clone()))?;
        let end_idx =
            usize::try_from(end).map_err(|_| NupError::ExtentOutOfBounds(f.name.clone()))?;
        Ok((f, &data[start_idx..end_idx]))
    }

    /// Streams decompressed file bytes into `writer` with a 128 KiB buffer and decompression-bomb guard.
    pub fn extract_file_to_writer<W: Write>(
        &self,
        file_index: usize,
        writer: &mut W,
    ) -> NupResult<u64> {
        let (f, extent) = self.extent_slice(file_index)?;
        if f.size == 0 {
            return Ok(0);
        }

        match f.codec {
            NupConstants::CODEC_RAW => {
                if extent.len() as u64 > f.size {
                    return Err(NupError::DecompressionBomb {
                        name: f.name.clone(),
                        declared: f.size,
                    });
                }
                let mut offset = 0usize;
                while offset < extent.len() {
                    let chunk_end = (offset + STREAM_BUF_SIZE).min(extent.len());
                    writer.write_all(&extent[offset..chunk_end])?;
                    offset = chunk_end;
                }
                Ok(extent.len() as u64)
            }
            NupConstants::CODEC_ZSTD => {
                let mut decoder = zstd::stream::read::Decoder::new(extent)?;
                let mut buf = vec![0u8; STREAM_BUF_SIZE];
                let mut total_written: u64 = 0;

                loop {
                    let n = decoder.read(&mut buf)?;
                    if n == 0 {
                        break;
                    }
                    total_written = total_written.saturating_add(n as u64);
                    if total_written > f.size {
                        return Err(NupError::DecompressionBomb {
                            name: f.name.clone(),
                            declared: f.size,
                        });
                    }
                    writer.write_all(&buf[..n])?;
                }
                Ok(total_written)
            }
            other => Err(NupError::UnknownCodec(other)),
        }
    }

    pub fn read_file_data(&self, file_index: usize) -> NupResult<Vec<u8>> {
        let f = self
            .files
            .get(file_index)
            .ok_or(NupError::FileIndexOutOfBounds(file_index))?;
        if f.size == 0 {
            return Ok(Vec::new());
        }
        const MAX_IN_MEMORY_FILE: u64 = 0x7FFF_FFC7;
        if f.size > MAX_IN_MEMORY_FILE {
            return Err(NupError::AllocationLimitExceeded(format!(
                "file '{}' size {} exceeds in-memory limit",
                f.name, f.size
            )));
        }
        let mut out = Vec::with_capacity(f.size as usize);
        let written = self.extract_file_to_writer(file_index, &mut out)?;
        if written != f.size {
            return Err(NupError::SizeMismatch {
                name: f.name.clone(),
                actual: written,
                expected: f.size,
            });
        }
        Ok(out)
    }

    pub fn hash_file_sha256(&self, file_index: usize) -> NupResult<[u8; 32]> {
        struct ShaWriter(Sha256);
        impl Write for ShaWriter {
            fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
                self.0.update(buf);
                Ok(buf.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        let mut sw = ShaWriter(Sha256::new());
        self.extract_file_to_writer(file_index, &mut sw)?;
        let digest = sw.0.finalize();
        let mut out = [0u8; 32];
        out.copy_from_slice(&digest);
        Ok(out)
    }
}

fn parse_superblock(data: &[u8]) -> NupResult<NupSuperblock> {
    let actual_size = data.len() as u64;
    if actual_size < NupConstants::HEADER_SIZE as u64 {
        return Err(NupError::TooSmall(actual_size));
    }

    if data[0..4] != NupConstants::MAGIC {
        return Err(NupError::InvalidMagic);
    }

    let header_size = read_u16(data, 4);
    let header_version = read_u16(data, 6);
    let toc_offset = read_u64(data, 8);
    let toc_size = read_u64(data, 16);
    let mut toc_sha256 = [0u8; 32];
    toc_sha256.copy_from_slice(&data[24..56]);
    let blob_offset = read_u64(data, 56);
    let file_size = read_u64(data, 64);
    let package_count = read_u32(data, 72);
    let flags = read_u32(data, 76);
    let mut uuid = [0u8; 16];
    uuid.copy_from_slice(&data[80..96]);

    if file_size != actual_size {
        return Err(NupError::FileSizeMismatch {
            header: file_size,
            actual: actual_size,
        });
    }

    if toc_offset < NupConstants::HEADER_SIZE as u64 {
        return Err(NupError::TocBeforeHeader);
    }

    if toc_size > MAX_TOC_SIZE {
        return Err(NupError::AllocationLimitExceeded(format!(
            "TOC size {toc_size} exceeds limit"
        )));
    }

    let toc_end = toc_offset
        .checked_add(toc_size)
        .ok_or(NupError::BlobOverlapsToc)?;

    if blob_offset < toc_end {
        return Err(NupError::BlobOverlapsToc);
    }

    if !blob_offset.is_multiple_of(NupConstants::EXTENT_ALIGN) {
        return Err(NupError::BlobUnaligned);
    }

    if toc_end > actual_size {
        return Err(NupError::TableOutOfBounds);
    }

    let toc_start_idx = usize::try_from(toc_offset).map_err(|_| NupError::TableOutOfBounds)?;
    let toc_end_idx = usize::try_from(toc_end).map_err(|_| NupError::TableOutOfBounds)?;

    let computed = Sha256::digest(&data[toc_start_idx..toc_end_idx]);
    if computed.as_slice() != toc_sha256 {
        return Err(NupError::TocSha256Mismatch);
    }

    Ok(NupSuperblock {
        header_size,
        header_version,
        toc_offset,
        toc_size,
        toc_sha256,
        blob_offset,
        file_size,
        package_count,
        flags,
        uuid,
    })
}

type ParsedToc = (
    Vec<NupPackage>,
    Vec<NupDirNode>,
    Vec<NupFileNode>,
    Vec<u32>,
    Vec<u8>,
);

fn parse_toc(data: &[u8], superblock: &NupSuperblock) -> NupResult<ParsedToc> {
    let toc_start =
        usize::try_from(superblock.toc_offset).map_err(|_| NupError::TableOutOfBounds)?;
    let toc_len = usize::try_from(superblock.toc_size).map_err(|_| NupError::TableOutOfBounds)?;
    let toc_end = toc_start
        .checked_add(toc_len)
        .ok_or(NupError::TableOutOfBounds)?;
    if toc_end > data.len() {
        return Err(NupError::TableOutOfBounds);
    }
    let toc = &data[toc_start..toc_end];

    if toc.len() < NupConstants::TOC_HEADER_SIZE {
        return Err(NupError::TocTooShort);
    }

    if toc[0..4] != NupConstants::TOC_MAGIC {
        return Err(NupError::InvalidTocMagic);
    }

    let pkg_count = read_u32(toc, 4);
    let dir_count = read_u64(toc, 8);
    let file_count = read_u64(toc, 16);
    let pool_off = read_u64(toc, 24);
    let pool_size = read_u64(toc, 32);

    if pkg_count != superblock.package_count {
        return Err(NupError::PackageCountMismatch);
    }

    if pkg_count > MAX_PACKAGES {
        return Err(NupError::AllocationLimitExceeded(format!(
            "package_count {pkg_count} exceeds cap"
        )));
    }
    if dir_count > MAX_DIRS {
        return Err(NupError::AllocationLimitExceeded(format!(
            "dir_count {dir_count} exceeds cap"
        )));
    }
    if file_count > MAX_FILES {
        return Err(NupError::AllocationLimitExceeded(format!(
            "file_count {file_count} exceeds cap"
        )));
    }

    let pool_end = pool_off
        .checked_add(pool_size)
        .ok_or(NupError::StringPoolOutOfBounds)?;
    if pool_end > toc.len() as u64 {
        return Err(NupError::StringPoolOutOfBounds);
    }

    let pool_off_idx = usize::try_from(pool_off).map_err(|_| NupError::StringPoolOutOfBounds)?;
    let pool_end_idx = usize::try_from(pool_end).map_err(|_| NupError::StringPoolOutOfBounds)?;
    let string_pool = toc[pool_off_idx..pool_end_idx].to_vec();

    // Validate full table byte lengths with checked arithmetic BEFORE allocating
    let pkgs_bytes = (pkg_count as usize)
        .checked_mul(NupConstants::PKG_SIZE)
        .ok_or(NupError::TableOutOfBounds)?;
    let dirs_bytes = usize::try_from(dir_count)
        .ok()
        .and_then(|c| c.checked_mul(NupConstants::DIR_SIZE))
        .ok_or(NupError::TableOutOfBounds)?;
    let files_bytes = usize::try_from(file_count)
        .ok()
        .and_then(|c| c.checked_mul(NupConstants::FILE_SIZE))
        .ok_or(NupError::TableOutOfBounds)?;

    let tables_end = NupConstants::TOC_HEADER_SIZE
        .checked_add(pkgs_bytes)
        .and_then(|v| v.checked_add(dirs_bytes))
        .and_then(|v| v.checked_add(files_bytes))
        .ok_or(NupError::TableOutOfBounds)?;

    if tables_end > toc.len() || tables_end > pool_off_idx {
        return Err(NupError::TableOutOfBounds);
    }

    let mut pos = NupConstants::TOC_HEADER_SIZE;

    let mut packages = Vec::with_capacity(pkg_count as usize);
    for i in 0..pkg_count as usize {
        let p = &toc[pos..pos + NupConstants::PKG_SIZE];
        pos += NupConstants::PKG_SIZE;
        packages.push(NupPackage {
            index: i,
            title_id: read_u64(p, 0),
            root_dir: read_u32(p, 8),
            exe_fs_dir: read_u32(p, 12),
            rom_fs_dir: read_u32(p, 16),
            version: read_u32(p, 20),
            kind: p[24],
            flags: p[25],
            meta_offset: read_u64(p, 32),
            meta_size: read_u32(p, 40),
        });
    }

    let mut dirs = Vec::with_capacity(dir_count as usize);
    for _ in 0..dir_count as usize {
        let d = &toc[pos..pos + NupConstants::DIR_SIZE];
        pos += NupConstants::DIR_SIZE;
        let name_off = read_u32(d, 0);
        let name_len = read_u32(d, 4);
        let first_child = read_u32(d, 8);
        let child_count = read_u32(d, 12);
        dirs.push(NupDirNode {
            name: get_pool_string(&string_pool, name_off, name_len)?,
            first_child,
            child_count,
        });
    }

    let mut files = Vec::with_capacity(file_count as usize);
    for i in 0..file_count as usize {
        let f = &toc[pos..pos + NupConstants::FILE_SIZE];
        pos += NupConstants::FILE_SIZE;
        let name_off = read_u32(f, 0);
        let name_len = read_u32(f, 4);
        let size = read_u64(f, 8);
        let extent_offset = read_u64(f, 16);
        let extent_size = read_u64(f, 24);
        let mut sha256 = [0u8; 32];
        sha256.copy_from_slice(&f[32..64]);
        let codec = f[64];
        let codec_flags = f[65];

        files.push(NupFileNode {
            index: i,
            name: get_pool_string(&string_pool, name_off, name_len)?,
            size,
            extent_offset,
            extent_size,
            sha256,
            codec,
            codec_flags,
        });
    }

    let child_bytes = pool_off_idx - pos;
    let child_count_total = child_bytes / 4;
    if child_count_total > MAX_CHILDREN {
        return Err(NupError::AllocationLimitExceeded(format!(
            "children count {child_count_total} exceeds cap"
        )));
    }
    let mut children = Vec::with_capacity(child_count_total);
    for _ in 0..child_count_total {
        children.push(read_u32(toc, pos));
        pos += 4;
    }

    Ok((packages, dirs, files, children, string_pool))
}

fn get_pool_string(pool: &[u8], off: u32, len: u32) -> NupResult<String> {
    let end = (off as usize)
        .checked_add(len as usize)
        .ok_or(NupError::StringPoolOutOfBounds)?;
    if end > pool.len() {
        return Err(NupError::StringPoolOutOfBounds);
    }
    Ok(String::from_utf8_lossy(&pool[off as usize..end]).into_owned())
}

#[inline]
fn read_u16(b: &[u8], off: usize) -> u16 {
    u16::from_le_bytes([b[off], b[off + 1]])
}

#[inline]
fn read_u32(b: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]])
}

#[inline]
fn read_u64(b: &[u8], off: usize) -> u64 {
    u64::from_le_bytes([
        b[off],
        b[off + 1],
        b[off + 2],
        b[off + 3],
        b[off + 4],
        b[off + 5],
        b[off + 6],
        b[off + 7],
    ])
}

fn strip_verbatim_prefix(p: PathBuf) -> PathBuf {
    let s = p.to_string_lossy();
    if let Some(stripped) = s.strip_prefix(r"\\?\") {
        PathBuf::from(stripped)
    } else {
        p
    }
}
