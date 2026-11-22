//! Fail-closed trust for the managed `llama-server` folder.
//!
//! The runtime is only ever unpacked from an archive whose sha256 was checked, into a FRESH folder
//! (never one that already has content), and a manifest of the sha256 of every unpacked file is
//! written next to it. Before the program is started the whole folder is re-hashed against that
//! manifest: any changed, missing or extra file (programs and libraries beside the binary count
//! too) or a missing manifest means "refuse, install again". A file that merely exists in the
//! runtime folder is never trusted.
//!
//! Residual risk, stated honestly: the manifest lives in the same user-owned data folder, so an
//! attacker who can already write there as this user can forge it too; this stops planted or
//! replaced files from other sources (leftovers, partial installs, other low-privilege writers
//! where the OS ACLs allow) and shrinks the check-to-spawn window to the hashing time right
//! before `Command::spawn`. A file swapped in that window by a same-user process is not detected.

use std::{
    fs,
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
};

use sha2::{Digest, Sha256};

use super::{
    download::sha256_file,
    extract::{ExtractError, extract_file, find_binary},
    manifest::ArchiveKind,
};

/// Name of the manifest inside an install folder.
pub const MANIFEST_NAME: &str = "praxi-runtime.manifest";
const MAX_FILES: usize = 5_000;

/// Why a runtime folder is not trusted.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum TrustError {
    #[error("the downloaded program does not match its checksum")]
    ArchiveMismatch,
    #[error("the installed program was changed or is incomplete; install it again")]
    Untrusted,
    #[error("could not unpack the program: {0}")]
    Extract(String),
    #[error("could not write the files: {0}")]
    Io(String),
}

impl From<std::io::Error> for TrustError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e.kind().to_string())
    }
}

impl From<ExtractError> for TrustError {
    fn from(e: ExtractError) -> Self {
        Self::Extract(e.to_string())
    }
}

fn rel_string(root: &Path, p: &Path) -> Option<String> {
    let rel = p.strip_prefix(root).ok()?;
    let mut parts = Vec::new();
    for c in rel.components() {
        let s = c.as_os_str().to_str()?;
        if s.contains(['\n', '\r', '\t']) {
            return None;
        }
        parts.push(s.to_owned());
    }
    Some(parts.join("/"))
}

/// Every regular file under `root` (sorted, relative, `/`-separated). Symlinks, devices or odd
/// names make the folder untrusted.
fn list_files(root: &Path) -> Result<Vec<(String, PathBuf)>, TrustError> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            let ty = entry.file_type()?;
            let path = entry.path();
            if ty.is_dir() {
                stack.push(path);
            } else if ty.is_file() {
                let rel = rel_string(root, &path).ok_or(TrustError::Untrusted)?;
                out.push((rel, path));
            } else {
                return Err(TrustError::Untrusted);
            }
            if out.len() > MAX_FILES {
                return Err(TrustError::Untrusted);
            }
        }
    }
    out.retain(|(rel, _)| rel != MANIFEST_NAME);
    out.sort();
    Ok(out)
}

fn render_manifest(entries: &[(String, String)]) -> String {
    let mut s = String::from("praxi-runtime-manifest v1\n");
    for (rel, sha) in entries {
        s.push_str(sha);
        s.push('\t');
        s.push_str(rel);
        s.push('\n');
    }
    s
}

fn parse_manifest(text: &str) -> Option<Vec<(String, String)>> {
    let mut lines = text.lines();
    if lines.next()? != "praxi-runtime-manifest v1" {
        return None;
    }
    let mut out = Vec::new();
    for l in lines {
        let (sha, rel) = l.split_once('\t')?;
        out.push((rel.to_owned(), sha.to_owned()));
    }
    out.sort();
    Some(out)
}

fn restrict_dir(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o700));
    }
    #[cfg(not(unix))]
    let _ = path; // Windows: inherits the user-profile ACL of the data folder.
}

/// Hashes `archive` once on an open handle, checks it against `expected_sha256`, then unpacks
/// that same handle into a brand-new folder under `runtime_dir`, and writes the manifest.
/// Returns the path of the verified `binary_name`. On any failure the new folder is removed.
///
/// # Errors
/// See [`TrustError`].
pub fn install_verified(
    kind: ArchiveKind,
    archive: &Path,
    expected_sha256: &str,
    runtime_dir: &Path,
    binary_name: &str,
) -> Result<PathBuf, TrustError> {
    fs::create_dir_all(runtime_dir)?;
    let mut open = fs::OpenOptions::new();
    open.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        open.share_mode(1); // FILE_SHARE_READ: nobody can write it while we hold it
    }
    let mut file = open.open(archive)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    let got = format!("{:x}", hasher.finalize());
    if !got.eq_ignore_ascii_case(expected_sha256) {
        return Err(TrustError::ArchiveMismatch);
    }
    file.seek(SeekFrom::Start(0))?;

    // A fresh folder: `create_dir` fails if the name exists, so nothing old is ever reused.
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let dest = runtime_dir.join(format!("i-{nanos:x}-{}", std::process::id()));
    fs::create_dir(&dest)?;
    restrict_dir(&dest);
    let result = (|| {
        extract_file(kind, file, &dest)?;
        let binary = find_binary(&dest, binary_name).ok_or(TrustError::Untrusted)?;
        let mut entries = Vec::new();
        for (rel, path) in list_files(&dest)? {
            entries.push((rel, sha256_file(&path)?));
        }
        let mut manifest = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(dest.join(MANIFEST_NAME))?;
        std::io::Write::write_all(&mut manifest, render_manifest(&entries).as_bytes())?;
        Ok(binary)
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(&dest);
    }
    result
}

/// Re-hashes the whole install folder that contains `binary` against its manifest.
///
/// # Errors
/// [`TrustError::Untrusted`] on a missing manifest or any difference.
pub fn verify_install(binary: &Path) -> Result<(), TrustError> {
    let root = install_root(binary).ok_or(TrustError::Untrusted)?;
    let text = fs::read_to_string(root.join(MANIFEST_NAME)).map_err(|_| TrustError::Untrusted)?;
    let want = parse_manifest(&text).ok_or(TrustError::Untrusted)?;
    let rel_bin = rel_string(&root, binary).ok_or(TrustError::Untrusted)?;
    if !want.iter().any(|(rel, _)| *rel == rel_bin) {
        return Err(TrustError::Untrusted);
    }
    let have = list_files(&root).map_err(|_| TrustError::Untrusted)?;
    if have.len() != want.len() {
        return Err(TrustError::Untrusted);
    }
    for ((rel, path), (wrel, wsha)) in have.iter().zip(&want) {
        if rel != wrel {
            return Err(TrustError::Untrusted);
        }
        let got = sha256_file(path).map_err(|_| TrustError::Untrusted)?;
        if !got.eq_ignore_ascii_case(wsha) {
            return Err(TrustError::Untrusted);
        }
    }
    Ok(())
}

/// The install folder (the one holding the manifest) above `binary`.
fn install_root(binary: &Path) -> Option<PathBuf> {
    binary
        .ancestors()
        .skip(1)
        .find(|a| a.join(MANIFEST_NAME).is_file())
        .map(Path::to_path_buf)
}

/// An already installed binary that passes [`verify_install`], if any. Folders without a valid
/// manifest (planted files, partial installs) are ignored, never used.
#[must_use]
pub fn verified_binary(runtime_dir: &Path, binary_name: &str) -> Option<PathBuf> {
    for entry in fs::read_dir(runtime_dir).ok()?.flatten() {
        let dir = entry.path();
        if !entry.file_type().is_ok_and(|t| t.is_dir()) || !dir.join(MANIFEST_NAME).is_file() {
            continue;
        }
        if let Some(bin) = find_binary(&dir, binary_name)
            && verify_install(&bin).is_ok()
        {
            return Some(bin);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    fn zip_bytes() -> Vec<u8> {
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut w = zip::ZipWriter::new(&mut buf);
            for (n, d) in [("b/llama-server", &b"real"[..]), ("b/lib.so", b"lib")] {
                w.start_file(n, zip::write::SimpleFileOptions::default())
                    .unwrap();
                w.write_all(d).unwrap();
            }
            w.finish().unwrap();
        }
        buf.into_inner()
    }

    fn installed(dir: &Path) -> (PathBuf, PathBuf) {
        let bytes = zip_bytes();
        let archive = dir.join("a.zip");
        fs::write(&archive, &bytes).unwrap();
        let sha = format!("{:x}", Sha256::digest(&bytes));
        let rt = dir.join("runtime");
        let bin = install_verified(ArchiveKind::Zip, &archive, &sha, &rt, "llama-server").unwrap();
        (rt, bin)
    }

    #[test]
    fn a_fresh_verified_install_passes_and_is_found_again() {
        let d = tempfile::tempdir().unwrap();
        let (rt, bin) = installed(d.path());
        verify_install(&bin).unwrap();
        assert_eq!(verified_binary(&rt, "llama-server"), Some(bin));
    }

    #[test]
    fn a_planted_binary_without_a_manifest_is_never_trusted() {
        let d = tempfile::tempdir().unwrap();
        let rt = d.path().join("runtime");
        fs::create_dir_all(rt.join("x")).unwrap();
        fs::write(rt.join("x").join("llama-server"), b"evil").unwrap();
        assert_eq!(verified_binary(&rt, "llama-server"), None);
        assert_eq!(
            verify_install(&rt.join("x").join("llama-server")),
            Err(TrustError::Untrusted)
        );
    }

    #[test]
    fn a_binary_changed_after_install_is_refused() {
        let d = tempfile::tempdir().unwrap();
        let (rt, bin) = installed(d.path());
        fs::write(&bin, b"evil").unwrap();
        assert_eq!(verify_install(&bin), Err(TrustError::Untrusted));
        assert_eq!(verified_binary(&rt, "llama-server"), None);
    }

    #[test]
    fn an_extra_or_changed_library_next_to_the_binary_is_refused() {
        let d = tempfile::tempdir().unwrap();
        let (_, bin) = installed(d.path());
        let extra = bin.parent().unwrap().join("evil.dll");
        fs::write(&extra, b"x").unwrap();
        assert_eq!(verify_install(&bin), Err(TrustError::Untrusted));
        fs::remove_file(extra).unwrap();
        verify_install(&bin).unwrap();
        fs::write(bin.parent().unwrap().join("lib.so"), b"evil").unwrap();
        assert_eq!(verify_install(&bin), Err(TrustError::Untrusted));
    }

    #[test]
    fn a_missing_manifest_is_refused() {
        let d = tempfile::tempdir().unwrap();
        let (_, bin) = installed(d.path());
        let root = install_root(&bin).unwrap();
        fs::remove_file(root.join(MANIFEST_NAME)).unwrap();
        assert_eq!(verify_install(&bin), Err(TrustError::Untrusted));
    }

    #[test]
    fn an_archive_with_the_wrong_hash_is_not_unpacked() {
        let d = tempfile::tempdir().unwrap();
        let archive = d.path().join("a.zip");
        fs::write(&archive, zip_bytes()).unwrap();
        let rt = d.path().join("runtime");
        let err = install_verified(
            ArchiveKind::Zip,
            &archive,
            &"0".repeat(64),
            &rt,
            "llama-server",
        )
        .unwrap_err();
        assert_eq!(err, TrustError::ArchiveMismatch);
        assert!(fs::read_dir(&rt).unwrap().next().is_none());
    }

    #[test]
    fn a_second_install_gets_its_own_folder_and_leaves_planted_files_alone() {
        let d = tempfile::tempdir().unwrap();
        let (rt, first) = installed(d.path());
        fs::write(&first, b"evil").unwrap();
        let bytes = zip_bytes();
        let sha = format!("{:x}", Sha256::digest(&bytes));
        let second = install_verified(
            ArchiveKind::Zip,
            &d.path().join("a.zip"),
            &sha,
            &rt,
            "llama-server",
        )
        .unwrap();
        assert_ne!(first.parent(), second.parent());
        assert_eq!(fs::read(&second).unwrap(), b"real");
        assert_eq!(verified_binary(&rt, "llama-server"), Some(second));
    }
}
