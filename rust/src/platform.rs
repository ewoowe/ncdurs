/* ncdu-rs - platform specific bits
 *
 * Everything that differs between operating systems is funnelled through this
 * module, so the rest of the code stays portable:
 *   - disk_usage(): bytes actually occupied on disk
 *   - file_id():    identity of a file, used to detect hard links
 *   - nlink():      number of hard links pointing to a file
 *
 * Unix (Linux, macOS, *BSD) uses the st_blocks / st_dev+st_ino / st_nlink
 * fields. Windows uses GetCompressedFileSizeW (the allocated size, which also
 * accounts for compressed and sparse files). A generic fallback keeps the
 * program buildable everywhere else.
 */

use std::fs::Metadata;
use std::path::Path;

/* --------------------------------- Unix -------------------------------- */

#[cfg(unix)]
mod imp {
    use super::*;
    use std::os::unix::fs::MetadataExt;

    /// st_blocks is expressed in units of 512 bytes (S_BLKSIZE).
    const BLOCK_SIZE: u64 = 512;

    pub fn disk_usage(_path: &Path, md: &Metadata) -> u64 {
        md.blocks() * BLOCK_SIZE
    }

    pub fn file_id(md: &Metadata) -> Option<(u64, u64)> {
        Some((md.dev(), md.ino()))
    }

    pub fn nlink(md: &Metadata) -> u64 {
        md.nlink()
    }
}

/* ------------------------------- Windows ------------------------------- */

#[cfg(windows)]
mod imp {
    use super::*;
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::{GetLastError, NO_ERROR};
    use windows_sys::Win32::Storage::FileSystem::GetCompressedFileSizeW;

    /// Returned on failure, unless GetLastError() says the size is genuine.
    const INVALID_FILE_SIZE: u32 = 0xFFFF_FFFF;

    pub fn disk_usage(path: &Path, md: &Metadata) -> u64 {
        let wide: Vec<u16> = path
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let mut high: u32 = 0;
        let low = unsafe { GetCompressedFileSizeW(wide.as_ptr(), &mut high) };
        if low == INVALID_FILE_SIZE {
            let err = unsafe { GetLastError() };
            if err != NO_ERROR {
                // Directories and some filesystems don't support the call;
                // fall back to the logical size (0 for directories on Windows).
                return md.len();
            }
        }
        ((high as u64) << 32) | low as u64
    }

    /// Hard link de-duplication is not implemented on Windows yet: std does not
    /// expose the file index / link count (still unstable, rust#63010), and
    /// getting them through the Win32 API requires opening a handle for every
    /// single entry, which is expensive. Returning None means "unknown", so
    /// entries are always counted - the same behaviour as `dir /s`.
    pub fn file_id(_md: &Metadata) -> Option<(u64, u64)> {
        None
    }

    pub fn nlink(_md: &Metadata) -> u64 {
        1
    }
}

/* ------------------------------ Fallback ------------------------------- */

#[cfg(not(any(unix, windows)))]
mod imp {
    use super::*;

    pub fn disk_usage(_path: &Path, md: &Metadata) -> u64 {
        md.len()
    }

    pub fn file_id(_md: &Metadata) -> Option<(u64, u64)> {
        None
    }

    pub fn nlink(_md: &Metadata) -> u64 {
        1
    }
}

pub use imp::{disk_usage, file_id, nlink};
