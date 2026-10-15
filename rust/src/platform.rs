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

    /// Identity and link count, all straight from the stat struct; the path is
    /// not needed on Unix.
    pub fn file_info(md: &Metadata, _path: &Path) -> Option<FileInfo> {
        let (dev, ino) = file_id(md)?;
        Some(FileInfo {
            dev,
            ino,
            nlink: nlink(md),
        })
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
    // file_id()/nlink() only exist for Unix now: everywhere else the work is
    // done by file_info() below.

    /// std does not expose the file index or the link count on Windows, so a
    /// handle is needed. `BY_HANDLE_FILE_INFORMATION` carries both, plus the
    /// volume serial number - the same three values the C version gets from
    /// Cygwin's POSIX layer, which is why hard links are de-duplicated there.
    pub fn file_info(_md: &Metadata, path: &Path) -> Option<FileInfo> {
        use std::os::windows::ffi::OsStrExt;

        // Declared here rather than pulled from windows-sys: CreateFileW wants
        // SECURITY_ATTRIBUTES, which would drag in the whole Win32_Security
        // feature just for a null pointer.
        #[repr(C)]
        struct HandleFileInfo {
            file_attributes: u32,
            creation_time: [u32; 2],
            last_access_time: [u32; 2],
            last_write_time: [u32; 2],
            volume_serial_number: u32,
            file_size_high: u32,
            file_size_low: u32,
            number_of_links: u32,
            file_index_high: u32,
            file_index_low: u32,
        }

        const FILE_READ_ATTRIBUTES: u32 = 0x0080;
        const FILE_SHARE_READ: u32 = 0x0000_0001;
        const FILE_SHARE_WRITE: u32 = 0x0000_0002;
        const FILE_SHARE_DELETE: u32 = 0x0000_0004;
        const OPEN_EXISTING: u32 = 3;
        // Without this, directories cannot be opened at all.
        const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
        const INVALID_HANDLE_VALUE: usize = usize::MAX;

        unsafe extern "system" {
            fn CreateFileW(
                lpfile_name: *const u16,
                desired_access: u32,
                share_mode: u32,
                security_attributes: *const std::ffi::c_void,
                creation_disposition: u32,
                flags_and_attributes: u32,
                template_file: usize,
            ) -> usize;
            fn GetFileInformationByHandle(handle: usize, info: *mut HandleFileInfo) -> i32;
            fn CloseHandle(handle: usize) -> i32;
        }

        let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        // A handle per entry is not cheap, which is why the scanner calls this
        // once per entry and reuses the result.
        unsafe {
            let handle = CreateFileW(
                wide.as_ptr(),
                FILE_READ_ATTRIBUTES,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS,
                0,
            );
            if handle == INVALID_HANDLE_VALUE {
                return None;
            }
            let mut raw: HandleFileInfo = std::mem::zeroed();
            let ok = GetFileInformationByHandle(handle, &mut raw);
            CloseHandle(handle);
            if ok == 0 {
                return None;
            }
            Some(FileInfo {
                dev: raw.volume_serial_number as u64,
                ino: ((raw.file_index_high as u64) << 32) | raw.file_index_low as u64,
                nlink: raw.number_of_links as u64,
            })
        }
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

    // file_id()/nlink() only exist for Unix now: everywhere else the work is
    // done by file_info() below.

    pub fn ext_info(_md: &Metadata) -> Option<ExtInfo> {
        None
    }

    /// The fallback has no identity to offer, so nothing is de-duplicated.
    pub fn file_info(_md: &Metadata, _path: &Path) -> Option<FileInfo> {
        None
    }
}

pub use imp::{disk_usage, ext_info, file_info};

/// Identity and link count of one file - the three things the scanner needs to
/// de-duplicate hard links.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileInfo {
    /// Device (Unix) or volume serial number (Windows).
    pub dev: u64,
    /// Inode (Unix) or file index (Windows).
    pub ino: u64,
    /// Number of hard links.
    pub nlink: u64,
}

/// True when `path` is reached through a firmlink (`--exclude-firmlinks`,
/// macOS only).
///
/// getattrlist() with ATTR_CMNEXT_NOFIRMLINKPATH hands back the path with
/// firmlinks left unresolved, so a difference means the path crosses one -
/// exactly what dir_scan.c:285-302 tests. On a stock macOS 11+ install this
/// makes /Users, /Applications, /private, ... come back as
/// /System/Volumes/Data/Users and so on.
#[cfg(target_os = "macos")]
pub fn is_firmlink(path: &Path) -> bool {
    use std::ffi::{CString, OsStr};
    use std::os::unix::ffi::OsStrExt;

    const ATTR_BIT_MAP_COUNT: u16 = 5;
    const ATTR_CMNEXT_NOFIRMLINKPATH: u32 = 0x0000_0020;

    #[repr(C)]
    struct AttrList {
        bitmapcount: u16,
        reserved: u16,
        commonattr: u32,
        volattr: u32,
        dirattr: u32,
        fileattr: u32,
        forkattr: u32,
    }

    // The kernel answers with { u_int32_t length; attrreference_t reference;
    // char extra[] }, and attr_dataoffset is relative to the reference field
    // itself - which sits 4 bytes into the buffer.
    unsafe extern "C" {
        fn getattrlist(
            path: *const libc::c_char,
            list: *mut std::ffi::c_void,
            buf: *mut std::ffi::c_void,
            size: libc::size_t,
            options: libc::c_ulong,
        ) -> libc::c_int;
    }

    let Ok(cpath) = CString::new(path.as_os_str().as_bytes()) else {
        return false;
    };
    let mut list = AttrList {
        bitmapcount: ATTR_BIT_MAP_COUNT,
        reserved: 0,
        commonattr: 0,
        volattr: 0,
        dirattr: 0,
        fileattr: 0,
        forkattr: ATTR_CMNEXT_NOFIRMLINKPATH,
    };
    let mut buf = [0u8; 4096];
    let rc = unsafe {
        getattrlist(
            cpath.as_ptr(),
            (&mut list as *mut AttrList).cast(),
            buf.as_mut_ptr().cast(),
            buf.len(),
            0,
        )
    };
    if rc != 0 {
        return false;
    }
    let offset = i32::from_ne_bytes(buf[4..8].try_into().unwrap_or([0; 4]));
    let start = 4usize.wrapping_add(offset.max(0) as usize);
    if start >= buf.len() {
        return false;
    }
    let tail = &buf[start..];
    let end = tail.iter().position(|&b| b == 0).unwrap_or(tail.len());
    let returned = Path::new(OsStr::from_bytes(&tail[..end]));
    // Compare the raw bytes, not the Paths.
    returned.as_os_str().as_bytes() != path.as_os_str().as_bytes()
}

#[cfg(not(target_os = "macos"))]
pub fn is_firmlink(_path: &Path) -> bool {
    false
}

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

/// True when the path lives on a Linux pseudo filesystem (`--exclude-kernfs`).
/// The list of magic numbers is the one from ncdu.1: binfmt, bpf, cgroup,
/// cgroup2, debug, devpts, proc, pstore, security, selinux, sys, trace.
#[cfg(target_os = "linux")]
pub fn is_kernfs(path: &Path) -> bool {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let Ok(cpath) = CString::new(path.as_os_str().as_bytes()) else {
        return false;
    };
    let mut buf: libc::statfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statfs(cpath.as_ptr(), &mut buf) } != 0 {
        return false;
    }
    matches!(
        buf.f_type as u64,
        0x9fa0        // proc
            | 0x62656572  // sysfs
            | 0x27e0eb    // cgroup
            | 0x63677270  // cgroup2
            | 0x64626720  // debugfs
            | 0x1cd1      // devpts
            | 0x42494e4d  // binfmtfs
            | 0xcafe4a11  // bpf
            | 0x6165676c  // pstore
            | 0x73636673  // securityfs
            | 0xf97cff8c  // selinuxfs
            | 0x74726163 // tracefs
    )
}

#[cfg(not(target_os = "linux"))]
pub fn is_kernfs(_path: &Path) -> bool {
    false
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
