//! Win32 queries that do not open a volume for data.
//!
//! The file is opened with `FILE_FLAG_OPEN_REPARSE_POINT` and not read.
//! `GetFinalPathNameByHandleW` then returns the normalized name of that handle.
//! `QueryDosDeviceW` and `GetDriveTypeW` read the DOS device map only.

#![allow(unsafe_code)]

use std::ffi::OsString;
use std::fs;
use std::io;
#[cfg(test)]
use std::os::windows::ffi::OsStrExt;
use std::os::windows::ffi::OsStringExt;
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::AsRawHandle;
use std::path::{Path, PathBuf};

#[cfg(test)]
use windows::Win32::Storage::FileSystem::{
    DDD_RAW_TARGET_PATH, DDD_REMOVE_DEFINITION, DefineDosDeviceW, GetShortPathNameW,
};
use windows::{
    Win32::{
        Foundation::{ERROR_FILE_NOT_FOUND, GetLastError, HANDLE},
        Storage::FileSystem::{
            FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
            FILE_NAME_NORMALIZED, FILE_READ_ATTRIBUTES, FindClose, FindFirstFileW, GetDriveTypeW,
            GetFinalPathNameByHandleW, QueryDosDeviceW, WIN32_FIND_DATAW,
        },
    },
    core::PCWSTR,
};

/// What `QueryDosDeviceW` reported for one drive letter.
pub(super) enum DosQuery {
    /// The letter is not a DOS device.
    Missing,
    /// The device's target string (not opened).
    Target(String),
    /// The query failed. Callers must fail closed.
    Failed,
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(test)]
fn wide_path(path: &Path) -> Vec<u16> {
    path.as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

fn from_wide(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}

/// DOS device target for `X:`. Does not open the volume.
pub(super) fn query_dos_device(name: &str) -> DosQuery {
    let wide_name = wide(name);
    let mut buf = vec![0u16; 4096];
    // SAFETY: `wide_name` is NUL-terminated and lives for the call. `buf` is a
    // writable buffer whose length is the capacity the binding passes through.
    let n = unsafe { QueryDosDeviceW(PCWSTR(wide_name.as_ptr()), Some(buf.as_mut_slice())) };
    if n == 0 {
        // SAFETY: read the error set by the failed `QueryDosDeviceW` above, before
        // any other Win32 call.
        let err = unsafe { GetLastError() };
        return if err == ERROR_FILE_NOT_FOUND {
            DosQuery::Missing
        } else {
            DosQuery::Failed
        };
    }
    DosQuery::Target(from_wide(&buf))
}

/// `GetDriveTypeW` code for a root such as `C:\`. Does not open the volume.
pub(super) fn drive_type(root: &str) -> u32 {
    let wide_root = wide(root);
    // SAFETY: `wide_root` is a NUL-terminated root path. The call only classifies
    // the drive letter; it does not open a file on the volume.
    unsafe { GetDriveTypeW(PCWSTR(wide_root.as_ptr())) }
}

/// Normalized DOS path of `path`, without following a final reparse point.
///
/// The handle is opened with `FILE_READ_ATTRIBUTES` and
/// `FILE_FLAG_OPEN_REPARSE_POINT`, so file bytes are not read and a junction
/// is not traversed.
pub(super) fn final_path_nofollow(path: &Path) -> io::Result<PathBuf> {
    // `FILE_FLAG_OPEN_REPARSE_POINT` names the link itself. Bytes are not read;
    // only `GetFinalPathNameByHandleW` runs on this handle.
    let file = fs::OpenOptions::new()
        .access_mode(FILE_READ_ATTRIBUTES.0)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS.0 | FILE_FLAG_OPEN_REPARSE_POINT.0)
        .open(path)?;
    let handle = HANDLE(file.as_raw_handle());
    let mut buf = vec![0u16; 512];
    loop {
        // SAFETY: `handle` is the open file from `file`, which stays alive for
        // this loop. `buf` is writable and its length is what the binding
        // passes through. `FILE_NAME_NORMALIZED` returns the long `\\?\` DOS path.
        let n = unsafe { GetFinalPathNameByHandleW(handle, &mut buf, FILE_NAME_NORMALIZED) };
        if n == 0 {
            return Err(io::Error::last_os_error());
        }
        let n = usize::try_from(n).unwrap_or(usize::MAX);
        if n < buf.len() {
            return Ok(PathBuf::from(OsString::from_wide(&buf[..n])));
        }
        buf.resize(n.saturating_add(1), 0);
    }
}

fn wide_find_path(path: &Path) -> Vec<u16> {
    let text = path.to_string_lossy().replace('/', "\\");
    let prefixed = if text.starts_with(r"\\?\") {
        text
    } else if let Some(rest) = text.strip_prefix(r"\\") {
        format!(r"\\?\UNC\{rest}")
    } else {
        format!(r"\\?\{text}")
    };
    wide(&prefixed)
}

/// Reparse tag from `FindFirstFileW`, or `None` when the path is not a reparse point.
///
/// The call does not open the file for data, so a cloud placeholder is not hydrated.
pub(super) fn reparse_tag(path: &Path) -> io::Result<Option<u32>> {
    let wide_name = wide_find_path(path);
    let mut data = WIN32_FIND_DATAW::default();
    // SAFETY: `wide_name` is NUL-terminated and lives for the call. `data` is a
    // writable `WIN32_FIND_DATAW`. `FindFirstFileW` returns metadata only.
    let handle = unsafe { FindFirstFileW(PCWSTR(wide_name.as_ptr()), &raw mut data) }
        .map_err(|_| io::Error::last_os_error())?;
    // SAFETY: `handle` is the successful find handle from the call above.
    unsafe { FindClose(handle) }.map_err(|_| io::Error::last_os_error())?;
    if data.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0 {
        Ok(Some(data.dwReserved0))
    } else {
        Ok(None)
    }
}

/// 8.3 path, used by tests to build the short name the denylist must still see.
#[cfg(test)]
pub(super) fn short_path(path: &Path) -> io::Result<PathBuf> {
    let wide_name = wide_path(path);
    let mut buf = vec![0u16; 1024];
    // SAFETY: `wide_name` is NUL-terminated and `buf` is a writable buffer.
    let n = unsafe { GetShortPathNameW(PCWSTR(wide_name.as_ptr()), Some(buf.as_mut_slice())) };
    if n == 0 {
        return Err(io::Error::last_os_error());
    }
    let n = usize::try_from(n).unwrap_or(usize::MAX);
    if n >= buf.len() {
        return Err(io::Error::other("short path exceeds the buffer"));
    }
    Ok(PathBuf::from(OsString::from_wide(&buf[..n])))
}

/// Install or remove a DOS device definition. Tests use this to simulate a
/// mapped letter without contacting the network.
#[cfg(test)]
pub(super) fn define_dos_device(letter: char, target: &str, remove: bool) -> io::Result<()> {
    let mut flags = DDD_RAW_TARGET_PATH;
    if remove {
        flags |= DDD_REMOVE_DEFINITION;
    }
    let name = wide(&format!("{letter}:"));
    let target = wide(target);
    // SAFETY: both strings are NUL-terminated. This edits the process DOS device
    // map only; it does not open `target`.
    unsafe { DefineDosDeviceW(flags, PCWSTR(name.as_ptr()), PCWSTR(target.as_ptr())) }
        .map_err(|_| io::Error::last_os_error())
}

/// Point an empty directory at `\\192.0.2.1\share` without asking `mklink`.
/// Junctions created through `mklink /J` cannot target a UNC path ("local
/// volumes are required"). The mount-point reparse buffer can.
#[cfg(test)]
pub(super) fn mount_unc(link: &Path) -> io::Result<()> {
    fs::create_dir(link)?;
    let file = fs::OpenOptions::new()
        .write(true)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS.0 | FILE_FLAG_OPEN_REPARSE_POINT.0)
        .open(link)?;
    let buf = mount_point_buffer(r"\??\UNC\192.0.2.1\share", r"\\192.0.2.1\share");
    let mut returned = 0u32;
    // SAFETY: `file` is a directory this process just created, opened for write
    // with backup and reparse flags. `buf` is a complete
    // `IO_REPARSE_TAG_MOUNT_POINT` buffer. The control code writes that buffer
    // onto the directory; it does not open the UNC target.
    let ok = unsafe {
        DeviceIoControl(
            file.as_raw_handle(),
            FSCTL_SET_REPARSE_POINT,
            buf.as_ptr(),
            u32::try_from(buf.len()).unwrap_or(u32::MAX),
            std::ptr::null_mut(),
            0,
            &raw mut returned,
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(test)]
const FSCTL_SET_REPARSE_POINT: u32 = 0x0009_00A4;

#[cfg(test)]
#[link(name = "kernel32")]
unsafe extern "system" {
    fn DeviceIoControl(
        hdevice: *mut core::ffi::c_void,
        dw_io_control_code: u32,
        lp_in_buffer: *const u8,
        n_in_buffer_size: u32,
        lp_out_buffer: *mut u8,
        n_out_buffer_size: u32,
        lp_bytes_returned: *mut u32,
        lp_overlapped: *mut core::ffi::c_void,
    ) -> i32;
}

#[cfg(test)]
fn mount_point_buffer(substitute: &str, print_name: &str) -> Vec<u8> {
    const IO_REPARSE_TAG_MOUNT_POINT: u32 = 0xA000_0003;
    let sub: Vec<u8> = substitute
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect();
    let print: Vec<u8> = print_name
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect();
    let print_off = u16::try_from(sub.len() + 2).unwrap_or(u16::MAX);
    let mut mount = Vec::new();
    mount.extend_from_slice(&0u16.to_le_bytes());
    mount.extend_from_slice(&u16::try_from(sub.len()).unwrap_or(u16::MAX).to_le_bytes());
    mount.extend_from_slice(&print_off.to_le_bytes());
    mount.extend_from_slice(&u16::try_from(print.len()).unwrap_or(u16::MAX).to_le_bytes());
    mount.extend_from_slice(&sub);
    mount.extend_from_slice(&0u16.to_le_bytes());
    mount.extend_from_slice(&print);
    mount.extend_from_slice(&0u16.to_le_bytes());
    let mut buf = Vec::new();
    buf.extend_from_slice(&IO_REPARSE_TAG_MOUNT_POINT.to_le_bytes());
    buf.extend_from_slice(&u16::try_from(mount.len()).unwrap_or(u16::MAX).to_le_bytes());
    buf.extend_from_slice(&0u16.to_le_bytes());
    buf.extend_from_slice(&mount);
    buf
}

/// Mark `path` with `IO_REPARSE_TAG_CLOUD` so `read_link` fails the way an
/// online-only `OneDrive` placeholder does. The buffer is not a symlink.
#[cfg(test)]
pub(super) fn mark_cloud_placeholder(path: &Path) -> io::Result<()> {
    // Generic reparse buffer: tag, data length, reserved, then 4 data bytes.
    const IO_REPARSE_TAG_CLOUD: u32 = 0x9000_001A;
    let file = fs::OpenOptions::new()
        .write(true)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS.0 | FILE_FLAG_OPEN_REPARSE_POINT.0)
        .open(path)?;
    let mut buf = Vec::new();
    buf.extend_from_slice(&IO_REPARSE_TAG_CLOUD.to_le_bytes());
    buf.extend_from_slice(&4u16.to_le_bytes());
    buf.extend_from_slice(&0u16.to_le_bytes());
    buf.extend_from_slice(&0u32.to_le_bytes());
    let mut returned = 0u32;
    // SAFETY: `file` is a file this process created, opened for write with
    // backup and reparse flags. `buf` is an `IO_REPARSE_TAG_CLOUD` buffer.
    // The control code writes that buffer onto the file; it does not open a
    // network path.
    let ok = unsafe {
        DeviceIoControl(
            file.as_raw_handle(),
            FSCTL_SET_REPARSE_POINT,
            buf.as_ptr(),
            u32::try_from(buf.len()).unwrap_or(u32::MAX),
            std::ptr::null_mut(),
            0,
            &raw mut returned,
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// A drive letter that is neither a DOS device nor a mounted root.
#[cfg(test)]
pub(super) fn unused_drive_letter() -> Option<char> {
    ('A'..='Z').rev().find(|letter| {
        let name = format!("{letter}:");
        matches!(query_dos_device(&name), DosQuery::Missing)
            && drive_type(&format!(r"{name}\")) == 1
    })
}
