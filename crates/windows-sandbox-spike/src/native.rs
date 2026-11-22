//! Native boundary; no reference implementation code is copied here.
#![allow(unsafe_code)]

use std::{ffi::c_void, io, mem::size_of, os::windows::ffi::OsStrExt, path::Path};
use windows::{
    Win32::{
        Foundation::{CloseHandle, HANDLE, HLOCAL, LocalFree, WAIT_OBJECT_0, WAIT_TIMEOUT},
        Security::{
            Authorization::{
                ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
            },
            CreateRestrictedToken, CreateWellKnownSid, DISABLE_MAX_PRIVILEGE, FreeSid,
            GetTokenInformation,
            Isolation::{CreateAppContainerProfile, DeleteAppContainerProfile},
            LUA_TOKEN, PSECURITY_DESCRIPTOR, PSID, SECURITY_ATTRIBUTES, SECURITY_CAPABILITIES,
            SID_AND_ATTRIBUTES, TOKEN_DUPLICATE, TOKEN_QUERY, TOKEN_USER, TokenUser,
            WinCapabilityInternetClientSid,
        },
        Storage::FileSystem::{CreateDirectoryW, GetDriveTypeW},
        System::{
            JobObjects::{
                AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_JOB_MEMORY,
                JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
                JobObjectExtendedLimitInformation, SetInformationJobObject,
            },
            SystemInformation::GetSystemDirectoryW,
            Threading::{
                CREATE_NO_WINDOW, CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT,
                CreateProcessAsUserW, DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT,
                GetCurrentProcess, GetExitCodeProcess, InitializeProcThreadAttributeList,
                LPPROC_THREAD_ATTRIBUTE_LIST, OpenProcessToken,
                PROC_THREAD_ATTRIBUTE_ALL_APPLICATION_PACKAGES_POLICY,
                PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES, PROCESS_INFORMATION, ResumeThread,
                STARTUPINFOEXW, TerminateProcess, UpdateProcThreadAttribute, WaitForSingleObject,
            },
        },
    },
    core::{PCWSTR, PWSTR},
};

use crate::SandboxConfig;

struct Handle(HANDLE);
impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: this wrapper exclusively owns a real handle returned by Windows.
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

struct LocalAllocation(*mut c_void);
impl Drop for LocalAllocation {
    fn drop(&mut self) {
        // SAFETY: only LocalAlloc-owned API results enter this wrapper.
        unsafe {
            let _ = LocalFree(Some(HLOCAL(self.0)));
        }
    }
}

struct Profile {
    name: Vec<u16>,
    sid: PSID,
    deleted: bool,
}
impl Profile {
    fn create() -> io::Result<Self> {
        let name = wide(format!("PraxiSpike.{}", uuid::Uuid::now_v7()));
        // SAFETY: all strings are terminated, no capabilities requested at setup.
        let sid = unsafe {
            CreateAppContainerProfile(
                PCWSTR(name.as_ptr()),
                PCWSTR(name.as_ptr()),
                PCWSTR(name.as_ptr()),
                None,
            )
        }
        .map_err(|e| io::Error::other(format!("Could not create the sandbox profile: {e}")))?;
        Ok(Self {
            name,
            sid,
            deleted: false,
        })
    }

    fn delete(&mut self) -> io::Result<()> {
        // SAFETY: this random profile belongs to this invocation only.
        unsafe { DeleteAppContainerProfile(PCWSTR(self.name.as_ptr())) }
            .map_err(io::Error::other)?;
        self.deleted = true;
        Ok(())
    }
}
impl Drop for Profile {
    fn drop(&mut self) {
        if !self.deleted {
            let _ = self.delete();
        }
        // SAFETY: CreateAppContainerProfile returns a SID to release with FreeSid.
        unsafe {
            let _ = FreeSid(self.sid);
        }
    }
}

struct Attributes {
    // usize storage provides native pointer alignment; Vec<u8> would not promise it.
    _storage: Vec<usize>,
    list: LPPROC_THREAD_ATTRIBUTE_LIST,
}
impl Attributes {
    fn new() -> io::Result<Self> {
        let mut bytes = 0;
        // SAFETY: documented size query with null list.
        unsafe {
            let _ = InitializeProcThreadAttributeList(None, 2, None, &raw mut bytes);
        }
        if bytes == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut storage = vec![0usize; bytes.div_ceil(size_of::<usize>())];
        let list = LPPROC_THREAD_ATTRIBUTE_LIST(storage.as_mut_ptr().cast());
        // SAFETY: storage has the queried size and correct alignment, and outlives list.
        unsafe { InitializeProcThreadAttributeList(Some(list), 2, None, &raw mut bytes) }
            .map_err(io::Error::other)?;
        Ok(Self {
            _storage: storage,
            list,
        })
    }

    fn set<T>(&mut self, attribute: u32, value: &T) -> io::Result<()> {
        // SAFETY: callers keep the typed values alive until process creation completes.
        unsafe {
            UpdateProcThreadAttribute(
                self.list,
                0,
                attribute as usize,
                Some(std::ptr::from_ref(value).cast()),
                size_of::<T>(),
                None,
                None,
            )
        }
        .map_err(io::Error::other)
    }
}
impl Drop for Attributes {
    fn drop(&mut self) {
        // SAFETY: initialized list, freed before its backing storage.
        unsafe {
            DeleteProcThreadAttributeList(self.list);
        }
    }
}

// A suspended child must die even if job assignment/resume fails.
struct Child {
    process: Handle,
    thread: Handle,
}
impl Drop for Child {
    fn drop(&mut self) {
        // SAFETY: owned process handle; terminating an already exited child is harmless.
        unsafe {
            let _ = TerminateProcess(self.process.0, 1);
            let _ = WaitForSingleObject(self.process.0, 5_000);
        }
    }
}

fn wide(value: impl AsRef<std::ffi::OsStr>) -> Vec<u16> {
    value.as_ref().encode_wide().chain(Some(0)).collect()
}

fn sid_string(sid: PSID) -> io::Result<String> {
    let mut text = PWSTR::null();
    // SAFETY: live SID, writable pointer; conversion allocation is owned below.
    unsafe { ConvertSidToStringSidW(sid, &raw mut text) }.map_err(io::Error::other)?;
    let _allocation = LocalAllocation(text.0.cast());
    // SAFETY: API returned a terminated UTF-16 string.
    unsafe { text.to_string() }.map_err(io::Error::other)
}

fn create_workspace(path: &Path, token: HANDLE, package: PSID) -> io::Result<()> {
    let mut bytes = 0;
    // SAFETY: query required TokenUser storage size.
    unsafe {
        let _ = GetTokenInformation(token, TokenUser, None, 0, &raw mut bytes);
    }
    let mut storage = vec![0usize; (bytes as usize).div_ceil(size_of::<usize>())];
    // SAFETY: aligned buffer of queried size, token is live.
    unsafe {
        GetTokenInformation(
            token,
            TokenUser,
            Some(storage.as_mut_ptr().cast()),
            bytes,
            &raw mut bytes,
        )
    }
    .map_err(io::Error::other)?;
    // SAFETY: successful TokenUser query initialized a TOKEN_USER at the buffer start.
    let user = unsafe { (*storage.as_ptr().cast::<TOKEN_USER>()).User.Sid };
    let sddl = wide(format!(
        "D:P(A;OICI;FA;;;{})(A;OICI;FA;;;SY)(A;OICI;0x1301bf;;;{})S:(ML;OICI;NW;;;LW)",
        sid_string(user)?,
        sid_string(package)?
    ));
    let mut descriptor = PSECURITY_DESCRIPTOR::default();
    // SAFETY: SDDL contains only converted SIDs, allocated descriptor lives through CreateDirectory.
    unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            PCWSTR(sddl.as_ptr()),
            1,
            &raw mut descriptor,
            None,
        )
    }
    .map_err(io::Error::other)?;
    let _allocation = LocalAllocation(descriptor.0);
    let attributes = SECURITY_ATTRIBUTES {
        nLength: u32::try_from(size_of::<SECURITY_ATTRIBUTES>()).unwrap(),
        lpSecurityDescriptor: descriptor.0,
        bInheritHandle: false.into(),
    };
    let path = wide(path);
    // SAFETY: creates one new directory atomically with protected DACL and low label.
    // Existing directories fail; no ACL on the caller's existing files is edited.
    unsafe { CreateDirectoryW(PCWSTR(path.as_ptr()), Some(&raw const attributes)) }
        .map_err(io::Error::other)
}

#[allow(clippy::too_many_lines)] // Keep the native resource lifetime/order visible in one place.
pub(super) fn run(config: &SandboxConfig, workspace: &Path, command: &str) -> io::Result<u32> {
    if !workspace.is_absolute() || workspace.as_os_str().encode_wide().any(|c| c == 0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Use an absolute path for a new local workspace.",
        ));
    }
    let parent = workspace
        .parent()
        .ok_or_else(|| io::Error::other("Workspace needs a parent directory."))?
        .canonicalize()?;
    // Only a local drive path. No UNC/network shares or arbitrary device namespaces.
    if !matches!(parent.components().next(), Some(std::path::Component::Prefix(prefix)) if matches!(prefix.kind(), std::path::Prefix::Disk(_) | std::path::Prefix::VerbatimDisk(_)))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "The workspace must be on a local drive.",
        ));
    }
    // cmd.exe treats the canonicalizer's verbatim drive prefix as a UNC working
    // directory and silently moves elsewhere. Preserve UTF-16 while removing it.
    let units: Vec<_> = parent.as_os_str().encode_wide().collect();
    let parent = if matches!(parent.components().next(), Some(std::path::Component::Prefix(prefix)) if matches!(prefix.kind(), std::path::Prefix::VerbatimDisk(_)))
    {
        std::path::PathBuf::from(std::ffi::OsString::from_wide(&units[4..]))
    } else {
        parent
    };
    let name = workspace
        .file_name()
        .ok_or_else(|| io::Error::other("Workspace needs a new directory name."))?;
    let workspace = parent.join(name);
    let volume: std::path::PathBuf = parent.components().take(2).collect();
    let volume = wide(volume);
    // SAFETY: terminated volume root. DRIVE_FIXED excludes mapped network drives.
    if unsafe { GetDriveTypeW(PCWSTR(volume.as_ptr())) } != 3
    /* DRIVE_FIXED */
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Use a workspace on a fixed local drive.",
        ));
    }
    let mut profile = Profile::create()
        .map_err(|e| io::Error::other(format!("Sandbox profile setup failed: {e}")))?;
    let mut raw_token = HANDLE::default();
    // SAFETY: pseudohandle is borrowed; returned primary token is owned.
    unsafe {
        OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_QUERY | TOKEN_DUPLICATE,
            &raw mut raw_token,
        )
    }
    .map_err(io::Error::other)?;
    let token = Handle(raw_token);
    let mut raw_restricted = HANDLE::default();
    // SAFETY: privilege removal and LUA filtering only; AppContainer supplies read isolation.
    unsafe {
        CreateRestrictedToken(
            token.0,
            DISABLE_MAX_PRIVILEGE | LUA_TOKEN,
            None,
            None,
            None,
            &raw mut raw_restricted,
        )
    }
    .map_err(io::Error::other)?;
    let restricted = Handle(raw_restricted);
    create_workspace(&workspace, token.0, profile.sid)
        .map_err(|e| io::Error::other(format!("Could not create the sandbox workspace: {e}")))?;

    let mut internet_sid = [0u32; 17];
    let mut sid_bytes = 68;
    // SAFETY: SECURITY_MAX_SID_SIZE bytes with DWORD alignment.
    unsafe {
        CreateWellKnownSid(
            WinCapabilityInternetClientSid,
            None,
            Some(PSID(internet_sid.as_mut_ptr().cast())),
            &raw mut sid_bytes,
        )
    }
    .map_err(io::Error::other)?;
    let mut capability = SID_AND_ATTRIBUTES {
        Sid: PSID(internet_sid.as_mut_ptr().cast()),
        Attributes: 4, /* SE_GROUP_ENABLED */
    };
    let capabilities = SECURITY_CAPABILITIES {
        AppContainerSid: profile.sid,
        Capabilities: if config.allow_network {
            &raw mut capability
        } else {
            std::ptr::null_mut()
        },
        CapabilityCount: u32::from(config.allow_network),
        Reserved: 0,
    };
    let mut attributes = Attributes::new()?;
    attributes.set(PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES, &capabilities)?;
    let lpac_policy = 1u32; // PROCESS_CREATION_ALL_APPLICATION_PACKAGES_OPT_OUT
    attributes.set(
        PROC_THREAD_ATTRIBUTE_ALL_APPLICATION_PACKAGES_POLICY,
        &lpac_policy,
    )?;

    // SAFETY: new unnamed job; noninheritable handle. Set limits before any child exists.
    let job = Handle(unsafe { CreateJobObjectW(None, PCWSTR::null()) }.map_err(io::Error::other)?);
    let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
    limits.BasicLimitInformation.LimitFlags =
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE | JOB_OBJECT_LIMIT_JOB_MEMORY;
    limits.JobMemoryLimit = config.memory_limit_bytes;
    unsafe {
        SetInformationJobObject(
            job.0,
            JobObjectExtendedLimitInformation,
            std::ptr::from_ref(&limits).cast(),
            u32::try_from(size_of_val(&limits)).unwrap(),
        )
    }
    .map_err(io::Error::other)?;

    let mut system = vec![0u16; 32768];
    // SAFETY: valid writable UTF-16 buffer. Never trust inherited COMSPEC/PATH/SystemRoot.
    let length = unsafe { GetSystemDirectoryW(Some(&mut system)) } as usize;
    if length == 0 || length >= system.len() {
        return Err(io::Error::last_os_error());
    }
    let system = std::path::PathBuf::from(std::ffi::OsString::from_wide(&system[..length]));
    let executable = system.join("cmd.exe");
    let app = wide(&executable);
    let mut line = wide(format!(
        "\"{}\" /d /s /c \"{command}\"",
        executable.display()
    ));
    let cwd = wide(&workspace);
    // Allowlist environment only: caller credentials, proxies, startup hooks and PATH are absent.
    let mut environment = Vec::new();
    for entry in [
        format!(
            "LOCALAPPDATA={}",
            std::env::var("LOCALAPPDATA").map_err(|_| io::Error::other(
                "Windows needs LOCALAPPDATA to locate the sandbox profile."
            ))?
        ),
        format!(
            "SystemRoot={}",
            system
                .parent()
                .ok_or_else(|| io::Error::other("Missing Windows directory."))?
                .display()
        ),
        format!("TEMP={}", workspace.display()),
        format!("TMP={}", workspace.display()),
    ] {
        environment.extend(wide(entry));
    }
    environment.push(0);
    let mut startup = STARTUPINFOEXW::default();
    startup.StartupInfo.cb = u32::try_from(size_of::<STARTUPINFOEXW>()).unwrap();
    startup.lpAttributeList = attributes.list;
    let mut info = PROCESS_INFORMATION::default();
    // SAFETY: all pointed-to buffers/attributes are live; no handle inheritance.
    // CREATE_SUSPENDED prevents uncontained code running before job assignment.
    unsafe {
        CreateProcessAsUserW(
            Some(restricted.0),
            PCWSTR(app.as_ptr()),
            Some(PWSTR(line.as_mut_ptr())),
            None,
            None,
            false,
            CREATE_SUSPENDED
                | CREATE_NO_WINDOW
                | CREATE_UNICODE_ENVIRONMENT
                | EXTENDED_STARTUPINFO_PRESENT,
            Some(environment.as_ptr().cast()),
            PCWSTR(cwd.as_ptr()),
            &raw const startup.StartupInfo,
            &raw mut info,
        )
    }
    .map_err(|e| io::Error::other(format!("Could not start the sandbox process: {e}")))?;
    let child = Child {
        process: Handle(info.hProcess),
        thread: Handle(info.hThread),
    };
    // SAFETY: child is suspended and owned; job has kill-on-close and no breakaway flags.
    unsafe { AssignProcessToJobObject(job.0, child.process.0) }.map_err(io::Error::other)?;
    if unsafe { ResumeThread(child.thread.0) } == u32::MAX {
        return Err(io::Error::last_os_error());
    }
    let wait = unsafe { WaitForSingleObject(child.process.0, config.timeout_ms) };
    let result = if wait == WAIT_OBJECT_0 {
        let mut exit = 0;
        unsafe { GetExitCodeProcess(child.process.0, &raw mut exit) }.map_err(io::Error::other)?;
        if exit == 0xc000_0142 {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "Windows could not initialize the sandboxed shell (STATUS_DLL_INIT_FAILED). This desktop or profile is not supported. No unrestricted retry was attempted.",
            ))
        } else {
            Ok(exit)
        }
    } else if wait == WAIT_TIMEOUT {
        Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "Sandbox command timed out. Its process tree was stopped.",
        ))
    } else {
        Err(io::Error::last_os_error())
    };
    drop(job); // kills descendants even when the shell exits before they do
    drop(child);
    profile.delete().map_err(|e| {
        io::Error::other(format!(
            "The command stopped, but sandbox profile cleanup failed: {e}"
        ))
    })?;
    result
}

use std::os::windows::ffi::OsStringExt;
