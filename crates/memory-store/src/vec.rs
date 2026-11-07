//! sqlite-vec registration (the only `unsafe` in the crate).

use std::sync::Once;

static INIT: Once = Once::new();

type ExtInit = unsafe extern "C" fn(
    *mut rusqlite::ffi::sqlite3,
    *mut *mut std::os::raw::c_char,
    *const rusqlite::ffi::sqlite3_api_routines,
) -> std::os::raw::c_int;

/// Register the sqlite-vec extension for all connections opened afterwards.
#[allow(unsafe_code)]
pub(crate) fn register() {
    INIT.call_once(|| {
        // SAFETY: `sqlite3_vec_init` is the documented extension entry point
        // with the `sqlite3_auto_extension` signature; the transmute only
        // erases the (unspecified) fn-pointer type exposed by the -sys crate.
        unsafe {
            rusqlite::ffi::sqlite3_auto_extension(Some(std::mem::transmute::<*const (), ExtInit>(
                sqlite_vec::sqlite3_vec_init as *const (),
            )));
        }
    });
}

/// f32 slice as little-endian bytes (sqlite-vec blob format).
pub(crate) fn to_blob(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|f| f.to_le_bytes()).collect()
}
