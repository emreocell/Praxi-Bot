use std::path::Path;

/// Keep Unix fixtures on the build binary's filesystem so publication can use
/// a hard link without a copy or a temporarily writable executable inode.
pub fn fixture_dir(binary: &Path) -> tempfile::TempDir {
    #[cfg(unix)]
    {
        tempfile::Builder::new()
            .prefix("cli-provider-fixture-")
            .tempdir_in(binary.parent().expect("fixture binary has a parent"))
            .expect("create fixture directory beside binary")
    }
    #[cfg(not(unix))]
    {
        let _ = binary;
        tempfile::tempdir().expect("create fixture directory")
    }
}

/// Executable aliases are immutable. Tests keep mutable state in separate
/// files and must never overwrite or chmod an installed Unix alias.
///
/// Copying an executable while another test forks can leave a writable file
/// descriptor in that child even after the copy returns, causing ETXTBSY in a
/// concurrent exec. Linking the already-built inode opens no writable handle.
pub fn install_binary(binary: &Path, destination: &Path) {
    #[cfg(unix)]
    std::fs::hard_link(binary, destination).expect("hard link immutable fixture binary");
    #[cfg(not(unix))]
    std::fs::copy(binary, destination).expect("copy fixture binary");
}
