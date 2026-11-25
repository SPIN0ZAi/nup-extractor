#![deny(unsafe_code)]

pub mod disk;
pub mod engine;
pub mod error;
pub mod format;
pub mod mmap;
pub mod path_safety;
pub mod reader;
pub mod writer;

pub use disk::{DiskSpaceInfo, check_disk_space};
pub use engine::{
    ProgressInfo, VerifyProgressInfo, extract, extract_single_file, format_bytes, get_dir_children,
    inspect, search_files, verify,
};
pub use error::{NupError, NupResult};
pub use format::{NupConstants, NupDirNode, NupFileNode, NupPackage, NupSuperblock, get_kind_name};
pub use reader::NupReader;
pub use writer::{PackOptions, PackProgressInfo, detect_package_summary, pack};
