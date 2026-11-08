//! Checked Win32 security for the identity file, always using the opened handle.

use std::ffi::c_void;
use std::mem::size_of;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};

use windows::Win32::Foundation::{HANDLE, HLOCAL, LocalFree, WIN32_ERROR};
use windows::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, GetSecurityInfo,
    SDDL_REVISION_1, SE_FILE_OBJECT,
};
use windows::Win32::Security::{
    ACCESS_ALLOWED_ACE, ACE_HEADER, ACL, DACL_SECURITY_INFORMATION, EqualSid, GetAce,
    GetSecurityDescriptorControl, GetTokenInformation, IsValidAcl, IsValidSid,
    OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID, SE_DACL_PROTECTED, SECURITY_ATTRIBUTES,
    SECURITY_MAX_SID_SIZE, TOKEN_QUERY, TOKEN_USER, TokenUser,
};
use windows::Win32::Storage::FileSystem::{
    BY_HANDLE_FILE_INFORMATION, CREATE_NEW, CreateFileW, FILE_ALL_ACCESS, FILE_ATTRIBUTE_DIRECTORY,
    FILE_ATTRIBUTE_NORMAL, FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_OPEN_REPARSE_POINT,
    FILE_GENERIC_READ, FILE_GENERIC_WRITE, FILE_SHARE_MODE, GetFileInformationByHandle,
    READ_CONTROL,
};
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use windows::core::{PCWSTR, PWSTR};

use super::{File, Path, Result, io};

fn win_io(error: &windows::core::Error) -> std::io::Error {
    // windows::core's blanket io conversion preserves the HRESULT. Recover the
    // Win32 code so a missing identity remains ErrorKind::NotFound for creation.
    WIN32_ERROR::from_error(error)
        .and_then(|code| i32::try_from(code.0).ok())
        .map_or_else(
            || std::io::Error::other(error.to_string()),
            std::io::Error::from_raw_os_error,
        )
}

struct LocalMemory(*mut c_void);

impl Drop for LocalMemory {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: only LocalAlloc-owned security descriptors/SID strings are
            // wrapped here; each wrapper has sole ownership of this allocation.
            unsafe {
                let _ = LocalFree(Some(HLOCAL(self.0)));
            }
        }
    }
}

struct CurrentUser {
    // The buffer must be aligned for TOKEN_USER and heap-backed: the SID pointer
    // returned by GetTokenInformation points into it, even when this struct moves.
    token_user: Vec<usize>,
}

impl CurrentUser {
    fn load() -> std::io::Result<Self> {
        let mut token = HANDLE::default();
        // SAFETY: token is a valid out pointer; GetCurrentProcess is a borrowed
        // pseudo handle. Success transfers the real token handle to OwnedHandle.
        unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &raw mut token) }
            .map_err(|error| win_io(&error))?;
        let token = unsafe { OwnedHandle::from_raw_handle(token.0) };
        let size = size_of::<TOKEN_USER>() + usize::try_from(SECURITY_MAX_SID_SIZE).unwrap();
        let mut token_user = vec![0usize; size.div_ceil(size_of::<usize>())];
        let mut returned_bytes = 0;
        // SAFETY: the buffer is aligned and large enough for TOKEN_USER plus the
        // largest Windows SID; token remains owned and open throughout this call.
        unsafe {
            GetTokenInformation(
                HANDLE(token.as_raw_handle()),
                TokenUser,
                Some(token_user.as_mut_ptr().cast()),
                u32::try_from(token_user.len() * size_of::<usize>()).unwrap(),
                &raw mut returned_bytes,
            )
        }
        .map_err(|error| win_io(&error))?;
        if usize::try_from(returned_bytes).unwrap() < size_of::<TOKEN_USER>() {
            return Err(std::io::Error::other("invalid current-user token data"));
        }
        let user = Self { token_user };
        // SAFETY: GetTokenInformation initialized the TOKEN_USER and its SID.
        if !unsafe { IsValidSid(user.sid()) }.as_bool() {
            return Err(std::io::Error::other("invalid current-user SID"));
        }
        Ok(user)
    }

    fn sid(&self) -> PSID {
        // SAFETY: TOKEN_USER is aligned, initialized, and its allocation is alive.
        unsafe { (*self.token_user.as_ptr().cast::<TOKEN_USER>()).User.Sid }
    }

    fn string_sid(&self) -> std::io::Result<String> {
        let mut sid = PWSTR::null();
        // SAFETY: the SID is valid while self lives; sid is a valid out pointer.
        unsafe { ConvertSidToStringSidW(self.sid(), &raw mut sid) }
            .map_err(|error| win_io(&error))?;
        let _allocation = LocalMemory(sid.0.cast());
        // SAFETY: on success Windows returned a terminated UTF-16 SID string.
        unsafe { sid.to_string() }.map_err(std::io::Error::other)
    }
}

fn descriptor(sddl: &str) -> std::io::Result<LocalMemory> {
    let mut wide: Vec<u16> = sddl.encode_utf16().collect();
    if wide.contains(&0) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "NUL in security descriptor",
        ));
    }
    wide.push(0);
    let mut descriptor = PSECURITY_DESCRIPTOR::default();
    // SAFETY: wide is a terminated string and descriptor is a valid out pointer.
    unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            PCWSTR(wide.as_ptr()),
            SDDL_REVISION_1,
            &raw mut descriptor,
            None,
        )
    }
    .map_err(|error| win_io(&error))?;
    Ok(LocalMemory(descriptor.0))
}

pub(super) fn open_existing(path: &Path) -> std::io::Result<File> {
    // No sharing prevents a concurrent handle from modifying the file/ACL while
    // it is verified and read. OPEN_REPARSE_POINT opens a link itself, not its
    // target; verify_private rejects that handle before reading any key bytes.
    std::fs::OpenOptions::new()
        .access_mode(FILE_GENERIC_READ.0 | READ_CONTROL.0)
        .share_mode(0)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT.0)
        .open(path)
}

pub(super) fn create_private(path: &Path) -> std::io::Result<File> {
    create_with_security(path, || {
        let sid = CurrentUser::load()?.string_sid()?;
        descriptor(&format!("O:{sid}D:P(A;;FA;;;{sid})"))
    })
}

fn create_with_security(
    path: &Path,
    security: impl FnOnce() -> std::io::Result<LocalMemory>,
) -> std::io::Result<File> {
    // Any owner/ACL construction failure is returned before CreateFileW. There
    // is no interval in which a file containing the key has an inherited DACL.
    let security = security()?;
    let attributes = SECURITY_ATTRIBUTES {
        nLength: u32::try_from(size_of::<SECURITY_ATTRIBUTES>()).unwrap(),
        lpSecurityDescriptor: security.0,
        bInheritHandle: false.into(),
    };
    let mut wide: Vec<u16> = path.as_os_str().encode_wide().collect();
    if wide.contains(&0) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "NUL in identity path",
        ));
    }
    wide.push(0);
    // SAFETY: the path and SECURITY_ATTRIBUTES point to live, correctly
    // initialized buffers for this call. CREATE_NEW never replaces an existing
    // file; the returned handle is immediately transferred to File ownership.
    let handle = unsafe {
        CreateFileW(
            PCWSTR(wide.as_ptr()),
            FILE_GENERIC_READ.0 | FILE_GENERIC_WRITE.0 | READ_CONTROL.0,
            FILE_SHARE_MODE(0),
            Some(&raw const attributes),
            CREATE_NEW,
            FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OPEN_REPARSE_POINT,
            None,
        )
    }
    .map_err(|error| win_io(&error))?;
    Ok(unsafe { File::from_raw_handle(handle.0) })
}

pub(super) fn verify_private(file: &File) -> Result<()> {
    let user = CurrentUser::load().map_err(|e| io(&e))?;
    verify_owner_and_acl(file, user.sid()).map_err(|e| io(&e))
}

fn verify_owner_and_acl(file: &File, user: PSID) -> std::io::Result<()> {
    let handle = HANDLE(file.as_raw_handle());
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    // SAFETY: file owns a live disk handle and info is a valid out pointer.
    unsafe { GetFileInformationByHandle(handle, &raw mut info) }.map_err(|error| win_io(&error))?;
    if info.dwFileAttributes & (FILE_ATTRIBUTE_REPARSE_POINT | FILE_ATTRIBUTE_DIRECTORY).0 != 0
        || info.nNumberOfLinks != 1
    {
        return Err(std::io::Error::other(
            "identity key must be a regular file with one link",
        ));
    }
    let mut owner = PSID::default();
    let mut dacl: *mut ACL = std::ptr::null_mut();
    let mut security = PSECURITY_DESCRIPTOR::default();
    // SAFETY: all out pointers are valid. The returned owner/ACL pointers remain
    // valid until the LocalAlloc security descriptor is freed at scope exit.
    unsafe {
        GetSecurityInfo(
            handle,
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            Some(&raw mut owner),
            None,
            Some(&raw mut dacl),
            None,
            Some(&raw mut security),
        )
    }
    .ok()
    .map_err(|error| win_io(&error))?;
    let _allocation = LocalMemory(security.0);
    let mut control = 0;
    let mut revision = 0;
    // SAFETY: the OS initialized this security descriptor and owner pointer.
    unsafe { GetSecurityDescriptorControl(security, &raw mut control, &raw mut revision) }
        .map_err(|error| win_io(&error))?;
    if owner.0.is_null()
        || dacl.is_null()
        || control & SE_DACL_PROTECTED.0 == 0
        || unsafe { EqualSid(owner, user) }.is_err()
        || !unsafe { IsValidAcl(dacl) }.as_bool()
    {
        return Err(std::io::Error::other(
            "identity owner or protected DACL is unsafe",
        ));
    }
    // Strictly require one explicit full-control allow ACE for the current SID.
    // No inherited ACE, Everyone, group, SYSTEM, administrator or unknown ACE is
    // silently accepted. An unsafe legacy file must be repaired by its owner.
    if unsafe { (*dacl).AceCount } != 1 {
        return Err(std::io::Error::other(
            "identity DACL must grant only the current user",
        ));
    }
    let mut ace = std::ptr::null_mut();
    // SAFETY: the OS ACL is valid and has exactly one ACE at index zero.
    unsafe { GetAce(dacl, 0, &raw mut ace) }.map_err(|error| win_io(&error))?;
    let header = unsafe { &*ace.cast::<ACE_HEADER>() };
    // ACCESS_ALLOWED_ACE_TYPE is 0 in the Windows ACL format. IsValidAcl above
    // validates ACE sizes; checking the type precedes interpreting its SID.
    if header.AceType != 0
        || header.AceFlags != 0
        || usize::from(header.AceSize) < size_of::<ACCESS_ALLOWED_ACE>()
    {
        return Err(std::io::Error::other(
            "identity DACL contains an unsafe ACE",
        ));
    }
    // SAFETY: type and size above establish the full ACCESS_ALLOWED_ACE layout.
    let allowed = unsafe { &*ace.cast::<ACCESS_ALLOWED_ACE>() };
    if allowed.Mask != FILE_ALL_ACCESS.0 {
        return Err(std::io::Error::other(
            "identity DACL must grant the owner full control",
        ));
    }
    let sid = PSID((&raw const allowed.SidStart).cast_mut().cast());
    // SAFETY: the validated ACCESS_ALLOWED_ACE embeds its SID at SidStart.
    if !unsafe { IsValidSid(sid) }.as_bool() || unsafe { EqualSid(sid, user) }.is_err() {
        return Err(std::io::Error::other(
            "identity DACL grants another principal",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::super::load_or_create_secret_key;
    use super::*;

    #[test]
    fn acl_setup_failure_propagates_without_creating_a_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("identity.key");
        // Exercise a real failing Win32 ACL construction, through the same gate
        // as normal creation. No secret or permissively-created file may remain.
        let error = create_with_security(&path, || descriptor("invalid SDDL")).unwrap_err();
        assert_ne!(error.kind(), std::io::ErrorKind::NotFound);
        assert!(!path.exists());
    }

    #[test]
    fn broad_existing_acl_is_rejected_without_rewriting() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("identity.key");
        let sid = CurrentUser::load().unwrap().string_sid().unwrap();
        let mut file =
            create_with_security(&path, || descriptor(&format!("O:{sid}D:P(A;;FA;;;WD)"))).unwrap();
        file.write_all(b"0000000000000000000000000000000000000000000000000000000000000000")
            .unwrap();
        drop(file); // share=0: close before reopening, including in tests.
        assert!(load_or_create_secret_key(&path).is_err());
        assert_eq!(std::fs::read(&path).unwrap().len(), 64);
    }

    #[test]
    fn existing_unprotected_acl_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("identity.key");
        let sid = CurrentUser::load().unwrap().string_sid().unwrap();
        let file = create_with_security(&path, || descriptor(&format!("O:{sid}D:(A;;FA;;;{sid})")))
            .unwrap();
        assert!(verify_private(&file).is_err());
    }

    #[test]
    fn creation_never_replaces_an_open_or_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("identity.key");
        let file = create_private(&path).unwrap();
        assert!(create_private(&path).is_err());
        assert!(open_existing(&path).is_err());
        drop(file);
        assert!(create_private(&path).is_err());
        assert!(open_existing(&path).is_ok());
    }
}
