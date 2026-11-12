//! Seal the consumed bootstrap descriptor before any untrusted children start.
//! Windows Command defaults can inherit unrelated inheritable handles; an
//! explicit flag matters even when a child's own stdin is redirected.
#![cfg(windows)]
#![allow(unsafe_code)]

use std::os::windows::io::{AsHandle, AsRawHandle};
use windows::Win32::Foundation::{HANDLE, HANDLE_FLAG_INHERIT, HANDLE_FLAGS, SetHandleInformation};

pub(crate) fn seal_stdin() -> std::io::Result<()> {
    seal(HANDLE(std::io::stdin().as_handle().as_raw_handle()))
}

fn seal(handle: HANDLE) -> std::io::Result<()> {
    // SAFETY: the caller holds the live stdin/pipe handle. This only clears
    // its inheritance flag; it neither closes nor reads the descriptor.
    unsafe { SetHandleInformation(handle, HANDLE_FLAG_INHERIT.0, HANDLE_FLAGS::default()) }
        .map_err(std::io::Error::other)
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::Foundation::GetHandleInformation;

    #[test]
    fn bootstrap_handle_is_explicitly_non_inheritable() {
        let (reader, _writer) = std::io::pipe().unwrap();
        let handle = HANDLE(reader.as_handle().as_raw_handle());
        // SAFETY: reader owns this pipe handle throughout the test; only its
        // inheritance metadata is changed. No real credential is present.
        unsafe { SetHandleInformation(handle, HANDLE_FLAG_INHERIT.0, HANDLE_FLAG_INHERIT) }
            .unwrap();
        seal(handle).unwrap();
        let mut flags = 0;
        // SAFETY: same live, owned handle; flags is a writable u32.
        unsafe { GetHandleInformation(handle, &raw mut flags) }.unwrap();
        assert_eq!(flags & HANDLE_FLAG_INHERIT.0, 0);
    }
}
