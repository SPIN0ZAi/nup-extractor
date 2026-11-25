#![forbid(unsafe_code)]

use thiserror::Error;

pub type NupResult<T> = Result<T, NupError>;

#[derive(Debug, Error)]
pub enum NupError {
    #[error("File too small for NUP header: {0} bytes")]
    TooSmall(u64),

    #[error("Invalid NUP magic bytes")]
    InvalidMagic,

    #[error("NUP file size mismatch: header {header} != actual {actual}")]
    FileSizeMismatch { header: u64, actual: u64 },

    #[error("TOC offset is before header end")]
    TocBeforeHeader,

    #[error("Blob offset overlaps TOC")]
    BlobOverlapsToc,

    #[error("Blob offset is not 16-byte aligned")]
    BlobUnaligned,

    #[error("TOC SHA-256 mismatch: container is corrupt or modified")]
    TocSha256Mismatch,

    #[error("TOC header too short")]
    TocTooShort,

    #[error("Invalid TOC magic")]
    InvalidTocMagic,

    #[error("Package count mismatch between Superblock and TOC")]
    PackageCountMismatch,

    #[error("String pool out of TOC bounds")]
    StringPoolOutOfBounds,

    #[error("TOC table exceeds TOC bounds")]
    TableOutOfBounds,

    #[error("Declared container allocation exceeds safety limit: {0}")]
    AllocationLimitExceeded(String),

    #[error("Extent out of bounds for file {0}")]
    ExtentOutOfBounds(String),

    #[error("Unknown codec: {0}")]
    UnknownCodec(u8),

    #[error("Decompressed output for '{name}' exceeded declared size {declared} bytes")]
    DecompressionBomb { name: String, declared: u64 },

    #[error("Size mismatch for {name}: read {actual} bytes, expected {expected}")]
    SizeMismatch {
        name: String,
        actual: u64,
        expected: u64,
    },

    #[error("Directory index {0} is out of range")]
    DirIndexOutOfBounds(u32),

    #[error("File index {0} is out of range")]
    FileIndexOutOfBounds(usize),

    #[error("Invalid package index {0}")]
    InvalidPackageIndex(usize),

    #[error("Subtree path component '{0}' not found")]
    SubtreeNotFound(String),

    #[error("Unsafe extraction path rejected: {0}")]
    PathSafety(String),

    #[error("{0}")]
    InvalidArgument(String),

    #[error("Operation cancelled by user.")]
    Cancelled,

    #[error("Failed to extract '{rel_path}' ({size_formatted}): {source}")]
    ExtractFileFailed {
        rel_path: String,
        size_formatted: String,
        #[source]
        source: Box<NupError>,
    },

    #[error("No packable files found in '{0}'.")]
    NoPackableFiles(String),

    #[error("Source directory not found: {0}")]
    SourceDirNotFound(String),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}
