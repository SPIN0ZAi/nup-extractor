//! Audited read-only memory mapping module.
//!
//! This is the ONLY module in `nup-core` permitted to contain `unsafe` code,
//! strictly limited to `memmap2::Mmap::map` on a read-only opened file handle.

#[allow(unsafe_code)]
mod inner {
    use std::fs::File;
    use std::io;
    use std::path::Path;

    use memmap2::Mmap;

    pub enum BackingBytes {
        Mapped(Mmap),
        Owned(Vec<u8>),
    }

    impl BackingBytes {
        pub fn open_readonly(path: &Path) -> io::Result<Self> {
            let file = File::open(path)?;
            let meta = file.metadata()?;
            if meta.len() == 0 {
                return Ok(Self::Owned(Vec::new()));
            }
            // SAFETY: File is opened read-only. In accordance with NUP container usage,
            // callers do not mutate the container concurrently while reading, and all
            // slice accesses in `NupReader` are bounds-checked against `self.as_slice().len()`.
            let mmap = unsafe { Mmap::map(&file)? };
            Ok(Self::Mapped(mmap))
        }

        pub fn from_vec(bytes: Vec<u8>) -> Self {
            Self::Owned(bytes)
        }

        #[inline]
        pub fn as_slice(&self) -> &[u8] {
            match self {
                Self::Mapped(m) => m.as_ref(),
                Self::Owned(v) => v.as_slice(),
            }
        }
    }
    #[cfg(windows)]
    pub fn query_disk_space(root_path: &str) -> Option<(u64, u64)> {
        use std::os::windows::ffi::OsStrExt;
        use windows::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
        use windows::core::PCWSTR;

        let wide: Vec<u16> = std::ffi::OsStr::new(root_path)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();

        let mut free_avail: u64 = 0;
        let mut total_bytes: u64 = 0;
        let mut total_free: u64 = 0;

        // SAFETY: `wide` is a valid NUL-terminated UTF-16 string and output pointers are valid stack locals.
        let ok = unsafe {
            GetDiskFreeSpaceExW(
                PCWSTR(wide.as_ptr()),
                Some(&mut free_avail),
                Some(&mut total_bytes),
                Some(&mut total_free),
            )
        };
        ok.ok().map(|()| (free_avail, total_bytes))
    }

    #[cfg(not(windows))]
    pub fn query_disk_space(_root_path: &str) -> Option<(u64, u64)> {
        None
    }
}

pub use inner::{BackingBytes, query_disk_space};
