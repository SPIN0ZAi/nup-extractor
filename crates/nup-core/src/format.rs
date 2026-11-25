#![forbid(unsafe_code)]

pub struct NupConstants;

impl NupConstants {
    pub const MAGIC: [u8; 4] = [b'N', b'U', b'P', 1];
    pub const TOC_MAGIC: [u8; 4] = [b'T', b'O', b'C', 1];
    pub const HEADER_SIZE: usize = 0x80;
    pub const TOC_HEADER_SIZE: usize = 48;
    pub const PKG_SIZE: usize = 0x40;
    pub const DIR_SIZE: usize = 0x18;
    pub const FILE_SIZE: usize = 0x50;
    pub const EXTENT_ALIGN: u64 = 16;
    pub const NO_DIR: u32 = 0xFFFF_FFFF;

    pub const CODEC_RAW: u8 = 0;
    pub const CODEC_ZSTD: u8 = 1;

    pub const KIND_BASE: u8 = 0;
    pub const KIND_UPDATE: u8 = 1;
    pub const KIND_ADDON: u8 = 2;
    pub const KIND_ADDON_UPDATE: u8 = 3;
    pub const KIND_SYSTEM: u8 = 4;

    pub const TLV_NPDM: u16 = 0x0001;
    pub const TLV_NACP: u16 = 0x0002;
    pub const TLV_ICON: u16 = 0x0003;
    pub const TLV_DISPLAY_VERSION: u16 = 0x0004;
    pub const TLV_TITLE_NAME: u16 = 0x0005;
    pub const TLV_PUBLISHER: u16 = 0x0006;
    pub const TLV_BUILD_ID: u16 = 0x0007;
    pub const TLV_SOURCE_DUMP: u16 = 0x0008;
    pub const TLV_LOCALIZED_TITLES: u16 = 0x0009;
    pub const TLV_LOCALIZED_ICONS: u16 = 0x000A;

    pub const PKG_FLAG_STANDALONE_UPDATE: u8 = 0x01;
}

pub fn get_kind_name(kind: u8) -> String {
    match kind {
        NupConstants::KIND_BASE => "base".to_string(),
        NupConstants::KIND_UPDATE => "update".to_string(),
        NupConstants::KIND_ADDON => "addon".to_string(),
        NupConstants::KIND_ADDON_UPDATE => "addon_update".to_string(),
        NupConstants::KIND_SYSTEM => "system".to_string(),
        other => format!("kind{other}"),
    }
}

pub fn hex_lower(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        use std::fmt::Write;
        let _ = write!(&mut s, "{b:02x}");
    }
    s
}

pub fn hex_upper(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        use std::fmt::Write;
        let _ = write!(&mut s, "{b:02X}");
    }
    s
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NupSuperblock {
    pub header_size: u16,
    pub header_version: u16,
    pub toc_offset: u64,
    pub toc_size: u64,
    pub toc_sha256: [u8; 32],
    pub blob_offset: u64,
    pub file_size: u64,
    pub package_count: u32,
    pub flags: u32,
    pub uuid: [u8; 16],
}

impl NupSuperblock {
    pub fn formatted_uuid(&self) -> String {
        let hex = hex_lower(&self.uuid);
        format!(
            "{}-{}-{}-{}-{}",
            &hex[0..8],
            &hex[8..12],
            &hex[12..16],
            &hex[16..20],
            &hex[20..32]
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NupPackage {
    pub index: usize,
    pub title_id: u64,
    pub root_dir: u32,
    pub exe_fs_dir: u32,
    pub rom_fs_dir: u32,
    pub version: u32,
    pub kind: u8,
    pub flags: u8,
    pub meta_offset: u64,
    pub meta_size: u32,
}

impl NupPackage {
    pub fn kind_name(&self) -> String {
        get_kind_name(self.kind)
    }

    pub fn title_id_hex(&self) -> String {
        format!("{:016X}", self.title_id)
    }

    pub fn version_str(&self) -> String {
        format!(
            "{}.{}.{}",
            self.version >> 16,
            (self.version >> 8) & 0xFF,
            self.version & 0xFF
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NupDirNode {
    pub name: String,
    pub first_child: u32,
    pub child_count: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NupFileNode {
    pub index: usize,
    pub name: String,
    pub size: u64,
    pub extent_offset: u64,
    pub extent_size: u64,
    pub sha256: [u8; 32],
    pub codec: u8,
    pub codec_flags: u8,
}
