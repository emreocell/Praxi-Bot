//! Unpacks a downloaded runtime archive (zip or tar.gz) into a folder we own.
//!
//! Defensive: only regular files and directories are written (symlinks and devices are skipped),
//! paths that are absolute or climb out of the target (`..`) are refused, and the number and total
//! size of entries are capped. Blocking; call from `spawn_blocking`.

use std::{
    fs,
    io::{self, Read},
    path::{Component, Path, PathBuf},
};

use crate::local_runtime::manifest::ArchiveKind;

const MAX_ENTRIES: usize = 5_000;
const MAX_TOTAL_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// Why unpacking failed.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ExtractError {
    #[error("the archive contains an unsafe path and was not unpacked")]
    UnsafePath,
    #[error("the archive is larger or has more files than expected")]
    TooBig,
    #[error("the archive is damaged: {0}")]
    Corrupt(String),
    #[error("could not write the files: {0}")]
    Io(String),
}

impl From<io::Error> for ExtractError {
    fn from(e: io::Error) -> Self {
        Self::Io(e.kind().to_string())
    }
}

/// A relative path made only of normal components, or `None`.
fn safe_relative(path: &Path) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::Normal(p) => out.push(p),
            Component::CurDir => {}
            _ => return None,
        }
    }
    (!out.as_os_str().is_empty()).then_some(out)
}

fn write_capped(
    reader: &mut dyn Read,
    target: &Path,
    total: &mut u64,
    unix_mode: Option<u32>,
) -> Result<(), ExtractError> {
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)?;
    }
    // create_new: never write through an existing file or symlink.
    let mut out = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(target)?;
    let budget = MAX_TOTAL_BYTES.saturating_sub(*total);
    let copied = io::copy(&mut reader.take(budget + 1), &mut out)?;
    *total += copied;
    if copied > budget {
        return Err(ExtractError::TooBig);
    }
    #[cfg(unix)]
    if let Some(mode) = unix_mode {
        use std::os::unix::fs::PermissionsExt;
        // Keep only the owner's execute bit decision from the archive; never setuid/setgid.
        fs::set_permissions(target, fs::Permissions::from_mode(mode & 0o755))?;
    }
    #[cfg(not(unix))]
    let _ = unix_mode;
    Ok(())
}

/// Unpacks `archive` into `dest` (created if missing).
///
/// # Errors
/// See [`ExtractError`].
pub fn extract_archive(kind: ArchiveKind, archive: &Path, dest: &Path) -> Result<(), ExtractError> {
    extract_file(kind, fs::File::open(archive)?, dest)
}

/// Like [`extract_archive`] but from an already opened (and, by the caller, already hashed) file.
pub(crate) fn extract_file(
    kind: ArchiveKind,
    file: fs::File,
    dest: &Path,
) -> Result<(), ExtractError> {
    fs::create_dir_all(dest)?;
    match kind {
        ArchiveKind::Zip => extract_zip(file, dest),
        ArchiveKind::TarGz => extract_tar_gz(file, dest),
    }
}

fn extract_zip(file: fs::File, dest: &Path) -> Result<(), ExtractError> {
    let mut zip = zip::ZipArchive::new(file).map_err(|e| ExtractError::Corrupt(e.to_string()))?;
    if zip.len() > MAX_ENTRIES {
        return Err(ExtractError::TooBig);
    }
    let mut total = 0u64;
    for i in 0..zip.len() {
        let mut entry = zip
            .by_index(i)
            .map_err(|e| ExtractError::Corrupt(e.to_string()))?;
        let name = entry.name().map_err(|_| ExtractError::UnsafePath)?;
        let rel = safe_relative(Path::new(name.as_ref())).ok_or(ExtractError::UnsafePath)?;
        if entry.is_symlink() {
            continue;
        }
        let target = dest.join(rel);
        if entry.is_dir() {
            fs::create_dir_all(&target)?;
        } else {
            let mode = entry.unix_mode();
            write_capped(&mut entry, &target, &mut total, mode)?;
        }
    }
    Ok(())
}

fn extract_tar_gz(file: fs::File, dest: &Path) -> Result<(), ExtractError> {
    let gz = flate2::read::GzDecoder::new(file);
    let mut tar = tar::Archive::new(gz);
    let mut total = 0u64;
    let mut count = 0usize;
    for entry in tar
        .entries()
        .map_err(|e| ExtractError::Corrupt(e.to_string()))?
    {
        let mut entry = entry.map_err(|e| ExtractError::Corrupt(e.to_string()))?;
        count += 1;
        if count > MAX_ENTRIES {
            return Err(ExtractError::TooBig);
        }
        let rel = safe_relative(&entry.path()?).ok_or(ExtractError::UnsafePath)?;
        let target = dest.join(rel);
        let ty = entry.header().entry_type();
        if ty.is_dir() {
            fs::create_dir_all(&target)?;
        } else if ty.is_file() {
            let mode = entry.header().mode().ok();
            write_capped(&mut entry, &target, &mut total, mode)?;
        }
        // Symlinks, hard links and devices are skipped on purpose.
    }
    Ok(())
}

/// Finds `file_name` anywhere under `root` (the archives nest one folder deep).
#[must_use]
pub fn find_binary(root: &Path, file_name: &str) -> Option<PathBuf> {
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).ok()?.flatten() {
            let path = entry.path();
            let Ok(ty) = entry.file_type() else { continue };
            if ty.is_dir() {
                stack.push(path);
            } else if ty.is_file() && entry.file_name() == file_name {
                return Some(path);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    fn zip_with(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut buf = io::Cursor::new(Vec::new());
        {
            let mut w = zip::ZipWriter::new(&mut buf);
            for (name, data) in entries {
                w.start_file(*name, zip::write::SimpleFileOptions::default())
                    .unwrap();
                w.write_all(data).unwrap();
            }
            w.finish().unwrap();
        }
        buf.into_inner()
    }

    #[test]
    fn unpacks_a_zip_and_finds_the_binary() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("a.zip");
        fs::write(
            &archive,
            zip_with(&[
                ("llama/llama-server.exe", b"bin"),
                ("llama/ggml.dll", b"dll"),
            ]),
        )
        .unwrap();
        let out = dir.path().join("out");
        extract_archive(ArchiveKind::Zip, &archive, &out).unwrap();
        let bin = find_binary(&out, "llama-server.exe").expect("binary");
        assert_eq!(fs::read(bin).unwrap(), b"bin");
    }

    #[test]
    fn a_zip_that_climbs_out_of_the_folder_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("evil.zip");
        fs::write(&archive, zip_with(&[("../evil.txt", b"x")])).unwrap();
        let out = dir.path().join("out");
        let err = extract_archive(ArchiveKind::Zip, &archive, &out).unwrap_err();
        assert_eq!(err, ExtractError::UnsafePath);
        assert!(!dir.path().join("evil.txt").exists());
    }

    #[test]
    fn unpacks_a_tar_gz_and_skips_symlinks() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("a.tar.gz");
        {
            let gz = flate2::write::GzEncoder::new(
                fs::File::create(&archive).unwrap(),
                flate2::Compression::fast(),
            );
            let mut b = tar::Builder::new(gz);
            let mut h = tar::Header::new_gnu();
            h.set_size(3);
            h.set_mode(0o755);
            h.set_cksum();
            b.append_data(&mut h, "llama/llama-server", &b"bin"[..])
                .unwrap();
            let mut l = tar::Header::new_gnu();
            l.set_entry_type(tar::EntryType::Symlink);
            l.set_size(0);
            l.set_cksum();
            b.append_link(&mut l, "llama/link", "/etc/passwd").unwrap();
            b.into_inner().unwrap().finish().unwrap();
        }
        let out = dir.path().join("out");
        extract_archive(ArchiveKind::TarGz, &archive, &out).unwrap();
        assert!(find_binary(&out, "llama-server").is_some());
        assert!(find_binary(&out, "link").is_none());
    }
}
