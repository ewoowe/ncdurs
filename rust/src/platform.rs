/* ncdurs - platform specific bits
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

    /// The `-e` fields: they all come straight from the stat struct.
    pub fn ext_info(md: &Metadata) -> Option<ExtInfo> {
        Some(ExtInfo {
            mode: md.mode(),
            uid: md.uid(),
            gid: md.gid(),
            mtime: md.mtime(),
        })
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

    /// Windows has no uid/gid/mode; the C version does not show them either.
    pub fn ext_info(_md: &Metadata) -> Option<ExtInfo> {
        None
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

    pub fn ext_info(_md: &Metadata) -> Option<ExtInfo> {
        None
    }
}

pub use imp::{disk_usage, ext_info, file_id, nlink};

/// Enables the user's locale, like the C version does before drawing anything.
/// Without this the thousands separator below would always be the C locale's
/// empty string and the info window would not match ncdu's output.
#[cfg(unix)]
pub fn init_locale() {
    unsafe {
        libc::setlocale(libc::LC_ALL, c"".as_ptr());
    }
}

#[cfg(not(unix))]
pub fn init_locale() {}

/// The thousands separator of the active locale (thou_sep in src/util.c).
/// Falls back to '.' when the locale does not define one, which is also what
/// the C version's default is.
#[cfg(unix)]
pub fn thousands_sep() -> char {
    unsafe {
        let lc = libc::localeconv();
        if lc.is_null() || (*lc).thousands_sep.is_null() {
            return '.';
        }
        let byte = *(*lc).thousands_sep as u8;
        if byte == 0 { '.' } else { byte as char }
    }
}

#[cfg(not(unix))]
pub fn thousands_sep() -> char {
    ','
}

/// Formats an mtime exactly like the C version: localtime_r() plus
/// strftime("%Y-%m-%d %H:%M:%S %z"). Going through libc keeps the output
/// character-for-character identical, timezone offset included.
#[cfg(unix)]
pub fn format_mtime(mtime: i64) -> String {
    use std::ffi::CStr;
    unsafe {
        let t = mtime as libc::time_t;
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&t, &mut tm).is_null() {
            return String::new();
        }
        let mut buf = [0 as libc::c_char; 64];
        let fmt = b"%Y-%m-%d %H:%M:%S %z\0";
        let n = libc::strftime(buf.as_mut_ptr(), buf.len(), fmt.as_ptr().cast(), &tm);
        if n == 0 {
            return String::new();
        }
        CStr::from_ptr(buf.as_ptr()).to_string_lossy().into_owned()
    }
}

#[cfg(not(unix))]
pub fn format_mtime(_mtime: i64) -> String {
    String::new()
}

/// fmtmode(): the `drwxr-xr-x` string, with setuid/setgid/sticky shown as
/// s/S/t/T like ls does.
pub fn fmtmode(mode: u32) -> String {
    let kind = match mode & 0o170000 {
        0o040000 => 'd',
        0o120000 => 'l',
        0o060000 => 'b',
        0o020000 => 'c',
        0o010000 => 'p',
        0o140000 => 's',
        _ => '-',
    };
    let bits = [
        (0o400, 'r'),
        (0o200, 'w'),
        (0o100, 'x'),
        (0o040, 'r'),
        (0o020, 'w'),
        (0o010, 'x'),
        (0o004, 'r'),
        (0o002, 'w'),
        (0o001, 'x'),
    ];
    let mut out = String::with_capacity(10);
    out.push(kind);
    for (i, (bit, ch)) in bits.iter().enumerate() {
        let set = mode & bit != 0;
        let special = match i {
            2 => mode & 0o4000 != 0,
            5 => mode & 0o2000 != 0,
            8 => mode & 0o1000 != 0,
            _ => false,
        };
        out.push(match (set, special) {
            (true, true) => {
                if i == 8 {
                    't'
                } else {
                    's'
                }
            }
            (false, true) => {
                if i == 8 {
                    'T'
                } else {
                    'S'
                }
            }
            (true, false) => *ch,
            (false, false) => '-',
        });
    }
    out
}

/// The extended information collected for `-e` (the C version's dir_ext).
/// Unix fills all four fields; platforms without the concept return None, and
/// then neither the mtime column nor the extra info-window lines appear.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ExtInfo {
    /// Full st_mode, like the C version exports it (e.g. 0o100644).
    pub mode: u32,
    pub uid: u32,
    pub gid: u32,
    /// Seconds since the epoch, as in `st_mtime`.
    pub mtime: i64,
}
